use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use grammers_client::Client;

use crate::config::{Config, OutputChannelConfig};
use crate::{fetch, fetch_tg, generate, models, store, telegram};

/// How to determine the generation time window.
pub enum TimeWindow {
    /// Relative duration from now (e.g., --since 7d).
    Since(Duration),
    /// Exact timestamps (e.g., --from ... --to ...).
    Explicit { from: DateTime<Utc>, to: DateTime<Utc> },
}

/// Result of a successful pipeline run.
pub struct PipelineResult {
    pub article: models::GeneratedArticle,
    pub raw_output: String,
}

/// Run the full generation pipeline for a single output channel.
///
/// If `fetch_content` is true, fetches RSS feeds and TG history before generation (CLI mode).
/// If false, assumes the poller/listener has already fetched content (daemon mode).
///
/// Returns `None` if no content items were found (generation skipped).
pub async fn run_generation(
    pool: &SqlitePool,
    config: &Config,
    channel_config: &OutputChannelConfig,
    time_window: Option<TimeWindow>,
    fetch_content: bool,
    tg_client: Option<&Client>,
    cancel: CancellationToken,
) -> Result<Option<PipelineResult>> {
    let channel = store::get_channel_by_slug(pool, &channel_config.slug)
        .await
        .context("looking up output channel")?
        .ok_or_else(|| anyhow::anyhow!("no output channel with slug '{}'", channel_config.slug))?;

    let source_ids = store::get_channel_source_ids(pool, &channel.id)
        .await
        .context("getting channel source IDs")?;

    if source_ids.is_empty() {
        warn!(channel = %channel.name, "no sources configured for this channel");
        return Ok(None);
    }

    let all_sources = store::get_sources_by_ids(pool, &source_ids)
        .await
        .context("getting sources")?;

    let sources: Vec<_> = all_sources.into_iter().filter(|s| s.enabled).collect();
    let source_ids: Vec<String> = sources.iter().map(|s| s.id.clone()).collect();

    if cancel.is_cancelled() {
        return Ok(None);
    }

    // Determine time window (needed before fetching so TG history knows the boundary)
    let now = Utc::now();
    let is_override = time_window.is_some();
    let (covers_from, covers_to) = match time_window {
        Some(TimeWindow::Since(d)) => {
            let duration = chrono::Duration::from_std(d).unwrap_or(chrono::Duration::days(7));
            (now - duration, now)
        }
        Some(TimeWindow::Explicit { from, to }) => (from, to),
        None => {
            let from = if let Some(ref last_gen) = channel.last_generated {
                *last_gen
            } else {
                now - chrono::Duration::days(7)
            };
            (from, now)
        }
    };

    info!(
        from = %covers_from.to_rfc3339(),
        to = %covers_to.to_rfc3339(),
        "content time window"
    );

    // One-shot content fetching (CLI mode only)
    if fetch_content {
        // RSS feeds
        let rss_sources: Vec<_> = sources.iter().filter(|s| s.source_type == "rss").collect();
        info!(count = rss_sources.len(), "fetching RSS sources");

        for source in &rss_sources {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            match fetch::fetch_rss_source(source).await {
                Ok(result) => {
                    let count = result.items.len();
                    for item in result.items {
                        store::upsert_content_item(pool, &item)
                            .await
                            .context("storing content item")?;
                    }
                    // Save fetch state (ETag, Last-Modified, last_fetched_at) so conditional
                    // GETs work on subsequent runs and the daemon poller knows when we last fetched
                    store::update_source_fetch_state(
                        pool,
                        &source.id,
                        Utc::now(),
                        result.etag.as_deref(),
                        result.last_modified.as_deref(),
                    )
                    .await
                    .context("updating source fetch state")?;
                    info!(source = %source.name, items = count, "fetched and stored items");
                }
                Err(e) => {
                    warn!(source = %source.name, error = %e, "failed to fetch source");
                }
            }
        }

        // TG message history
        if let Some(client) = tg_client {
            let tg_sources: Vec<_> = sources
                .iter()
                .filter(|s| s.source_type.starts_with("telegram_"))
                .cloned()
                .collect();
            if !tg_sources.is_empty() {
                info!(count = tg_sources.len(), "fetching TG source history");
                fetch_tg::fetch_tg_sources(client, pool, &tg_sources, covers_from, &cancel)
                    .await
                    .context("fetching TG sources")?;
            }
        }
    }

    let items = store::get_items_in_window(pool, &source_ids, covers_from, covers_to)
        .await
        .context("querying content items")?;

    if items.is_empty() {
        let source_names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
        warn!(
            channel = %channel.name,
            from = %covers_from.to_rfc3339(),
            to = %covers_to.to_rfc3339(),
            sources = ?source_names,
            "no content items in time window, skipping generation"
        );
        // Update last_generated so the next run doesn't re-check this empty window (PRD §9.7)
        if !is_override {
            store::update_last_generated(pool, &channel.id, covers_to)
                .await
                .context("updating last_generated")?;
        }
        return Ok(None);
    }

    info!(items = items.len(), "content items collected for generation");

    let source_map: std::collections::HashMap<String, &models::Source> =
        sources.iter().map(|s| (s.id.clone(), s)).collect();

    // Gather folder channel maps for per-channel workspace splitting
    let mut folder_channels: std::collections::HashMap<
        String,
        std::collections::HashMap<i64, (String, Option<String>)>,
    > = std::collections::HashMap::new();
    for source in &sources {
        if source.source_type == "telegram_folder" {
            let channels = store::get_folder_channel_map(pool, &source.id)
                .await
                .context("getting folder channel map")?;
            folder_channels.insert(source.id.clone(), channels);
        }
    }

    if cancel.is_cancelled() {
        return Ok(None);
    }

    // Generate with retry
    let max_retries = config.opencode.max_retries;
    let mut last_err = None;
    let mut result = None;

    for attempt in 0..=max_retries {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        if attempt > 0 {
            let delay = std::time::Duration::from_secs(30);
            warn!(attempt, delay_secs = 30, "retrying generation");
            tokio::select! {
                _ = cancel.cancelled() => return Ok(None),
                _ = tokio::time::sleep(delay) => {}
            }
        }

        match generate::generate_article(
            config,
            channel_config,
            &channel,
            &items,
            &source_map,
            &folder_channels,
            covers_from,
            covers_to,
            cancel.clone(),
        )
        .await
        {
            Ok(r) => {
                result = Some(r);
                break;
            }
            Err(e) => {
                error!(attempt, error = %e, "generation failed");
                last_err = Some(e);
            }
        }
    }

    let (article, raw_output) = match result {
        Some(r) => r,
        None => return Err(last_err.unwrap().context("generation failed after all retries")),
    };

    // Store article
    store::insert_generated_article(pool, &article)
        .await
        .context("storing generated article")?;

    // Mark TG channels as read if configured (PRD §10.7)
    if channel_config.mark_tg_read.unwrap_or(false) {
        if let Some(client) = tg_client {
            telegram::mark_channels_as_read(client, pool, &items).await;
        } else {
            warn!(channel = %channel.name, "mark_tg_read is enabled but no Telegram client available");
        }
    }

    // Update last_generated (skip for --since/--from/--to overrides)
    if !is_override {
        store::update_last_generated(pool, &channel.id, covers_to)
            .await
            .context("updating last_generated")?;
    }

    info!(title = %article.title, "article generated successfully");

    Ok(Some(PipelineResult { article, raw_output }))
}

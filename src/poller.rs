use chrono::Utc;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{fetch, store};

/// Global minimum poll interval to prevent abuse (see docs/specs/rss-sources.md "Polling").
const MIN_POLL_INTERVAL_SECS: i64 = 300; // 5 minutes

/// RSS polling loop. Wakes every 60 seconds and fetches due sources.
pub async fn polling_loop(pool: SqlitePool, cancel: CancellationToken) {
    info!("RSS poller started");
    // Short initial delay before first poll cycle
    tokio::select! {
        _ = cancel.cancelled() => return,
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("RSS poller shutting down");
                return;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
        }

        let sources = match store::get_all_enabled_sources(&pool).await {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "failed to load sources for polling");
                continue;
            }
        };

        let now = Utc::now();
        let min_interval = chrono::Duration::seconds(MIN_POLL_INTERVAL_SECS);

        for source in &sources {
            if source.source_type != "rss" {
                continue;
            }

            // Check if poll_interval has elapsed since last fetch
            let poll_interval = match humantime::parse_duration(&source.poll_interval) {
                Ok(d) => {
                    let dur = chrono::Duration::from_std(d).unwrap_or(chrono::Duration::minutes(30));
                    // Enforce global minimum (see docs/specs/rss-sources.md "Polling")
                    if dur < min_interval { min_interval } else { dur }
                }
                Err(_) => chrono::Duration::minutes(30),
            };

            if let Some(ref last_fetched) = source.last_fetched_at
                && now - *last_fetched < poll_interval
            {
                debug!(source = %source.name, "not due for polling yet");
                continue;
            }

            if cancel.is_cancelled() {
                return;
            }

            info!(source = %source.name, "polling RSS feed");

            let (etag, last_modified) = match fetch::fetch_rss_source(source).await {
                Ok(result) => {
                    let count = result.items.len();
                    for item in result.items {
                        if let Err(e) = store::upsert_content_item(&pool, &item).await {
                            warn!(source = %source.name, error = %e, "failed to store content item");
                        }
                    }
                    if count > 0 {
                        info!(source = %source.name, items = count, "polled and stored items");
                    }
                    (result.etag, result.last_modified)
                }
                Err(e) => {
                    warn!(source = %source.name, error = %e, "RSS fetch failed");
                    (source.last_etag.clone(), source.last_modified_header.clone())
                }
            };

            // Update last_fetched_at + cache headers regardless of success (avoid hammering broken feeds)
            if let Err(e) =
                store::update_source_fetch_state(&pool, &source.id, now, etag.as_deref(), last_modified.as_deref()).await
            {
                error!(source = %source.name, error = %e, "failed to update source fetch state");
            }
        }
    }
}

mod cli;
mod config;
mod db;
mod error;
mod fetch;
mod generate;
mod models;
mod store;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{error, info, warn};

use crate::cli::{Cli, Commands};
use crate::config::{load_config, validate_config};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let config = load_config(&cli.config).with_context(|| format!("loading config from {}", cli.config.display()))?;

    // Initialize tracing
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.pail.log_level));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    info!(config_path = %cli.config.display(), "config loaded");

    validate_config(&config).context("config validation failed")?;
    info!("config validated successfully");

    match cli.command {
        Commands::Validate => {
            println!("Configuration is valid.");
        }
        Commands::Generate { slug, output, since } => {
            // Parse --since if provided
            let since_duration = if let Some(ref since_str) = since {
                Some(
                    humantime::parse_duration(since_str)
                        .with_context(|| format!("invalid --since duration: '{since_str}'"))?,
                )
            } else {
                None
            };

            let pool = db::create_pool(&config).await.context("creating database")?;
            info!(db_path = %config.db_path().display(), "database ready");

            // Sync config to DB
            store::sync_config_to_db(&pool, &config)
                .await
                .context("syncing config to database")?;
            info!("config synced to database");

            // Find the output channel
            let channel = store::get_channel_by_slug(&pool, &slug)
                .await
                .context("looking up output channel")?
                .ok_or_else(|| anyhow::anyhow!("no output channel with slug '{slug}'"))?;

            // Get the channel config for prompt/model info
            let channel_config = config
                .output_channel
                .iter()
                .find(|c| c.slug == slug)
                .ok_or_else(|| anyhow::anyhow!("no output channel config for slug '{slug}'"))?;

            // Get source IDs for this channel
            let source_ids = store::get_channel_source_ids(&pool, &channel.id)
                .await
                .context("getting channel source IDs")?;

            if source_ids.is_empty() {
                warn!(channel = %channel.name, "no sources configured for this channel");
                return Ok(());
            }

            // Fetch RSS feeds for all sources (only enabled ones)
            let all_sources = store::get_sources_by_ids(&pool, &source_ids)
                .await
                .context("getting sources")?;

            let sources: Vec<_> = all_sources.into_iter().filter(|s| s.enabled).collect();
            let source_ids: Vec<String> = sources.iter().map(|s| s.id.clone()).collect();

            let rss_sources: Vec<_> = sources.iter().filter(|s| s.source_type == "rss").collect();

            info!(count = rss_sources.len(), "fetching RSS sources");

            for source in &rss_sources {
                match fetch::fetch_rss_source(source).await {
                    Ok(items) => {
                        let count = items.len();
                        for item in items {
                            store::upsert_content_item(&pool, &item)
                                .await
                                .context("storing content item")?;
                        }
                        info!(source = %source.name, items = count, "fetched and stored items");
                    }
                    Err(e) => {
                        warn!(source = %source.name, error = %e, "failed to fetch source");
                    }
                }
            }

            // Determine time window
            let now = chrono::Utc::now();
            let covers_from = if let Some(duration) = since_duration {
                now - chrono::Duration::from_std(duration).context("converting duration")?
            } else if let Some(ref last_gen) = channel.last_generated {
                *last_gen
            } else {
                // First run: default to last 7 days
                now - chrono::Duration::days(7)
            };

            info!(
                from = %covers_from.to_rfc3339(),
                to = %now.to_rfc3339(),
                "content time window"
            );

            // Collect content items in the window
            let items = store::get_items_in_window(&pool, &source_ids, covers_from, now)
                .await
                .context("querying content items")?;

            if items.is_empty() {
                warn!(
                    channel = %channel.name,
                    from = %covers_from.to_rfc3339(),
                    to = %now.to_rfc3339(),
                    "no content items in time window, skipping generation"
                );

                // Still update last_generated so the next run doesn't re-check this empty window (PRD §9.7)
                if since.is_none() {
                    store::update_last_generated(&pool, &channel.id, now)
                        .await
                        .context("updating last_generated")?;
                }

                return Ok(());
            }

            info!(items = items.len(), "content items collected for generation");

            // Build source map for workspace
            let source_map: std::collections::HashMap<String, &models::Source> =
                sources.iter().map(|s| (s.id.clone(), s)).collect();

            // Generate with retry
            let max_retries = config.opencode.max_retries;
            let mut last_err = None;
            let mut result = None;

            for attempt in 0..=max_retries {
                if attempt > 0 {
                    let delay = std::time::Duration::from_secs(30);
                    warn!(attempt, delay_secs = 30, "retrying generation");
                    tokio::time::sleep(delay).await;
                }

                match generate::generate_article(
                    &config,
                    channel_config,
                    &channel,
                    &items,
                    &source_map,
                    covers_from,
                    now,
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
            store::insert_generated_article(&pool, &article)
                .await
                .context("storing generated article")?;

            // Only update last_generated for normal runs, not --since overrides
            if since.is_none() {
                store::update_last_generated(&pool, &channel.id, now)
                    .await
                    .context("updating last_generated")?;
            }

            info!(title = %article.title, "article generated successfully");

            // Write output file if requested (raw output.md exactly as opencode wrote it)
            if let Some(output_path) = output {
                std::fs::write(&output_path, &raw_output)
                    .with_context(|| format!("writing output to {}", output_path.display()))?;
                info!(path = %output_path.display(), "wrote markdown output");
                println!("Article written to: {}", output_path.display());
            } else {
                println!("Article generated: {}", article.title);
            }
        }
    }

    Ok(())
}

mod cleanup;
mod cli;
mod config;
mod daemon;
mod db;
mod error;
mod fetch;
mod fetch_tg;
mod generate;
mod models;
mod pipeline;
mod poller;
mod scheduler;
mod server;
mod store;
mod telegram;
mod tg_listener;
mod tg_session;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;

use crate::cli::{Cli, Commands, TgCommands};
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
        Some(Commands::Validate) => {
            println!("Configuration is valid.");
        }
        Some(Commands::Generate { slug, output, since }) => {
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

            store::sync_config_to_db(&pool, &config)
                .await
                .context("syncing config to database")?;
            info!("config synced to database");

            let channel_config = config
                .output_channel
                .iter()
                .find(|c| c.slug == slug)
                .ok_or_else(|| anyhow::anyhow!("no output channel config for slug '{slug}'"))?;

            let cancel = tokio_util::sync::CancellationToken::new();

            // Check if this channel has TG sources
            let has_tg_sources = channel_config.sources.iter().any(|name| {
                config
                    .source
                    .iter()
                    .any(|s| s.name == *name && s.source_type.starts_with("telegram_"))
            });

            let tg_conn = if has_tg_sources && config.telegram.enabled {
                if config.telegram.api_id.is_none() || config.telegram.api_hash.is_none() {
                    anyhow::bail!("Telegram sources require [telegram].api_id and api_hash");
                }
                let conn = telegram::connect(&config, &pool)
                    .await
                    .context("connecting to Telegram")?;

                // Check auth
                match conn.client.is_authorized().await {
                    Ok(true) => {}
                    Ok(false) => anyhow::bail!("Telegram not authorized. Run 'pail tg login' first."),
                    Err(e) => anyhow::bail!("Telegram auth check failed: {e}"),
                }

                // Resolve source IDs and folders (same as daemon::start_telegram)
                let tg_sources = store::get_tg_sources(&pool).await?;
                telegram::resolve_source_ids(&conn.client, &pool, &tg_sources).await?;
                let folder_sources: Vec<_> = tg_sources
                    .iter()
                    .filter(|s| s.source_type == "telegram_folder")
                    .cloned()
                    .collect();
                telegram::resolve_folders(&conn.client, &pool, &folder_sources).await?;
                telegram::ensure_peer_cache(&conn.client, &pool, &tg_sources).await?;

                Some(conn)
            } else {
                None
            };

            let tg_client_ref = tg_conn.as_ref().map(|c| &c.client);

            let result = pipeline::run_generation(
                &pool,
                &config,
                channel_config,
                since_duration,
                true,
                tg_client_ref,
                cancel,
            )
            .await?;

            match result {
                Some(r) => {
                    if let Some(output_path) = output {
                        std::fs::write(&output_path, &r.raw_output)
                            .with_context(|| format!("writing output to {}", output_path.display()))?;
                        info!(path = %output_path.display(), "wrote markdown output");
                        println!("Article written to: {}", output_path.display());
                    } else {
                        println!("Article generated: {}", r.article.title);
                    }
                }
                None => {
                    println!("No content items found — generation skipped.");
                }
            }

            // Cleanup TG connection
            if let Some(conn) = tg_conn {
                conn.client.disconnect();
                conn.runner_handle.abort();
            }
        }
        Some(Commands::Tg { command }) => {
            // Validate telegram config
            match config.telegram.api_id {
                None | Some(0) => {
                    anyhow::bail!(
                        "Telegram requires a valid [telegram].api_id in config \
                         (get one at https://my.telegram.org)"
                    );
                }
                _ => {}
            }
            if config.telegram.api_hash.as_deref().is_none_or(|h| h.is_empty()) {
                anyhow::bail!(
                    "Telegram requires a valid [telegram].api_hash in config \
                     (get one at https://my.telegram.org)"
                );
            }

            let pool = db::create_pool(&config).await.context("creating database")?;
            let conn = telegram::connect(&config, &pool)
                .await
                .context("connecting to Telegram")?;

            match command {
                TgCommands::Login => {
                    telegram::login(&conn.client, &config).await.context("Telegram login")?;
                    println!("Session saved. You can now enable Telegram sources in config.");
                }
                TgCommands::Status => {
                    telegram::status(&conn.client).await.context("Telegram status")?;
                }
            }

            // Disconnect cleanly
            conn.client.disconnect();
            conn.runner_handle.abort();
        }
        None => {
            daemon::run(config).await?;
        }
    }

    Ok(())
}

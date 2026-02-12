mod cleanup;
mod cli;
mod config;
mod daemon;
mod db;
mod error;
mod fetch;
mod generate;
mod models;
mod pipeline;
mod poller;
mod scheduler;
mod server;
mod store;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;

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

            let result = pipeline::run_generation(&pool, &config, channel_config, since_duration, true, cancel).await?;

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
        }
        None => {
            daemon::run(config).await?;
        }
    }

    Ok(())
}

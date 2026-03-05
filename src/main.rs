mod benchmark;
mod cleanup;
mod cli;
mod config;
mod config_edit;
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
mod strategy;
mod telegram;
mod tg_listener;
mod tg_session;
mod tui;

use anyhow::{Context, Result};
use clap::Parser;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::cli::{BenchmarkCommands, Cli, Commands, ConfigCommands, StrategyCommands, TgCommands};
use crate::config::{Config, OutputChannelConfig, load_config, validate_config};
use crate::strategy::StrategyRegistry;
use crate::telegram::TgConnection;

/// Shared CLI setup for commands that run a pipeline (Generate, Interactive).
struct CliPipelineSetup<'a> {
    pool: SqlitePool,
    channel_config: &'a OutputChannelConfig,
    time_window: Option<pipeline::TimeWindow>,
    cancel: CancellationToken,
    tg_conn: Option<TgConnection>,
}

/// Set up DB, config sync, channel lookup, cancellation, and TG connection.
async fn setup_pipeline<'a>(
    config: &'a Config,
    slug: &str,
    since: &Option<String>,
    from: &Option<String>,
    to: &Option<String>,
) -> Result<CliPipelineSetup<'a>> {
    let time_window = cli::parse_time_window(since, from, to)?;

    let pool = db::create_pool(config).await.context("creating database")?;
    info!(db_path = %config.db_path().display(), "database ready");

    store::sync_config_to_db(&pool, config)
        .await
        .context("syncing config to database")?;
    info!("config synced to database");

    let channel_config = config
        .output_channel
        .iter()
        .find(|c| c.slug == slug)
        .ok_or_else(|| anyhow::anyhow!("no output channel config for slug '{slug}'"))?;

    let cancel = CancellationToken::new();
    let cancel_signal = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        cancel_signal.cancel();
    });

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
        let conn = telegram::connect(config, &pool)
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

    Ok(CliPipelineSetup {
        pool,
        channel_config,
        time_window,
        cancel,
        tg_conn,
    })
}

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

    // Load strategy registry (built-in + user-defined strategies)
    let registry = StrategyRegistry::load(config.pail.strategies_dir.as_deref()).context("loading strategy registry")?;
    strategy::validate_strategy_config(&config, &registry).context("strategy validation failed")?;
    info!("strategy registry loaded");

    match cli.command {
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Validate => {
                println!("Configuration is valid.");
            }
            ConfigCommands::Edit => {
                // Try to connect to Telegram if enabled and configured
                let tg_conn = if config.telegram.enabled
                    && config.telegram.api_id.is_some_and(|id| id != 0)
                    && config.telegram.api_hash.as_deref().is_some_and(|h| !h.is_empty())
                {
                    let pool = db::create_pool(&config).await.context("creating database")?;
                    match telegram::connect(&config, &pool).await {
                        Ok(conn) => match conn.client.is_authorized().await {
                            Ok(true) => Some(conn),
                            _ => {
                                println!("Telegram session not authorized. Run 'pail tg login' first.");
                                None
                            }
                        },
                        Err(e) => {
                            println!("Could not connect to Telegram: {e:#}");
                            None
                        }
                    }
                } else {
                    None
                };

                let result = tui::run_config_editor(&cli.config, tg_conn.as_ref()).await;

                // Cleanup TG connection
                if let Some(conn) = tg_conn {
                    conn.client.disconnect();
                    conn.runner_handle.abort();
                }

                result?;
            }
        },
        Some(Commands::Generate {
            slug,
            output,
            strategy,
            since,
            from,
            to,
        }) => {
            let setup = setup_pipeline(&config, &slug, &since, &from, &to).await?;
            let tg_client_ref = setup.tg_conn.as_ref().map(|c| &c.client);

            let result = pipeline::run_generation(
                &setup.pool,
                &config,
                setup.channel_config,
                &registry,
                strategy.as_deref(),
                setup.time_window,
                true,
                tg_client_ref,
                setup.cancel,
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
            if let Some(conn) = setup.tg_conn {
                conn.client.disconnect();
                conn.runner_handle.abort();
            }
        }
        Some(Commands::Interactive {
            slug,
            strategy,
            since,
            from,
            to,
        }) => {
            let setup = setup_pipeline(&config, &slug, &since, &from, &to).await?;
            let tg_client_ref = setup.tg_conn.as_ref().map(|c| &c.client);

            let result = pipeline::run_interactive(
                &setup.pool,
                &config,
                setup.channel_config,
                &registry,
                strategy.as_deref(),
                setup.time_window,
                tg_client_ref,
                setup.cancel,
            )
            .await?;

            match result {
                Some(count) => {
                    println!("Interactive session ended ({count} content items in workspace).");
                }
                None => {
                    println!("No content items found — nothing to explore.");
                }
            }

            // Cleanup TG connection
            if let Some(conn) = setup.tg_conn {
                conn.client.disconnect();
                conn.runner_handle.abort();
            }
        }
        Some(Commands::Benchmark { command }) => match command {
            BenchmarkCommands::Run {
                since,
                from,
                to,
                channel,
                strategy,
                samples,
                delay,
                timeout,
                models,
            } => {
                benchmark::run_benchmark(
                    &config,
                    &registry,
                    benchmark::BenchmarkRunArgs {
                        since,
                        from,
                        to,
                        channel,
                        strategy,
                        samples,
                        delay,
                        timeout,
                        models,
                    },
                )
                .await?;
            }
        },
        Some(Commands::Strategy { command }) => match command {
            StrategyCommands::List => {
                let strategies = registry.list();
                println!(
                    "{:<12} {:<8} {:<8} {:<6} DESCRIPTION",
                    "NAME", "SOURCE", "TIMEOUT", "TOOLS"
                );
                for s in &strategies {
                    let source = match s.source {
                        strategy::StrategySource::BuiltIn => "built-in",
                        strategy::StrategySource::User => "user",
                    };
                    let tool_count = s.meta.tools.len();
                    println!(
                        "{:<12} {:<8} {:<8} {:<6} {}",
                        s.meta.name, source, s.meta.timeout, tool_count, s.meta.description
                    );
                }
            }
            StrategyCommands::Show { name } => {
                let strat = registry
                    .get(&name)
                    .ok_or_else(|| anyhow::anyhow!("strategy '{name}' not found"))?;
                let merged = strategy::resolve_opencode_config(strat)?;

                println!("Strategy: {}", strat.meta.name);
                println!("Description: {}", strat.meta.description);
                println!(
                    "Source: {}",
                    match strat.source {
                        strategy::StrategySource::BuiltIn => "built-in",
                        strategy::StrategySource::User => "user",
                    }
                );
                println!("Timeout: {}", strat.meta.timeout);
                println!("Max retries: {}", strat.meta.max_retries);
                println!(
                    "Tools: {}",
                    if strat.meta.tools.is_empty() {
                        "(none)".to_string()
                    } else {
                        strat.meta.tools.join(", ")
                    }
                );
                println!("\n--- Merged opencode.json ---");
                println!("{}", serde_json::to_string_pretty(&merged)?);
                println!("\n--- Prompt preview (first 20 lines) ---");
                for line in strat.prompt_body.lines().take(20) {
                    println!("{line}");
                }
                if strat.prompt_body.lines().count() > 20 {
                    println!("... ({} more lines)", strat.prompt_body.lines().count() - 20);
                }
            }
            StrategyCommands::Validate { path } => match strategy::load_user_strategy(&path) {
                Ok(s) => {
                    println!("Strategy '{}' is valid.", s.meta.name);
                    println!("Description: {}", s.meta.description);
                    println!("Timeout: {}", s.meta.timeout);
                    println!("Tools: {:?}", s.meta.tools);
                }
                Err(e) => {
                    eprintln!("Validation failed: {e:#}");
                    std::process::exit(1);
                }
            },
        },
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
            daemon::run(config, registry).await?;
        }
    }

    Ok(())
}

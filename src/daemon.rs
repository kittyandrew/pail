use std::sync::Arc;

use anyhow::{Context, Result};
use rand::Rng;
use sqlx::SqlitePool;
use tokio::sync::{RwLock, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use rand::distr::Alphanumeric;

use crate::config::Config;
use crate::{cleanup, db, poller, scheduler, server, store, telegram, tg_listener};

pub async fn run(config: Config) -> Result<()> {
    let pool = db::create_pool(&config).await.context("creating database")?;
    info!(db_path = %config.db_path().display(), "database ready");

    // Sync config to DB
    store::sync_config_to_db(&pool, &config)
        .await
        .context("syncing config to database")?;
    info!("config synced to database");

    // Bootstrap feed token
    let feed_token = bootstrap_feed_token(&pool, &config).await?;

    let config = Arc::new(config);
    let cancel = CancellationToken::new();
    let semaphore = Arc::new(Semaphore::new(config.pail.max_concurrent_generations as usize));

    // Start Telegram before the scheduler so the client is available for mark-as-read
    let (tg_handle, tg_client) = if config.telegram.enabled {
        match start_telegram(&config, &pool, cancel.clone()).await {
            Ok((handle, client)) => (Some(handle), Some(client)),
            Err(e) => {
                error!(error = %e, "failed to start Telegram listener, continuing without TG");
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    // Spawn background tasks
    let scheduler_handle = tokio::spawn(scheduler::scheduler_loop(
        pool.clone(),
        config.clone(),
        semaphore.clone(),
        tg_client,
        cancel.clone(),
    ));

    let poller_handle = tokio::spawn(poller::polling_loop(pool.clone(), cancel.clone()));

    let cleanup_handle = tokio::spawn(cleanup::cleanup_loop(pool.clone(), config.clone(), cancel.clone()));

    // Build and start HTTP server
    let app_state = server::AppState {
        pool: pool.clone(),
        feed_token,
    };

    let router = server::build_router(app_state);
    let listener = tokio::net::TcpListener::bind(&config.pail.listen)
        .await
        .with_context(|| format!("binding to {}", config.pail.listen))?;

    info!(listen = %config.pail.listen, "HTTP server listening");

    // Run the server with graceful shutdown
    let server_cancel = cancel.clone();
    let server_handle = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                server_cancel.cancelled().await;
            })
            .await
    });

    // Wait for shutdown signal
    wait_for_shutdown().await;
    info!("shutdown signal received");

    // Cancel all tasks
    cancel.cancel();

    // Wait for tasks with timeout
    let shutdown_timeout = std::time::Duration::from_secs(10);
    let _ = tokio::time::timeout(shutdown_timeout, async {
        let _ = scheduler_handle.await;
        let _ = poller_handle.await;
        let _ = cleanup_handle.await;
        let _ = server_handle.await;
        if let Some(h) = tg_handle {
            let _ = h.await;
        }
    })
    .await;

    // Close DB pool
    pool.close().await;
    info!("shutdown complete");

    Ok(())
}

/// Start the Telegram listener. Returns a JoinHandle for the listener task and a cloned Client
/// for use by the scheduler (mark-as-read).
async fn start_telegram(
    config: &Config,
    pool: &SqlitePool,
    cancel: CancellationToken,
) -> Result<(tokio::task::JoinHandle<()>, grammers_client::Client)> {
    // Connect (session data is stored in the database, loaded by SqlxSession)
    let conn = telegram::connect(config, pool)
        .await
        .context("connecting to Telegram")?;

    // Check authorization
    match conn.client.is_authorized().await {
        Ok(true) => {
            let me = conn.client.get_me().await.context("getting TG user info")?;
            info!(
                user = %me.full_name(),
                username = ?me.username(),
                "Telegram session authorized"
            );
        }
        Ok(false) => {
            error!("Telegram session not authorized. Run 'pail tg login' first.");
            conn.client.disconnect();
            conn.runner_handle.abort();
            anyhow::bail!("Telegram not authorized");
        }
        Err(e) => {
            error!(error = %e, "failed to check Telegram authorization");
            conn.client.disconnect();
            conn.runner_handle.abort();
            anyhow::bail!("Telegram auth check failed: {e}");
        }
    }

    // Resolve source usernames -> tg_ids
    let tg_sources = store::get_tg_sources(pool).await.context("loading TG sources")?;

    telegram::resolve_source_ids(&conn.client, pool, &tg_sources)
        .await
        .context("resolving TG source IDs")?;

    // Resolve folder sources
    let folder_sources: Vec<_> = tg_sources
        .iter()
        .filter(|s| s.source_type == "telegram_folder")
        .cloned()
        .collect();

    telegram::resolve_folders(&conn.client, pool, &folder_sources)
        .await
        .context("resolving TG folders")?;

    telegram::ensure_peer_cache(&conn.client, pool, &tg_sources)
        .await
        .context("warming TG peer cache")?;

    // Build subscription map
    // Re-fetch sources after resolution to get updated tg_ids
    let tg_sources = store::get_tg_sources(pool).await.context("reloading TG sources")?;
    let direct_sources: Vec<_> = tg_sources
        .iter()
        .filter(|s| s.source_type != "telegram_folder")
        .cloned()
        .collect();

    let folder_channels = store::get_all_folder_channel_ids(pool)
        .await
        .context("loading folder channel IDs")?;

    let subscription_map = telegram::build_subscription_map(&direct_sources, &folder_channels);
    let subscribed_count = subscription_map.len();
    let subscriptions = Arc::new(RwLock::new(subscription_map));

    info!(subscribed_chats = subscribed_count, "Telegram listener started");

    // Clone client for the scheduler (mark-as-read) before moving it into the listener
    let scheduler_client = conn.client.clone();

    // Spawn listener task
    let pool = pool.clone();
    let handle = tokio::spawn(async move {
        tg_listener::listener_loop(conn.client, pool, subscriptions, conn.updates_rx, cancel).await;
        // Clean shutdown: disconnect and stop runner
        conn.runner_handle.abort();
    });

    Ok((handle, scheduler_client))
}

async fn bootstrap_feed_token(pool: &SqlitePool, config: &Config) -> Result<String> {
    // Priority: config value -> DB stored value -> auto-generate
    if let Some(ref token) = config.pail.feed_token {
        // Store config-provided token in DB for consistency
        store::set_setting(pool, "feed_token", token).await?;
        info!("using feed token from config");
        return Ok(token.clone());
    }

    if let Some(token) = store::get_setting(pool, "feed_token").await? {
        info!("using stored feed token");
        return Ok(token);
    }

    // Auto-generate
    let token = generate_token();
    store::set_setting(pool, "feed_token", &token).await?;
    warn!(
        token = %token,
        "feed token generated — save this, it won't be shown again"
    );
    Ok(token)
}

fn generate_token() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

async fn wait_for_shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();

    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {},
            _ = sigterm.recv() => {},
        }
    }

    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }
}

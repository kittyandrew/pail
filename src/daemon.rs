use std::sync::Arc;

use anyhow::{Context, Result};
use rand::Rng;
use sqlx::SqlitePool;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::Config;
use crate::{cleanup, db, poller, scheduler, server, store};

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

    // Spawn background tasks
    let scheduler_handle = tokio::spawn(scheduler::scheduler_loop(
        pool.clone(),
        config.clone(),
        semaphore.clone(),
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
    })
    .await;

    // Close DB pool
    pool.close().await;
    info!("shutdown complete");

    Ok(())
}

async fn bootstrap_feed_token(pool: &SqlitePool, config: &Config) -> Result<String> {
    // Priority: config value → DB stored value → auto-generate
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
    use rand::distr::Alphanumeric;
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

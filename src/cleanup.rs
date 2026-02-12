use std::sync::Arc;

use chrono::Utc;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::config::Config;
use crate::store;

/// Content retention cleanup loop. Wakes every hour.
pub async fn cleanup_loop(pool: SqlitePool, config: Arc<Config>, cancel: CancellationToken) {
    info!("cleanup job started");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("cleanup job shutting down");
                return;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(3600)) => {}
        }

        let retention = match humantime::parse_duration(&config.pail.retention) {
            Ok(d) => chrono::Duration::from_std(d).unwrap_or(chrono::Duration::days(7)),
            Err(e) => {
                error!(error = %e, retention = %config.pail.retention, "invalid retention duration");
                chrono::Duration::days(7)
            }
        };

        let cutoff = Utc::now() - retention;

        match store::delete_old_content_items(&pool, cutoff).await {
            Ok(deleted) => {
                if deleted > 0 {
                    info!(deleted, cutoff = %cutoff.to_rfc3339(), "cleaned up old content items");
                }
            }
            Err(e) => {
                error!(error = %e, "content cleanup failed");
            }
        }
    }
}

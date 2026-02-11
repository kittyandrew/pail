use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};
use tracing::info;

use crate::config::Config;

pub async fn create_pool(config: &Config) -> Result<SqlitePool> {
    let db_path = config.db_path();

    // Ensure the parent directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating data directory: {}", parent.display()))?;
    }

    let options = SqliteConnectOptions::new()
        .filename(&db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("connecting to database: {}", db_path.display()))?;

    info!(path = %db_path.display(), "database connected (WAL mode, foreign keys enabled)");

    run_migrations(&pool).await?;

    Ok(pool)
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    // Run the migration SQL directly since sqlx::migrate! needs compile-time checking
    let migration_sql = include_str!("../migrations/20260211_000001_initial_schema.sql");

    pool.execute(migration_sql)
        .await
        .context("running database migrations")?;

    info!("database migrations applied");
    Ok(())
}

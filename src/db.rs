use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, Row, SqlitePool};
use tracing::info;

use crate::config::Config;

/// Ordered list of migrations. Each entry is (version, name, sql).
/// Versions must be monotonically increasing.
const MIGRATIONS: &[(i64, &str, &str)] = &[
    (
        1,
        "initial_schema",
        include_str!("../migrations/20260211_000001_initial_schema.sql"),
    ),
    (2, "phase1b", include_str!("../migrations/20260211_000002_phase1b.sql")),
    (
        3,
        "phase2_telegram",
        include_str!("../migrations/20260212_000003_phase2_telegram.sql"),
    ),
    (
        4,
        "workspace_improvements",
        include_str!("../migrations/20260213_000004_workspace_improvements.sql"),
    ),
    (
        5,
        "nullable_schedule",
        include_str!("../migrations/20260218_000005_nullable_schedule.sql"),
    ),
];

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
    // Create schema_version table if it doesn't exist
    pool.execute(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY NOT NULL,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
        )",
    )
    .await
    .context("creating schema_version table")?;

    // Get the current max version
    let row = sqlx::query("SELECT COALESCE(MAX(version), 0) as v FROM schema_version")
        .fetch_one(pool)
        .await
        .context("querying schema version")?;
    let current_version: i64 = row.get("v");

    let mut applied = 0;
    for &(version, name, sql) in MIGRATIONS {
        if version <= current_version {
            continue;
        }
        pool.execute(sql)
            .await
            .with_context(|| format!("applying migration v{version} ({name})"))?;
        sqlx::query("INSERT INTO schema_version (version, name) VALUES (?, ?)")
            .bind(version)
            .bind(name)
            .execute(pool)
            .await
            .with_context(|| format!("recording migration v{version}"))?;
        applied += 1;
        info!(version, name, "applied migration");
    }

    if applied == 0 {
        info!(current_version, "database schema up to date");
    } else {
        info!(applied, "database migrations applied");
    }

    Ok(())
}

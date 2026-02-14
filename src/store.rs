use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use tracing::debug;
use uuid::Uuid;

use crate::config::Config;
use crate::models::{ContentItem, GeneratedArticle, GeneratedArticleRow, OutputChannel, Source};

/// All source columns in SELECT order (must match Source struct field order).
const SOURCE_COLUMNS: &str = "id, source_type, name, enabled, url, poll_interval, max_items,
    auth_type, auth_username, auth_password, auth_token, auth_header_name, auth_header_value,
    last_fetched_at, last_etag, last_modified_header,
    tg_id, tg_username, tg_folder_id, tg_folder_name, tg_exclude, description";

/// Upsert a source by name — insert or update if it already exists.
pub async fn upsert_source(pool: &SqlitePool, source: &crate::config::SourceConfig) -> Result<String> {
    let (auth_type, auth_username, auth_password, auth_token, auth_header_name, auth_header_value) =
        if let Some(auth) = &source.auth {
            (
                Some(auth.auth_type.clone()),
                auth.username.clone(),
                auth.password.clone(),
                auth.token.clone(),
                auth.header_name.clone(),
                auth.header_value.clone(),
            )
        } else {
            (None, None, None, None, None, None)
        };

    let enabled = source.enabled.unwrap_or(true);
    let tg_exclude = source
        .exclude
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default());

    // Check if source exists by name
    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM sources WHERE name = ?")
        .bind(&source.name)
        .fetch_optional(pool)
        .await
        .context("checking for existing source")?;

    let id = if let Some((existing_id,)) = existing {
        sqlx::query(
            "UPDATE sources SET source_type = ?, enabled = ?, url = ?, poll_interval = ?, max_items = ?,
             auth_type = ?, auth_username = ?, auth_password = ?, auth_token = ?, auth_header_name = ?, auth_header_value = ?,
             tg_id = COALESCE(?, tg_id), tg_username = ?, tg_folder_name = ?, tg_exclude = ?, description = ?,
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE id = ?",
        )
        .bind(&source.source_type)
        .bind(enabled)
        .bind(&source.url)
        .bind(&source.poll_interval)
        .bind(source.max_items as i32)
        .bind(&auth_type)
        .bind(&auth_username)
        .bind(&auth_password)
        .bind(&auth_token)
        .bind(&auth_header_name)
        .bind(&auth_header_value)
        .bind(source.tg_id)
        .bind(&source.tg_username)
        .bind(&source.tg_folder_name)
        .bind(&tg_exclude)
        .bind(&source.description)
        .bind(&existing_id)
        .execute(pool)
        .await
        .context("updating source")?;

        debug!(name = %source.name, id = %existing_id, "updated source");
        existing_id
    } else {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO sources (id, source_type, name, enabled, url, poll_interval, max_items,
             auth_type, auth_username, auth_password, auth_token, auth_header_name, auth_header_value,
             tg_id, tg_username, tg_folder_name, tg_exclude, description)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&source.source_type)
        .bind(&source.name)
        .bind(enabled)
        .bind(&source.url)
        .bind(&source.poll_interval)
        .bind(source.max_items as i32)
        .bind(&auth_type)
        .bind(&auth_username)
        .bind(&auth_password)
        .bind(&auth_token)
        .bind(&auth_header_name)
        .bind(&auth_header_value)
        .bind(source.tg_id)
        .bind(&source.tg_username)
        .bind(&source.tg_folder_name)
        .bind(&tg_exclude)
        .bind(&source.description)
        .execute(pool)
        .await
        .context("inserting source")?;

        debug!(name = %source.name, id = %id, "created source");
        id
    };

    Ok(id)
}

/// Upsert an output channel by slug.
pub async fn upsert_output_channel(
    pool: &SqlitePool,
    channel: &crate::config::OutputChannelConfig,
    source_ids: &[String],
) -> Result<String> {
    let enabled = channel.enabled.unwrap_or(true);

    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM output_channels WHERE slug = ?")
        .bind(&channel.slug)
        .fetch_optional(pool)
        .await
        .context("checking for existing output channel")?;

    let id = if let Some((existing_id,)) = existing {
        sqlx::query(
            "UPDATE output_channels SET name = ?, schedule = ?, prompt = ?, model = ?, language = ?, enabled = ?,
             updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
             WHERE id = ?",
        )
        .bind(&channel.name)
        .bind(&channel.schedule)
        .bind(&channel.prompt)
        .bind(&channel.model)
        .bind(&channel.language)
        .bind(enabled)
        .bind(&existing_id)
        .execute(pool)
        .await
        .context("updating output channel")?;

        debug!(slug = %channel.slug, id = %existing_id, "updated output channel");
        existing_id
    } else {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO output_channels (id, name, slug, schedule, prompt, model, language, enabled)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&channel.name)
        .bind(&channel.slug)
        .bind(&channel.schedule)
        .bind(&channel.prompt)
        .bind(&channel.model)
        .bind(&channel.language)
        .bind(enabled)
        .execute(pool)
        .await
        .context("inserting output channel")?;

        debug!(slug = %channel.slug, id = %id, "created output channel");
        id
    };

    // Sync junction table
    sqlx::query("DELETE FROM output_channel_sources WHERE output_channel_id = ?")
        .bind(&id)
        .execute(pool)
        .await
        .context("clearing output channel sources")?;

    for source_id in source_ids {
        sqlx::query("INSERT INTO output_channel_sources (output_channel_id, source_id) VALUES (?, ?)")
            .bind(&id)
            .bind(source_id)
            .execute(pool)
            .await
            .context("linking source to output channel")?;
    }

    Ok(id)
}

/// Sync all sources and output channels from config to DB.
/// Sources and channels not in config are deleted (cascading to content_items).
pub async fn sync_config_to_db(pool: &SqlitePool, config: &Config) -> Result<()> {
    // First, upsert all sources and build a name->id map
    let mut source_name_to_id = std::collections::HashMap::new();
    for source in &config.source {
        let id = upsert_source(pool, source).await?;
        source_name_to_id.insert(source.name.clone(), id);
    }

    // Then, upsert output channels with resolved source IDs
    let mut config_channel_slugs = std::collections::HashSet::new();
    for channel in &config.output_channel {
        config_channel_slugs.insert(channel.slug.clone());
        let source_ids: Vec<String> = channel
            .sources
            .iter()
            .filter_map(|name| source_name_to_id.get(name).cloned())
            .collect();
        upsert_output_channel(pool, channel, &source_ids).await?;
    }

    // Delete sources not in config
    let config_source_ids: Vec<&str> = source_name_to_id.values().map(|s| s.as_str()).collect();
    let db_sources: Vec<(String, String)> = sqlx::query_as("SELECT id, name FROM sources")
        .fetch_all(pool)
        .await
        .context("listing sources for cleanup")?;

    for (id, name) in &db_sources {
        if !config_source_ids.contains(&id.as_str()) {
            sqlx::query("DELETE FROM sources WHERE id = ?")
                .bind(id)
                .execute(pool)
                .await
                .context("deleting orphaned source")?;
            debug!(name = %name, "deleted orphaned source");
        }
    }

    // Delete output channels not in config
    let db_channels: Vec<(String, String)> = sqlx::query_as("SELECT id, slug FROM output_channels")
        .fetch_all(pool)
        .await
        .context("listing channels for cleanup")?;

    for (id, slug) in &db_channels {
        if !config_channel_slugs.contains(slug.as_str()) {
            sqlx::query("DELETE FROM output_channels WHERE id = ?")
                .bind(id)
                .execute(pool)
                .await
                .context("deleting orphaned output channel")?;
            debug!(slug = %slug, "deleted orphaned output channel");
        }
    }

    Ok(())
}

/// Get an output channel by slug.
pub async fn get_channel_by_slug(pool: &SqlitePool, slug: &str) -> Result<Option<OutputChannel>> {
    let channel = sqlx::query_as::<_, OutputChannel>(
        "SELECT id, name, slug, schedule, prompt, model, language, enabled, last_generated
         FROM output_channels WHERE slug = ?",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("querying output channel by slug")?;

    Ok(channel)
}

/// Get source IDs linked to an output channel.
pub async fn get_channel_source_ids(pool: &SqlitePool, channel_id: &str) -> Result<Vec<String>> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT source_id FROM output_channel_sources WHERE output_channel_id = ?")
            .bind(channel_id)
            .fetch_all(pool)
            .await
            .context("querying channel source IDs")?;

    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Get sources by their IDs.
pub async fn get_sources_by_ids(pool: &SqlitePool, ids: &[String]) -> Result<Vec<Source>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
    let query = format!(
        "SELECT {SOURCE_COLUMNS} FROM sources WHERE id IN ({})",
        placeholders.join(", ")
    );

    let mut q = sqlx::query_as::<_, Source>(&query);
    for id in ids {
        q = q.bind(id);
    }

    let sources = q.fetch_all(pool).await.context("querying sources by IDs")?;

    Ok(sources)
}

/// Upsert a content item (skip if same source_id + dedup_key exists).
pub async fn upsert_content_item(pool: &SqlitePool, item: &ContentItem) -> Result<()> {
    sqlx::query(
        "INSERT INTO content_items (id, source_id, ingested_at, original_date, content_type, title, body, url, author, metadata, dedup_key)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(source_id, dedup_key) DO UPDATE SET
           upstream_changed = (excluded.body IS NOT content_items.body OR excluded.title IS NOT content_items.title)",
    )
    .bind(&item.id)
    .bind(&item.source_id)
    .bind(item.ingested_at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
    .bind(item.original_date.format("%Y-%m-%dT%H:%M:%SZ").to_string())
    .bind(&item.content_type)
    .bind(&item.title)
    .bind(&item.body)
    .bind(&item.url)
    .bind(&item.author)
    .bind(&item.metadata)
    .bind(&item.dedup_key)
    .execute(pool)
    .await
    .context("upserting content item")?;

    Ok(())
}

/// Get content items within a time window for the given source IDs.
pub async fn get_items_in_window(
    pool: &SqlitePool,
    source_ids: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<ContentItem>> {
    if source_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders: Vec<&str> = source_ids.iter().map(|_| "?").collect();
    let query = format!(
        "SELECT id, source_id, ingested_at, original_date, content_type, title, body, url, author, metadata, dedup_key, upstream_changed
         FROM content_items
         WHERE source_id IN ({})
           AND original_date >= ?
           AND original_date <= ?
         ORDER BY original_date ASC",
        placeholders.join(", ")
    );

    let mut q = sqlx::query_as::<_, ContentItem>(&query);
    for id in source_ids {
        q = q.bind(id);
    }
    q = q
        .bind(from.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .bind(to.format("%Y-%m-%dT%H:%M:%SZ").to_string());

    let items = q.fetch_all(pool).await.context("querying content items in window")?;

    Ok(items)
}

/// Insert a generated article.
pub async fn insert_generated_article(pool: &SqlitePool, article: &GeneratedArticle) -> Result<()> {
    let content_item_ids_json =
        serde_json::to_string(&article.content_item_ids).context("serializing content_item_ids")?;
    let topics_json = serde_json::to_string(&article.topics).context("serializing topics")?;

    sqlx::query(
        "INSERT INTO generated_articles (id, output_channel_id, generated_at, covers_from, covers_to,
         title, topics, body_html, body_markdown, content_item_ids, generation_log, model_used, token_count)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&article.id)
    .bind(&article.output_channel_id)
    .bind(article.generated_at.format("%Y-%m-%dT%H:%M:%SZ").to_string())
    .bind(article.covers_from.format("%Y-%m-%dT%H:%M:%SZ").to_string())
    .bind(article.covers_to.format("%Y-%m-%dT%H:%M:%SZ").to_string())
    .bind(&article.title)
    .bind(&topics_json)
    .bind(&article.body_html)
    .bind(&article.body_markdown)
    .bind(&content_item_ids_json)
    .bind(&article.generation_log)
    .bind(&article.model_used)
    .bind(article.token_count)
    .execute(pool)
    .await
    .context("inserting generated article")?;

    Ok(())
}

/// Update the last_generated timestamp on an output channel.
pub async fn update_last_generated(pool: &SqlitePool, channel_id: &str, timestamp: DateTime<Utc>) -> Result<()> {
    sqlx::query("UPDATE output_channels SET last_generated = ? WHERE id = ?")
        .bind(timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .bind(channel_id)
        .execute(pool)
        .await
        .context("updating last_generated")?;

    Ok(())
}

/// Read a setting from the settings table.
pub async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .context("reading setting")?;
    Ok(row.map(|(v,)| v))
}

/// Upsert a setting in the settings table.
pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .context("upserting setting")?;
    Ok(())
}

/// Update fetch state on a source: last_fetched_at, ETag, and Last-Modified.
pub async fn update_source_fetch_state(
    pool: &SqlitePool,
    source_id: &str,
    timestamp: DateTime<Utc>,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<()> {
    sqlx::query("UPDATE sources SET last_fetched_at = ?, last_etag = ?, last_modified_header = ? WHERE id = ?")
        .bind(timestamp.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .bind(etag)
        .bind(last_modified)
        .bind(source_id)
        .execute(pool)
        .await
        .context("updating source fetch state")?;
    Ok(())
}

/// Delete content items older than the cutoff. Returns number of deleted rows.
pub async fn delete_old_content_items(pool: &SqlitePool, cutoff: DateTime<Utc>) -> Result<u64> {
    let result = sqlx::query("DELETE FROM content_items WHERE ingested_at < ?")
        .bind(cutoff.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .execute(pool)
        .await
        .context("deleting old content items")?;
    Ok(result.rows_affected())
}

/// Get recent generated articles for an output channel (for Atom feed).
pub async fn get_recent_articles(pool: &SqlitePool, channel_id: &str, limit: i64) -> Result<Vec<GeneratedArticleRow>> {
    let articles = sqlx::query_as::<_, GeneratedArticleRow>(
        "SELECT id, output_channel_id, generated_at, covers_from, covers_to,
         title, topics, body_html, body_markdown, content_item_ids, generation_log, model_used, token_count
         FROM generated_articles
         WHERE output_channel_id = ?
         ORDER BY generated_at DESC
         LIMIT ?",
    )
    .bind(channel_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .context("querying recent articles")?;
    Ok(articles)
}

/// Get all enabled output channels.
pub async fn get_all_enabled_channels(pool: &SqlitePool) -> Result<Vec<OutputChannel>> {
    let channels = sqlx::query_as::<_, OutputChannel>(
        "SELECT id, name, slug, schedule, prompt, model, language, enabled, last_generated
         FROM output_channels WHERE enabled = 1",
    )
    .fetch_all(pool)
    .await
    .context("querying enabled output channels")?;
    Ok(channels)
}

/// Get all enabled sources.
pub async fn get_all_enabled_sources(pool: &SqlitePool) -> Result<Vec<Source>> {
    let query = format!("SELECT {SOURCE_COLUMNS} FROM sources WHERE enabled = 1");
    let sources = sqlx::query_as::<_, Source>(&query)
        .fetch_all(pool)
        .await
        .context("querying enabled sources")?;
    Ok(sources)
}

// ── Telegram-specific queries ──────────────────────────────────────────

/// Get enabled sources where type starts with "telegram_".
pub async fn get_tg_sources(pool: &SqlitePool) -> Result<Vec<Source>> {
    let query = format!("SELECT {SOURCE_COLUMNS} FROM sources WHERE enabled = 1 AND source_type LIKE 'telegram_%'");
    let sources = sqlx::query_as::<_, Source>(&query)
        .fetch_all(pool)
        .await
        .context("querying TG sources")?;
    Ok(sources)
}

/// Store resolved numeric tg_id for a source.
pub async fn update_source_tg_id(pool: &SqlitePool, source_id: &str, tg_id: i64) -> Result<()> {
    sqlx::query("UPDATE sources SET tg_id = ? WHERE id = ?")
        .bind(tg_id)
        .bind(source_id)
        .execute(pool)
        .await
        .context("updating source tg_id")?;
    Ok(())
}

/// Store resolved folder ID for a folder source.
pub async fn update_source_tg_folder_id(pool: &SqlitePool, source_id: &str, folder_id: i32) -> Result<()> {
    sqlx::query("UPDATE sources SET tg_folder_id = ? WHERE id = ?")
        .bind(folder_id)
        .bind(source_id)
        .execute(pool)
        .await
        .context("updating source tg_folder_id")?;
    Ok(())
}

/// Upsert a channel belonging to a folder source.
pub async fn upsert_folder_channel(
    pool: &SqlitePool,
    folder_source_id: &str,
    channel_tg_id: i64,
    name: Option<&str>,
    username: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO tg_folder_channels (folder_source_id, channel_tg_id, channel_name, channel_username)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(folder_source_id, channel_tg_id) DO UPDATE SET
           channel_name = excluded.channel_name,
           channel_username = excluded.channel_username",
    )
    .bind(folder_source_id)
    .bind(channel_tg_id)
    .bind(name)
    .bind(username)
    .execute(pool)
    .await
    .context("upserting folder channel")?;
    Ok(())
}

/// Delete all channels for a folder source (used before re-sync).
pub async fn delete_folder_channels(pool: &SqlitePool, folder_source_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM tg_folder_channels WHERE folder_source_id = ?")
        .bind(folder_source_id)
        .execute(pool)
        .await
        .context("deleting folder channels")?;
    Ok(())
}

/// Get channels belonging to a folder source with their info.
/// Returns (channel_tg_id, channel_name, channel_username) for enabled channels.
pub async fn get_folder_channels_with_info(
    pool: &SqlitePool,
    folder_source_id: &str,
) -> Result<Vec<(i64, Option<String>, Option<String>)>> {
    let rows: Vec<(i64, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT channel_tg_id, channel_name, channel_username FROM tg_folder_channels WHERE folder_source_id = ? AND enabled = 1",
    )
    .bind(folder_source_id)
    .fetch_all(pool)
    .await
    .context("querying folder channels with info")?;
    Ok(rows)
}

/// Get all folder channel entries: (source_id, channel_tg_id) for building the subscription map.
pub async fn get_all_folder_channel_ids(pool: &SqlitePool) -> Result<Vec<(String, i64)>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT fc.folder_source_id, fc.channel_tg_id
         FROM tg_folder_channels fc
         JOIN sources s ON s.id = fc.folder_source_id
         WHERE fc.enabled = 1 AND s.enabled = 1",
    )
    .fetch_all(pool)
    .await
    .context("querying all folder channel IDs")?;
    Ok(rows)
}

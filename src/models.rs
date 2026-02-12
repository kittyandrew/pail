use chrono::{DateTime, Utc};
use sqlx::FromRow;

#[derive(Debug, Clone, FromRow)]
pub struct Source {
    pub id: String,
    pub source_type: String,
    pub name: String,
    pub enabled: bool,
    pub url: Option<String>,
    pub poll_interval: String,
    pub max_items: i32,
    pub auth_type: Option<String>,
    pub auth_username: Option<String>,
    pub auth_password: Option<String>,
    pub auth_token: Option<String>,
    pub auth_header_name: Option<String>,
    pub auth_header_value: Option<String>,
    pub last_fetched_at: Option<DateTime<Utc>>,
    pub last_etag: Option<String>,
    pub last_modified_header: Option<String>,
}

#[derive(Debug, Clone, FromRow)]
pub struct OutputChannel {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub schedule: String,
    pub prompt: String,
    pub model: Option<String>,
    pub language: Option<String>,
    pub enabled: bool,
    pub last_generated: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct ContentItem {
    pub id: String,
    pub source_id: String,
    pub ingested_at: DateTime<Utc>,
    pub original_date: DateTime<Utc>,
    pub content_type: String,
    pub title: Option<String>,
    pub body: String,
    pub url: Option<String>,
    pub author: Option<String>,
    pub metadata: String,
    pub dedup_key: String,
    pub upstream_changed: bool,
}

/// A generated article ready to be stored.
/// Not a FromRow — built by the generation engine.
pub struct GeneratedArticle {
    pub id: String,
    pub output_channel_id: String,
    pub generated_at: DateTime<Utc>,
    pub covers_from: DateTime<Utc>,
    pub covers_to: DateTime<Utc>,
    pub title: String,
    pub topics: Vec<String>,
    pub body_html: String,
    pub body_markdown: String,
    pub content_item_ids: Vec<String>,
    pub generation_log: String,
    pub model_used: String,
    pub token_count: Option<i64>,
}

/// Read model for articles from DB (used by Atom feed builder).
#[derive(Debug, Clone, FromRow)]
pub struct GeneratedArticleRow {
    pub id: String,
    pub output_channel_id: String,
    pub generated_at: DateTime<Utc>,
    pub covers_from: DateTime<Utc>,
    pub covers_to: DateTime<Utc>,
    pub title: String,
    pub topics: String,
    pub body_html: String,
    pub body_markdown: String,
    pub content_item_ids: String,
    pub generation_log: String,
    pub model_used: String,
    pub token_count: Option<i64>,
}

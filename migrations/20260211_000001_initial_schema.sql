-- Sources (RSS feeds, Telegram channels, etc.)
CREATE TABLE IF NOT EXISTS sources (
    id TEXT PRIMARY KEY NOT NULL,
    source_type TEXT NOT NULL,
    name TEXT NOT NULL UNIQUE,
    enabled INTEGER NOT NULL DEFAULT 1,
    url TEXT,
    poll_interval TEXT NOT NULL DEFAULT '30m',
    max_items INTEGER NOT NULL DEFAULT 200,
    auth_type TEXT,
    auth_username TEXT,
    auth_password TEXT,
    auth_token TEXT,
    auth_header_name TEXT,
    auth_header_value TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- Output channels
CREATE TABLE IF NOT EXISTS output_channels (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    schedule TEXT NOT NULL,
    prompt TEXT NOT NULL,
    model TEXT,
    language TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_generated TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- Junction table: output_channel <-> sources
CREATE TABLE IF NOT EXISTS output_channel_sources (
    output_channel_id TEXT NOT NULL REFERENCES output_channels(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    PRIMARY KEY (output_channel_id, source_id)
);

-- Content items (ingested feed entries, messages, etc.)
CREATE TABLE IF NOT EXISTS content_items (
    id TEXT PRIMARY KEY NOT NULL,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    ingested_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    original_date TEXT NOT NULL,
    content_type TEXT NOT NULL DEFAULT 'text',
    title TEXT,
    body TEXT NOT NULL,
    url TEXT,
    author TEXT,
    metadata TEXT NOT NULL DEFAULT '{}',
    dedup_key TEXT NOT NULL,
    upstream_changed INTEGER NOT NULL DEFAULT 0,
    UNIQUE(source_id, dedup_key)
);

CREATE INDEX IF NOT EXISTS idx_content_items_source_date
    ON content_items(source_id, original_date);

CREATE INDEX IF NOT EXISTS idx_content_items_ingested
    ON content_items(ingested_at);

-- Generated articles
CREATE TABLE IF NOT EXISTS generated_articles (
    id TEXT PRIMARY KEY NOT NULL,
    output_channel_id TEXT NOT NULL REFERENCES output_channels(id) ON DELETE CASCADE,
    generated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    covers_from TEXT NOT NULL,
    covers_to TEXT NOT NULL,
    title TEXT NOT NULL,
    topics TEXT NOT NULL DEFAULT '[]',
    body_html TEXT NOT NULL,
    body_markdown TEXT NOT NULL,
    content_item_ids TEXT NOT NULL DEFAULT '[]',
    generation_log TEXT NOT NULL DEFAULT '',
    model_used TEXT NOT NULL DEFAULT '',
    token_count INTEGER
);

CREATE INDEX IF NOT EXISTS idx_generated_articles_channel
    ON generated_articles(output_channel_id, generated_at DESC);

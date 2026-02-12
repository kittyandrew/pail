-- Settings key-value store (feed token, etc.)
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- Track when each source was last fetched (for RSS polling)
ALTER TABLE sources ADD COLUMN last_fetched_at TEXT;

-- HTTP conditional GET headers for efficient RSS polling
ALTER TABLE sources ADD COLUMN last_etag TEXT;
ALTER TABLE sources ADD COLUMN last_modified_header TEXT;

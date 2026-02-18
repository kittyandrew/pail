-- Make schedule column nullable on output_channels.
-- The initial migration defined it as TEXT NOT NULL, but the code stores NULL
-- for CLI-only output channels that have no schedule.
-- SQLite doesn't support ALTER COLUMN, so we recreate the table.
-- Foreign keys must be temporarily disabled to avoid cascade-deleting
-- generated_articles and output_channel_sources during the DROP.

PRAGMA foreign_keys = OFF;

CREATE TABLE output_channels_new (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    schedule TEXT,
    prompt TEXT NOT NULL,
    model TEXT,
    language TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    last_generated TEXT,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

INSERT INTO output_channels_new SELECT * FROM output_channels;

DROP TABLE output_channels;

ALTER TABLE output_channels_new RENAME TO output_channels;

PRAGMA foreign_keys = ON;

-- Telegram-specific columns on sources
ALTER TABLE sources ADD COLUMN tg_id INTEGER;
ALTER TABLE sources ADD COLUMN tg_username TEXT;
ALTER TABLE sources ADD COLUMN tg_folder_id INTEGER;
ALTER TABLE sources ADD COLUMN tg_folder_name TEXT;
ALTER TABLE sources ADD COLUMN tg_exclude TEXT;  -- JSON array of @usernames to exclude

-- Track channels belonging to a folder source (for folder->channel resolution)
CREATE TABLE IF NOT EXISTS tg_folder_channels (
    folder_source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    channel_tg_id INTEGER NOT NULL,
    channel_name TEXT,
    channel_username TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (folder_source_id, channel_tg_id)
);

-- grammers MTProto session data (custom Session trait impl backed by sqlx)
-- Mirrors the schema from grammers-session's built-in SqliteSession, but stored
-- in pail's own database to avoid the libsql-ffi / libsqlite3-sys symbol conflict.

CREATE TABLE IF NOT EXISTS tg_dc_home (
    dc_id INTEGER NOT NULL,
    PRIMARY KEY (dc_id)
);

CREATE TABLE IF NOT EXISTS tg_dc_option (
    dc_id INTEGER NOT NULL,
    ipv4 TEXT NOT NULL,
    ipv6 TEXT NOT NULL,
    auth_key BLOB,
    PRIMARY KEY (dc_id)
);

CREATE TABLE IF NOT EXISTS tg_peer_info (
    peer_id INTEGER NOT NULL,
    hash INTEGER,
    subtype INTEGER,
    PRIMARY KEY (peer_id)
);

CREATE TABLE IF NOT EXISTS tg_update_state (
    pts INTEGER NOT NULL,
    qts INTEGER NOT NULL,
    date INTEGER NOT NULL,
    seq INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS tg_channel_state (
    peer_id INTEGER NOT NULL,
    pts INTEGER NOT NULL,
    PRIMARY KEY (peer_id)
);

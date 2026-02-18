# Configuration System

## Dual Config: File + Database

**File config (TOML):**
- Primary config for deployment, CI/CD, declarative setups (NixOS module)
- Defines: daemon settings, database path, opencode path, global defaults
- Can also define sources and output channels declaratively
- Read-only mode supported: if config file defines sources/channels, they cannot be modified via web UI (shown as locked)

**Database:**
- Stores runtime state: TG sessions, generation history, content store
- Stores user-created sources and output channels (from web UI)
- File config is loaded on startup and merged with DB state
- Conflicts: file config wins for items it defines; DB stores user additions

## Config File Structure

```toml
[pail]
version = 1                         # config schema version (for future migration support)
listen = "0.0.0.0:8080"             # HTTP server bind address
data_dir = "./data"                 # data directory (PAIL_DATA_DIR env var overrides)
retention = "7d"                    # content retention period
timezone = "Europe/Kyiv"            # user timezone for schedule interpretation (default: UTC)
log_level = "info,grammers_session=warn,grammers_mtsender=warn,grammers_mtproto=warn"
max_concurrent_generations = 1
# feed_token = "my-secret-token"  # optional: if omitted, auto-generated on first run

[database]
# SQLite by default. Path relative to data_dir if not absolute.
path = "pail.db"                    # resolves to <data_dir>/pail.db
# Optional: PostgreSQL backend (future)
# url = "postgres://pail:secret@localhost:5432/pail"

[opencode]
binary = "opencode"
default_model = "opencode/kimi-k2.5-free"
timeout = "10m"
max_retries = 1
system_prompt = """..."""            # required, must contain {editorial_directive}

[telegram]
enabled = false
api_id = 12345
api_hash = "abc123"

[[source]]
name = "Hacker News"
type = "rss"
url = "https://hnrss.org/frontpage"
poll_interval = "15m"

[[source]]
name = "Private Feed"
type = "rss"
url = "https://example.com/feed.xml"
[source.auth]
type = "bearer"
token = "my-api-token"

[[source]]
name = "Ukrainian Tech News"
type = "telegram_channel"
tg_username = "tech_ukraine"
description = "Ukrainian tech industry news channel, posts in Ukrainian"

[[source]]
name = "News Folder"
type = "telegram_folder"
tg_folder_name = "News"
exclude = ["@some_noisy_channel"]

[[output_channel]]
name = "Morning Tech Digest"
slug = "tech-morning"
schedule = "at:08:00"
model = "opencode/kimi-k2.5-free"
sources = ["Hacker News", "Lobsters", "Ukrainian Tech News"]
prompt = """
Write a morning tech digest for a senior software engineer.
"""

[[output_channel]]
name = "News Folder Digest"
slug = "news-digest"
schedule = "at:08:00,20:00"
mark_tg_read = true
sources = ["News Folder"]
prompt = """
Summarize the key topics from my Telegram news channels.
"""
```

## Source Name References

In TOML config, output channel `sources` reference source names (resolved to UUIDs on startup). Source names must be unique within the config file. In the DB/API, sources are always referenced by UUID.

## Config Validation

On startup:
1. Parse config file, validate against schema
2. Report errors clearly (file, line, field, expected vs. got)
3. In read-only mode: if DB has conflicting items, log warnings but file config wins
4. If config references TG sources but `[telegram].enabled = false`, fail with a validation error
5. Validate schedule expressions
6. Validate source references in output channels
7. Validate source names: must contain at least one alphanumeric character; allowed characters are letters, digits, spaces, `- _ . ( ) & , + '`
8. Validate source descriptions (if provided): no control characters, double quotes, or backslashes
9. Validate output channel slugs: non-empty, lowercase letters + digits + hyphens only, cannot start or end with hyphen
10. Validate system prompt: must be non-empty and contain `{editorial_directive}` placeholder
11. Validate duration fields (`retention`, `opencode.timeout`): parsed via `humantime`

## Source Removal Cascade

Removing a source from config deletes it and all its content items from the DB on next startup (`ON DELETE CASCADE`). Re-adding the same source later starts fresh with no history.

## Decisions

- **Config format:** TOML.
  Options: TOML / YAML / JSON / HCL.
  Rationale: human-readable, good Rust ecosystem support (serde), standard for Rust projects. Fits declarative NixOS-style setups.

- **Dual config (file + DB):** file config wins on conflicts.
  Options: file wins / DB wins / merge with conflict errors / file-only.
  Rationale: file config represents the declarative "desired state." DB stores user additions from web UI. Declarative setups need file to be authoritative.

- **Source names in TOML:** resolved to UUIDs on startup.
  Options: UUIDs in config / names in config / both supported.
  Rationale: names are ergonomic for manual config editing. UUIDs would be error-prone to type.

- **Source name validation:** must contain at least one alphanumeric; allowed chars: letters, digits, spaces, `- _ . ( ) & , + '`.
  Options: unrestricted / alphanumeric only / restricted charset.
  Rationale: names appear in YAML frontmatter in generation workspace files. Restricted charset ensures they're safe without escaping.

- **Source removal cascade:** `ON DELETE CASCADE` removes all content items.
  Options: cascade delete / soft-delete / orphan content items.
  Rationale: re-adding the same source starts fresh. Orphaned content with no source is useless. Cascade is clean.

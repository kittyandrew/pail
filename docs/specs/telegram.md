# Telegram Integration

Telegram as an input source type via MTProto userbot (grammers).

## Library

- **Repository:** https://codeberg.org/Lonami/grammers (Codeberg is canonical; GitHub mirror is archived)
- **Author:** Lonami (same author as Telethon for Python)
- **Language:** Rust
- **Status:** Pre-1.0, actively developed

## Source Types

### Channels

- Live event stream via grammers MTProto session — no backfill on source addition
- New messages are stored as they arrive via the live event stream
- Store message text, links, forwarded-from info, reply chains
- For media messages: store caption text, note media type, skip binary content
- For voice/video messages: flag as "media — no transcript" (speech-to-text is a future feature)

### Groups/Chats

Same as channels, but:
- Messages have senders (not anonymous)
- May have threaded replies
- Content may be more conversational — the AI prompt handles this
- Key use case: digest every 12 hours of ongoing topics, with links to discussion beginnings and key messages/references

### Folders

- Read folder definitions via `messages.getDialogFilters` on startup and when a new folder-type source is created
- A folder source = all channels/groups within that folder
- Channel name and username are resolved via a single batched `channels.getChannels` call per folder (all channel peers in one request)
- Channels within a folder can be individually disabled via the web UI
- **Live folder membership updates:** MTProto delivers [`updateDialogFilter`](https://core.telegram.org/constructor/updateDialogFilter) and [`updateDialogFilterOrder`](https://core.telegram.org/api/folders) events when the user modifies folders in any Telegram client. pail listens for these events and immediately updates its folder-to-channel mapping — no polling needed.
- New channels added to the folder by the user in Telegram are automatically picked up via these events

**Folder source splitting:** `telegram_folder` sources are split into per-channel files in the generation workspace `sources/`. Each channel within the folder gets its own file with the real channel name in frontmatter. This gives the AI correct per-channel identity for attribution and skipped-item labeling.

## Read-Only Contract

- NEVER send messages, join/leave channels, or modify anything via the TG API
- The ONLY write operation allowed: optionally mark channels/groups as "read" after digest generation (configurable, default off)
- Never access private DM conversations
- Only access channels/groups the user is already subscribed to

## Session Management

- grammers uses a session to persist MTProto authorization (auth keys, DC options, update state, cached peers)
- **CLI login wizard:** `pail tg login` — interactive command that handles the full MTProto auth flow:
  1. Prompts for phone number (masked in logs: `+380****1234`)
  2. Sends/receives verification code
  3. Prompts for 2FA password if enabled (echo suppressed via `rpassword`)
  4. Saves session to the database
- **Custom `Session` trait implementation backed by sqlx.** grammers' built-in `SqliteSession` uses `libsql` (a sqlite3 fork by Turso), which statically links its own bundled sqlite3 via `libsql-ffi`. pail uses `sqlx` for its database, which depends on `libsqlite3-sys` (upstream sqlite3). Both produce duplicate symbols at link time. **Solution:** disable grammers-session's `sqlite-storage` feature and implement the `Session` trait ourselves using pail's existing sqlx `SqlitePool`.
- **Peer cache warming:** Sources configured with a bare `tg_id` (no `@username`) never trigger a `resolve_username` API call, so their access hashes may not be in the peer cache. On startup (both CLI and daemon), pail checks for uncached peers among direct TG sources and, if any are found, iterates the user's full dialog list via `messages.getDialogs`. grammers auto-caches all peers from the response.
- Session must be long-lived — reconnects automatically on network issues
- The daemon itself never prompts for input — if the session is missing/expired, it logs an error and disables TG sources until `pail tg login` is re-run

## Event Handling

The Telegram session runs in a background task (tokio async):
```
loop {
    match client.next_update().await {
        NewMessage(msg) => {
            if msg.chat is in subscribed_sources {
                store_content_item(msg)
            }
        }
        // handle other relevant events
    }
}
```

## Gap Handling

pail does **not** backfill Telegram history on source addition or daemon restart. In daemon mode, content is only collected via the live event stream.

**CLI exception:** `pail generate` performs one-shot `messages.getHistory` fetching for TG sources in the target output channel. The fetch window matches the generation time window (since `last_generated` or `--since`).

If the daemon was down:
1. Messages sent during downtime are **missed** — accepted trade-off for simplicity
2. The next scheduled generation covers content since `last_generated`, which may have gaps
3. For brief outages, MTProto's built-in update gap recovery may deliver missed messages automatically on reconnect

**Rationale:** Backfill requires `getHistory` calls per channel (rate-limited at 1-2s each). For 50+ channels, that's 1-2 minutes of API calls on every restart, with FloodWait risk. Live events are passive and free.

## Content Extraction

For each message, extract and store:
- `message_id` — for constructing `t.me` links
- `date` — message timestamp
- `text` — message text with entities resolved (links, mentions, etc.)
- `sender` — username or name (for groups; anonymous for channels)
- `reply_to_msg_id` — for threading context
- `forward_from` — if forwarded, original source
- `media_type` — extracted from grammers `Media` enum: "photo", "document", "sticker", "contact", "poll", "geo", "dice", "venue", "geo_live", "webpage", or "other". Note: video and voice messages appear as "document" in grammers since they're `Document` variants internally.
- `url` — `t.me` link to the message itself (public: `https://t.me/<username>/<id>`, private: `https://t.me/c/<numeric_id>/<id>`)

## Rate Limiting

- Respect all FloodWait errors with proper backoff (grammers handles this automatically at the RPC level)
- CLI history fetching (`pail generate`) adds a 500ms delay between consecutive channel `getHistory` calls to avoid aggressive API bursts
- Live event stream is passive (no API calls for receiving updates)

## Mark-as-Read (Optional)

After a successful digest generation, optionally mark the consumed channels/groups as read:
- Uses `messages.readHistory` / `channels.readHistory`
- Configurable per output channel (default: off)
- This is the ONLY write operation pail performs on Telegram

## TLS Note

MTProto uses its own encryption (AES-CTR, RSA, DH) over raw TCP — no TLS involved. All grammers crypto crates are pure Rust with no C dependencies.

## Config

```toml
[telegram]
enabled = false
api_id = 12345                      # from my.telegram.org
api_hash = "abc123"                 # from my.telegram.org
# Session stored in the database — no session file.

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
```

## Decisions

- **Telegram library:** grammers.
  Options: grammers (Rust) / TDLib (C++, FFI) / Telethon (Python) / GramJS (JS/TS) / Pyrogram (Python).
  Rationale: Rust-native MTProto by Lonami (Telethon author). No FFI, no separate runtime. Pre-1.0 but actively developed.

- **Session storage:** custom `Session` trait backed by pail's sqlx `SqlitePool`.
  Options: grammers built-in `SqliteSession` / custom `Session` impl with sqlx / file-based session.
  Rationale: grammers' `SqliteSession` depends on `libsql` which bundles a sqlite3 fork that produces duplicate symbols at link time with sqlx's `libsqlite3-sys`. Custom impl also simplifies multi-user migration (no separate session files per user).

- **Backfill on source addition / daemon restart:** none (live events only in daemon mode).
  Options: backfill via `getHistory` / no backfill / configurable.
  Rationale: backfill requires per-channel `getHistory` calls (rate-limited 1-2s each). For 50+ channels that's 1-2 minutes of API calls with FloodWait risk. Live events are passive and free. CLI `generate` does fetch history as a one-shot exception.

- **Folder membership refresh:** live via MTProto `updateDialogFilter` events.
  Options: periodic polling of `getDialogFilters` / live events / manual refresh.
  Rationale: MTProto delivers folder change events natively (Layer 111+), no polling needed.

- **Folder source splitting in workspace:** per-channel files, not one file per folder.
  Options: one file per folder / one file per channel within folder.
  Rationale: gives the AI correct per-channel identity for attribution and skipped-item labeling. Without splitting, all items appear as coming from the folder name.

- **TG login:** CLI wizard (`pail tg login`).
  Options: CLI wizard / web UI login / config-only (session file path).
  Rationale: interactive MTProto auth requires real-time user input (phone, code, 2FA). CLI is simplest for initial implementation. Web UI login deferred to multi-user phase.

- **Rate limiting for CLI history fetch:** 500ms delay between consecutive `getHistory` calls.
  Options: no delay / 500ms / 1s / adaptive based on FloodWait.
  Rationale: avoids aggressive API bursts without being overly conservative. grammers handles FloodWait automatically at the RPC level as a safety net.

- **Mark-as-read:** optional, off by default.
  Options: always mark read / never / configurable per output channel.
  Rationale: this is the only TG write operation pail performs — keep it opt-in to respect the read-only contract.

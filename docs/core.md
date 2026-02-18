# pail — Core Architecture

## 1. Project Overview

**pail** (Personal AI Lurker) — a self-hosted service that passively monitors RSS feeds and Telegram channels/groups/folders, then uses AI (via [opencode](https://github.com/anomalyco/opencode)) to generate high-quality digest articles published as Atom feeds.

**Name:** "a bucket that collects and distills information."

### Core Value Proposition

Replace the daily chore of reading dozens of RSS feeds and Telegram channels with a single, curated, AI-written digest per topic — with editor notes, fact-checking, and references to original sources.

---

## 2. Problem Statement

A technically inclined user subscribes to many RSS feeds and Telegram channels across topics (tech news, security, local news, niche communities). The volume is overwhelming:
- Most content is noise
- Interesting discussions in Telegram chats are easy to miss
- Foreign-language channels are useful but hard to follow
- No tool exists that bridges RSS and Telegram into a unified, AI-summarized digest

Existing solutions are fragmented:
- RSS+AI tools (RSSbrew, auto-news, Matcha) don't support Telegram
- Telegram summarizer bots are small, limited to single chats, and don't produce structured digests
- No open-source project combines both source types with configurable AI-driven article generation

---

## 3. Target Users

**Primary:** Self-hosting enthusiasts who want automated, high-quality news/content digests from their existing information sources (RSS + Telegram).

**Secondary:** Small teams or friend groups who want a shared digest service with per-user source configuration and topic focus.

---

## 4. Core Concepts

### 4.1 Source
An input data stream. Types:
- **RSS Feed** — standard RSS/Atom feed URL (see [RSS Sources spec](specs/rss-sources.md))
- **Telegram Channel** — public or subscribed channel (see [Telegram spec](specs/telegram.md))
- **Telegram Group/Chat** — group the user is a member of
- **Telegram Folder** — a Telegram client-side folder containing multiple channels/groups

### 4.2 Output Channel
A named configuration that defines:
- Which sources feed into it (one or many)
- A schedule (e.g., `at:08:00`, `at:08:00,20:00`, `weekly:monday,08:00`) — see [Atom Feed spec](specs/atom-feed.md)
- A system prompt / editorial directive (focus areas, tone, language, fact-checking preferences)
- LLM model preference (passed to opencode)
- The resulting Atom feed URL (e.g., `http://localhost:8080/feed/<username>/tech-digest.atom`)

### 4.3 Content Store
A local buffer of ingested content (messages, articles, posts) indexed by source and timestamp. Content is **retained after generation** — items are not deleted on consumption. Which items were included in a given digest is tracked via `generated_article.content_item_ids`. Content is pruned when:
- It exceeds the configurable retention TTL (default: 7 days after ingestion)
- It is explicitly flushed by the user
- **Its source is removed from config** — on startup, config-to-DB sync deletes sources not present in the config file, and `ON DELETE CASCADE` removes all their content items. Removing a source from config and re-adding it later starts fresh with no historical content.

This allows re-generation, debugging, and reuse of the same content across multiple output channels.

### 4.4 Digest Article
The AI-generated output. A single article in the output Atom feed containing:
- A synthesized summary of activity across the output channel's sources
- Per-topic or per-source sections as appropriate
- Inline references/links to original messages, posts, or articles
- Optional "Editor Notes" sections where the AI flags questionable claims, provides fact-checks, or adds context
- Generated title and publication date

---

## 5. Architecture

### 5.1 High-Level

```
┌─────────────────────────────────────────────────────────────┐
│                        pail daemon                           │
│                                                              │
│  ┌──────────────────┐     ┌──────────────────────────────┐  │
│  │   Ingest Layer    │     │       Content Store           │  │
│  │                  │     │  (local DB / files)           │  │
│  │  RSS Poller      │────▶│                              │  │
│  │  (periodic fetch)│     │  Messages, articles, posts   │  │
│  │                  │     │  indexed by source + time    │  │
│  │  TG Session      │────▶│                              │  │
│  │  (live events)   │     │  Retention: configurable     │  │
│  └──────────────────┘     │  (default 7 days)            │  │
│                           └──────────┬───────────────────┘  │
│                                      │                       │
│  ┌──────────────────┐                │                       │
│  │   Scheduler       │               │                       │
│  │                  │               │                       │
│  │  Per output-chan  │───────────────┘                       │
│  │  schedule ticks   │                                       │
│  │                  │     ┌──────────────────────────────┐  │
│  │  On tick:         │────▶│   Generation Engine          │  │
│  │  1. Collect data  │     │                              │  │
│  │  2. Write to disk │     │  1. Prepare data files       │  │
│  │  3. Invoke        │     │  2. Invoke opencode          │  │
│  │     opencode      │     │     (subprocess)             │  │
│  │  4. Parse output  │     │  3. opencode reads files,    │  │
│  │  5. Publish to    │     │     dispatches subagents,    │  │
│  │     Atom feed     │     │     writes article           │  │
│  └──────────────────┘     │  4. Parse result, validate   │  │
│                           └──────────────────────────────┘  │
│                                                              │
│  ┌──────────────────┐     ┌──────────────────────────────┐  │
│  │   Feed Server     │     │       Web UI (planned)        │  │
│  │  (HTTP)           │     │  (not yet implemented —       │  │
│  │                  │     │   see ideas/web-ui.md)       │  │
│  │  /feed/<user>/    │     │                              │  │
│  │                  │     │  - Browse TG channels/folders│  │
│  │  Supports:        │     │  - Toggle sources on/off     │  │
│  │  - Atom 1.0       │     │  - Edit prompts, schedules   │  │
│  │                  │     │  - View generation logs      │  │
│  └──────────────────┘     └──────────────────────────────┘  │
│                                                              │
│  ┌──────────────────┐                                        │
│  │   SQLite          │  Users, output channels, state,       │
│  │   (on-disk file)  │  generation history, TG session       │
│  └──────────────────┘                                        │
└─────────────────────────────────────────────────────────────┘
```

### 5.2 Process Model

pail runs as a single long-lived daemon process with:
- **Telegram session** — persistent MTProto connection via grammers, receiving live events (new messages in subscribed channels/groups). Events are written to the content store as they arrive.
- **RSS poller** — periodic fetcher (configurable interval per feed, default every 30 minutes). Fetched items written to content store.
- **Scheduler** — checks output channel schedules, triggers generation when a tick is due.
- **HTTP server** — serves generated Atom feeds and the web UI.
- **Cleanup job** — periodic (e.g., hourly) sweep to delete content older than the retention window.

See [Daemon spec](specs/daemon.md) for details.

### 5.3 No Cron Jobs

The scheduler is internal to the daemon. No external cron/systemd timers needed. The daemon manages its own schedule state, persisted to DB so it survives restarts (knows when the last generation happened per output channel).

---

## 6. Data Model

### 6.1 Source

```
source {
    id: UUID
    type: "rss" | "telegram_channel" | "telegram_group" | "telegram_folder"
    name: String               # human-readable label
    enabled: bool              # global toggle
    # RSS-specific
    url: Option<String>        # RSS feed URL
    poll_interval: Duration    # default 30m
    max_items: u32             # max items to keep per poll (default: 200)
    auth: Option<SourceAuth>   # feed authentication (see below)
    # Telegram-specific
    tg_id: Option<i64>        # Telegram chat/channel ID
    tg_username: Option<String> # @username if available
    tg_folder_id: Option<i32>  # Telegram folder ID (resolved at runtime from tg_folder_name, stored in DB)
    tg_folder_name: Option<String> # folder name (used in config, resolved to ID via getDialogFilters)
    exclude: Option<Vec<String>> # TG @usernames to exclude from a folder source
    description: Option<String>  # user-provided context about the source (language, focus area, reliability)
}

source_auth {
    type: "basic" | "bearer" | "header"
    # basic: username + password (HTTP Basic Auth)
    username: Option<String>
    password: Option<String>
    # bearer: token (Authorization: Bearer <token>)
    token: Option<String>
    # header: custom header name + value (e.g., X-API-Key)
    header_name: Option<String>
    header_value: Option<String>
}
```

### 6.2 Output Channel

```
output_channel {
    id: UUID
    name: String               # human-readable label
    slug: String               # URL-safe, unique per user, used in feed path: /feed/<username>/<slug>.atom
    sources: Vec<UUID>         # list of source UUIDs
    schedule: Option<Schedule>  # wall-clock times (see atom-feed spec); None for CLI-only channels
    prompt: String             # editorial directive for the AI
    model: Option<String>      # LLM model preference (passed to opencode)
    language: Option<String>   # output language (for translation use case)
    enabled: bool
    last_generated: Option<DateTime>
}
```

### 6.3 Content Item

```
content_item {
    id: UUID
    source_id: UUID
    ingested_at: DateTime
    original_date: DateTime    # publication date from source
    content_type: "text" | "link" | "media" | "forward"
    title: Option<String>      # for RSS articles
    body: String               # full text / message text
    url: Option<String>        # link to original
    author: Option<String>
    metadata: JSON             # source-specific extras (TG message_id, reply_to, forward_from, etc.)
    dedup_key: String          # GUID or hash — UNIQUE(source_id, dedup_key)
    upstream_changed: bool     # true if a later fetch saw different body/title for the same dedup_key
}
```

### 6.4 Generated Article

```
generated_article {
    id: UUID
    output_channel_id: UUID
    generated_at: DateTime
    covers_from: DateTime      # start of time window
    covers_to: DateTime        # end of time window
    title: String
    topics: Vec<String>        # AI-generated topics from frontmatter, used for Atom feed categories
    body_html: String          # cached HTML (rendered from markdown via pulldown-cmark)
    body_markdown: String      # source of truth — the full article in markdown
    content_item_ids: Vec<UUID> # all items available in the time window
    generation_log: String     # opencode stdout/stderr for debugging
    model_used: String         # which model opencode actually used
    token_count: Option<i64>   # if reported by opencode
}
```

---

## 7. Technical Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Language | Rust | Performance, safety, testability, single static binary |
| Telegram lib | [grammers](https://codeberg.org/Lonami/grammers) (Codeberg) | By Lonami (Telethon author), Rust-native MTProto. Pre-1.0, actively developed (last commit Feb 2026). |
| LLM integration | Shell out to [opencode](https://github.com/anomalyco/opencode) | Gets all model support, MCP, tools, agentic for free. Non-interactive via `opencode run`. |
| Config | TOML + DB | TOML for declarative/NixOS; DB for web UI and runtime state |
| DB | SQLite (default) | On-disk file in data_dir, docker volume. WAL mode for concurrent reads. PostgreSQL as optional backend later. |
| DB IDs | UUID v4 | All primary keys use UUID4, not auto-increment integers. |
| HTTP framework | [axum](https://github.com/tokio-rs/axum) | Async, tower-based, well-maintained, good ecosystem |
| Async runtime | [tokio](https://github.com/tokio-rs/tokio) | Standard for Rust async |
| Feed parsing | [feed-rs](https://github.com/feed-rs/feed-rs) | Mature Rust RSS/Atom/JSON Feed parser (~196 stars, actively maintained) |
| Feed generation | [`atom_syndication`](https://github.com/rust-syndication/atom) | Standard Rust Atom 1.0 serializer (~97 stars, v0.12.7, actively maintained) |
| Feed output | Atom 1.0 ([RFC 4287](https://www.rfc-editor.org/rfc/rfc4287)) | Strictly specified, universal reader support. Additional formats deferred. |
| Markdown to HTML | [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) | Standard Rust markdown parser |
| Serialization | serde + toml + serde_json | Standard Rust serialization |
| YAML frontmatter | [`gray_matter`](https://lib.rs/crates/gray_matter) | `serde_yaml` is deprecated. **Avoid `serde_yml`** ([RUSTSEC-2025-0068](https://rustsec.org/advisories/RUSTSEC-2025-0068.html)). |
| DB migrations | Custom runner (`include_str!` + sqlx) | Embedded migrations compiled into the binary, run on startup. See Decisions below. |

---

## 8. Non-Goals

- **Not a feed reader UI** — pail generates Atom feeds; you use your own reader (FreshRSS, Miniflux, Newsboat, etc.)
- **Not a chatbot** — pail does not respond to messages or interact in Telegram
- **Not real-time** — digests are generated on a schedule, not on every new message
- **Not a general-purpose automation tool** — it does one thing (digest generation) well
- **Not a crawler** — it reads feeds and channels you're already subscribed to; it does not discover new sources
- **No mobile app** — consumed via RSS, which already has great mobile readers

---

## 9. Open Questions

1. **grammers maturity:** How production-ready is grammers for long-running sessions? Need to evaluate reconnection handling, error recovery, and memory usage over days/weeks of continuous operation. (grammers is pre-1.0; last commit Feb 2026.)

2. **Content tokenization:** Before preparing workspace files for opencode, we should estimate token counts to split large sources into manageable files. Rough estimation (chars/4) is probably fine for splitting; opencode/the LLM handles the actual context window management.

3. **Article quality feedback loop:** Manual prompt editing is sufficient initially. A thumbs-up/down on articles in the web UI could feed into prompt tuning later.

4. **Telegram API credentials:** `api_id` and `api_hash` from [my.telegram.org](https://my.telegram.org) are per-app, not per-user. Multiple users can share the same app credentials — each user authenticates with their phone number.

---

## 10. References

### Existing Projects Researched

**RSS + AI (most relevant):**
- [auto-news](https://github.com/finaldie/auto-news) — 828 stars, Python, most feature-complete RSS+AI aggregator
- [RSSbrew](https://github.com/yinan-c/RSSbrew) — 268 stars, Python, first-class daily/weekly digest
- [Matcha](https://github.com/piqoni/matcha) — 716 stars, Go, daily markdown digest, in nixpkgs
- [UglyFeed](https://github.com/fabriziosalmi/UglyFeed) — 297 stars, Python, article rewriting
- [Precis](https://github.com/leozqin/precis) — 87 stars, Python, notification-first, Nix support
- [RSS-GPT](https://github.com/yinan-c/RSS-GPT) — Python, GitHub Actions, serverless
- [Folo](https://github.com/RSSNext/Folo) — ~37k stars (Feb 2026), TypeScript, daily AI digest ([not self-hostable](https://github.com/RSSNext/Folo/issues/4164))
- FreshRSS + AI extensions (FeedDigest, AI Assistant, NewsAssistant)
- Miniflux + AI add-ons (miniflux-ai by Qetesh, by zhu327)

**Telegram summarization (most relevant):**
- [telegram-chat-summarizer](https://github.com/dev0x13/telegram-chat-summarizer) — 92 stars, Python, Telethon userbot
- [telegram-summary-bot](https://github.com/asukaminato0721/telegram-summary-bot) — 185 stars, TypeScript, CF Workers
- [summary-gpt-bot](https://github.com/tpai/summary-gpt-bot) — 238 stars, Python, content summarization
- [Telegram-Summarize-Bot](https://github.com/dudynets/Telegram-Summarize-Bot) — 75 stars, Python, local Ollama
- Telegram's native AI summaries (Jan 2026) — individual posts only

**Multi-source / workflow:**
- [n8n](https://github.com/n8n-io/n8n) — 174k stars, workflow automation with RSS+TG+AI templates
- [Huginn](https://github.com/huginn/huginn) — 48.7k stars, Ruby, agent-based automation
- [ChatArk](https://www.chatark.app/) — SaaS, TG+Discord+WhatsApp monitoring
- [RSSHub](https://github.com/DIYgod/RSSHub) — 41.8k stars, converts TG channels to RSS

**Telegram client libraries:**
- [grammers](https://codeberg.org/Lonami/grammers) — Rust MTProto, by Lonami (Telethon author)
- [Telethon](https://github.com/LonamiWebs/Telethon) — 11.8k stars, Python MTProto
- [Pyrogram](https://github.com/pyrogram/pyrogram) — 4.6k stars, Python MTProto
- [GramJS](https://github.com/gram-js/gramjs) — 1.6k stars, JS/TS MTProto
- [TDLib](https://github.com/tdlib/td) — official C++ library

### Key Technical Constraints

- **Telegram Bot API cannot read message history** — bots can only receive updates for new messages, not fetch history ([Bot API docs](https://core.telegram.org/bots/api)). Userbot (MTProto) with `messages.getHistory` is required.
- **Telegram FloodWait** — dynamic rate limits, must respect with exponential backoff ([MTProto errors](https://core.telegram.org/api/errors#420-flood))
- **Telegram folders** — client-side feature, accessible via [`messages.getDialogFilters`](https://core.telegram.org/method/messages.getDialogFilters) MTProto method. Live updates via [`updateDialogFilter`](https://core.telegram.org/constructor/updateDialogFilter) (Layer 111+).
- **No existing tool combines RSS + Telegram + AI digest** — this is the gap pail fills

**LLM integration:**
- [opencode](https://github.com/anomalyco/opencode) — CLI agentic coding/tool-use assistant. Non-interactive mode via `opencode run`. [Docs](https://opencode.ai/docs/cli/).

**Telegram API references:**
- [Telegram Folders API](https://core.telegram.org/api/folders) — folder update events, `getDialogFilters`
- [updateDialogFilter constructor](https://core.telegram.org/constructor/updateDialogFilter) — push update for folder changes
- [Atom 1.0 / RFC 4287](https://www.rfc-editor.org/rfc/rfc4287) — feed output format specification

---

## Decisions

- **Project name:** pail (Personal AI Lurker).
  Options: `lurkai` / `ailur` (red panda genus) / `pailurker` / `pail`.
  Rationale: short, memorable, "a bucket that collects and distills information."

- **Language:** Rust.
  Options: Rust / Python / Go / TypeScript.
  Rationale: performance, safety, testability, single static binary.

- **Database:** SQLite (default), PostgreSQL as optional backend later.
  Options: SQLite / PostgreSQL / both.
  Rationale: on-disk file fits single-user self-hosted use case, docker volume, WAL mode for concurrent reads. PostgreSQL deferred for multi-user scaling.

- **DB primary keys:** UUID v4.
  Options: UUID v4 / auto-increment integers.
  Rationale: globally unique, no coordination needed, safe for distributed/multi-user future.

- **Content lifecycle:** retained with configurable TTL (default 7 days), not deleted on consumption.
  Options: delete after generation / retain with TTL / retain forever.
  Rationale: allows re-generation, debugging, multi-channel reuse. `consumed_by` tracking removed — `generated_article.content_item_ids` is sufficient.

- **Source reference format:** UUID4 database IDs internally, human-readable names in TOML config (resolved on startup).
  Options: UUID everywhere / names everywhere / names in config, UUIDs in DB.
  Rationale: names are ergonomic for config editing, UUIDs are unambiguous for DB/API.

- **DB migrations:** custom runner (`db.rs`) with `include_str!` embedding, not sqlx's built-in `migrate!()` macro.
  Options: sqlx `migrate!()` macro / custom runner with `include_str!` + sqlx for execution.
  Rationale: sqlx `migrate!()` wraps each migration in a transaction. SQLite's `PRAGMA foreign_keys = OFF` [cannot be set inside a transaction](https://github.com/launchbadge/sqlx/issues/2085) — the statement silently does nothing. This makes table-recreation migrations unsafe: `DROP TABLE` with `ON DELETE CASCADE` would cascade-delete child rows (e.g., `generated_articles`, `output_channel_sources`) because foreign keys remain enforced. Table recreation is the only way to change column constraints in SQLite (no `ALTER COLUMN`), so this is a recurring need. The custom runner uses `pool.execute(sql)` which runs all statements via `sqlite3_exec` on a single connection with no wrapping transaction, so PRAGMA changes take effect immediately. Trade-off: no checksum validation (detects edited migrations) or dirty-state detection — acceptable since migrations are append-only and immutable by convention. sqlx 0.9+ adds a `-- no-transaction` directive for individual migrations, but is not yet stable.

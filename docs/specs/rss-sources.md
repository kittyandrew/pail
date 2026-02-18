# RSS Sources

RSS feed ingestion and parsing.

## Source Types

- RSS 2.0, Atom 1.0, JSON Feed 1.1 (all parsed via [`feed-rs`](https://github.com/feed-rs/feed-rs))

## Ingestion

- Standard RSS 2.0, Atom, JSON Feed parsing
- Periodic polling at configurable intervals (daemon mode) or one-shot HTTP fetch (CLI mode)
- ETag / Last-Modified / If-None-Match for efficient polling
- Full article content extraction where available (prefer `content:encoded` over `description`)
- For feeds that only provide summaries, see [Full-Text Extraction idea](../ideas/full-text-extraction.md)

## Content Stored

- Title, link, author, publication date, full body text

## Authentication

Configured per-source via `auth` field in the source config:

- **HTTP Basic Auth** — username + password
- **Bearer token** — `Authorization: Bearer <token>`
- **Custom header** — arbitrary header name + value (e.g., `X-API-Key`)

```toml
[[source]]
name = "Private Feed"
type = "rss"
url = "https://example.com/feed.xml"
[source.auth]
type = "bearer"
token = "my-api-token"
```

## Polling

- Configurable per-feed poll interval (default: 30 minutes)
- Global minimum interval to prevent abuse (default: 5 minutes)
- Timeout per request: 30 seconds
- Uses a standard HTTP client (reqwest) per fetch call
- Respects `Cache-Control`, `ETag`, `Last-Modified` headers
- Saves HTTP cache headers and `last_fetched_at` so conditional GETs work on subsequent runs

## Deduplication

- Dedup via `content_item.dedup_key`: use GUID if available, otherwise SHA-256 hex digest of URL + title
- SHA-256 chosen over `DefaultHasher` because the standard library hasher is not stable across Rust versions — upgrading the compiler could silently change hash outputs, causing mass re-ingestion
- Enforced by `UNIQUE(source_id, dedup_key)` constraint

### Immutable Ingestion

On dedup key collision, the stored content is **not** overwritten. The original body, title, and dates are preserved as-is. However, if the incoming body or title differs from what's stored, the `upstream_changed` flag is set to `true` — indicating the source has been edited upstream. This keeps the content store stable for digest generation while still tracking staleness.

## Edge Cases

- Feeds behind authentication — configured per-source via `auth` field
- Feeds that change item GUIDs (dedup by URL + title hash)
- Very large feeds (limit to most recent items per poll, configurable via `max_items`, default 200)

## Config

```toml
[[source]]
name = "Hacker News"
type = "rss"
url = "https://hnrss.org/frontpage"
poll_interval = "15m"
# max_items = 200            # max items to keep per poll (default: 200)
```

## Decisions

- **Feed parsing library:** feed-rs.
  Options: feed-rs / rss + atom_syndication (separate crates) / custom parser.
  Rationale: mature, supports RSS 2.0, Atom 1.0, and JSON Feed 1.1 in one crate.

- **Dedup key hashing:** SHA-256.
  Options: SHA-256 / `DefaultHasher` / MD5 / no hash (GUID only).
  Rationale: `DefaultHasher` is not stable across Rust versions — compiler upgrades could silently change hash outputs, causing mass re-ingestion. SHA-256 is stable and well-known.

- **Dedup constraint:** `UNIQUE(source_id, dedup_key)`.
  Options: `UNIQUE(source_id, dedup_key)` / `UNIQUE(source_id, url)`.
  Rationale: `UNIQUE(source_id, url)` breaks on NULL URLs (some feed items lack URLs).

- **Content on dedup collision:** immutable — don't overwrite, set `upstream_changed` flag.
  Options: overwrite with latest / keep original + flag / keep original silently.
  Rationale: keeps content store stable for digest generation (what was ingested is what gets used) while tracking staleness for future tooling.

- **Max items per RSS poll:** configurable per-source via `max_items`, default 200.
  Options: unlimited / fixed cap / configurable per-source.
  Rationale: prevents very large feeds from overwhelming the content store, while allowing tuning for high-volume feeds.

- **Content preference:** `content:encoded` over `description`/`summary`.
  Options: prefer full content / prefer summary / configurable.
  Rationale: full content gives the AI more to work with; summaries are a fallback.

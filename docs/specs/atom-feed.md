# Atom Feed Output

Output channel configuration, scheduling, and Atom 1.0 feed serving.

## Feed URL

Each output channel publishes an Atom feed at:
```
http://<host>:<port>/feed/<username>/<slug>.atom
```

In single-user mode, `<username>` is hardcoded to `default`: `http://localhost:8080/feed/default/tech-digest.atom`.

## Atom 1.0

Atom 1.0 (RFC 4287) is used exclusively — strictly specified, universal reader support. Additional formats (RSS 2.0, JSON Feed) may be added later.

### Feed Metadata

- **Title:** output channel name
- **Subtitle:** output channel name (Atom `<subtitle>`)
- **Author:** `pail-opencode-<model>` per entry (e.g., `pail-opencode-opus-4.6`), derived from `generated_article.model_used`. Set per-entry since different articles may use different models.
- **Link:** `<link rel="self">` pointing to the feed's own URL (derived from request `Host` and `X-Forwarded-Proto` headers, without auth token)
- **Items:** generated articles, most recent first, limited to last 50

### Article Entries

- **Title:** AI-generated title
- **Content:** full HTML article body
- **Publication date:** generation timestamp
- **ID:** `urn:uuid:<article_id>` (Atom `<id>` must be an IRI per RFC 4287 §4.2.6)
- **Link:** `<link rel="alternate">` pointing to `/article/<article_id>`, an unauthenticated HTML permalink. The article UUID (v4, 122 bits of entropy) is unguessable.
- **Categories:** AI-generated topics

## Feed Authentication

Output feeds require authentication. Two methods supported:

1. **HTTP Basic Auth** — username + per-user feed token as password. Supported by most self-hosted RSS readers (FreshRSS, Miniflux, Newsboat, Feedbin, tt-rss). URL format: `https://user:token@host/feed/user/slug.atom`
2. **Query parameter token** — `?token=<secret>` appended to the feed URL. Works with readers that don't support HTTP auth. Format: `https://host/feed/user/slug.atom?token=abc123`

### Token Bootstrap

- If a token is provided in the TOML config (`feed_token = "..."`) or via environment variable, it is used directly.
- If no token is configured, a random token is generated on first run, stored in DB, and logged once at WARN level: `"Feed token generated: <token> — save this, it won't be shown again."`
- Token comparison uses constant-time comparison (`subtle::ConstantTimeEq`) to prevent timing attacks.

Unauthenticated requests return `401 Unauthorized`.

## Schedule

Schedules are **wall-clock anchored** — no interval-based drift. Each generation covers content since the previous scheduled time.

### Format Options

```toml
schedule = "at:08:00"              # once daily at 08:00 local time
schedule = "at:08:00,20:00"        # twice daily at 08:00 and 20:00
schedule = "at:08:00,12:00,16:00,22:00"  # four times daily
schedule = "weekly:monday,08:00"   # weekly on Monday at 08:00
schedule = "cron:0 8 * * *"        # raw cron expression (UTC only)
```

Each digest covers content since the last successful generation (`last_generated`) to the current time.

### Timezone

- Each user has a `timezone` preference (e.g., `Europe/Kyiv`)
- `at:` and `weekly:` schedule times are interpreted in the user's timezone
- `cron:` expressions are evaluated in UTC
- All internal timestamps are stored in UTC
- The AI is informed of the user's timezone for temporal context

## Prompt / Editorial Directive

A free-form text field included in the system prompt to opencode during generation. Examples:

```
You are an editor creating a daily tech digest for a software engineer.
Focus on: AI/ML developments, systems programming, NixOS, self-hosting.
Ignore: startup funding news, cryptocurrency prices.
For any factual claims in news articles, add an [Editor's Note] with your
assessment of the claim's credibility and any counter-evidence you can find.
Write in English. Reference original sources with inline links.
```

```
You are summarizing a Ukrainian Telegram chat about the housing market in Lviv.
The messages are in Ukrainian. Write the digest in English.
Group discussions by topic/property. Note any price trends mentioned.
Link to key messages by Telegram message URL.
```

## Config

```toml
[[output_channel]]
name = "Morning Tech Digest"
slug = "tech-morning"
schedule = "at:08:00"
model = "opencode/kimi-k2.5-free"
sources = ["Hacker News", "Lobsters", "Ukrainian Tech News"]
prompt = """
Write a morning tech digest for a senior software engineer.
Focus on: systems programming, AI/ML, NixOS, Rust, self-hosting.
Skip: startup funding, crypto prices, social media drama.
Add Editor's Notes for any claims that seem dubious.
Write in English.
"""
```

Schedule is optional for output channels used only via CLI `generate`. See [CLI spec](cli.md).

## Decisions

- **Feed output format:** Atom 1.0 only (RFC 4287).
  Options: Atom 1.0 / RSS 2.0 / JSON Feed 1.1 / all three.
  Rationale: Atom is more strictly specified than RSS 2.0 (proper XML namespace, required fields, ISO 8601 dates) and supported by every major feed reader. Additional formats deferred.

- **Feed authentication:** required, two methods.
  Options: no auth / HTTP Basic Auth only / query param only / both.
  Rationale: both methods cover all RSS reader capabilities. Basic Auth is the standard; query param handles readers that don't support auth headers (e.g., Feedly).

- **Token bootstrap:** config value or auto-generated on first run.
  Options: always require config / auto-generate / both.
  Rationale: auto-generate enables zero-config startup. Config option enables declarative setups (NixOS/agenix).

- **Token comparison:** constant-time via `subtle::ConstantTimeEq`.
  Options: constant-time / regular string comparison.
  Rationale: prevents timing attacks on feed token.

- **Atom entry ID:** `urn:uuid:<article_id>`.
  Options: `urn:uuid:` / RSS-style `<guid>` / URL-based.
  Rationale: Atom `<id>` must be an IRI per RFC 4287 §4.2.6. URN-UUID is the standard pattern.

- **Atom author:** per-entry `pail-opencode-<model>`.
  Options: per-feed static author / per-entry with model name / no author.
  Rationale: different articles may use different models. Per-entry attribution is accurate.

- **Atom subtitle:** `<subtitle>` element (not `<description>`).
  Options: `<subtitle>` / `<description>`.
  Rationale: `<description>` is RSS 2.0 terminology. Atom uses `<subtitle>` per RFC 4287 §4.2.12.

- **Schedule type:** wall-clock anchored only, no interval-based.
  Options: wall-clock (`at:08:00`) / interval (`every:6h`) / both.
  Rationale: intervals drift over time (restarts, failures). Wall-clock times are predictable: "my digest arrives at 8am."

- **Timezone handling:** `at:` and `weekly:` in user timezone, `cron:` in UTC.
  Options: everything in UTC / everything in user TZ / mixed.
  Rationale: wall-clock schedules should match the user's day. Cron is traditionally UTC; converting would surprise cron users.

- **Missed ticks:** skipped, wait for next.
  Options: catch-up (generate all missed) / skip / configurable.
  Rationale: catch-up generates stale articles nobody wants. Skipping loses no data since the next tick covers from `last_generated`.

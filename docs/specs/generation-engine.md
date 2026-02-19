# Generation Engine

LLM integration via opencode for digest article generation.

## Why opencode

[opencode](https://github.com/anomalyco/opencode) is a CLI tool / agentic assistant that supports:
- Multiple LLM providers (OpenAI, Anthropic, Google, local via Ollama, free models)
- MCP (Model Context Protocol) servers for tool use
- Custom skills
- Agentic behavior (can dispatch subagents, read files, use tools)
- All authentication configured once in opencode's config

By shelling out to opencode, pail gets all of this for free without implementing LLM client code in Rust.

opencode is a **hard runtime dependency** — it is included in the Docker image alongside pail. There is no fallback if it's missing.

## Generation Flow

When an output channel's schedule ticks (or CLI `generate` is invoked):

### 1. Collect

Query content store for all items from the output channel's sources within the time window (since `last_generated` to now).

**First generation (new output channel, `last_generated` is NULL):**
- **Daemon mode:** Scheduler waits for the next scheduled tick — does not fire immediately on startup.
- **CLI mode (`pail generate`):** Runs immediately with a 7-day default lookback window.
- **Override:** `--since <duration>` (CLI) sets an explicit lookback window.
- **Telegram sources:** In CLI mode, fetch recent message history via `getHistory`. In daemon mode, content is collected via the live event stream.

### 2. Prepare Workspace

Create a temporary directory:
```
/tmp/pail-gen-<uuid>/
  manifest.json          # metadata: output channel config, time window, source list
  opencode.json          # project config from [opencode.project_config]
  prompt.md              # system prompt template with editorial directive inlined
  output.md              # empty file — opencode writes the article here
  sources/
    <source-slug>.md     # one file per source: YAML frontmatter + content items
```

Each source file has YAML frontmatter (name, type, item_count, description) followed by content items separated by `---`.

**manifest.json schema:**
```json
{
  "channel": { "name": "Morning Tech Digest", "slug": "tech-morning", "language": "en" },
  "window": { "from": "2026-02-10T20:00:00Z", "to": "2026-02-11T08:00:00Z" },
  "timezone": "Europe/Kyiv",
  "sources": [
    { "slug": "hacker-news", "name": "Hacker News", "type": "rss", "item_count": 42 },
    { "slug": "lobsters", "name": "Lobsters", "type": "rss", "item_count": 18 }
  ]
}
```

### 3. Invoke opencode

Run as subprocess:
```bash
cd /tmp/pail-gen-<uuid>/ && opencode run \
  --model <provider/model> \
  -- \
  "<full rendered prompt text>"
```

The workspace includes an `opencode.json` written from `[opencode.project_config]` (defaults include `share: "auto"` and `agent.build.variant: "max"`), so every session is automatically shared and reviewable via a shareable link. stdout/stderr is captured as the generation log. The article is written by the AI agent to `output.md`.

### 4. Parse Output

If opencode exits with a non-zero code, pail logs a warning but still attempts to parse `output.md` — some models write valid output despite reporting an error exit.

Read `output.md`, validate it's non-empty and well-formed:
- Parse YAML frontmatter for metadata (title, topics; falls back to first `# ` heading, then "Untitled Digest")
- Extract markdown body after the frontmatter
- Convert markdown body to HTML via pulldown-cmark
- If the generation log contains an opencode share URL (`https://opncd.ai/share/...`), append it as a `[opencode session](url)` link at the end of the article body

### 5. Publish

Insert as a new `generated_article` in the DB, update the output channel's `last_generated` timestamp. If `mark_tg_read` is enabled for the channel, mark Telegram chats as read (see [Telegram spec](telegram.md)).

**Override exception:** When `--since` or `--from`/`--to` is used, `last_generated` is NOT updated — these are ad-hoc runs that shouldn't affect the scheduler's window tracking.

### 6. Cleanup Workspace

Delete `/tmp/pail-gen-<uuid>/`.

## Article Output Format

opencode writes the article to `output.md` using YAML frontmatter:

```markdown
---
title: "AI Models, NixOS Updates, and Self-Hosting Wins"
topics:
  - "AI/ML"
  - "NixOS"
  - "Self-hosting"
---

# AI Models, NixOS Updates, and Self-Hosting Wins

## The Opus 4.6 Launch Shakes Up the AI Landscape
...article body in markdown...

## Skipped
- [Some Article](https://example.com) — off-topic
```

The frontmatter is structured data that pail parses directly. The body after `---` is the article content, converted to HTML for the Atom feed.

## System Prompt Template

Defined in `[opencode].system_prompt` in `config.toml` (required, must be non-empty). Must include `{editorial_directive}` as a placeholder, which pail replaces with the output channel's `prompt` field at render time.

**Workspace context** (the `## Workspace` section describing `manifest.json`, `sources/`, and `output.md`) is generated by code (`generate::workspace_context()`) and prepended to the rendered prompt automatically. This section is NOT part of the config template — it's defined once in code and shared between generate mode (prepended to prompt) and interactive mode (written as `AGENTS.md`).

The full default prompt is shipped in `config.example.toml`. It covers:

- Editorial directive insertion
- Source reading instructions (manifest, source files)
- Pre-write research step (websearch/webfetch for claims that need editor's notes)
- Condensation and fidelity rules
- RSS source handling (fetch full articles from links)
- Telegram source handling (threading, forwarded messages, attribution with WRONG/RIGHT examples, link formats)
- Output format (YAML frontmatter + markdown body)
- Article body format (sections by topic, hyperlinks to originals, language consistency, Skipped section — no separate Sources section)
- Editor's Notes (fact-checking blockquotes + inline annotations, no unsourced data, never trust training data over sources)
- References and citations preservation
- Post-write URL audit (webfetch every URL not from source files, fix or remove dead links)
- Link verification rules (never include unverified URLs)
- Writing style (Reuters correspondent, no AI-smell)

See `config.example.toml` for the full prompt text.

## opencode Configuration

pail allows configuring:
- Path to opencode binary (default: `opencode` in PATH)
- Default model in `provider/model` format (can be overridden per output channel)
- Timeout for generation (default: 10 minutes)
- Maximum retries on failure (default: 1)
- Project-level opencode config (`[opencode.project_config]`) — written as `opencode.json` in the workspace

**Implicit behavior:** pail always sets `OPENCODE_ENABLE_EXA=1` in the opencode subprocess environment, enabling websearch tools. LLM API keys and other environment variables are inherited naturally from the parent process.

**Default model:** `opencode/kimi-k2.5-free` — a free model available without authentication. Model format is `provider/model` without date suffix.

**Authentication:** opencode manages its own auth — pail does not handle LLM API keys directly. Supports `opencode auth login`, `/connect` in TUI mode for OAuth, and environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.).

## Failure Handling

When generation fails (opencode timeout, API error, malformed output):

1. **First failure:** Log the full error at ERROR level. Retry once after **30 seconds**.
2. **Retry failure:** Log as CRITICAL. Nothing is published — feed simply has no new article for this tick. Next scheduled tick covers content since `last_generated` (unchanged), so no data is lost.

Feed output and error logging are strictly separated — the feed only ever contains real digest articles, never error/status messages.

Generation logs from successful generations are stored in `generated_article.generation_log`. Failed generation logs are only emitted to stdout/stderr at ERROR level.

## Empty Digest Handling

When a scheduled generation finds no content items in the time window:

- **Skip generation** — nothing is published
- Log at WARN level with structured fields: channel name, time window, sources checked
- `last_generated` is still updated so the next generation doesn't re-check an empty window
- **Override exception:** When `--since` or `--from`/`--to` is used, `last_generated` is NOT updated (same as for successful generations — ad-hoc runs don't affect the scheduler)

## Generation Concurrency

```toml
[pail]
max_concurrent_generations = 1    # default: 1 (serialized)
```

When set to 1, generations are queued and processed one at a time. Higher values allow parallel opencode subprocesses. The scheduler tracks in-flight generations per channel using a RAII drop guard (`InFlightGuard`) to prevent double-firing and ensure cleanup on panic.

## Config

```toml
[opencode]
binary = "opencode"
default_model = "opencode/kimi-k2.5-free"
timeout = "10m"
max_retries = 1
system_prompt = """..."""   # required, must contain {editorial_directive}

[opencode.project_config]
share = "auto"

[opencode.project_config.agent.build]
variant = "max"
```

## Decisions

- **LLM integration method:** shell out to opencode as a subprocess.
  Options: shell out to opencode / direct LLM API calls in Rust / Python subprocess / MCP client.
  Rationale: gets all model support, MCP tools, agentic behavior, authentication for free. No LLM client code to maintain in Rust.

- **opencode as hard dependency:** required, no fallback or degraded mode.
  Options: hard dependency / optional with fallback / pluggable backends.
  Rationale: opencode is the core of the generation pipeline. A fallback would be a different (worse) product. Included in Docker image.

- **opencode invocation:** `cd <workspace> && opencode run --model <model> -- "<prompt>"`.
  Options: CWD set to workspace + prompt as arg / `-f` file attachment / stdin pipe.
  Rationale: CWD lets opencode discover and read workspace files directly. Project config is written from `[opencode.project_config]` as `opencode.json` in the workspace (works for both `run` and TUI modes). Prompt as positional arg is simplest.

- **Project config over CLI flags:** `[opencode.project_config]` written as `opencode.json` instead of `extra_args` CLI flags.
  Options: `extra_args` CLI flags / `opencode.json` project config / both.
  Rationale: CLI flags like `--variant` only work with `opencode run`, not TUI mode. `opencode.json` works for both modes and maps directly to opencode's config schema — any setting opencode supports can be set without pail code changes.

- **Default model:** `opencode/kimi-k2.5-free` (free, no auth needed).
  Options: `opencode/big-pickle` / `opencode/kimi-k2.5-free` / `anthropic/claude-sonnet-4-5` / no default (require user to set).
  Rationale: free model means zero-config works out of the box. Switched from big-pickle to kimi-k2.5-free for better quality.

- **Model format:** `provider/model` without date suffix.
  Options: with date suffix / without date suffix / model name only.
  Rationale: matches opencode's convention. No date suffix means you always get the latest version.

- **Article output format:** YAML frontmatter + markdown body in `output.md`.
  Options: YAML frontmatter + markdown / JSON / plain markdown with convention-based parsing.
  Rationale: markdown body is natural for the AI to write and iterate on. YAML frontmatter gives pail structured metadata (title, topics) without complex parsing.

- **YAML frontmatter parsing library:** `gray_matter` or `serde_yaml_ng`.
  Options: `gray_matter` / `serde_yaml_ng` / `serde_yaml` / `serde_yml`.
  Rationale: `serde_yaml` is deprecated (archived Mar 2024). `serde_yml` is unsound ([RUSTSEC-2025-0068](https://rustsec.org/advisories/RUSTSEC-2025-0068.html)).

- **Failure handling:** retry once after 30s, then log CRITICAL and skip.
  Options: no retry / retry once / retry with exponential backoff / retry indefinitely.
  Rationale: single retry catches transient API errors. More retries waste time and API quota. Feed only contains real articles — never error messages.

- **Empty digest:** skip generation, log WARN, still update `last_generated`.
  Options: skip + update timestamp / skip + don't update / generate "no content" article.
  Rationale: updating `last_generated` prevents re-checking an empty window. "No content" articles are noise.

- **Generation concurrency:** configurable semaphore, default 1 (serialized).
  Options: always serialized / always parallel / configurable.
  Rationale: serialized is safest default (predictable resources, no parallel API costs). Power users with many channels can increase.

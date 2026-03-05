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

Resolve the generation strategy (channel override → `[pail].default_strategy` → `"simple"`), then create a temporary directory:

```
/tmp/pail-gen-<uuid>/
  manifest.json          # metadata: output channel config, time window, source list
  opencode.json          # merged opencode config (global base + strategy overlay)
  prompt.md              # strategy prompt with editorial directive inlined
  output.md              # empty file — opencode writes the article here
  sources/
    <source-slug>.md     # one file per source: YAML frontmatter + content items
  .opencode/
    tools/
      fetch-article.ts   # custom Readability-based article extractor (if strategy uses it)
    package.json         # npm dependencies for strategy tools
```

Each source file has YAML frontmatter (name, type, item_count, description) followed by content items separated by `---`.

**Strategy-driven workspace:** The tools written to `.opencode/tools/` depend on the strategy's `tools` frontmatter list. Built-in tools (e.g., `fetch-article`) are embedded in the binary via `include_str!` from `src/opencode_tools/`. User strategy tools are copied from the strategy directory. opencode auto-discovers tools from `.opencode/tools/*.ts` and auto-installs dependencies from `.opencode/package.json` via `bun install`.

The `opencode.json` is produced by deep-merging a global base config (`src/strategies/opencode.json`) with the strategy's optional overlay. See [Generation Strategies spec](generation-strategies.md) for merge semantics.

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

The workspace includes an `opencode.json` produced by merging the global base config with the strategy's overlay (defaults include `share: "auto"` and `agent.build.variant: "high"`; the agentic strategy overrides to `"max"`). Every session is automatically shared and reviewable via a shareable link. stdout/stderr is captured as the generation log. The article is written by the AI agent to `output.md`.

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

## Context Management

Models can exhaust their context window during generation when fetching raw web pages. Two mechanisms address this:

### Custom Tool: `fetch-article`

opencode's built-in `WebFetch` returns raw HTML→markdown (via turndown), including navigation, sidebars, cookie banners, footers — ~80KB per page where the actual article body is only ~6-18KB. The custom `fetch-article` tool uses Mozilla Readability (the same algorithm behind Firefox Reader View) to extract just the article body, then converts to markdown. Returns ~3K tokens instead of ~25K per article.

The tool source lives at `src/opencode_tools/fetch-article.ts` and is embedded in the binary via `include_str!`. Dependencies (`@mozilla/readability`, `jsdom`, `turndown`) are in `src/opencode_tools/package.json`.

### Researcher Subagent

Defined in the agentic strategy's `opencode.json` overlay (`agent.researcher`). The researcher runs in an isolated session — its fetch-article/WebFetch outputs never enter the parent agent's context. Only its final text response (a structured brief) comes back.

The main agent dispatches research tasks in batches of 3-5 articles via opencode's Task tool with `subagent_type: "researcher"`. Each batch prompt tells the researcher to fetch articles, summarize them, extract key quotes, and fact-check notable claims. The researcher has access to `read`, `glob`, `webfetch`, `websearch`, and `fetch_article` tools; all other tools are denied.

This architecture means the parent agent works from compact briefs (~1-2K tokens per article) rather than raw page content (~25K tokens per article), keeping total context well within limits even for large source sets.

### Verifier Subagent

Defined in the agentic strategy's `opencode.json` overlay (`agent.verifier`). The verifier runs after the main agent writes the article to `output.md`. Its single job: scan every sentence for named sources (books, reports, news articles, studies, datasets, etc.), find URLs for any that lack hyperlinks, and edit the article to add them. If a URL cannot be found after exhaustive searching, the verifier rewrites the passage to remove the unverifiable attribution.

The main agent dispatches the verifier via opencode's Task tool with `subagent_type: "verifier"`. The verifier has access to `read`, `edit`, `write`, `websearch`, and `webfetch` tools; all other tools are denied.

This separation of concerns works because the main agent has competing priorities (writing quality, structure, editorial voice) that cause it to skip or rush reference verification. A dedicated agent with a single mandate — "every named source gets a URL or gets rewritten" — catches references that the main agent's multi-step workflow misses.

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

## System Prompt

Each generation strategy defines its own system prompt in `prompt.md` (YAML frontmatter + prompt body). The prompt must include `{editorial_directive}` as a placeholder, which pail replaces with the output channel's `prompt` field at render time.

**Workspace context** (the `## Workspace` section describing `manifest.json`, `sources/`, tools, and `output.md`) is generated by code (`strategy::workspace_context()`) and prepended to the rendered prompt automatically. This section is NOT part of the strategy prompt — it's defined once in code and shared between generate mode (prepended to prompt) and interactive mode (written as `AGENTS.md`). The workspace context is dynamic — it lists tools based on the strategy's frontmatter rather than hardcoding.

Three built-in strategies are shipped in the binary:
- **`simple`** — direct fetch + write, no subagents, works with any model
- **`agentic`** — full researcher + verifier subagent pipeline, requires capable models
- **`brief`** — condensed bullet-point digest, works with any model

See [Generation Strategies spec](generation-strategies.md) for strategy details, prompt contents, and user-defined strategies.

## opencode Configuration

pail allows configuring:
- Path to opencode binary (default: `opencode` in PATH)
- Default model in `provider/model` format (can be overridden per output channel)
- Default generation strategy (default: `simple`; can be overridden per output channel)
- Strategy-specific timeout, retries, tools, and opencode project config

The `opencode.json` workspace config is produced by deep-merging a compiled-in global base (`src/strategies/opencode.json`) with the active strategy's overlay. See [Generation Strategies spec](generation-strategies.md).

**Implicit behavior:** pail always sets `OPENCODE_ENABLE_EXA=1` in the opencode subprocess environment, enabling websearch tools. LLM API keys and other environment variables are inherited naturally from the parent process.

**Default model:** `opencode/big-pickle` — a free model available without authentication. Model format is `provider/model` without date suffix.

**Authentication:** opencode manages its own auth — pail does not handle LLM API keys directly. Supports `opencode auth login`, `/connect` in TUI mode for OAuth, and environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.).

## Failure Handling

When generation fails (opencode timeout, API error, malformed output):

1. **First failure:** Log the full error at ERROR level. Retry after **30 seconds** (up to `max_retries` from strategy frontmatter, default: 1).
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
[pail]
default_strategy = "simple"           # or "agentic", "brief", or a user-defined strategy
# strategies_dir = "./my-strategies"  # optional path to user-defined strategies

[opencode]
binary = "opencode"
default_model = "opencode/big-pickle"

[[output_channel]]
# strategy = "agentic"               # optional per-channel override
```

Timeout, max_retries, system prompt, and opencode project config are all defined by the strategy. See [Generation Strategies spec](generation-strategies.md) and [Config spec](config.md).

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

- **Default model:** `opencode/big-pickle` (free, no auth needed).
  Options: `opencode/big-pickle` / `opencode/minimax-m2.5-free` / `anthropic/claude-sonnet-4-5` / no default (require user to set).
  Rationale: free model means zero-config works out of the box. Switched back to big-pickle after kimi-k2.5-free was removed from opencode.

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

- **LLM output sanitization:** replace/strip characters invalid in XML 1.0 before rendering.
  Options: sanitize in parse_output / sanitize in Atom serializer / patch quick-xml / switch to XML 1.1.
  Rationale: XML 1.0 (mandated by Atom RFC 4287) forbids C0 controls except \t, \n, \r — even as character references. Neither quick-xml nor atom_syndication sanitize these. XML 1.1 is dead and wouldn't help (only allows them as references, not directly). Sanitized in two places: parse_output (cleans on ingest) and build_atom_feed (safety net for articles already in DB from before the fix). Two tiers: Tier A replaces/strips invalid XML chars (U+0019→apostrophe observed in gpt-5-nano); Tier B maps C1 range (U+0080-U+009F) from Windows-1252 ghosts to correct Unicode (smart quotes, dashes, etc.). See `sanitize_xml_text()` in `generate.rs`.

- **Context exhaustion fix:** custom `fetch-article` tool + researcher subagent.
  Options: custom tool only / subagent only / both / reduce source count / switch to summarization API.
  Rationale: the custom tool reduces per-article tokens from ~25K to ~3K by stripping boilerplate via Readability. The subagent isolates raw content from the parent context entirely — only compact briefs flow back. Together they reduce total context from 400-550K tokens to ~50-80K, making all models viable including those with 131K hard limits. No changes to the generation engine or opencode itself — the fix is entirely in the workspace files that pail generates.

- **Custom tool embedding:** `include_str!` from `src/opencode_tools/`.
  Options: embed via `include_str!` / write inline in Rust / ship as separate files.
  Rationale: embedding keeps the tool source in version control as readable TypeScript while ensuring it's always available in the binary. No external file dependencies at runtime.

- **Researcher subagent permissions:** deny all except read, glob, webfetch, websearch, fetch_article.
  Options: allow all / deny all except needed / custom per-tool.
  Rationale: minimal permissions prevent the researcher from writing files or using tools that could affect the workspace. It only needs to read source files, fetch articles, and search the web.

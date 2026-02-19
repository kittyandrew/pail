# Interactive Mode

Launch an interactive opencode TUI session with collected source data available in the workspace, for ad-hoc questions and exploration instead of batch article generation.

## Usage

```bash
pail interactive <slug> --since 1d
```

Same flags as `generate` for time window (`--since`, `--from`/`--to`). No `--output` flag — there's no article to write.

## Implementation

1. Same pipeline as `generate` up to workspace preparation: fetch RSS, fetch TG history, write source files + manifest to `/tmp/pail-gen-<uuid>/`
2. Write `AGENTS.md` with workspace context (file layout description) so opencode picks it up automatically
3. Launch `opencode <workspace> --model <model>` (TUI mode, no `run` subcommand) with inherited stdio, `OPENCODE_ENABLE_EXA=1`
4. On exit, clean up the workspace (tempdir auto-drop)

No article is parsed or stored. No `last_generated` update. This is purely exploratory.

## Workspace Context

The `## Workspace` section (describing `manifest.json`, `sources/`, file formats) is defined once in code (`generate::workspace_context()`) and used by both modes:
- **Generate mode:** prepended to the rendered system prompt in `prompt.md` (includes `output.md` bullet)
- **Interactive mode:** written as `AGENTS.md` in the workspace (omits `output.md` bullet)

This replaced the previous approach where the workspace description was part of the `system_prompt` template in `config.toml`.

## Decisions

- **Approach:** launch opencode TUI (not a custom REPL).
  Options: opencode TUI / custom REPL / both.
  Rationale: simplest implementation, full opencode features. pail just builds the workspace and hands off.

- **Prompt delivery for interactive mode:** `AGENTS.md` file with workspace context only.
  Options: full editorial prompt / `AGENTS.md` context-only / no prompt at all.
  Rationale: the user is driving the session — they don't need article generation instructions. `AGENTS.md` is auto-discovered by opencode's TUI, giving the model workspace layout knowledge without a system prompt.

- **Workspace context extraction:** defined once in code (`generate::workspace_context()`), used by both modes.
  Options: keep in config template / extract to code / duplicate in both places.
  Rationale: single source of truth. The config template's `system_prompt` no longer contains the workspace section — it's prepended by code at render time. Users with existing `config.toml` may have a duplicate `## Workspace` section (harmless but untidy — they can remove it manually).

- **No output.md in interactive workspace:** omitted since there's no generation target.
  Options: include empty output.md / omit it.
  Rationale: interactive sessions are exploratory. Including an empty output.md would confuse the model into thinking it needs to write an article.

- **Workspace cleanup:** automatic via `tempfile::TempDir` drop (same as generate mode).
  Options: auto-cleanup on drop / persist for manual inspection / ask user.
  Rationale: consistent with generate mode. The user can copy files out during the TUI session if needed.

- **opencode flags:** `--model <model>` passed, workspace path as positional `[project]` arg. `opencode.json` is written from `[opencode.project_config]` in `prepare_workspace()`, shared by both modes.
  Options: `--share` CLI flag / `opencode.json` project config / no sharing.
  Rationale: `--share` is only available on `opencode run`, not TUI mode. `[opencode.project_config]` maps directly to opencode's `opencode.json` schema, giving consistent behavior across both generate and interactive modes without CLI-flag hacks.

- **No timeout or cancellation token:** the user controls the TUI session directly.
  Options: inherit generate's timeout / no timeout.
  Rationale: interactive sessions have no defined end time — the user quits when done.

## Future Work: Remote Interactive Sessions

opencode has built-in server and web UI capabilities that could enable interactive sessions on deployed (headless) pail instances.

### opencode capabilities (as of v1.2.6)

- **`opencode web`** — starts a browser-based interactive UI (same capabilities as the TUI). Configurable `--port`, `--hostname`. Auth via `OPENCODE_SERVER_PASSWORD` / `OPENCODE_SERVER_USERNAME` env vars.
- **`opencode serve`** — headless HTTP server exposing ~80 REST endpoints + SSE streaming. OpenAPI 3.1 spec at `/doc`. The TUI is just a thin client over this same API.
- **`opencode attach <url>`** — connect a remote TUI to a running `opencode serve` instance.
- **`opencode run --attach <url>`** — send non-interactive commands to a remote server.
- **`@opencode-ai/sdk`** — TypeScript SDK auto-generated from the OpenAPI spec, full programmatic access.
- **`opencode.json` `server` section** — configure port, hostname, mDNS, CORS via project config.
- **Sharing (`opncd.ai/share/`)** — read-only replay only, not interactive.

### Options to explore

1. **Web mode flag** (`pail interactive --web`): launch `opencode web` instead of the TUI. Simplest path — pail prepares the workspace, then runs `opencode web <workspace> --model <model> --port <port> --hostname 0.0.0.0`. User opens browser to `https://pail.example.com:<port>`. Needs reverse proxy for HTTPS.

2. **Persistent opencode server**: pail manages a long-lived `opencode serve` process. Batch generation uses `opencode run --attach`, interactive sessions use the web UI or `opencode attach`. Single process for both modes, avoids cold-start per generation.

3. **Embedded in pail's HTTP server**: pail reverse-proxies the opencode web UI under a subpath (e.g., `/interactive/`), reusing pail's existing axum server and feed auth token. No extra ports needed.

4. **Custom UI via SDK/API**: build a pail-specific web UI using the opencode REST API or TypeScript SDK. Most control, most effort.

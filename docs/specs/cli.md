# CLI Commands

## config validate

```bash
pail config validate
```

Parse and validate config, report errors, exit. No DB side effects — does not create or touch the database.

## config edit

```bash
pail config edit
```

Interactive TUI for managing Telegram sources. See [CLI Config Editor spec](cli-config-editor.md) for full details.

Requires a Telegram connection (`pail tg login` first). All operations involve browsing live TG dialogs/folders.

## generate

```bash
pail generate <slug>
pail generate <slug> --output ./article.md
pail generate <slug> --since 7d
pail generate <slug> --since 7d --output ./article.md
pail generate <slug> --since 7d --strategy agentic
pail generate <slug> --from 2026-02-14T20:00:00Z --to 2026-02-16T08:00:00Z
pail generate <slug> --from ... --to ... --output ./article.md
```

Flags: `--output` (write markdown to file), `--strategy` (override generation strategy, default: channel config → `[pail].default_strategy` → `"simple"`), `--since`/`--from`/`--to` (time window).

**Self-contained one-shot pipeline:**
1. Open/create the SQLite DB, sync config to DB
2. Fetch all RSS sources for the given output channel (one-shot HTTP fetch). Save HTTP cache headers so conditional GETs work on subsequent runs.
3. If TG sources are in the output channel, fetch message history via `getHistory` (requires `[telegram]` config and prior `pail tg login`)
4. Store new items in the content store
5. Collect items in the time window:
   - Default: since `last_generated`
   - First run (`last_generated` is NULL): items from the last 7 days
   - `--since <duration>`: ignore `last_generated`, collect items from the last N duration
   - `--from <RFC 3339> --to <RFC 3339>`: exact time window boundaries (mutually exclusive with `--since`)
6. Prepare workspace, invoke opencode, parse output
7. Store the generated article in DB. Update `last_generated` — unless `--since` or `--from`/`--to` was used, in which case `last_generated` is left unchanged so the production schedule isn't affected.
8. If `--output <path>` is provided, write the raw markdown article to that file
9. Exit

The daemon does not need to be running. This makes `pail generate` the primary tool for iterating on editorial prompts: edit prompt in config -> run with `--since 7d --output ./article.md` -> read output -> repeat.

The pipeline logs the resolved `from`/`to` timestamps on every run, so you can copy them for later replay with `--from`/`--to`.

## tg login

```bash
pail tg login
```

Interactive MTProto auth wizard: phone number, verification code, optional 2FA password. Stores session in the database.

## tg status

```bash
pail tg status
```

Show Telegram session status.

## benchmark run

```bash
pail benchmark run --since 7d
pail benchmark run --from 2026-02-14T00:00:00Z --to 2026-02-21T00:00:00Z
pail benchmark run --since 7d --samples 3 --channel tech-digest
pail benchmark run --since 7d --models opencode/big-pickle,opencode/glm-5-free
pail benchmark run --since 7d --strategy simple --models opencode/glm-5-free
```

Run all free opencode models (or a specified list) against the same workspace and collect article outputs for comparison. See [Agentic Benchmark spec](agentic-benchmark.md) for full details.

Flags: `--since`/`--from`/`--to` (time window), `--channel` (output channel slug, default: first), `--strategy` (override generation strategy, default: channel's configured strategy), `--samples` (per model, default: 5), `--delay` (between samples, default: 5s), `--timeout` (per generation, default: 15m), `--models` (comma-separated, default: auto-discover `opencode/*`).

Results are written to `benchmarks/results/<run-id>/`. Use the `/bench-judge` Claude Code skill to evaluate results.

## strategy list

```bash
pail strategy list
```

List all available strategies (built-in + user-defined) with name, source, timeout, tool count, and description.

## strategy show

```bash
pail strategy show <name>
```

Show a strategy's resolved config: merged opencode.json, prompt preview, tool list.

## strategy validate

```bash
pail strategy validate <path>
```

Validate a user strategy directory (parse prompt.md, check frontmatter, verify tool references).

## daemon (default)

```bash
pail --config config.toml
```

No subcommand starts the daemon. See [Daemon spec](daemon.md).

## Decisions

- **`--since` / `--from`/`--to` and `last_generated`:** override flags do NOT update `last_generated`.
  Options: always update / never update on override / configurable.
  Rationale: iteration runs (`--since 7d` to test a prompt edit) shouldn't affect the production schedule's time window.

- **CLI generate fetches TG history:** yes, via `getHistory` (same pattern as RSS one-shot).
  Options: require daemon for TG content / fetch in CLI / skip TG sources in CLI.
  Rationale: CLI `generate` should be self-contained. Fetching history makes it usable without the daemon running.

- **Default subcommand:** daemon mode (no subcommand).
  Options: require explicit `serve` / `daemon` subcommand / no subcommand = daemon.
  Rationale: `pail --config config.toml` is the shortest path to running the service. Matches common patterns (e.g., nginx, caddy).

# Daemon Mode

Long-running service with scheduled generation, RSS polling, TG listening, and feed serving.

## Overview

`pail --config config.toml` (no subcommand) starts the daemon. It runs as a single long-lived process with:
- **Scheduler** — per-output-channel, wall-clock anchored generation triggers
- **RSS poller** — periodic fetch at configurable intervals per feed
- **TG listener** — persistent MTProto connection receiving live events
- **HTTP server** — serves Atom feeds and article permalinks
- **Cleanup job** — periodic sweep to delete content older than retention window

## Scheduler

The scheduler checks output channel schedules and triggers generation when a tick is due.

- Tracks `last_generated` per output channel, persisted to DB (survives restarts)

### Missed Ticks

On restart, **missed ticks are skipped** — waits for the next upcoming tick. No catch-up generation.
Content since `last_generated` is always covered by the next tick, so no data is lost.

**New channels (`last_generated` is NULL):** The scheduler does **not** fire immediately. It records the time it first saw the channel and waits for the next scheduled tick. This ensures pollers/listeners have time to collect content before the first generation runs. When the tick arrives, the pipeline uses the 7-day default lookback for content collection.

## RSS Poller

Background task that periodically fetches all enabled RSS sources at their configured intervals. Results are written to the content store. Uses HTTP cache headers (ETag, Last-Modified) for efficient polling.

## Content Cleanup

Periodic (e.g., hourly) sweep to delete content items older than the configurable retention window (default: 7 days after ingestion).

## Graceful Shutdown

On `SIGTERM` or `SIGINT`:

1. **Stop accepting new work:** Scheduler stops ticking, RSS poller stops fetching.
2. **Cancel in-progress generations:** Kill running opencode subprocesses via `child.kill()` (SIGKILL — more reliable than SIGTERM since opencode doesn't need graceful cleanup). Capture whatever stdout/stderr has been produced so far and store as a partial generation log. `last_generated` is not updated, so the next tick after restart covers the full window.
3. **Flush pending writes:** Ensure all content items from TG events and RSS fetches are committed to the DB.
4. **Close Telegram session:** Cleanly disconnect the MTProto session so it can be resumed on next startup without re-auth.
5. **Close DB connections.**
6. **Exit.**

Shutdown should complete in seconds, not minutes. Each step is logged at INFO level.

**CLI generate mode:** Registers its own Ctrl+C handler via `CancellationToken`. On signal, `invoke_opencode` kills the child process and exits immediately.

## Decisions

- **Scheduler location:** internal to daemon, no external cron/systemd timers.
  Options: internal scheduler / systemd timer / cron job / external orchestrator.
  Rationale: self-contained — one binary manages its own schedule. State persisted to DB survives restarts.

- **Missed ticks on restart:** skipped — wait for next upcoming tick.
  Options: catch-up all missed ticks / skip / generate one catch-up covering the full gap.
  Rationale: catch-up produces stale articles. No data lost since next tick covers from `last_generated`.

- **New channels (`last_generated` is NULL):** wait for next scheduled tick, don't fire immediately.
  Options: fire immediately / wait for next tick.
  Rationale: gives the RSS poller and TG listener time to collect content before the first generation. Uses 7-day default lookback when the tick arrives.

- **Opencode process kill on shutdown:** SIGKILL via `child.kill()`.
  Options: SIGTERM (graceful) / SIGKILL (immediate).
  Rationale: SIGKILL is more reliable. opencode doesn't need graceful cleanup — it's a subprocess writing to a temp workspace. Fast shutdown matters more than clean process exit.

- **First generation lookback:** 7 days default.
  Options: 1 day / 7 days / 30 days / configurable.
  Rationale: 7 days captures a reasonable amount of content for the first digest without overwhelming the AI with stale data.

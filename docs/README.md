# pail — Documentation

**pail** (Personal AI Lurker) — a self-hosted service that monitors RSS feeds and Telegram channels, generates AI digest articles via opencode, and publishes them as Atom feeds.

## Architecture

- [Core](core.md) — overview, data model, architecture, technical decisions

## Specs (implemented)

| Spec | Description |
|------|-------------|
| [RSS Sources](specs/rss-sources.md) | Feed polling, parsing, auth, dedup |
| [Telegram](specs/telegram.md) | MTProto integration, channels, groups, folders, live events |
| [Generation Engine](specs/generation-engine.md) | opencode invocation, workspace, prompt template, output parsing |
| [Atom Feed](specs/atom-feed.md) | Feed output, authentication, schedule system |
| [Daemon](specs/daemon.md) | Scheduler, poller, cleanup, graceful shutdown |
| [CLI](specs/cli.md) | validate, generate, interactive, tg login/status |
| [Config](specs/config.md) | TOML + DB dual config, validation |
| [Docker](specs/docker.md) | Image build, compose, CI/CD |
| [Interactive Mode](specs/interactive-mode.md) | opencode TUI session with collected source data |

## Ideas (not yet implemented)

| Idea | Effort | Builds On |
|------|--------|-----------|
| [Web UI](ideas/web-ui.md) | Large | — |
| [Multi-User](ideas/multi-user.md) | Large | Web UI |
| [Discord Source](ideas/discord-source.md) | Large | Blocked on feasibility research |
| [CLI Config Editor](ideas/cli-config-editor.md) | Medium | — |
| [Image Support](ideas/image-support.md) | Medium | — |
| [Full-Text Extraction](ideas/full-text-extraction.md) | Medium | — |
| [MCP Tools](ideas/mcp-tools.md) | Medium | — |
| [Shorter Digests](ideas/shorter-digests.md) | Small | — |
| [NixOS Module](ideas/nixos-module.md) | Medium | — |
| [Predictive Scheduling](ideas/predictive-scheduling.md) | Medium | — |
| [Voice Transcription](ideas/voice-transcription.md) | Medium | — |
| [Failure Notifications](ideas/failure-notifications.md) | Small | — |
| [Cross-Source Dedup](ideas/cross-source-dedup.md) | Medium | — |
| [Config Export/Import](ideas/config-export-import.md) | Small | — |
| [Additional Feed Formats](ideas/additional-feed-formats.md) | Small | — |
| [Prometheus Metrics](ideas/prometheus-metrics.md) | Small | — |
| [OPML Endpoint](ideas/opml-endpoint.md) | Small | — |

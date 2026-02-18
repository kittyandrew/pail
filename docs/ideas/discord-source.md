# Discord Source

Add Discord servers/channels as input sources, similar to Telegram integration.

## Feasibility: Open Question

The entire feature depends on whether a userbot approach (user token, not bot token) is viable on Discord. Unlike Telegram, Discord **explicitly prohibits** user account automation in their ToS. This needs research:

- What's the actual enforcement risk? (account ban likelihood, detection methods)
- Are there existing Rust or Go libraries for Discord user tokens? (discord.js-selfbot exists for JS)
- Is there a read-only API path that's less likely to trigger detection? (passive message reading vs active actions)
- Does Discord have any legitimate "data export" or "authorized app" pathway that could work?

**Do not start implementation until this research is complete.** If userbot approach is too risky, this feature may not be feasible — bot tokens require server admin cooperation, which defeats the "lurker" use case.

## Scope (if feasible)

- Text channels, forum channels, and threads
- Source types: `discord_server` (all text channels in a server), `discord_channel` (specific channel)
- Per-channel exclusion similar to TG folders
- CLI config editor integration (browse servers/channels, auto-generate source entries) — reuse the TUI pattern from the [CLI Config Editor](cli-config-editor.md)

## Architecture (sketch)

- WebSocket gateway connection for live message events (similar to TG listener)
- REST API for history fetching (similar to TG `getHistory` for CLI mode)
- Session/token persistence in DB

## Decisions

- **Access model:** user token (selfbot), not bot token.
  Options: bot token / user token / undecided.
  Rationale: bot token requires server admin to invite the bot — defeats the passive lurker use case. User token accesses everything the user sees. However, this violates Discord ToS and carries ban risk. **Feature is blocked on feasibility research.**

- **Channel scope:** text channels + forum channels + threads.
  Options: text only / text + forum / text + forum + threads.
  Rationale: forums and threads contain substantive discussions worth digesting.

- **Priority:** depends on feasibility research effort.
  Options: build soon / backlog / depends.
  Rationale: if userbot approach is viable and a good Rust library exists, could be done relatively quickly given the TG integration is already a template. If not, indefinitely deferred.

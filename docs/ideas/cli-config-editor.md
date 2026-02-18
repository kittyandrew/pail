# CLI Config Editor

Interactive TUI for adding Telegram sources to the config without manually figuring out technical details (tg_id, source type, etc.).

## Scope

TG source management only. RSS is already simple enough (just paste a URL). The TUI pattern should be reusable for future source types that need discovery (e.g., Discord).

## Core Flow

1. `pail config add-source` (or similar subcommand)
2. Check config file is writable — fail early with a clear error if not
3. Connect to Telegram (requires prior `pail tg login`)
4. Fetch all dialogs and folder definitions
5. Present interactive selection:
   - Browse folders (including Archive) — list channels/groups within each
   - Search all dialogs by name
   - Show metadata: name, type (channel/group/supergroup), username, member count, description
   - Filter out DMs automatically (only show channels/groups)
   - Warn if a folder has a very large number of sources
6. User selects channel(s)/group(s) or entire folder (with per-channel exclusion)
7. Auto-generate `[[source]]` TOML blocks:
   - `name`: from channel title
   - `type`: inferred from chat type (`telegram_channel` / `telegram_group` / `telegram_folder`)
   - `tg_username` or `tg_id`: from resolved metadata
   - `description`: from channel/group description, annotated as the original description
8. Append to config file, show diff for confirmation

## Config File Editing

Directly edits the TOML file (insert `[[source]]` blocks). Does NOT write to DB — DB sync happens on next `pail generate` or daemon startup as usual.

## Display

- Show accurate counts: total channels/groups in a folder (excluding DMs)
- For each item: name, type, username/@handle, member count if available
- Mark items that are already in the config as "already added"

## Decisions

- **Storage target:** edit TOML file directly, not DB.
  Options: edit TOML file / write to DB / both with fallback.
  Rationale: the point of this tool is to generate config entries the user can see and edit. TOML is the source of truth for declarative setups.

- **Archive folder:** shown as a normal browsable folder.
  Options: show as normal / hide / show with warning.
  Rationale: archive is just another folder. DMs are filtered out regardless. Large folders get a count warning.

- **DM filtering:** automatically excluded from all folder/dialog listings.
  Options: show DMs / hide DMs / configurable.
  Rationale: pail never accesses private DMs (read-only contract). Showing them would be confusing.

- **Scope:** TG sources only, not RSS or output channels.
  Options: TG only / TG + RSS / full config editing.
  Rationale: the TUI exists because TG source setup requires API data (IDs, types, metadata) that users can't easily look up. RSS is just a URL. The TUI pattern should be reusable for Discord when that integration is built.

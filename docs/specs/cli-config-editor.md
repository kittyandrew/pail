# CLI Config Editor

Interactive TUI for managing Telegram sources in the config without manually figuring out technical details (tg_id, source type, etc.). Uses a channel-centric flow: output channels are the primary object, and you toggle TG dialogs on/off for each channel.

## Commands

```bash
pail config edit       # Launch interactive TUI
pail config validate   # Validate config
```

## Scope

TG source management only — configure which Telegram dialogs feed into each output channel. RSS is already simple enough (just paste a URL). The TUI pattern should be reusable for future source types that need discovery (e.g., Discord).

## Architecture

Two modules:
- **`src/config_edit.rs`** — TOML manipulation primitives using `toml_edit::DocumentMut`. No TUI, no async, fully testable. Handles parsing, querying, adding, editing, and removing `[[source]]` blocks while preserving comments and formatting. Also manages output channel `sources` arrays.
- **`src/tui.rs`** — Interactive TUI orchestration using `inquire`. Connects to Telegram, presents menus, calls `config_edit` for mutations, handles backup/validation/rollback.

Dependencies:
- **`inquire`** — interactive prompts with fuzzy search, multi-select, vim mode
- **`toml_edit`** — document-preserving TOML editing (keeps comments/formatting intact)

## Flow

```
1. Load ALL TG dialogs + folders ONCE (preserve native TG order: pinned first, then recency)
2. Show output channels from config (with source summary per channel: N TG, N folder, N RSS)
3. User selects an output channel
4. Show channel info (RSS sources displayed read-only) and view selector menu
5. User switches between two views:
   a. Channels / Groups — MultiSelect of individual TG dialogs
   b. Folders — MultiSelect of TG folders
6. Selections persist across view switches
7. Esc from view selector → diff → create new [[source]] blocks, update channel's sources array, auto-remove orphaned TG sources
8. Loop back to step 2
```

If TG is not available (no session/not enabled), the TUI exits with a warning since all operations require a Telegram connection.

### Output Channel Selection

```
? Select output channel (Esc to quit):
> Tech Digest — 3 TG, 1 folder, 2 RSS
  Security Feed — 1 TG
```

Shows each output channel with a summary of its source types. Esc/Ctrl+C exits.

### Channel Edit — View Selector

```
Channel: Tech Digest
RSS: rss_hn, rss_lobsters

? Select view to edit (Esc to save & exit):
> Channels / Groups (5 selected)
  Folders (2 selected)
```

The view selector shows current selection counts. Esc/Ctrl+C applies changes and returns to channel selection. RSS sources are displayed as read-only info above the menu (not editable here).

### Channels / Groups View

```
? Select channels/groups:
  [x] [Channel] Tech Ukraine (@tech_ukraine)
  [ ] [Group] Rust Community (@rust_lang)
  [x] [Channel] Security News (@secnews)
  ...
```

- Pre-checked items: dialogs whose matching source is already in this channel
- Dialog matching: by `tg_id` (primary) or `tg_username` (fallback), skipping folder sources
- Page size: 20, vim mode enabled
- Esc returns to view selector without changing selections

### Folders View

```
? Select folders:
  [x] Tech (15 channels)
  [ ] News (8 channels)
  ...
```

- Pre-checked items: folders whose matching source is already in this channel
- Page size: 15, vim mode enabled
- Esc returns to view selector without changing selections

### Apply Selection

When Esc exits the view selector:
1. For each selected item (dialog or folder) with an existing source, reuse that source name
2. For new items, create a `[[source]]` block with auto-generated unique name
3. Build final `sources` array: `[non_tg_sources] + [selected_tg_sources]`
4. Show diff summary (added/removed TG sources, new `[[source]]` blocks)
5. Mutate document → remove orphaned TG sources → write with validation (rollback on failure)

## Source Lifecycle

Sources are managed implicitly:
- **Created** when a dialog is first selected for a channel and no matching source exists
- **Reused** when a dialog matches an existing source (by tg_id or username)
- **Orphaned** and auto-removed when a TG source is no longer referenced by any output channel

## Post-Write Validation

After every config file write:
1. Backup original content in memory before writing
2. Write modified TOML to disk
3. Re-parse with `load_config()` + `validate_config()`
4. If validation fails, restore backup and show error
5. Show diff (added/removed lines with `+`/`-` prefixes)

## Telegram Integration

Types exposed by `telegram.rs` for the TUI:

```rust
#[derive(Clone)]
pub enum TgChatType { Channel, Group }

#[derive(Clone)]
pub struct TgDialog { name, chat_type, username, tg_id }

pub struct TgFolder { name, channels: Vec<TgDialog> }

pub async fn list_dialogs(client) -> Result<Vec<TgDialog>>;
pub async fn list_folders(client) -> Result<Vec<TgFolder>>;
```

`list_dialogs()` wraps `client.iter_dialogs()`, filtering out DMs/bots/self. Returns dialogs in native TG order (pinned first, then by recency).
`list_folders()` reuses the `getDialogFilters` + `batch_resolve_channels()` pattern from existing `resolve_folders()`.

Note: grammers maps both basic groups and supergroups to `Peer::Group` (only broadcast channels become `Peer::Channel`), so `TgChatType` has two variants, not three.

## Config Edit Functions

Key functions in `config_edit.rs` for the channel-centric flow:

```rust
pub struct TgSourceInfo { name, tg_id, tg_username, tg_folder_name }

pub fn get_output_channel_names(doc) -> Vec<String>;
pub fn get_channel_sources(doc, channel_name) -> Vec<String>;
pub fn set_channel_sources(doc, channel_name, &[String]) -> bool;
pub fn get_tg_sources_detailed(doc) -> Vec<TgSourceInfo>;
pub fn get_all_source_names_in_any_channel(doc) -> HashSet<String>;
```

## Edge Cases

| Case | Handling |
|------|----------|
| TG session missing / not enabled | Warn and exit — all operations require TG connection |
| Channel has no TG sources yet | All dialogs unchecked; user selects what to add |
| Channel has folder sources | Folder sources shown in Folders view; individually selectable |
| Channel has RSS-only sources | RSS sources preserved and shown as read-only info; TG views start empty |
| Deselect all TG sources | All TG sources removed from channel; orphans cleaned up; validation may fail if channel ends up with empty sources (rollback protects) |
| Name collision on auto-create | `make_unique_source_name()` appends ` (2)`, ` (3)`, etc. |
| Source used by multiple channels | Not orphaned when removed from one; only cleaned up when in no channel |
| Config file read-only | Fail early before entering TUI |
| No output channels in config | Show message and return |
| Ctrl+C / Esc mid-flow | Return to previous menu level |
| No `[[source]]` section yet | `toml_edit` creates it on first add |

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

- **CLI structure:** `pail config validate` + `pail config edit`, no top-level alias.
  Options: `pail tg add` / `pail config add-source` + `pail config edit-source` / `pail config validate` + `pail config edit` (with or without `pail validate` alias).
  Rationale: unified `config` subgroup. `edit` covers add/edit/remove in one interactive TUI. Old `pail validate` removed — `config validate` is the canonical command.

- **TUI library:** `inquire`.
  Options: `inquire` / `dialoguer` / no preference.
  Rationale: built-in fuzzy search on Select and MultiSelect (critical for browsing hundreds of TG dialogs). `dialoguer`'s FuzzySelect doesn't support multi-select with fuzzy.

- **TOML editing:** `toml_edit` for document-preserving mutations.
  Options: raw string append / `toml` serde round-trip / `toml_edit`.
  Rationale: raw append can't handle edit/remove. Serde round-trip destroys comments. `toml_edit` preserves everything.

- **Post-write validation:** re-parse with serde + `validate_config()`, restore backup on failure.
  Options: validate before write / validate after write with rollback / no validation.
  Rationale: validates the actual file on disk, catches edge cases that pre-validation might miss. Backup ensures no data loss.

- **Chat type mapping:** two-variant `TgChatType` (Channel, Group) instead of three.
  Options: Channel/Group/Supergroup / Channel/Group.
  Rationale: grammers' `Peer` enum maps supergroups (non-broadcast channels) to `Peer::Group`. Cannot distinguish at the API level. Both map to `telegram_group` in config anyway.

- **Channel-centric redesign:** output channels as primary object, toggle dialogs per channel.
  Options: source-centric CRUD (add/edit/remove sources) / channel-centric (select channel, toggle dialogs).
  Rationale: the real user goal is "configure which TG sources feed into this output channel." Source-centric CRUD required manual source array management. Channel-centric flow handles source creation/orphan cleanup implicitly.

- **Dialog ordering:** native TG order (pinned first, then recency) instead of alphabetical.
  Options: alphabetical sort / native TG order / configurable.
  Rationale: grammers' `iter_dialogs()` already returns TG native order, which matches what users see in their Telegram client. Alphabetical was unintuitive for large dialog lists.

- **Orphan cleanup:** auto-remove all TG sources not referenced by any output channel.
  Options: auto-remove all / auto-remove non-folder only / warn only / manual cleanup.
  Rationale: sources are created implicitly when dialogs are selected; they should be cleaned up implicitly when deselected from all channels. Folder sources are treated the same — they're just as easy to recreate (name + folder_name).

- **No provider menu:** go straight to output channel selection.
  Options: provider menu (Telegram / Exit) / direct to output channels.
  Rationale: the provider abstraction added an unnecessary step. The TUI only supports TG sources anyway. Going straight to output channels with RSS shown as read-only info is simpler and shows the user what they care about immediately.

- **Two-view switching for folders vs channels/groups.**
  Options: single combined MultiSelect / two separate views with view selector / folder import prompt.
  Rationale: folders and individual channels are conceptually different — folders contain many channels and map to `telegram_folder` sources. A combined list is confusing (folders mixed with hundreds of channels). The folder import prompt was also confusing. Two separate views with a switching menu keeps each list clean and lets users toggle freely with selections persisting across switches.

- **Esc-to-save instead of explicit "Done" option.**
  Options: "Done — apply changes" menu item / Esc to save & exit.
  Rationale: Esc is the natural "I'm done" gesture in a terminal TUI. Adding a menu item is redundant — users switch views by selecting an option, and exit by pressing Esc. The prompt text "(Esc to save & exit)" makes this discoverable.

- **No confirmation prompt before write:** changes are applied immediately with rollback protection.
  Options: ask "Apply? [Y/n]" after diff / write immediately with rollback.
  Rationale: the diff summary is shown before write, and the rollback mechanism (restore original on validation failure) prevents data loss. An extra prompt adds friction to the iterative workflow of toggling sources across multiple channels.

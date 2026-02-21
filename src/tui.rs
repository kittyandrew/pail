use std::collections::HashSet;
use std::fmt;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use inquire::{InquireError, MultiSelect, Select};

use grammers_client::Client;

use crate::config::{load_config, validate_config};
use crate::config_edit::{self, NewSource, TgSourceInfo};
use crate::telegram::{TgConnection, TgDialog, TgFolder};

// ─── Display types ───

struct OutputChannelItem {
    name: String,
    source_summary: String,
}

impl fmt::Display for OutputChannelItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} — {}", self.name, self.source_summary)
    }
}

/// Item for the folders MultiSelect.
struct FolderSelectItem {
    name: String,
    channel_count: usize,
}

impl fmt::Display for FolderSelectItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({} channels)", self.name, self.channel_count)
    }
}

/// Item for the channels/groups MultiSelect.
struct DialogSelectItem {
    dialog: TgDialog,
}

impl fmt::Display for DialogSelectItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let username = self
            .dialog
            .username
            .as_ref()
            .map(|u| format!(" (@{u})"))
            .unwrap_or_default();
        write!(f, "[{}] {}{}", self.dialog.chat_type, self.dialog.name, username)
    }
}

/// View selector menu item.
enum ViewAction {
    ChannelsGroups(usize),
    Folders(usize),
}

impl fmt::Display for ViewAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ViewAction::ChannelsGroups(n) => write!(f, "Channels / Groups ({n} selected)"),
            ViewAction::Folders(n) => write!(f, "Folders ({n} selected)"),
        }
    }
}

/// Collected selections from the two views, passed to apply_selection.
enum SelectedItem {
    Folder {
        name: String,
        existing_source_name: Option<String>,
    },
    Dialog {
        dialog: TgDialog,
        existing_source_name: Option<String>,
    },
}

// ─── Entry point ───

/// Entry point for the config editor TUI.
pub async fn run_config_editor(config_path: &Path, tg_conn: Option<&TgConnection>) -> Result<()> {
    let metadata = std::fs::metadata(config_path).context("checking config file")?;
    if metadata.permissions().readonly() {
        anyhow::bail!("Config file is read-only: {}", config_path.display());
    }

    if tg_conn.is_none() {
        println!("Note: Telegram is not available (no session or not enabled).");
        println!("      Cannot edit sources without a Telegram connection.\n");
        return Ok(());
    }

    let tg_conn = tg_conn.unwrap();

    println!("Fetching dialogs from Telegram...");
    let dialogs = crate::telegram::list_dialogs(&tg_conn.client).await?;
    if dialogs.is_empty() {
        println!("No channels or groups found.");
        return Ok(());
    }

    println!("Fetching folders from Telegram...");
    let folders = crate::telegram::list_folders(&tg_conn.client).await?;

    println!("Loaded {} dialogs, {} folders.\n", dialogs.len(), folders.len());

    // Main loop: output channel selection
    loop {
        clear_screen();

        let content = std::fs::read_to_string(config_path)?;
        let doc = config_edit::parse_document(&content)?;

        let channel_names = config_edit::get_output_channel_names(&doc);
        if channel_names.is_empty() {
            println!("No output channels found in config. Add an [[output_channel]] first.");
            return Ok(());
        }

        let tg_sources = config_edit::get_tg_sources_detailed(&doc);
        let all_source_names = config_edit::get_all_source_names(&doc);

        let items: Vec<OutputChannelItem> = channel_names
            .iter()
            .map(|name| {
                let ch_sources = config_edit::get_channel_sources(&doc, name);
                let summary = build_source_summary(&ch_sources, &tg_sources, &all_source_names);
                OutputChannelItem {
                    name: name.clone(),
                    source_summary: summary,
                }
            })
            .collect();

        let selected = match Select::new("Select output channel (Esc to quit):", items).prompt() {
            Ok(s) => s,
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        let result = run_channel_edit(config_path, &selected.name, &dialogs, &folders, &tg_conn.client).await;
        match result {
            Ok(()) => {}
            Err(e) => {
                if is_cancel(&e) {
                    continue;
                }
                eprintln!("Error: {e:#}");
            }
        }
    }
}

/// Build a short summary like "2 TG, 1 folder, 3 RSS".
fn build_source_summary(ch_sources: &[String], tg_sources: &[TgSourceInfo], all_source_names: &[String]) -> String {
    let mut tg_count = 0;
    let mut folder_count = 0;
    let mut rss_count = 0;

    for name in ch_sources {
        match tg_sources.iter().find(|ts| &ts.name == name) {
            Some(ts) if ts.tg_folder_name.is_some() => folder_count += 1,
            Some(_) => tg_count += 1,
            None => {
                // It's a source but not TG — assume RSS (or unknown)
                if all_source_names.contains(name) {
                    rss_count += 1;
                }
            }
        }
    }

    let mut parts = Vec::new();
    if tg_count > 0 {
        parts.push(format!("{tg_count} TG"));
    }
    if folder_count > 0 {
        parts.push(format!("{folder_count} folder"));
    }
    if rss_count > 0 {
        parts.push(format!("{rss_count} RSS"));
    }
    if parts.is_empty() {
        "no sources".to_string()
    } else {
        parts.join(", ")
    }
}

// ─── Channel edit ───

/// Core flow for editing TG sources on one output channel.
/// View-switching loop: toggle between Channels/Groups and Folders views.
/// Esc from the view selector saves and exits.
async fn run_channel_edit(
    config_path: &Path,
    channel_name: &str,
    dialogs: &[TgDialog],
    folders: &[TgFolder],
    client: &Client,
) -> Result<()> {
    let content = std::fs::read_to_string(config_path)?;
    let doc = config_edit::parse_document(&content)?;

    let channel_sources = config_edit::get_channel_sources(&doc, channel_name);
    let tg_sources = config_edit::get_tg_sources_detailed(&doc);

    // Partition: non-TG sources are preserved as-is
    let mut non_tg_sources: Vec<String> = Vec::new();
    for src_name in &channel_sources {
        if !tg_sources.iter().any(|ts| &ts.name == src_name) {
            non_tg_sources.push(src_name.clone());
        }
    }

    // Track selections across view switches
    let mut selected_dialog_ids: HashSet<i64> = HashSet::new();
    let mut selected_folder_names: HashSet<String> = HashSet::new();

    // Pre-populate from current channel config
    for d in dialogs {
        if let Some(ref src_name) = match_dialog_to_source(d, &tg_sources)
            && channel_sources.contains(src_name)
        {
            selected_dialog_ids.insert(d.tg_id);
        }
    }
    for f in folders {
        if let Some(ts) = tg_sources
            .iter()
            .find(|ts| ts.tg_folder_name.as_deref() == Some(&f.name))
            && channel_sources.contains(&ts.name)
        {
            selected_folder_names.insert(f.name.clone());
        }
    }

    loop {
        clear_screen();
        println!("Channel: {channel_name}");
        if !non_tg_sources.is_empty() {
            println!("RSS: {}", non_tg_sources.join(", "));
        }
        println!();

        let actions = vec![
            ViewAction::ChannelsGroups(selected_dialog_ids.len()),
            ViewAction::Folders(selected_folder_names.len()),
        ];

        let action = match Select::new("Select view to edit (Esc to save & exit):", actions).prompt() {
            Ok(a) => a,
            Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => break,
            Err(e) => return Err(e.into()),
        };

        match action {
            ViewAction::ChannelsGroups(_) => {
                let items: Vec<DialogSelectItem> =
                    dialogs.iter().map(|d| DialogSelectItem { dialog: d.clone() }).collect();

                let defaults: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter_map(|(i, item)| selected_dialog_ids.contains(&item.dialog.tg_id).then_some(i))
                    .collect();

                match MultiSelect::new("Select channels/groups:", items)
                    .with_default(&defaults)
                    .with_page_size(20)
                    .with_vim_mode(true)
                    .prompt()
                {
                    Ok(selected) => {
                        selected_dialog_ids.clear();
                        for item in &selected {
                            selected_dialog_ids.insert(item.dialog.tg_id);
                        }
                    }
                    Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            ViewAction::Folders(_) => {
                if folders.is_empty() {
                    println!("No TG folders found.");
                    continue;
                }

                let items: Vec<FolderSelectItem> = folders
                    .iter()
                    .map(|f| FolderSelectItem {
                        name: f.name.clone(),
                        channel_count: f.channels.len(),
                    })
                    .collect();

                let defaults: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter_map(|(i, item)| selected_folder_names.contains(&item.name).then_some(i))
                    .collect();

                match MultiSelect::new("Select folders:", items)
                    .with_default(&defaults)
                    .with_page_size(15)
                    .with_vim_mode(true)
                    .prompt()
                {
                    Ok(selected) => {
                        selected_folder_names.clear();
                        for item in &selected {
                            selected_folder_names.insert(item.name.clone());
                        }
                    }
                    Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }

    // Build the final selection list and apply
    let mut selected_items: Vec<SelectedItem> = Vec::new();

    for f in folders {
        if selected_folder_names.contains(&f.name) {
            let existing = tg_sources
                .iter()
                .find(|ts| ts.tg_folder_name.as_deref() == Some(&f.name))
                .map(|ts| ts.name.clone());
            selected_items.push(SelectedItem::Folder {
                name: f.name.clone(),
                existing_source_name: existing,
            });
        }
    }

    for d in dialogs {
        if selected_dialog_ids.contains(&d.tg_id) {
            selected_items.push(SelectedItem::Dialog {
                dialog: d.clone(),
                existing_source_name: match_dialog_to_source(d, &tg_sources),
            });
        }
    }

    apply_selection(
        &ApplyContext {
            config_path,
            channel_name,
            non_tg_sources: &non_tg_sources,
            old_channel_sources: &channel_sources,
            selected: &selected_items,
        },
        client,
    )
    .await
}

// ─── Dialog-to-source matching ───

/// Match a dialog to an existing TG source by tg_id (primary) or tg_username (fallback).
/// Skips folder sources.
fn match_dialog_to_source(dialog: &TgDialog, tg_sources: &[TgSourceInfo]) -> Option<String> {
    for source in tg_sources {
        if source.tg_folder_name.is_some() {
            continue;
        }

        if let Some(src_id) = source.tg_id
            && src_id == dialog.tg_id
        {
            return Some(source.name.clone());
        }

        if let (Some(src_username), Some(dialog_username)) = (&source.tg_username, &dialog.username) {
            let src_clean = src_username.trim_start_matches('@').to_lowercase();
            let dialog_clean = dialog_username.trim_start_matches('@').to_lowercase();
            if src_clean == dialog_clean {
                return Some(source.name.clone());
            }
        }
    }

    None
}

// ─── Apply selection ───

struct ApplyContext<'a> {
    config_path: &'a Path,
    channel_name: &'a str,
    non_tg_sources: &'a [String],
    old_channel_sources: &'a [String],
    selected: &'a [SelectedItem],
}

/// Diff computation and atomic write. Fetches descriptions for new sources from TG.
async fn apply_selection(ctx: &ApplyContext<'_>, client: &Client) -> Result<()> {
    let content = std::fs::read_to_string(ctx.config_path)?;
    let mut doc = config_edit::parse_document(&content)?;

    let all_existing_names = config_edit::get_all_source_names(&doc);
    let mut pending_names: HashSet<String> = HashSet::new();
    let mut sources_to_add: Vec<NewSource> = Vec::new();
    let mut new_tg_names: Vec<String> = Vec::new();

    for item in ctx.selected {
        match item {
            SelectedItem::Folder {
                name: folder_name,
                existing_source_name,
            } => {
                if let Some(existing) = existing_source_name {
                    new_tg_names.push(existing.clone());
                } else {
                    let unique = make_unique_source_name(folder_name, &all_existing_names, &pending_names);
                    pending_names.insert(unique.clone());

                    sources_to_add.push(NewSource {
                        name: unique.clone(),
                        source_type: "telegram_folder".to_string(),
                        tg_username: None,
                        tg_id: None,
                        tg_folder_name: Some(folder_name.clone()),

                        description: None,
                    });

                    new_tg_names.push(unique);
                }
            }
            SelectedItem::Dialog {
                dialog,
                existing_source_name,
            } => {
                if let Some(existing) = existing_source_name {
                    new_tg_names.push(existing.clone());
                } else {
                    let unique = make_unique_source_name(&dialog.name, &all_existing_names, &pending_names);
                    pending_names.insert(unique.clone());

                    let description = crate::telegram::fetch_chat_about(client, dialog).await;

                    sources_to_add.push(NewSource {
                        name: unique.clone(),
                        source_type: dialog.chat_type.config_type().to_string(),
                        tg_username: dialog.username.clone(),
                        tg_id: Some(dialog.tg_id),
                        tg_folder_name: None,

                        description,
                    });

                    new_tg_names.push(unique);
                }
            }
        }
    }

    // Build final sources array: [non_tg] + [tg selections]
    let mut final_sources: Vec<String> = Vec::new();
    final_sources.extend_from_slice(ctx.non_tg_sources);
    final_sources.extend(new_tg_names.iter().cloned());

    // ── Compute diff ──
    let old_set: HashSet<&String> = ctx.old_channel_sources.iter().collect();
    let new_set: HashSet<&String> = final_sources.iter().collect();

    let added: Vec<&String> = final_sources.iter().filter(|s| !old_set.contains(s)).collect();
    let removed: Vec<&String> = ctx
        .old_channel_sources
        .iter()
        .filter(|s| !new_set.contains(s))
        .collect();

    if added.is_empty() && removed.is_empty() && sources_to_add.is_empty() {
        println!("No changes.");
        return Ok(());
    }

    let channel_name = ctx.channel_name;
    println!("\nChanges to channel '{channel_name}':");
    for name in &added {
        println!("  + {name}");
    }
    for name in &removed {
        println!("  - {name}");
    }
    if !sources_to_add.is_empty() {
        println!("\nNew [[source]] blocks to create:");
        for src in &sources_to_add {
            if let Some(ref folder) = src.tg_folder_name {
                println!("  {} (type=telegram_folder, folder={})", src.name, folder);
            } else {
                let username = src.tg_username.as_deref().unwrap_or("?");
                println!(
                    "  {} (type={}, @{}, tg_id={})",
                    src.name,
                    src.source_type,
                    username,
                    src.tg_id.unwrap_or(0)
                );
            }
        }
    }

    // Mutate doc: add new sources
    for src in &sources_to_add {
        config_edit::add_source(&mut doc, src);
    }

    // Update channel's sources array
    config_edit::set_channel_sources(&mut doc, ctx.channel_name, &final_sources);

    // Remove orphaned TG sources (not referenced by any channel)
    let referenced = config_edit::get_all_source_names_in_any_channel(&doc);
    let all_tg = config_edit::get_tg_sources_detailed(&doc);
    let mut orphans_removed = Vec::new();
    for ts in &all_tg {
        if !referenced.contains(&ts.name) {
            config_edit::remove_source(&mut doc, &ts.name);
            orphans_removed.push(ts.name.clone());
        }
    }

    if !orphans_removed.is_empty() {
        println!("Removed orphaned sources: {}", orphans_removed.join(", "));
    }

    let new_content = config_edit::render(&doc);
    write_with_validation(ctx.config_path, &content, &new_content)?;

    println!("Config updated.");
    Ok(())
}

// ─── Helpers ───

/// Generate a unique source name by appending ` (2)`, ` (3)`, etc. on collision.
fn make_unique_source_name(base: &str, existing: &[String], pending: &HashSet<String>) -> String {
    if !existing.contains(&base.to_string()) && !pending.contains(base) {
        return base.to_string();
    }

    let mut n = 2;
    loop {
        let candidate = format!("{base} ({n})");
        if !existing.contains(&candidate) && !pending.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Clear the terminal screen and move cursor to top-left.
fn clear_screen() {
    print!("\x1B[2J\x1B[1;1H");
    std::io::stdout().flush().ok();
}

/// Check if an error is a user cancellation.
fn is_cancel(e: &anyhow::Error) -> bool {
    e.downcast_ref::<InquireError>()
        .is_some_and(|ie| matches!(ie, InquireError::OperationCanceled | InquireError::OperationInterrupted))
}

/// Write new content to config, validate, rollback on failure, and show diff.
fn write_with_validation(config_path: &Path, original: &str, new_content: &str) -> Result<()> {
    std::fs::write(config_path, new_content).context("writing config file")?;

    match load_config(config_path).and_then(|cfg| validate_config(&cfg).map(|()| cfg)) {
        Ok(_) => {
            show_diff(original, new_content);
            Ok(())
        }
        Err(e) => {
            std::fs::write(config_path, original).context("restoring config backup")?;
            Err(e).context("config validation failed after write — restored original")
        }
    }
}

/// Show a simple diff between old and new content.
fn show_diff(old: &str, new: &str) {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut has_changes = false;

    for line in &new_lines {
        if !old_lines.contains(line) {
            if !has_changes {
                println!("\nChanges:");
                has_changes = true;
            }
            println!("  + {line}");
        }
    }
    for line in &old_lines {
        if !new_lines.contains(line) {
            if !has_changes {
                println!("\nChanges:");
                has_changes = true;
            }
            println!("  - {line}");
        }
    }
}

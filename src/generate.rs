use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gray_matter::Matter;
use gray_matter::engine::YAML;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use tokio::io::AsyncReadExt;

use crate::config::{Config, OutputChannelConfig};
use crate::error::GenerationError;
use crate::models::{ContentItem, GeneratedArticle, OutputChannel, Source};

/// Key for grouping content items in the workspace.
/// Non-folder sources group by source_id; folder sources split into per-channel groups.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum SourceKey {
    Source(String),
    FolderChannel { source_id: String, chat_id: i64 },
}

/// Info needed to write a source file's frontmatter and filename.
struct SourceFileInfo {
    name: String,
    source_type: String,
    description: String,
    slug: String,
}

/// Classify a content item into its SourceKey, splitting folder items by chat_id.
fn item_source_key(item: &ContentItem, source_map: &HashMap<String, &Source>) -> SourceKey {
    let source = source_map.get(&item.source_id);
    let is_folder = source.is_some_and(|s| s.source_type == "telegram_folder");
    if is_folder {
        let meta: serde_json::Value = serde_json::from_str(&item.metadata).unwrap_or_default();
        let chat_id = meta.get("chat_id").and_then(|v| v.as_i64()).unwrap_or(0);
        SourceKey::FolderChannel {
            source_id: item.source_id.clone(),
            chat_id,
        }
    } else {
        SourceKey::Source(item.source_id.clone())
    }
}

/// Build SourceFileInfo for each SourceKey that has items.
fn build_source_file_infos(
    keys: &[SourceKey],
    source_map: &HashMap<String, &Source>,
    folder_channels: &HashMap<String, HashMap<i64, (String, Option<String>)>>,
) -> HashMap<SourceKey, SourceFileInfo> {
    // Track slug usage for dedup
    let mut slug_counts: HashMap<String, usize> = HashMap::new();
    let mut result = HashMap::new();

    // Sort keys for deterministic slug assignment
    let mut sorted_keys = keys.to_vec();
    sorted_keys.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));

    for key in &sorted_keys {
        let (name, source_type, description) = match key {
            SourceKey::Source(id) => {
                let source = source_map.get(id);
                (
                    source.map(|s| s.name.clone()).unwrap_or_else(|| "Unknown".to_string()),
                    source
                        .map(|s| s.source_type.clone())
                        .unwrap_or_else(|| "unknown".to_string()),
                    source.and_then(|s| s.description.clone()).unwrap_or_default(),
                )
            }
            SourceKey::FolderChannel { source_id, chat_id } => {
                let channel_info = folder_channels.get(source_id).and_then(|m| m.get(chat_id));
                let ch_name = channel_info
                    .map(|(n, _)| n.clone())
                    .unwrap_or_else(|| format!("Channel {chat_id}"));
                (ch_name, "telegram_channel".to_string(), String::new())
            }
        };

        let base_slug = slug_from_name(&name);
        let count = slug_counts.entry(base_slug.clone()).or_default();
        let slug = if *count == 0 {
            base_slug.clone()
        } else {
            format!("{base_slug}-{}", *count + 1)
        };
        *count += 1;

        result.insert(
            key.clone(),
            SourceFileInfo {
                name,
                source_type,
                description,
                slug,
            },
        );
    }

    result
}

/// Generate a digest article for a channel.
/// Returns (article, raw_output) where raw_output is the exact content of output.md.
#[allow(clippy::too_many_arguments)]
pub async fn generate_article(
    config: &Config,
    channel_config: &OutputChannelConfig,
    channel: &OutputChannel,
    items: &[ContentItem],
    source_map: &HashMap<String, &Source>,
    folder_channels: &HashMap<String, HashMap<i64, (String, Option<String>)>>,
    covers_from: DateTime<Utc>,
    covers_to: DateTime<Utc>,
    cancel: CancellationToken,
) -> Result<(GeneratedArticle, String)> {
    // Create workspace
    let workspace = tempfile::Builder::new()
        .prefix("pail-gen-")
        .tempdir()
        .map_err(GenerationError::Workspace)?;

    let ws_path = workspace.path();
    info!(workspace = %ws_path.display(), "preparing generation workspace");

    // Build source keys and file info for workspace generation
    let keys: Vec<SourceKey> = items
        .iter()
        .map(|item| item_source_key(item, source_map))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let file_infos = build_source_file_infos(&keys, source_map, folder_channels);

    // Write workspace files
    write_manifest(
        ws_path,
        channel_config,
        items,
        source_map,
        &file_infos,
        covers_from,
        covers_to,
        &config.pail.timezone,
    )
    .await
    .context("writing manifest")?;

    let prompt = write_prompt(ws_path, config, channel_config)
        .await
        .context("writing prompt")?;

    write_source_content(ws_path, items, source_map, &file_infos)
        .await
        .context("writing source content")?;

    // Create empty output.md
    tokio::fs::write(ws_path.join("output.md"), "")
        .await
        .map_err(GenerationError::Workspace)?;

    // Determine model
    let model = channel_config
        .model
        .as_deref()
        .or(config.opencode.default_model.as_deref())
        .unwrap_or("opencode/big-pickle");

    // Invoke opencode
    let (generation_log, exit_code) = invoke_opencode(
        &config.opencode.binary,
        ws_path,
        model,
        &prompt,
        &config.opencode.timeout,
        &config.opencode.extra_args,
        cancel,
    )
    .await
    .context("invoking opencode")?;

    if exit_code != Some(0) {
        warn!(
            exit_code = ?exit_code,
            "opencode exited with non-zero code, checking output anyway"
        );
    }

    // Parse output
    let output_path = ws_path.join("output.md");
    let output_content = tokio::fs::read_to_string(&output_path)
        .await
        .map_err(GenerationError::Workspace)?;

    if output_content.trim().is_empty() {
        error!(
            generation_log = %generation_log,
            "output.md is empty — opencode log above may indicate the cause"
        );
        return Err(GenerationError::OutputParse("output.md is empty".to_string()).into());
    }

    let (title, topics, mut body_markdown) = parse_output(&output_content).context("parsing output")?;

    // Append opencode session share link if present in generation log
    let share_suffix = extract_share_url(&generation_log).map(|url| format!("\n\n---\n\n[opencode session]({url})\n"));
    if let Some(ref suffix) = share_suffix {
        body_markdown.push_str(suffix);
    }

    // Convert markdown to HTML
    let body_html = markdown_to_html(&body_markdown);

    // Also append to raw output so --output file includes the link
    let mut output_content = output_content;
    if let Some(ref suffix) = share_suffix {
        output_content.push_str(suffix);
    }

    let content_item_ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();

    let article = GeneratedArticle {
        id: Uuid::new_v4().to_string(),
        output_channel_id: channel.id.clone(),
        generated_at: Utc::now(),
        covers_from,
        covers_to,
        title,
        topics,
        body_html,
        body_markdown,
        content_item_ids,
        generation_log,
        model_used: model.to_string(),
        token_count: None,
    };

    // Workspace is cleaned up when `workspace` is dropped
    Ok((article, output_content))
}

#[allow(clippy::too_many_arguments)]
async fn write_manifest(
    ws_path: &Path,
    channel_config: &OutputChannelConfig,
    items: &[ContentItem],
    source_map: &HashMap<String, &Source>,
    file_infos: &HashMap<SourceKey, SourceFileInfo>,
    covers_from: DateTime<Utc>,
    covers_to: DateTime<Utc>,
    timezone: &str,
) -> Result<()> {
    // Count items per source key
    let mut key_item_counts: HashMap<SourceKey, usize> = HashMap::new();
    for item in items {
        let key = item_source_key(item, source_map);
        *key_item_counts.entry(key).or_default() += 1;
    }

    // Sort by name for deterministic manifest output
    let mut sorted_infos: Vec<_> = file_infos.iter().collect();
    sorted_infos.sort_by_key(|(_, info)| &info.name);

    let sources_json: Vec<serde_json::Value> = sorted_infos
        .into_iter()
        .map(|(key, info)| {
            serde_json::json!({
                "slug": info.slug,
                "name": info.name,
                "type": info.source_type,
                "item_count": key_item_counts.get(key).unwrap_or(&0),
            })
        })
        .collect();

    let manifest = serde_json::json!({
        "channel": {
            "name": channel_config.name,
            "slug": channel_config.slug,
            "language": channel_config.language.as_deref().unwrap_or("en"),
        },
        "window": {
            "from": covers_from.to_rfc3339(),
            "to": covers_to.to_rfc3339(),
        },
        "timezone": timezone,
        "sources": sources_json,
    });

    let manifest_str = serde_json::to_string_pretty(&manifest).context("serializing manifest")?;

    tokio::fs::write(ws_path.join("manifest.json"), manifest_str)
        .await
        .map_err(GenerationError::Workspace)?;

    debug!("wrote manifest.json");
    Ok(())
}

async fn write_prompt(ws_path: &Path, config: &Config, channel_config: &OutputChannelConfig) -> Result<String> {
    let prompt = config
        .opencode
        .system_prompt
        .replace("{editorial_directive}", channel_config.prompt.trim());

    // Write to workspace for debugging/inspection only
    tokio::fs::write(ws_path.join("prompt.md"), &prompt)
        .await
        .map_err(GenerationError::Workspace)?;

    debug!("wrote prompt.md");
    Ok(prompt)
}

async fn write_source_content(
    ws_path: &Path,
    items: &[ContentItem],
    source_map: &HashMap<String, &Source>,
    file_infos: &HashMap<SourceKey, SourceFileInfo>,
) -> Result<()> {
    // Group items by source key
    let mut items_by_key: HashMap<SourceKey, Vec<&ContentItem>> = HashMap::new();
    for item in items {
        let key = item_source_key(item, source_map);
        items_by_key.entry(key).or_default().push(item);
    }

    let sources_dir = ws_path.join("sources");
    tokio::fs::create_dir_all(&sources_dir)
        .await
        .map_err(GenerationError::Workspace)?;

    for (key, source_items) in &items_by_key {
        let info = match file_infos.get(key) {
            Some(i) => i,
            None => {
                warn!(key = ?key, "no file info for source key, skipping");
                continue;
            }
        };

        // Build flat file: YAML frontmatter + content items
        // Channel names from tg_folder_channels may contain quotes, so escape them.
        let escaped_name = info.name.replace('"', r#"\""#);
        let escaped_desc = info.description.replace('"', r#"\""#);
        let mut content = format!(
            "---\nname: \"{escaped_name}\"\ntype: {}\nitem_count: {}\ndescription: \"{escaped_desc}\"\n---\n\n",
            info.source_type,
            source_items.len(),
        );

        for (i, item) in source_items.iter().enumerate() {
            content.push_str(&format_content_item(item));
            if i < source_items.len() - 1 {
                content.push_str("\n---\n\n");
            }
        }

        let filename = format!("{}.md", info.slug);
        tokio::fs::write(sources_dir.join(&filename), &content)
            .await
            .map_err(GenerationError::Workspace)?;

        debug!(source = %info.name, items = source_items.len(), "wrote source content");
    }

    Ok(())
}

fn format_content_item(item: &ContentItem) -> String {
    let mut md = String::new();

    // Parse metadata for TG-specific fields (message_id, reply_to, forward, media)
    let meta: serde_json::Value = serde_json::from_str(&item.metadata).unwrap_or_default();
    let message_id = meta.get("message_id").and_then(|v| v.as_i64());
    let reply_to = meta.get("reply_to_msg_id").and_then(|v| v.as_i64());
    let forward_from = meta.get("forward_from").and_then(|v| v.as_str());
    let forward_from_id = meta.get("forward_from_id").and_then(|v| v.as_i64());
    let forward_post_author = meta.get("forward_post_author").and_then(|v| v.as_str());
    let media_type = meta.get("media_type").and_then(|v| v.as_str());
    let is_forward = item.content_type == "forward";

    // Make the title a clickable link when URL is available — this makes the URL
    // structurally part of the article identity, so the LLM is more likely to preserve
    // it in the output rather than ignoring a separate **Link:** metadata field.
    match (&item.title, &item.url) {
        (Some(title), Some(url)) => md.push_str(&format!("### [{title}]({url})\n\n")),
        (Some(title), None) => md.push_str(&format!("### {title}\n\n")),
        _ => {}
    }

    md.push_str(&format!(
        "**Date:** {}\n",
        item.original_date.format("%Y-%m-%d %H:%M UTC")
    ));

    // For forwards, label the sender as "Forwarded by" to avoid misattribution
    if let Some(ref author) = item.author {
        if is_forward {
            md.push_str(&format!("**Forwarded by:** {author}\n"));
        } else {
            md.push_str(&format!("**Author:** {author}\n"));
        }
    }

    if let Some(msg_id) = message_id {
        md.push_str(&format!("**Message ID:** #{msg_id}\n"));
    }

    if let Some(reply_id) = reply_to {
        md.push_str(&format!("**Reply to:** #{reply_id}\n"));
    }

    // Original source of the forward
    if let Some(fwd) = forward_from {
        md.push_str(&format!("**Original source:** {fwd}\n"));
    } else if let Some(fwd_id) = forward_from_id {
        md.push_str(&format!("**Original source:** [channel/user ID {fwd_id}]\n"));
    }
    if let Some(post_author) = forward_post_author {
        md.push_str(&format!("**Original author:** {post_author}\n"));
    }

    if let Some(media) = media_type {
        md.push_str(&format!("**Media:** {media}\n"));
    }

    if let Some(ref url) = item.url {
        md.push_str(&format!("**Link:** {url}\n"));
    }

    md.push('\n');

    if item.body.is_empty() {
        if let Some(media) = media_type {
            md.push_str(&format!("[{media} — no caption, see link]\n"));
        }
    } else {
        md.push_str(&item.body);
        md.push('\n');
    }

    md
}

async fn invoke_opencode(
    binary: &str,
    workspace: &Path,
    model: &str,
    prompt: &str,
    timeout_str: &str,
    extra_args: &[String],
    cancel: CancellationToken,
) -> Result<(String, Option<i32>)> {
    let timeout = humantime::parse_duration(timeout_str).context("parsing opencode timeout")?;

    info!(
        binary = %binary,
        model = %model,
        workspace = %workspace.display(),
        "invoking opencode"
    );

    let mut cmd = tokio::process::Command::new(binary);
    cmd.arg("run")
        .arg("--share")
        .arg("--model")
        .arg(model)
        .args(extra_args)
        .arg("--")
        .arg(prompt)
        .current_dir(workspace)
        // Enable opencode's Exa-powered websearch tool so the model can verify
        // facts and find real URLs instead of hallucinating from training data.
        .env("OPENCODE_ENABLE_EXA", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(GenerationError::OpencodeBinaryNotFound(binary.to_string()).into());
        }
        Err(e) => {
            return Err(GenerationError::OpencodeExecution {
                exit_code: None,
                stderr: e.to_string(),
            }
            .into());
        }
    };

    // Take stdout/stderr handles so we can read them after wait/kill
    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();

    // Wait for completion, timeout, or cancellation (PRD §9.9: kill subprocess on shutdown)
    tokio::select! {
        r = tokio::time::timeout(timeout, child.wait()) => {
            match r {
                Ok(Ok(status)) => {
                    let (stdout, stderr) = read_child_pipes(child_stdout, child_stderr).await;
                    let log = format!("=== STDOUT ===\n{stdout}\n=== STDERR ===\n{stderr}");
                    let exit_code = status.code();
                    if !status.success() {
                        warn!(
                            exit_code = ?exit_code,
                            stderr = %stderr.chars().take(500).collect::<String>(),
                            "opencode exited with error"
                        );
                    }
                    Ok((log, exit_code))
                }
                Ok(Err(e)) => Err(GenerationError::OpencodeExecution {
                    exit_code: None,
                    stderr: e.to_string(),
                }.into()),
                Err(_) => {
                    warn!("opencode timed out, killing subprocess");
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    let (stdout, stderr) = read_child_pipes(child_stdout, child_stderr).await;
                    let partial_log = format!("=== STDOUT (partial) ===\n{stdout}\n=== STDERR (partial) ===\n{stderr}");
                    Err(GenerationError::Timeout(
                        format!("{timeout_str}. Partial log:\n{partial_log}")
                    ).into())
                }
            }
        }
        _ = cancel.cancelled() => {
            warn!("generation cancelled, killing opencode subprocess");
            let _ = child.kill().await;
            let _ = child.wait().await;
            let (stdout, stderr) = read_child_pipes(child_stdout, child_stderr).await;
            let partial_log = format!("=== STDOUT (partial) ===\n{stdout}\n=== STDERR (partial) ===\n{stderr}");
            Err(GenerationError::OpencodeExecution {
                exit_code: None,
                stderr: format!("cancelled during shutdown. Partial log:\n{partial_log}"),
            }.into())
        }
    }
}

async fn read_child_pipes(
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
) -> (String, String) {
    let stdout_str = if let Some(mut out) = stdout {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).to_string()
    } else {
        String::new()
    };
    let stderr_str = if let Some(mut err) = stderr {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).to_string()
    } else {
        String::new()
    };
    (stdout_str, stderr_str)
}

fn extract_share_url(generation_log: &str) -> Option<String> {
    const PREFIX: &str = "https://opncd.ai/share/";
    let start = generation_log.find(PREFIX)?;
    let rest = &generation_log[start..];
    // URL ends at the first character that isn't valid in a URL path segment
    let end = rest
        .find(|c: char| c.is_whitespace() || c.is_control() || c == '\x1b')
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

fn parse_output(content: &str) -> Result<(String, Vec<String>, String)> {
    let matter = Matter::<YAML>::new();
    let result = matter.parse(content);

    // Extract frontmatter data into an owned hashmap
    let frontmatter = result.data.as_ref().and_then(|d| d.as_hashmap().ok());

    let title = frontmatter
        .as_ref()
        .and_then(|m| m.get("title"))
        .and_then(|v| v.as_string().ok())
        .unwrap_or_else(|| {
            // Fallback: extract title from first # heading
            content
                .lines()
                .find(|l| l.starts_with("# "))
                .map(|l| l.trim_start_matches("# ").to_string())
                .unwrap_or_else(|| "Untitled Digest".to_string())
        });

    let topics: Vec<String> = frontmatter
        .as_ref()
        .and_then(|m| m.get("topics"))
        .and_then(|v| v.as_vec().ok())
        .map(|vec| vec.into_iter().filter_map(|v| v.as_string().ok()).collect())
        .unwrap_or_default();

    let body = result.content;

    if body.trim().is_empty() {
        return Err(GenerationError::OutputParse("article body is empty".to_string()).into());
    }

    Ok((title, topics, body))
}

fn markdown_to_html(markdown: &str) -> String {
    let parser = pulldown_cmark::Parser::new(markdown);
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, parser);
    html
}

fn slug_from_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

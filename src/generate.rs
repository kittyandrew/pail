use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gray_matter::Matter;
use gray_matter::engine::YAML;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use tokio::io::AsyncReadExt;

use crate::config::{Config, OutputChannelConfig};
use crate::error::GenerationError;
use crate::models::{ContentItem, GeneratedArticle, OutputChannel, Source};

/// Generate a digest article for a channel.
/// Returns (article, raw_output) where raw_output is the exact content of output.md.
#[allow(clippy::too_many_arguments)]
pub async fn generate_article(
    config: &Config,
    channel_config: &OutputChannelConfig,
    channel: &OutputChannel,
    items: &[ContentItem],
    source_map: &HashMap<String, &Source>,
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

    // Compute disambiguated slugs for each source (used by both manifest and workspace dirs)
    let source_slugs = compute_source_slugs(source_map);

    // Write workspace files
    write_manifest(
        ws_path,
        channel_config,
        items,
        source_map,
        &source_slugs,
        covers_from,
        covers_to,
        &config.pail.timezone,
    )
    .await
    .context("writing manifest")?;

    let prompt = write_prompt(ws_path, config, channel_config)
        .await
        .context("writing prompt")?;

    write_source_content(ws_path, items, source_map, &source_slugs)
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
        return Err(GenerationError::OutputParse("output.md is empty".to_string()).into());
    }

    let (title, topics, body_markdown) = parse_output(&output_content).context("parsing output")?;

    // Convert markdown to HTML
    let body_html = markdown_to_html(&body_markdown);

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

/// Compute disambiguated slugs for each source, ensuring no two sources share a directory name.
fn compute_source_slugs(source_map: &HashMap<String, &Source>) -> HashMap<String, String> {
    let mut slug_counts: HashMap<String, usize> = HashMap::new();
    let mut result: HashMap<String, String> = HashMap::new();

    // Sort by source name for deterministic slug assignment when names collide
    let mut entries: Vec<_> = source_map.iter().collect();
    entries.sort_by_key(|(_, source)| &source.name);

    for (id, source) in entries {
        let base_slug = slug_from_name(&source.name);
        let count = slug_counts.entry(base_slug.clone()).or_default();
        let slug = if *count == 0 {
            base_slug.clone()
        } else {
            format!("{base_slug}-{}", *count + 1)
        };
        *count += 1;
        result.insert(id.clone(), slug);
    }

    result
}

#[allow(clippy::too_many_arguments)]
async fn write_manifest(
    ws_path: &Path,
    channel_config: &OutputChannelConfig,
    items: &[ContentItem],
    source_map: &HashMap<String, &Source>,
    source_slugs: &HashMap<String, String>,
    covers_from: DateTime<Utc>,
    covers_to: DateTime<Utc>,
    timezone: &str,
) -> Result<()> {
    // Count items per source
    let mut source_item_counts: HashMap<&str, usize> = HashMap::new();
    for item in items {
        *source_item_counts.entry(&item.source_id).or_default() += 1;
    }

    // Sort by source name for deterministic manifest output
    let mut sorted_sources: Vec<_> = source_map.iter().collect();
    sorted_sources.sort_by_key(|(_, source)| &source.name);

    let sources_json: Vec<serde_json::Value> = sorted_sources
        .into_iter()
        .map(|(id, source)| {
            serde_json::json!({
                "slug": source_slugs.get(id).cloned().unwrap_or_else(|| slug_from_name(&source.name)),
                "name": source.name,
                "type": source.source_type,
                "item_count": source_item_counts.get(id.as_str()).unwrap_or(&0),
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
    source_slugs: &HashMap<String, String>,
) -> Result<()> {
    // Group items by source
    let mut items_by_source: HashMap<&str, Vec<&ContentItem>> = HashMap::new();
    for item in items {
        items_by_source.entry(&item.source_id).or_default().push(item);
    }

    let sources_dir = ws_path.join("sources");
    tokio::fs::create_dir_all(&sources_dir)
        .await
        .map_err(GenerationError::Workspace)?;

    for (source_id, source_items) in &items_by_source {
        let source = match source_map.get(*source_id) {
            Some(s) => s,
            None => {
                warn!(source_id = %source_id, "unknown source ID, skipping");
                continue;
            }
        };

        let slug = source_slugs
            .get(*source_id)
            .cloned()
            .unwrap_or_else(|| slug_from_name(&source.name));

        // Build flat file: YAML frontmatter + content items
        // Source names and descriptions are validated at config load time to contain only
        // safe characters (no control chars, quotes, or backslashes), so no escaping needed.
        let description = source.description.as_deref().unwrap_or("");
        let mut content = format!(
            "---\nname: \"{}\"\ntype: {}\nitem_count: {}\ndescription: \"{description}\"\n---\n\n",
            source.name,
            source.source_type,
            source_items.len(),
        );

        for (i, item) in source_items.iter().enumerate() {
            content.push_str(&format_content_item(item));
            if i < source_items.len() - 1 {
                content.push_str("\n---\n\n");
            }
        }

        let filename = format!("{slug}.md");
        tokio::fs::write(sources_dir.join(&filename), &content)
            .await
            .map_err(GenerationError::Workspace)?;

        debug!(source = %source.name, items = source_items.len(), "wrote source content");
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

    if let Some(ref title) = item.title {
        md.push_str(&format!("### {title}\n\n"));
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

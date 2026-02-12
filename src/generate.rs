use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use gray_matter::Matter;
use gray_matter::engine::YAML;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::config::{Config, OutputChannelConfig};
use crate::error::GenerationError;
use crate::models::{ContentItem, GeneratedArticle, OutputChannel, Source};

const MAX_SOURCE_FILE_CHARS: usize = 50_000;

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

    write_prompt(ws_path, channel_config).await.context("writing prompt")?;

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

async fn write_prompt(ws_path: &Path, channel_config: &OutputChannelConfig) -> Result<()> {
    let prompt_template = format!(
        r#"You are pail's digest generator. Your job is to read collected content from
multiple sources and write a single, high-quality digest article.

## Editorial Directive
{editorial_directive}

## Workspace
All input data is in the current directory:
- `manifest.json` — generation metadata (channel config, time window, source list)
- `sources/` — subdirectories per source, each with content files
- `output.md` — write the final article HERE

## Instructions
1. Follow the editorial directive above closely — it defines the user's preferences.
2. Read `manifest.json` for the time window, source list, and channel metadata.
3. Read each source's content files in `sources/`.
4. Handle each source type according to the rules below (§ RSS Sources, § Telegram Sources).
5. For large inputs, consider summarizing per-source first, then synthesizing.
6. Write the final article to `output.md`.
7. Re-read your output and iterate if the quality is insufficient.

## Condensation and Fidelity
As source volume grows, you will need to condense more aggressively. This is expected —
a digest covering 200 articles cannot give each one a full paragraph. However:
- **Preserve the author's intent.** Condensation must retain the core argument, key evidence,
  and nuance of each piece. If an article's point is subtle or counterintuitive, make sure
  that subtlety survives the summary. Do not flatten complex arguments into generic platitudes.
- **Stay specific.** A condensed section should still contain concrete details: names, numbers,
  mechanisms, conclusions. "Researchers found interesting results" is useless. "MIT researchers
  showed 40% latency reduction using speculative decoding on Llama 3" is a digest.
- **Do not mislead by omission.** If condensing forces you to drop important caveats or
  counter-arguments, either keep them or skip the article entirely rather than presenting
  a misleading one-sided summary.
- **Scale gracefully.** With few articles, write thorough sections. With many, write tighter
  summaries but never sacrifice clarity for brevity. The reader should understand *why*
  something matters, not just *that* it happened.

## RSS Sources
- Source content files contain RSS summaries or excerpts, not the full text.
- **IMPORTANT: Fetch full articles.** For every item that has a **Link** URL,
  you MUST fetch the full article from that URL before writing about it. Do not write
  about an article based only on a title or summary — get the real content first.
  Skip items where the full content cannot be retrieved.

## Telegram Sources
- Source content files contain the full message text as collected from the live event stream.
  No additional fetching is needed — the content is already complete.
- Link formats differ by chat type:
  - Public channels/groups (has @username): `https://t.me/<username>/<message_id>`
  - Private channels/groups (no username): `https://t.me/c/<numeric_id>/<message_id>`
  - Forum topics: `https://t.me/<username_or_c/id>/<topic_id>/<message_id>`
- Conversations may be threaded — look for reply chains and group related messages.
- Media messages (photos, videos, voice) are noted by type but binary content is not included;
  describe them based on captions and context.

## Output Format
Write `output.md` with YAML frontmatter followed by the article body:

    ---
    title: "Your Article Title"
    topics:
      - "Topic 1"
      - "Topic 2"
    ---

    # Your Article Title
    ...article body...

## Article Body Format
- Start with a `# Title` matching the frontmatter title
- Use `## Sections` to organize by topic, not by source
- Synthesize related ideas across posts, find connections
- Use inline links `[text](url)` to reference original articles/messages
- Link to original articles. Skip anything that's just a short announcement with no substance
- End with a `## Sources` section listing all referenced sources
- **Never silently ignore articles.** If you skip an article for any reason (too short,
  off-topic, couldn't fetch content, etc.), list it in a final `## Skipped` section
  with the link and a one-line reason why it was excluded

## Editor's Notes
There are two types of editor's notes. Use both where appropriate.

**Tone and Framework:** Editor's notes are written in a distinctly different voice from
the main digest. The main body reports and synthesizes — editor's notes *assess*. Adopt
a rationalist epistemological framework: verified evidence and trusted primary sources
always take priority over pure reasoning. However, when no external source is available
or the point does not require one, clear logical reasoning from established premises is
the next best tool — and is far better than leaving a dubious claim unchallenged. State
your epistemic basis explicitly: "data from X shows..." vs "reasoning from Y, we would
expect..." so the reader can calibrate trust accordingly.

1. **Fact-checking blockquotes:** If a post makes bold or original claims, add
   `> **Editor's Note:**` blockquotes with your assessment. Be firm and fair.
   Consider fact-checking a key part of your job, not just parroting articles.
   When evidence exists, cite it — link to the study, the dataset, the counter-argument.
   When it does not, reason clearly from what is known and flag the uncertainty.
   If the claim is plausible but unverified, say so and explain what evidence
   would confirm or refute it.

2. **Inline annotations:** If a post contains specialized language, commonly confused
   or unusual terms, add an inline editor's note explaining what it actually means.
   You may also add verified, valid additional references as markdown hyperlinks.

   Examples of where inline notes are useful:
   - "Meanwhile, OpenAI hired Dylan Scandinaro (formerly X at Y) as Head of
     Preparedness (OpenAI's team responsible for evaluating catastrophic risks), ..."
   - "... documents obtained in cooperation with [Dallas](https://dallas-park.com/),
     a Ukrainian analytical company specializing in leaked Russian documents, ..."
   - "... systems like the [Koalitsiya](https://en.wikipedia.org/wiki/2S35_Koalitsiya-SV)
     and [Msta](https://en.wikipedia.org/wiki/2S19_Msta) self-propelled howitzers, ..."

## References and Citations
- Preserve references to external data, studies, papers, and other sources from the
  original articles as much as possible. If the original text cites something, keep
  that citation in the digest with a working link
- If an article lists references separately (e.g., at the end, in footnotes, or in a
  bibliography), incorporate them inline into the text as markdown hyperlinks rather
  than leaving them as a separate list
- When an article mentions a specific claim with a source, link directly to that source,
  not just to the article making the claim

## Link Verification — CRITICAL
**NEVER include a URL you have not verified.** Every hyperlink in the article — whether
in the main body, editor's notes, or inline annotations — must be either:
1. A URL that appeared in the source content files (already verified by pail), OR
2. A URL you have fetched yourself during this session and confirmed returns real content

If you want to reference something in an editor's note (a study, a dataset, a counter-argument),
you MUST fetch the URL first to confirm it exists and says what you claim it says. If you cannot
find a working URL, either omit the reference or state the claim without a link and note that
you could not locate a primary source. A fabricated link is worse than no link — it destroys
reader trust in the entire digest.

## Writing Style
- Write like a Reuters correspondent. Avoid typical AI-smell like em-dash saturation
- Do not address the reader directly. The editor does not know the reader's country,
  so specify what and who you are talking about, but do not overexplain
- Tone should reflect confidence in factuality. Do not prefer political leaning
  over facts and evidence
- Highlight what is genuinely new or significant
- Be honest about uncertainty — if something seems unverified, say so
- Respect the editorial directive's stated interests and ignore topics it asks to skip
"#,
        editorial_directive = channel_config.prompt.trim()
    );

    tokio::fs::write(ws_path.join("prompt.md"), prompt_template)
        .await
        .map_err(GenerationError::Workspace)?;

    debug!("wrote prompt.md");
    Ok(())
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
        let source_dir = sources_dir.join(&slug);
        tokio::fs::create_dir_all(&source_dir)
            .await
            .map_err(GenerationError::Workspace)?;

        // Write metadata.json
        let metadata = serde_json::json!({
            "name": source.name,
            "type": source.source_type,
            "item_count": source_items.len(),
        });
        tokio::fs::write(
            source_dir.join("metadata.json"),
            serde_json::to_string_pretty(&metadata)?,
        )
        .await
        .map_err(GenerationError::Workspace)?;

        // Build content markdown, splitting if needed
        let mut content_parts: Vec<String> = Vec::new();
        let mut current_part = String::new();

        for item in source_items {
            let item_md = format_content_item(item);
            if !current_part.is_empty() && current_part.len() + item_md.len() > MAX_SOURCE_FILE_CHARS {
                content_parts.push(std::mem::take(&mut current_part));
            }
            current_part.push_str(&item_md);
            current_part.push_str("\n---\n\n");
        }
        if !current_part.is_empty() {
            content_parts.push(current_part);
        }

        // Write content files
        if content_parts.len() == 1 {
            tokio::fs::write(source_dir.join("content.md"), &content_parts[0])
                .await
                .map_err(GenerationError::Workspace)?;
        } else {
            for (i, part) in content_parts.iter().enumerate() {
                let filename = format!("content_{:03}.md", i + 1);
                tokio::fs::write(source_dir.join(&filename), part)
                    .await
                    .map_err(GenerationError::Workspace)?;
            }
        }

        debug!(source = %source.name, items = source_items.len(), "wrote source content");
    }

    Ok(())
}

fn format_content_item(item: &ContentItem) -> String {
    let mut md = String::new();

    if let Some(ref title) = item.title {
        md.push_str(&format!("### {title}\n\n"));
    }

    md.push_str(&format!(
        "**Date:** {}\n",
        item.original_date.format("%Y-%m-%d %H:%M UTC")
    ));

    if let Some(ref author) = item.author {
        md.push_str(&format!("**Author:** {author}\n"));
    }

    if let Some(ref url) = item.url {
        md.push_str(&format!("**Link:** {url}\n"));
    }

    md.push('\n');
    md.push_str(&item.body);
    md.push('\n');

    md
}

async fn invoke_opencode(
    binary: &str,
    workspace: &Path,
    model: &str,
    timeout_str: &str,
    extra_args: &[String],
    cancel: CancellationToken,
) -> Result<(String, Option<i32>)> {
    let timeout = humantime::parse_duration(timeout_str).context("parsing opencode timeout")?;

    let inline_prompt = "Read prompt.md for your full instructions, then generate a digest article \
         into output.md using the sources in the workspace.";

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
        .arg(inline_prompt)
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
    use tokio::io::AsyncReadExt;

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

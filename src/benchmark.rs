use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::strategy::{self, StrategyRegistry};
use crate::{cli, db, generate, pipeline, store};

/// Arguments parsed from `pail benchmark run`.
pub struct BenchmarkRunArgs {
    pub since: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub channel: Option<String>,
    pub strategy: Option<String>,
    pub samples: usize,
    pub delay: String,
    pub timeout: Option<String>,
    pub models: Option<String>,
}

#[derive(Serialize)]
struct SampleMeta {
    model: String,
    strategy: String,
    sample: usize,
    duration_ms: u128,
    exit_code: Option<i32>,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

struct SampleResult {
    duration: Duration,
    success: bool,
    has_output: bool,
}

/// Discover available models by running `opencode models`.
/// If `filter` is Some, parse as comma-separated list and validate each exists.
/// If `filter` is None, return all lines starting with `opencode/`.
async fn discover_models(binary: &str, filter: Option<&str>) -> Result<Vec<String>> {
    let output = tokio::process::Command::new(binary)
        .arg("models")
        .output()
        .await
        .context("running `opencode models` — is opencode installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`opencode models` failed (exit {}): {}", output.status, stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let all_models: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if let Some(filter_str) = filter {
        let requested: Vec<String> = filter_str.split(',').map(|s| s.trim().to_string()).collect();
        for model in &requested {
            if !all_models.contains(model) {
                warn!(model = %model, "requested model not found in `opencode models` output — will try anyway");
            }
        }
        Ok(requested)
    } else {
        let mut free_models: Vec<String> = all_models.into_iter().filter(|m| m.starts_with("opencode/")).collect();
        free_models.sort();
        if free_models.is_empty() {
            anyhow::bail!("no opencode/* models found — run `opencode models` to check available models");
        }
        Ok(free_models)
    }
}

/// Generate a unique run ID: `YYYY-MM-DD-<slug>`, appending `-2`, `-3` if exists.
fn make_run_id(slug: &str) -> String {
    let date = Utc::now().format("%Y-%m-%d");
    let base = format!("{date}-{slug}");
    let results_dir = PathBuf::from("benchmarks/results");

    if !results_dir.join(&base).exists() {
        return base;
    }

    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !results_dir.join(&candidate).exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Strip `opencode/` prefix and replace `/` with `-` to make a filesystem-safe slug.
fn model_slug(model: &str) -> String {
    model.strip_prefix("opencode/").unwrap_or(model).replace('/', "-")
}

/// Extract the share suffix from opencode's stderr output.
///
/// When `share = "auto"` is set, opencode prints a line like:
///   `https://opncd.ai/share/C2tYo22n`
/// The suffix (e.g. `C2tYo22n`) matches the tail of the session ID (`ses_...C2tYo22n`).
fn parse_share_suffix(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let marker = "opncd.ai/share/";
        let pos = line.find(marker)?;
        let rest = &line[pos + marker.len()..];
        let suffix: String = rest.chars().take_while(|c| c.is_alphanumeric()).collect();
        if suffix.is_empty() { None } else { Some(suffix) }
    })
}

/// Recursively copy a directory tree.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .with_context(|| format!("copying {} -> {}", src_path.display(), dst_path.display()))?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct SessionListEntry {
    id: String,
}

/// Export the opencode session created in `workspace_dir` directly to `dest`.
///
/// Parses the share URL suffix from `log_text` (opencode's stderr) and matches
/// it against recent sessions. Falls back to the most recent session if no
/// share suffix is found.
async fn export_session(binary: &str, workspace_dir: &Path, dest: &Path, log_text: &str) -> bool {
    // List recent sessions — use -n 20 to find the right one among concurrent runs
    let list_output = match tokio::process::Command::new(binary)
        .args(["session", "list", "-n", "20", "--format", "json"])
        .current_dir(workspace_dir)
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return false,
    };

    if !list_output.status.success() {
        debug!("opencode session list failed");
        return false;
    }

    let sessions: Vec<SessionListEntry> = match serde_json::from_slice(&list_output.stdout) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Match session by share URL suffix from stderr, fall back to first (most recent)
    let share_suffix = parse_share_suffix(log_text);
    let session_id = if let Some(ref suffix) = share_suffix {
        sessions
            .iter()
            .find(|s| s.id.ends_with(suffix.as_str()))
            .or(sessions.first())
    } else {
        sessions.first()
    };
    let session_id = match session_id {
        Some(s) => s.id.clone(),
        None => return false,
    };
    if share_suffix.is_some() {
        debug!(session_id = %session_id, suffix = ?share_suffix, "matched session by share suffix");
    }

    // Export directly to file — avoids pipe buffer truncation on large sessions
    let file = match std::fs::File::create(dest) {
        Ok(f) => f,
        Err(e) => {
            debug!(error = %e, "failed to create session export file");
            return false;
        }
    };

    let status = match tokio::process::Command::new(binary)
        .args(["export", &session_id])
        .current_dir(workspace_dir)
        .stdout(file)
        .stderr(std::process::Stdio::null())
        .status()
        .await
    {
        Ok(s) => s,
        Err(e) => {
            debug!(session_id = %session_id, error = %e, "opencode export failed");
            return false;
        }
    };

    if !status.success() {
        debug!(session_id = %session_id, "opencode export exited with error");
        return false;
    }

    true
}

/// Run N samples for a single model, saving artifacts to `run_dir/<model-slug>/`.
#[allow(clippy::too_many_arguments)]
async fn run_model_samples(
    run_dir: &Path,
    workspace_dir: &Path,
    binary: &str,
    model: &str,
    strategy_name: &str,
    prompt: &str,
    samples: usize,
    timeout: &str,
    delay: Duration,
    cancel: CancellationToken,
) -> Vec<SampleResult> {
    let slug = model_slug(model);
    let mut results = Vec::new();

    for sample_num in 1..=samples {
        if cancel.is_cancelled() {
            break;
        }

        info!(model = %model, sample = %format!("{sample_num}/{samples}"), "starting sample");

        // Copy workspace to a temp dir
        let tmp = match tempfile::Builder::new().prefix("pail-bench-").tempdir() {
            Ok(t) => t,
            Err(e) => {
                warn!(model = %model, sample = sample_num, error = %e, "failed to create temp dir");
                results.push(SampleResult {
                    duration: Duration::ZERO,
                    success: false,
                    has_output: false,
                });
                continue;
            }
        };

        if let Err(e) = copy_dir_recursive(workspace_dir, tmp.path()) {
            warn!(model = %model, sample = sample_num, error = %e, "failed to copy workspace");
            results.push(SampleResult {
                duration: Duration::ZERO,
                success: false,
                has_output: false,
            });
            continue;
        }

        // Write empty output.md
        if let Err(e) = tokio::fs::write(tmp.path().join("output.md"), "").await {
            warn!(model = %model, sample = sample_num, error = %e, "failed to write output.md");
            continue;
        }

        let start = Instant::now();
        let invoke_result = generate::invoke_opencode(binary, tmp.path(), model, prompt, timeout, cancel.clone()).await;
        let duration = start.elapsed();

        let (log, exit_code, error) = match invoke_result {
            Ok((log, code)) => (log, code, None),
            Err(e) => {
                let err_str = format!("{e:#}");
                warn!(model = %model, sample = sample_num, error = %err_str, "opencode invocation failed");
                (String::new(), None, Some(err_str))
            }
        };

        // Read output.md from the temp dir
        let output_content = tokio::fs::read_to_string(tmp.path().join("output.md"))
            .await
            .unwrap_or_default();

        let has_output = !output_content.trim().is_empty();
        let success = exit_code == Some(0) && has_output;

        // Save artifacts
        let sample_dir = run_dir.join(&slug).join(format!("sample-{sample_num}"));
        if let Err(e) = std::fs::create_dir_all(&sample_dir) {
            warn!(error = %e, "failed to create sample dir");
        } else {
            let _ = std::fs::write(sample_dir.join("output.md"), &output_content);
            let _ = std::fs::write(sample_dir.join("log.txt"), &log);

            // Build log_for_export before moving `error` into SampleMeta
            let log_for_export: String = if !log.is_empty() {
                log.clone()
            } else {
                error.as_deref().unwrap_or("").to_string()
            };

            let meta = SampleMeta {
                model: model.to_string(),
                strategy: strategy_name.to_string(),
                sample: sample_num,
                duration_ms: duration.as_millis(),
                exit_code,
                timestamp: Utc::now().to_rfc3339(),
                error,
            };
            if let Ok(meta_json) = serde_json::to_string_pretty(&meta) {
                let _ = std::fs::write(sample_dir.join("meta.json"), meta_json);
            }

            // Export opencode session transcript directly to file
            let session_path = sample_dir.join("session.json");
            if export_session(binary, tmp.path(), &session_path, &log_for_export).await {
                info!(model = %model, sample = sample_num, "session exported");
            } else {
                debug!(model = %model, sample = sample_num, "no session to export");
            }
        }

        info!(
            model = %model,
            sample = %format!("{sample_num}/{samples}"),
            duration = %format!("{:.0?}", duration),
            exit_code = ?exit_code,
            success = success,
            "sample complete"
        );

        results.push(SampleResult {
            duration,
            success,
            has_output,
        });

        // Delay between samples (skip after last)
        if sample_num < samples && !cancel.is_cancelled() {
            tokio::time::sleep(delay).await;
        }
    }

    results
}

/// Top-level benchmark orchestrator.
pub(crate) async fn run_benchmark(config: &Config, registry: &StrategyRegistry, args: BenchmarkRunArgs) -> Result<()> {
    let time_window = cli::parse_time_window(&args.since, &args.from, &args.to)?;
    let delay = humantime::parse_duration(&args.delay).context("invalid --delay duration")?;

    // Resolve channel config
    let channel_config = if let Some(ref slug) = args.channel {
        config
            .output_channel
            .iter()
            .find(|c| c.slug == *slug)
            .ok_or_else(|| anyhow::anyhow!("no output channel with slug '{slug}'"))?
    } else {
        config
            .output_channel
            .first()
            .ok_or_else(|| anyhow::anyhow!("no output channels configured"))?
    };

    info!(channel = %channel_config.slug, "benchmark channel selected");

    // Create temp DB for pipeline setup
    let temp_data = tempfile::tempdir().context("creating temp data dir")?;
    let mut bench_config = config.clone();
    bench_config.pail.data_dir = temp_data.path().to_path_buf();
    let pool = db::create_pool(&bench_config).await.context("creating temp database")?;
    store::sync_config_to_db(&pool, &bench_config)
        .await
        .context("syncing config to temp database")?;

    let cancel = CancellationToken::new();
    let cancel_signal = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        cancel_signal.cancel();
    });

    // Prepare pipeline context (fetches RSS, queries items)
    info!("fetching content and preparing workspace...");
    let ctx = pipeline::prepare_pipeline_context(&pool, channel_config, time_window, true, None, &cancel)
        .await
        .context("preparing pipeline context")?
        .ok_or_else(|| anyhow::anyhow!("no content items found in the specified time window"))?;

    // Resolve strategy (--strategy flag overrides channel/default)
    let strategy_name = args
        .strategy
        .as_deref()
        .map(|s| s.to_string())
        .unwrap_or_else(|| strategy::resolve_strategy_name(config, channel_config));
    let strat = registry
        .get(&strategy_name)
        .ok_or_else(|| anyhow::anyhow!("strategy '{strategy_name}' not found in registry"))?;
    let merged_opencode_config = strategy::resolve_opencode_config(strat)?;

    info!(strategy = %strategy_name, "using strategy for benchmark");

    // Build source reference map
    let source_ref_map: HashMap<String, &_> = ctx.source_map.iter().map(|(k, v)| (k.clone(), v)).collect();

    // Prepare workspace
    let ws = generate::prepare_workspace(
        config,
        channel_config,
        strat,
        &merged_opencode_config,
        &ctx.items,
        &source_ref_map,
        &ctx.folder_channels,
        ctx.covers_from,
        ctx.covers_to,
    )
    .await
    .context("preparing workspace")?;

    let prompt = generate::write_prompt(ws.path(), strat, channel_config)
        .await
        .context("writing prompt")?;

    // Write empty output.md to workspace
    tokio::fs::write(ws.path().join("output.md"), "")
        .await
        .context("writing empty output.md")?;

    // Create run directory
    let run_id = make_run_id(&channel_config.slug);
    let run_dir = PathBuf::from("benchmarks/results").join(&run_id);
    std::fs::create_dir_all(&run_dir).context("creating run directory")?;

    // Copy workspace snapshot
    let workspace_snapshot = run_dir.join("workspace");
    copy_dir_recursive(ws.path(), &workspace_snapshot).context("copying workspace snapshot")?;
    info!(path = %workspace_snapshot.display(), "workspace snapshot saved");

    // Discover models
    let models = discover_models(&config.opencode.binary, args.models.as_deref()).await?;
    info!(count = models.len(), models = ?models, "models discovered");

    // Spawn one task per model
    let mut join_set = tokio::task::JoinSet::new();
    for model in &models {
        let run_dir = run_dir.clone();
        let workspace_snapshot = workspace_snapshot.clone();
        let binary = config.opencode.binary.clone();
        let model = model.clone();
        let strategy_name = strategy_name.clone();
        let prompt = prompt.clone();
        let timeout = args.timeout.clone().unwrap_or_else(|| strat.meta.timeout.clone());
        let cancel = cancel.clone();
        let samples = args.samples;

        join_set.spawn(async move {
            let results = run_model_samples(
                &run_dir,
                &workspace_snapshot,
                &binary,
                &model,
                &strategy_name,
                &prompt,
                samples,
                &timeout,
                delay,
                cancel,
            )
            .await;
            (model, results)
        });
    }

    // Wait for all
    let mut all_results = Vec::new();
    while let Some(result) = join_set.join_next().await {
        all_results.push(result);
    }

    // Print summary
    println!("\nBenchmark complete: {run_id}");

    // Find max model name length for alignment
    let max_name_len = models.iter().map(|m| model_slug(m).len()).max().unwrap_or(20);

    for handle_result in &all_results {
        match handle_result {
            Ok((model, results)) => {
                let slug = model_slug(model);
                let passed = results.iter().filter(|r| r.success).count();
                let partial = results.iter().filter(|r| !r.success && r.has_output).count();
                let total = results.len();
                let mean_duration = if !results.is_empty() {
                    let total_ms: u128 = results.iter().map(|r| r.duration.as_millis()).sum();
                    Duration::from_millis((total_ms / results.len() as u128) as u64)
                } else {
                    Duration::ZERO
                };
                let partial_note = if partial > 0 {
                    format!(" ({partial} partial)")
                } else {
                    String::new()
                };
                println!(
                    "  {:<width$} {}/{} passed{}, mean {:.0?}",
                    format!("{slug}:"),
                    passed,
                    total,
                    partial_note,
                    mean_duration,
                    width = max_name_len + 1,
                );
            }
            Err(e) => {
                println!("  [task error]: {e}");
            }
        }
    }

    println!("\nResults in: {}", run_dir.display());
    println!("Judge with: /benchmark (or review individual outputs manually)");

    Ok(())
}

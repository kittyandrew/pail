# Agentic Benchmark

Automated evaluation framework to determine which LLM agent produces the best digest articles for pail. Two phases: (1) a Rust subcommand runs all models and produces artifacts, (2) a Claude Code skill judges the results.

## Goal

Find the best default model for `opencode.default_model` by empirically measuring article quality across all free opencode models. The benchmark produces structured artifacts; judging is done by a Claude Code skill that can also be invoked independently.

## Architecture

```
Phase 1 — pail benchmark run (Rust):
  1. Prepare workspace from live RSS feeds using --from/--to dates
  2. Spawn all models in parallel — each model runs its 5 samples sequentially:
       model A: sample 1 → delay → sample 2 → ... → sample 5  ┐
       model B: sample 1 → delay → sample 2 → ... → sample 5  ├─ concurrent
       model C: sample 1 → delay → sample 2 → ... → sample 5  ┘
     Each sample: copy workspace → opencode run → collect output.md + log + timing
  3. Write all artifacts to benchmarks/results/<run-id>/

Phase 2 — Claude Code judge skill:
  1. Read run artifacts (workspace, articles, metadata)
  2. Extract scoring rubric from the system prompt
  3. Evaluate each article against the rubric
  4. Write judgment.json per article + summary.md
```

The two phases are independent — you can re-run judging without re-running generation, and vice versa.

## Phase 1: `pail benchmark run`

### Dataset: Config + Date Range

A benchmark uses the pail config's output channel and a `--from`/`--to` date range. The workspace is prepared fresh each run by reusing the existing pipeline code (`prepare_pipeline_context` → `prepare_workspace`).

This requires a temporary SQLite database — the pipeline syncs config to DB, fetches RSS, stores content items, then queries them back. The benchmark creates a temp DB that's discarded after workspace preparation.

The initial dataset uses the existing `config.example.toml` output channel ("Morning Tech Digest" with Hacker News + Lobsters) and a recent date range.

Since all models run in the same invocation against the same freshly-prepared workspace, they all see identical source data.

### Model Discovery

Models are discovered via `opencode models`, filtered to the `opencode/*` provider prefix (all free models). The user can override with `--models` to pass an explicit comma-separated list or a different glob pattern.

Current free models (as of 2026-02-21):

| Model ID | Notes |
|----------|-------|
| `opencode/big-pickle` | Stealth model, free during beta |
| `opencode/glm-5-free` | Free during feedback period |
| `opencode/gpt-5-nano` | Free GPT variant |
| `opencode/minimax-m2.5-free` | Free during feedback period |
| `opencode/trinity-large-preview-free` | Preview model, free |

### Sample Count and Execution Order

5 samples per model (configurable via `--samples`). Models run **in parallel** — one tokio task per model. Within each model's task, samples run **sequentially** with a configurable delay between them (default: 5s) to avoid per-model rate limits.

```
┌─ model A: sample 1 → delay → sample 2 → delay → ... → sample 5
├─ model B: sample 1 → delay → sample 2 → delay → ... → sample 5
├─ model C: sample 1 → delay → sample 2 → delay → ... → sample 5
└─ ...
```

### Workspace Handling

1. **Prepare once:** Call `prepare_pipeline_context` + `prepare_workspace` to create the workspace (manifest.json, sources/, opencode.json, prompt.md, empty output.md)
2. **Persist to run dir:** Copy the prepared workspace to `benchmarks/results/<run-id>/workspace/` for reference and for the judge to read later
3. **Copy per sample:** For each sample, copy the workspace to a fresh temp dir, run opencode against it, then extract `output.md` and generation log to the results dir. The temp dir is cleaned up after each sample.

### Runner Flow

```rust
// Pseudocode
async fn benchmark_run(config, channel_slug, from, to, samples, delay, models_filter) {
    // 1. Temp DB + pipeline setup (RSS fetch, no TG)
    let temp_db = create_temp_db();
    sync_config_to_db(&temp_db, &config);
    let ctx = prepare_pipeline_context(&temp_db, channel_config, time_window, true, None, cancel);

    // 2. Prepare workspace
    let ws = prepare_workspace(config, channel_config, items, ...);
    let prompt = write_prompt(ws.path(), config, channel_config);
    copy_workspace_to(ws.path(), &run_dir.join("workspace"));

    // 3. Discover models
    let available = run_opencode_models();  // parse `opencode models` output
    let models = filter_models(available, models_filter);  // default: opencode/*

    // 4. Spawn parallel tasks
    let handles: Vec<_> = models.iter().map(|model| {
        tokio::spawn(async move {
            for sample in 1..=samples {
                let tmp = copy_workspace_to_temp(run_dir.join("workspace"));
                let (log, exit_code, duration) = invoke_opencode(binary, tmp, model, prompt, timeout);
                save_artifacts(run_dir, model, sample, output_md, log, meta);
                tokio::time::sleep(delay).await;
            }
        })
    }).collect();

    // 5. Wait for all
    join_all(handles).await;

    // 6. Print summary of what was produced
    print_run_summary(run_dir);
}
```

### CLI Interface

```bash
# Run benchmark (uses first output_channel in config, or --channel to pick)
pail benchmark run --from 2026-02-14T00:00:00Z --to 2026-02-21T00:00:00Z

# With overrides
pail benchmark run --since 7d --samples 3 --delay 10s --channel tech-digest

# Explicit model list (free models)
pail benchmark run --since 7d --models opencode/big-pickle,opencode/glm-5-free

# Mix free and paid models (any model from `opencode models` works)
pail benchmark run --since 7d --models opencode/big-pickle,anthropic/claude-sonnet-4-6,openai/gpt-5.2
```

Flags:
- `--from`/`--to` or `--since` — time window (same parsing as `pail generate`)
- `--channel <slug>` — output channel to benchmark (default: first in config)
- `--strategy <name>` — override generation strategy (default: channel's configured strategy). Useful for comparing strategy-model pairs across runs.
- `--samples <N>` — samples per model (default: 5)
- `--delay <duration>` — delay between samples of the same model (default: 5s)
- `--timeout <duration>` — per-generation timeout (default: 15m)
- `--models <list>` — comma-separated model IDs from any provider (default: auto-discover `opencode/*`). Any model listed by `opencode models` works — e.g., `anthropic/claude-sonnet-4-6`, `openai/gpt-5.2`

### Artifacts Produced

```
benchmarks/results/<run-id>/
  workspace/                     # prepared workspace snapshot
    manifest.json
    prompt.md
    opencode.json
    output.md                    # empty (template)
    sources/
      hacker-news.md
      lobsters.md
  big-pickle/
    sample-1/
      output.md                  # generated article
      log.txt                    # opencode stdout/stderr
      meta.json                  # { model, strategy, sample, duration_ms, exit_code, timestamp }
      session.json               # opencode session export (if available)
    sample-2/
      ...
  glm-5-free/
    ...
```

Run ID format: `<date>-<channel-slug>`, e.g., `2026-02-21-tech-digest`. If the directory exists, append a numeric suffix (`-2`, `-3`).

### Progress Reporting

Each model task logs progress via `tracing`:
```
INFO benchmark: model=opencode/big-pickle sample=1/5 status=running
INFO benchmark: model=opencode/big-pickle sample=1/5 status=done duration=4m12s
INFO benchmark: model=opencode/glm-5-free sample=1/5 status=running
...
```

Since models run in parallel, logs interleave. A final summary is printed after all tasks complete:
```
Benchmark complete: 2026-02-21-tech-digest
  big-pickle:                   5/5 passed, mean 4m12s
  glm-5-free:                   4/5 passed, mean 5m02s
  gpt-5-nano:                   5/5 passed, mean 3m08s
  minimax-m2.5-free:            5/5 passed, mean 6m15s
  trinity-large-preview-free:   3/5 passed, mean 7m22s

Results in: benchmarks/results/2026-02-21-tech-digest/
Judge with: /benchmark (or review individual outputs manually)
```

## Phase 2: Claude Code Judge Skill

The `/benchmark` skill handles both running benchmarks and judging results. It reads a benchmark run's artifacts, derives strategy-specific scoring rubrics, and produces evaluations. This runs in the user's local Claude Code session — same instance used for development.

### Skill Invocation

```
/benchmark
```

The skill orchestrates the full sweep: all strategies × all models, with per-strategy rubrics and a unified comparison report. See `.claude/skills/benchmark/SKILL.md` for the full specification.

### Judge Flow

1. **Read workspace:** Load `workspace/prompt.md` (the system prompt given to all models) and `workspace/sources/*.md` (the source data)
2. **Extract rubric:** Analyze the system prompt and produce a scoring rubric with 8-12 criteria, each rated 1-5 with weights. Write to `rubric.json`
3. **Evaluate each article:** For each `<model>/sample-N/output.md`:
   - Read the article
   - Read `meta.json` for context (model name, timing, exit code)
   - Score against the rubric
   - Write `judgment.json` alongside the article
4. **Aggregate:** Compute per-model statistics, produce ranking table, write `summary.md`

### Rubric Schema

```json
{
  "criteria": [
    {
      "name": "Source Attribution",
      "description": "1 = no attribution, 5 = every claim attributed with source name in text",
      "weight": 2.0
    },
    ...
  ]
}
```

### Judgment Schema

```json
{
  "model": "opencode/big-pickle",
  "sample": 1,
  "criteria_scores": [
    { "name": "Source Attribution", "score": 4, "justification": "..." }
  ],
  "overall_score": 7.5,
  "weighted_total": 38.5,
  "issues": ["Missing link for HN article about X", "..."],
  "strengths": ["Good section organization", "..."]
}
```

### Summary Format

```markdown
# Benchmark Results: 2026-02-21-tech-digest

Dataset: Morning Tech Digest (Hacker News + Lobsters)
Window: 2026-02-14 to 2026-02-21
Judge: Claude Code (claude-opus-4-6)

## Ranking

| Rank | Model | Mean Score | Std Dev | Min | Max | Pass Rate | Mean Time |
|------|-------|-----------|---------|-----|-----|-----------|-----------|
| 1 | opencode/big-pickle | 7.8 | 0.4 | 7.2 | 8.3 | 5/5 | 4m12s |
| 2 | opencode/minimax-m2.5-free | 7.2 | 1.1 | 5.8 | 8.1 | 5/5 | 3m45s |
| ...

## Per-Model Analysis

### opencode/big-pickle (Rank 1, Mean: 7.8)
**Strengths:** ...
**Issues:** ...
**Sample scores:** 7.2, 8.3, 7.8, 7.5, 8.1

### opencode/glm-5-free (Rank 3, Mean: 6.5)
...
```

## Edge Cases

| Case | Handling |
|------|----------|
| Model produces empty output.md | Record as failure in meta.json, skip judging, count toward pass rate |
| Model times out | Kill subprocess, record timeout in meta.json |
| Model not in `opencode models` output | Warn and try anyway (model may be valid but absent from cached list) |
| RSS feed unavailable during workspace prep | Fail early before any model runs |
| Free model becomes unavailable mid-run | Record error for remaining samples, continue other models |
| All samples for a model fail | Report 0% pass rate in summary |
| Run dir already exists | Append numeric suffix to run ID |
| `opencode models` command fails | Fail with clear error — opencode must be available |
| Judge skill run on incomplete results | Judge what's available, note missing samples |
| Workspace too large for model context | Mitigated by the researcher subagent architecture — raw article content stays in isolated subagent sessions. If a model still exhausts context, recorded as failure (exit 0, empty output). See Decisions: "Dataset size" |

## File Layout

```
benchmarks/
  results/                       # gitignored except summary.md per run
    <run-id>/
      workspace/                 # frozen workspace snapshot
      rubric.json                # judge-produced scoring rubric
      summary.md                 # judge-produced ranking (tracked in git)
      <model-slug>/
        sample-<N>/
          output.md
          log.txt
          meta.json
          session.json           # opencode session export (if available)
          judgment.json          # judge-produced evaluation
```

## Decisions

- **Two-phase architecture:** Rust subcommand produces artifacts, Claude Code skill judges.
  Options: all-in-one Rust / Rust + opencode judge / Rust + Claude Code skill / shell scripts.
  Rationale: the Rust subcommand handles the mechanical part (spawning opencode, managing files, parallel execution). Judging requires nuanced article evaluation that Claude Code excels at. Separating the phases means you can re-judge without re-running generation, and the skill is invocable from any Claude Code session.

- **Model discovery:** `opencode models` filtered to `opencode/*` prefix, with `--models` override.
  Options: hardcoded list / config file / dynamic discovery with prefix filter.
  Rationale: free models come and go (kimi-k2.5-free was removed). Dynamic discovery ensures current models are tested. The `opencode/*` prefix captures all free models. `--models` override allows testing specific models or paid ones.

- **Dataset definition:** output channel + date range, workspace prepared fresh each run.
  Options: frozen workspace files in repo / live fetch each run.
  Rationale: simpler, no files to manage. All models in one run see the same data. RSS-only means no auth needed.

- **Sources:** RSS-only, no Telegram.
  Options: RSS only / RSS + Telegram / configurable.
  Rationale: public RSS feeds need no authentication. Makes the benchmark runnable anywhere.

- **Non-blind judging:** judge sees the model name alongside the article.
  Options: blind / non-blind.
  Rationale: transparency for debugging. The judge can reference model-specific patterns in its reasoning.

- **Sample count:** 5 per model, configurable.
  Options: 1 / 3 / 5 / 10.
  Rationale: 5 gives reliable statistics for free models with no cost.

- **Execution order:** models in parallel, samples sequential per model.
  Options: fully sequential / models parallel + samples sequential / fully parallel.
  Rationale: models use different API backends (safe to parallelize). Samples within a model hit the same backend (sequential + delay avoids rate limits).

- **Temp database for benchmark:** create and discard per run.
  Options: use existing pail.db / temp DB / in-memory DB.
  Rationale: the pipeline requires a DB for config sync and content storage. A temp DB avoids polluting the user's real database with benchmark data. Discarded after workspace preparation.

- **Judge implementation:** Claude Code skill, not opencode or direct API.
  Options: opencode run / Claude Code skill / direct Anthropic API / manual.
  Rationale: Claude Code is already running in the user's terminal, is highly capable at file reading and structured evaluation, and a skill makes judging reproducible and invocable. No new dependencies needed.

- **Rubric extraction:** done by the judge skill, not pre-computed.
  Options: pre-compute rubric in Rust / judge extracts rubric / manual rubric.
  Rationale: the judge (Claude Code) is the one that needs to understand the rubric. Extracting it as part of the judge flow ensures consistency. The rubric is saved to rubric.json for inspection and re-use.

- **`SampleResult` unused fields:** `sample` and `exit_code` fields produce a dead_code warning.
  Options: remove them / suppress with `#[allow(dead_code)]` / keep the warning.
  Rationale: the CLI summary only uses `success` and `duration` — per-sample detail is already written to `meta.json` on disk and read by the judge skill. The fields are kept for symmetry with `SampleMeta` and potential future use (e.g., `--verbose` summary). Warning is accepted rather than suppressed per project convention.

- **Dataset size:** previously constrained to `--since 3d` due to context exhaustion. Now relaxed thanks to the custom `fetch-article` tool and researcher subagent architecture.
  Options: no guidance / document recommended window / enforce max workspace size.
  Rationale: the original 7-day window with 16 RSS sources produced a 455KB workspace. Models would read all sources (~168K tokens), then WebFetch 5-8 full article URLs (~25K tokens each), totalling 400-550K tokens — exceeding context limits for Anthropic models and trinity's 131K hard limit. The fix: (1) `fetch-article` custom tool uses Readability to extract article bodies as ~3K tokens instead of ~25K, (2) researcher subagent fetches articles in isolated sessions so raw content never enters the parent's context. The workspace size constraint is relaxed because models no longer accumulate raw page content in their own context.

- **Default model update:** change hardcoded fallback and config.example.toml from `kimi-k2.5-free` to `glm-5-free`.
  Options: big-pickle / glm-5-free / gpt-5-nano / no default.
  Rationale: kimi-k2.5-free was removed from opencode. glm-5-free is a temporary choice — the benchmark will determine the actual best free model.

- **Websearch rate limiting:** Documented as known limitation for free-tier models.
  Free models using opencode's Exa-powered websearch hit 429 rate limits during
  reference URL building (step 7) and sometimes within researcher subagents.
  Options: accept as free-tier limitation / use alternative search provider /
  move all URL building into researcher subagents.
  Rationale: accepted for now — free models get free search, rate limits are expected.
  Added prompt guidance to not retry on 429 and proceed with available URLs.
  Need to research alternative search providers or paid Exa tiers as future improvement.

- **Session export cross-contamination:** Fixed by matching share URL suffix from stderr.
  Options: match by share URL suffix / use workspace-scoped session list / tag sessions with model name.
  Rationale: opencode prints `opncd.ai/share/<suffix>` to stderr when `share = "auto"`. The suffix
  matches the tail of the session ID. Listing recent sessions and matching by suffix correctly
  identifies the per-model session even when multiple models run concurrently.

- **Parallel researcher dispatch:** Emphasized in system prompt (step 3).
  Options: leave as-is (models interpret "for each batch" as sequential) / explicitly require parallel dispatch.
  Rationale: sequential dispatch wastes 5+ minutes and makes individual API hangs fatal. The prompt
  now explicitly instructs models to issue all Task calls in a single response.

- **Partial result tracking:** Added `has_output` field to distinguish timed-out runs that produced output.
  Options: bool only (success/fail) / add has_output flag / numeric output_bytes field.
  Rationale: models like glm-5-free wrote valid 20KB articles but timed out — identical display to
  models that produced nothing. The `has_output` flag shows "(N partial)" in the summary line.

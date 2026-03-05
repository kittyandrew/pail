---
name: benchmark
description: Run a benchmark sweep — models x eligible strategies — with scoring rubrics and a comparison report. Free models use simple strategy only.
disable-model-invocation: false
allowed-tools: Read, Write, Glob, Grep, Agent, Bash(command:cargo build *), Bash(command:target/release/pail *), Bash(command:opencode *), Bash(command:nix develop *), Bash(command:which *), Bash(command:wc *), Bash(command:ls *), Bash(command:cp *), Bash(command:mkdir *), Bash(command:cat *), Bash(command:grep *), Bash(command:head *), Bash(command:date *), AskUserQuestion
argument-hint: "[--models opus,pickle] [--since 3d] [--samples 1]"
---

# Benchmark Sweep

You are running a pail benchmark sweep: discovered models scored against eligible strategies,
then writing a unified comparison report. Free models (`opencode/*`) are restricted to the
`simple` strategy due to API rate limits — see Phase 0 for details.

**Before starting, load reference docs:**
- `docs/specs/agentic-benchmark.md` — benchmark CLI flags, artifact layout, runner behavior
- `docs/specs/generation-strategies.md` — strategy design, storage layout, CLI commands
- `docs/specs/cli.md` — full CLI reference for `pail benchmark run`, `pail strategy list/show`

These specs are the source of truth for CLI flags and behavior. Do NOT guess flags — check the specs.

**Write incrementally.** Each phase writes its files as soon as it's done. Do NOT accumulate
everything in memory and write once at the end.

## Argument Parsing

Parse `$ARGUMENTS` for optional flags:
- `--models <list>` — comma-separated model IDs (default: auto-discover free models)
- `--since <dur>` — time window duration (default: `3d`)
- `--samples <N>` — samples per model (default: `1`)
- `--channel <slug>` — output channel slug (default: first in config)

## Prerequisites: Build & Verify

Before any benchmark commands, ensure the binary is fresh and tools are available.

1. **Build pail from source:**
   ```bash
   cargo build --release
   ```
   This ensures you're running the latest code, not a stale cached binary.
   All subsequent `pail` commands use `target/release/pail` (it is NOT on PATH).

2. **Verify opencode is available:**
   ```bash
   which opencode || nix develop -c which opencode
   ```
   If `opencode` is not on PATH, prefix all `opencode` commands with `nix develop -c`
   (e.g., `nix develop -c opencode models`).

## Phase 0: Load Context & Discover

1. Read the three spec files listed above.
2. Run `target/release/pail strategy list` to discover all available strategies (builtin + user-defined).
   Output columns: NAME, SOURCE, TIMEOUT, TOOLS (count), DESCRIPTION.
3. Run `target/release/pail strategy show <name>` for each strategy to get **metadata** (description, tools,
   timeout, merged opencode.json). Note: `strategy show` only previews the first 20 lines
   of the prompt — do NOT use it for full prompt text. The full prompt will be read from
   `workspace/prompt.md` in each run's results after Phase 2.
4. Run `opencode models` to get available models. Unless `--models` was provided, filter to
   `opencode/*` (free models).
5. **Filter strategies by model tier.** Free models (`opencode/*` prefix) have strict rate
   limits on opencode.ai that cannot sustain complex strategies or concurrent subagent dispatch.
   - **Free models → `simple` strategy ONLY.** Never run free models against `brief`, `agentic`,
     or any other strategy. They will hit 429 rate limits and produce zero output.
   - **Paid/self-hosted models** (no `opencode/` prefix) → all strategies.
   - If all discovered models are free, only the `simple` strategy is used regardless of how
     many strategies exist.
6. Print a plan summary:
   ```
   Benchmark plan: <N strategies> x <M models> x <S samples> = <total> generations
   Strategies: <list (after filtering)>
   Models: <list>
   Window: --since <dur>
   ```

**Edge cases:**
- If `pail strategy list` returns only one strategy, proceed (still useful for model comparison).
- If `opencode models` fails, stop and report — opencode must be available.
- If no models match the filter, ask the user which models to use.

## Phase 1: Pin Time Window

To ensure all strategies see identical source data, pin the time window to absolute timestamps
before running any benchmarks.

If `--since` was used (the default), convert it to `--from`/`--to`:
```bash
# Compute absolute timestamps
TO=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
FROM=$(date -u -d "<dur> ago" +"%Y-%m-%dT%H:%M:%SZ")
```

All benchmark runs in Phase 2 will use `--from $FROM --to $TO` instead of `--since`.
This prevents RSS drift between strategy runs.

## Phase 2: Run + Judge (per strategy)

For each discovered strategy, **run the benchmark then judge immediately** before moving to
the next strategy. This interleaved approach means partial results survive session interruptions.

### 2a: Derive Rubric

Read the strategy's full prompt to derive a scoring rubric. For the **first** strategy, read
the prompt from `src/strategies/<name>/prompt.md` (builtins). For subsequent strategies, you
can also read from `workspace/prompt.md` in a completed run's results (this is the full
rendered prompt with editorial directive and workspace context).

**How to derive a rubric:**
1. Read the strategy's prompt text.
2. Identify every concrete requirement — output format rules, linking rules, workflow steps
   (subagent dispatch, fetching, verification), style constraints, content rules (editor's
   notes, skipped section, condensation), etc.
3. Group related requirements into 6-11 scoring criteria. Each criterion:
   - Maps to one or more specific requirements from the prompt
   - Has a clear 1-5 scale description (1 = requirement violated, 5 = fully satisfied)
   - Has a weight using these tiers:
     - **Weight 3.0:** Requirements the prompt emphasizes as critical, appears in verification
       steps, or has explicit "NEVER"/"MUST" language (e.g., link verification, source linking)
     - **Weight 2.0:** Requirements with their own numbered step or distinct section
       (e.g., condensation, editor's notes, subagent dispatch)
     - **Weight 1.0-1.5:** Stylistic preferences, formatting details (e.g., writing style,
       output format, language consistency)
   - Has a `source` field: `"output"` (scorable from the article alone) or `"session"`
     (requires session.json to evaluate — e.g., subagent dispatch, tool usage patterns)
4. Criteria that differentiate this strategy must be present. If the prompt requires subagent
   dispatch → criterion for it (source: session). If the prompt forbids editor's notes →
   criterion checking absence (source: output). If the prompt requires bullet-point format →
   criterion for it (source: output).

**Write the rubric** to `benchmarks/rubrics/<strategy>-rubric.json`:

```json
{
  "strategy": "<name>",
  "criteria": [
    {
      "name": "Source Attribution",
      "description": "Every covered item has an inline hyperlink to its source URL. 1 = no links, 5 = every item linked",
      "weight": 3.0,
      "source": "output"
    },
    {
      "name": "Researcher Dispatch",
      "description": "Dispatched researcher subagents in parallel batches. 1 = no dispatch, 5 = all batches parallel",
      "weight": 2.0,
      "source": "session"
    }
  ]
}
```

Also copy the rubric into the run directory after the benchmark completes (step 2b).

### 2b: Run Benchmark

Run the benchmark for this strategy using the pinned time window from Phase 1:
```
target/release/pail benchmark run --from $FROM --to $TO --strategy <name> --models <list> --samples <N> --channel <slug>
```

**Parse the run-id** from stdout. The benchmark prints `Results in: benchmarks/results/<run-id>/`
near the end — grep for `"Results in: "` to extract the path.

After the run completes:
```
mkdir -p benchmarks/rubrics
cp benchmarks/rubrics/<strategy>-rubric.json benchmarks/results/<run-id>/rubric.json
```

### 2c: Judge All Samples for This Strategy

**Re-read the strategy's full prompt** from `benchmarks/results/<run-id>/workspace/prompt.md`
to calibrate expectations. This is the actual rendered prompt the models received.

For each `<model>/sample-N/` directory in the run:

1. Read `output.md`. If empty or missing → record as failed sample (score 0).
2. Read `meta.json` for duration, exit code, model name.
3. Score the article against `"output"`-sourced rubric criteria (1-5 each, with justification).
4. If `session.json` exists, score `"session"`-sourced criteria. If no session.json, mark
   session criteria as `"n/a"` and exclude from the weighted total.
5. Count lines and characters in `output.md` (for the report tables).
6. Compute:
   - `weighted_total` = sum of (score * weight) for scored criteria
   - `max_possible` = sum of (5 * weight) for scored criteria
   - `overall_score` = (weighted_total / max_possible) * 10
7. Write `judgment.json` alongside the sample:

```json
{
  "model": "opencode/example-model",
  "strategy": "<strategy-name>",
  "sample": 1,
  "criteria_scores": [
    { "name": "Source Attribution", "score": 4, "source": "output", "justification": "..." },
    { "name": "Researcher Dispatch", "score": "n/a", "source": "session", "justification": "session.json not available" }
  ],
  "overall_score": 7.5,
  "weighted_total": 38.5,
  "line_count": 145,
  "char_count": 8720,
  "issues": ["..."],
  "strengths": ["..."]
}
```

**Be consistent.** Apply the same standards to all models. If you penalize one model for an
issue, penalize all models equally for the same issue.

**Check links.** If an article claims to link to a source, verify the URL appears in the
workspace source files. Fabricated URLs are a severe penalty.

### 2d: Session Analysis for This Strategy

If `session.json` files exist in any sample directory, analyze agent behavior. **Do NOT read
session.json files into the main context** — they can be hundreds of KB each and will exhaust
the context window.

Instead, dispatch one Agent subagent per strategy run. The subagent reads the session files
in its own isolated context and returns a structured summary. Use `subagent_type: "general-purpose"`
with a prompt like:

> You are analyzing opencode session transcripts for a benchmark run.
> The run directory is `benchmarks/results/<run-id>/`.
> Read `session.json` from each `<model>/sample-N/` directory that has one.
>
> For each model, analyze all its sessions and produce a JSON summary. Write
> the result to `benchmarks/results/<run-id>/session-analysis.json` with this structure:
>
> ```json
> {
>   "<model-slug>": {
>     "tool_counts": { "read": N, "write": N, "edit": N, "websearch": N, "webfetch": N, "fetch_article": N, "Task": N, "other": N },
>     "mean_tool_calls": N,
>     "workflow_pattern": "read manifest → read sources → fetch articles → write output → verify links",
>     "workflow_consistent": true,
>     "pre_write_searches": N,
>     "post_write_audit": true,
>     "tasks_dispatched": N,
>     "tasks_parallel": true,
>     "errors": ["403 on webfetch example.com", "..."],
>     "error_recovery": "retried with alternative URL",
>     "notable": "any other interesting observations"
>   }
> }
> ```
>
> This is research only — read session files and write the JSON summary. Do not modify any
> other files.

Wait for the subagent to complete. The summary JSON feeds into Phase 4's unified report.
If the Agent tool is unavailable, skip session analysis gracefully — the output-quality
scoring in Phase 2c is the primary evaluation.

Repeat Phase 2 (a-d) for each strategy.

## Phase 3: Per-Run Summaries

After all strategies are run and judged, write `summary.md` in each run directory. Include:
- Run metadata (strategy, models, time window, samples)
- Per-model results table with scores
- Brief analysis of notable patterns
- Session behavior summary (if session.json was available)

These are tracked by git (`!benchmarks/results/*/summary.md` is un-ignored) so they serve
as the permanent record for each run.

## Phase 4: Write Unified Report

Write `benchmarks/benchmark-report.md` with the full cross-strategy comparison.

### 1. Header

Run date, strategies tested, models tested, samples per model, time window (from/to),
judge identity, total generation count.

### 2. Per-Strategy Tables

One table per strategy. All use identical column structure:

```
| Model | Pass | Duration | Lines | <Crit1> | <Crit2> | ... | Score |
```

- **Pass**: pass rate (e.g., 1/1)
- **Duration**: mean generation time
- **Lines**: mean line count of output.md
- Criterion columns: mean score (1-5) across samples (names from the strategy's rubric;
  only `"output"`-sourced criteria — session criteria go in the behavior section)
- **Score**: overall weighted score (0-10)
- Sort by Score descending

### 3. Cross-Strategy Comparison (skip if only one strategy was run)

```
| Model | <Strategy1> | <Strategy2> | ... | Best | Mean |
```

- Strategy columns are dynamic — one per discovered strategy
- Each shows the overall score (0-10)
- **Best**: highest score across strategies
- **Mean**: average across all strategies

### 4. Per-Criterion Cross-Strategy Comparison (skip if only one strategy was run)

For criteria that appear across multiple strategies (source attribution, link verification,
output format, skipped section, writing style, etc.), show how the same model scores on the
same criterion under different strategies:

```
| Model | Criterion | <Strategy1> | <Strategy2> | ... | Delta |
```

Group by criterion, sort by largest delta. Only include criteria shared by 2+ strategies.

### 5. Agent Behavior Comparison

If session.json data was available, summarize agent behavior:

```
| Model | Strategy | Tool Calls | Searches | Fetches | Tasks | Errors |
```

### 6. Model Rankings

- Overall ranking by score
- Per-strategy winners (if multiple strategies)
- Best model recommendation

### 7. Key Findings

3-5 bullets summarizing the most important observations. Focus on actionable insights:
which model-strategy pairs work, which don't, and why.

### 8. Failure Analysis

For any model with failed samples: explain exactly what went wrong. Quote errors from
meta.json or log.txt. Group by failure mode (timeout, empty output, content filter, etc.).

### 9. Recommendation

- Best overall model
- Best free model per strategy
- Default model recommendation with rationale
- Which strategy to use for which use case

## Important Rules

- **Self-contained.** This skill does NOT call /bench-judge. It does its own judging.
- **Free models = simple only.** Free opencode models (`opencode/*` prefix) MUST only run
  against the `simple` strategy. They hit 429 rate limits on complex strategies (agentic, brief)
  because those require subagent dispatch and concurrent API calls that exceed the free tier quota.
- **Dynamic discovery.** Strategy list and rubrics are derived at runtime from `pail strategy list`
  and actual prompt files. Nothing is hardcoded — a new strategy added tomorrow works automatically.
- **Interleaved execution.** Run + judge + analyze each strategy before starting the next.
  Partial results survive interruptions.
- **Handle failures gracefully.** Empty outputs, timeouts, and errors should be noted and
  scored 0 but not crash the evaluation. Continue with remaining samples.
- **Verify before asserting.** Check workspace source files to verify link claims in articles.
- **Pinned time window.** Always convert `--since` to `--from`/`--to` before the first run.
  All strategies must see identical source data.

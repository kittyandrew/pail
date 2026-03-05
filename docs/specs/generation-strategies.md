# Generation Strategies

Named, self-contained bundles of system prompt, opencode config, and tool definitions that control how pail generates digest articles. Different strategies enable different quality/complexity trade-offs — a simple strategy works with free models, while a complex agentic strategy leverages subagents and verification for state-of-the-art models.

## Motivation

The current generation pipeline has a single system prompt and opencode config defined in `config.toml`. This prompt has grown to include researcher subagent dispatching, verifier subagent dispatching, reference library building, and parallel task coordination. Free models (opencode/* tier) cannot handle this complexity — they fail to dispatch subagents correctly, get stuck in loops, or produce empty output. Meanwhile, powerful paid models (Claude, GPT) thrive with it.

The project needs at least two modes:
- **Simple** — read sources, fetch articles directly, write a digest with basic verification. Works with any model.
- **Agentic** — full research pipeline with researcher subagents, verifier subagent, reference library building. Requires capable models (Claude Sonnet+, GPT-4+).
- **Brief** — concise, tightly condensed digests optimized for quick reading. Can work with any model.

Beyond these, users may want custom strategies for specific use cases (e.g., a translation-focused strategy, a deep-analysis strategy).

Note: this feature directly enables the [Shorter Digests](../ideas/shorter-digests.md) idea — a `brief` strategy with a concise-focused prompt makes it nearly free to implement.

## Design

### What Is a Strategy

A strategy is a named directory containing:

```
<strategy-name>/
  prompt.md             # YAML frontmatter (metadata + params) + prompt body
  opencode.json         # optional: overlay on the global base opencode config
  tools/                # optional: user strategy custom tools (not for built-ins)
    custom-tool.ts
```

The `prompt.md` file's YAML frontmatter defines the strategy's identity and execution parameters:

```yaml
---
format_version: 1
name: agentic
description: Full research pipeline with researcher and verifier subagents
timeout: 20m
max_retries: 1
tools:
  - fetch-article
---

You are pail's digest generator. Your job is to read collected content from
multiple sources and write a single, high-quality digest article.

## Editorial Directive
{editorial_directive}

## Instructions
...
```

**Frontmatter fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `format_version` | yes | Strategy format version (must be `1`). For future schema evolution. |
| `name` | yes | Unique identifier, used in config (`strategy = "agentic"`) |
| `description` | yes | Human-readable summary |
| `timeout` | no | Per-generation timeout (default: `20m`) |
| `max_retries` | no | Retry count on failure (default: `1`) |
| `tools` | no | List of tools to include in the workspace (see § Tools) |

Note: `variant` (opencode agent reasoning effort) is NOT in the frontmatter. It lives exclusively in the opencode.json config (global base or strategy overlay). This avoids a dual-source-of-truth where a frontmatter default could silently override an opencode.json value.

**Prompt body** follows the frontmatter. Must contain `{editorial_directive}` as a placeholder (same pattern as today). The code-generated `## Workspace` section is prepended automatically — strategy prompts do not include it. If the output channel's `prompt` field is empty, the placeholder is replaced with an empty string and a warning is logged.

### Tools

All tools — both built-in and user-defined — must be explicitly listed in the strategy's `tools` frontmatter. Only listed tools are written to the workspace. This makes each strategy self-documenting: you can see exactly what tools it uses by reading its frontmatter.

**Built-in tools** are compiled into the pail binary via `include_str!` from `src/opencode_tools/` (same location as today). Each built-in tool has a name that strategies reference:

| Name | Source files | Description |
|------|-------------|-------------|
| `fetch-article` | `src/opencode_tools/fetch-article.ts`, `src/opencode_tools/package.json` | Readability-based article extraction. Returns clean markdown instead of raw HTML. |

**User strategy tools** live in the strategy's `tools/` subdirectory and are referenced by relative path in frontmatter:

```yaml
tools:
  - fetch-article              # built-in, resolved from src/opencode_tools/
  - ./tools/my-custom-tool.ts  # user tool, relative to strategy dir
```

**Shared user tools** can be placed in a `shared_tools/` directory inside `strategies_dir` and referenced by relative path from any user strategy:

```
my-strategies/
  shared_tools/
    common-extractor.ts
  strategy-a/
    prompt.md              # tools: [fetch-article, ../shared_tools/common-extractor.ts]
  strategy-b/
    prompt.md              # tools: [../shared_tools/common-extractor.ts]
```

**npm dependencies:** Built-in tools ship with a `package.json` that declares their npm dependencies (e.g., `@mozilla/readability`, `jsdom`, `turndown`). When the workspace is prepared, all required `package.json` files are merged into a single `.opencode/package.json`. opencode auto-runs `bun install` on this file. User tools that need npm dependencies include a `package.json` in their `tools/` directory; its dependencies are merged into the same output file.

Note: `websearch`, `webfetch`, `read`, `write`, `edit`, `glob`, and `Task` are opencode built-in tools, not pail-managed tools. They are always available to the model regardless of the strategy's `tools` list. The `tools` frontmatter only controls which *pail-provided custom tools* are written to `.opencode/tools/` in the workspace.

### opencode.json Layering

The opencode project config written to each workspace is produced by merging two layers:

1. **Global base** — a default `opencode.json` compiled into the binary from `src/strategies/opencode.json`. Contains settings that apply to all strategies: `share`, `agent.build.variant`, provider settings, and other advanced opencode options.

2. **Strategy overlay** — the strategy's `opencode.json` (if present) is deep-merged on top. The agentic strategy uses this to define researcher and verifier subagent configs. The simple strategy may not need an overlay at all.

**Deep merge semantics:**
- **Objects** are merged recursively — strategy keys override base keys at the same path.
- **Arrays** are replaced wholesale (not appended).
- **`null` values** in the overlay delete the key from the result.
- **Type conflicts** (e.g., base has an object, overlay has a string at the same path): overlay wins, replacing the entire value.

```
Global base (src/strategies/opencode.json):
  {
    "share": "auto",
    "agent": { "build": { "variant": "high" } },
    ... other global settings ...
  }

Agentic overlay (src/strategies/agentic/opencode.json):
  {
    "agent": {
      "build": { "variant": "max" },
      "researcher": { "mode": "subagent", ... },
      "verifier": { "mode": "subagent", ... }
    }
  }

Result written to workspace:
  {
    "share": "auto",
    "agent": {
      "build": { "variant": "max" },              // overlay wins
      "researcher": { "mode": "subagent", ... },   // added by overlay
      "verifier": { "mode": "subagent", ... }       // added by overlay
    },
    ... other global settings ...
  }
```

### Storage

**Built-in strategies** are compiled into the binary. Strategy prompt files live in `src/strategies/`, the global base opencode.json lives alongside them, and built-in tools remain in `src/opencode_tools/`:

```
src/opencode_tools/             # built-in tools (unchanged from today)
  fetch-article.ts
  package.json

src/strategies/
  opencode.json                 # global base config (compiled in)
  simple/
    prompt.md
  agentic/
    prompt.md
    opencode.json               # overlay (adds subagent definitions)
  brief/
    prompt.md
```

Each file is embedded via explicit `include_str!` macros. The number of built-in strategy files is small and known at compile time, so no build-script or directory-embedding crate is needed.

**User-defined strategies** live in a directory specified by `[pail].strategies_dir`:

```toml
[pail]
strategies_dir = "./my-strategies"
```

```
my-strategies/
  shared_tools/                 # shared across user strategies
    common-extractor.ts
  deep-analysis/
    prompt.md
    opencode.json
    tools/
      custom-tool.ts
```

User strategies follow the exact same directory layout. Name collision with a built-in strategy is a **validation error** — user strategies cannot override built-ins.

### Config Changes

**Removed from `[opencode]`:**
- `system_prompt` — moved to strategy `prompt.md`
- `timeout` — moved to strategy frontmatter
- `max_retries` — moved to strategy frontmatter
- `project_config` — replaced by compiled-in global base + strategy overlay

**Remaining `[opencode]`:**

```toml
[opencode]
binary = "opencode"
default_model = "opencode/glm-5-free"
```

**New fields:**

```toml
[pail]
default_strategy = "simple"             # used when output_channel doesn't specify one
strategies_dir = "./my-strategies"       # optional: path to user-defined strategies

[[output_channel]]
strategy = "agentic"                    # override the default strategy for this channel
```

**Model stays separate.** The model is NOT part of the strategy — it remains as `[opencode].default_model` (global) and optional `model = "..."` per output channel. Models change frequently (new releases, provider changes, cost shifts), while strategies are stable configurations. Separating them means you can switch models without touching strategy definitions.

**Model-strategy compatibility** is a known limitation: nothing prevents pairing a complex agentic strategy with a weak model that can't handle subagent dispatch. This is intentional — strategies are never restricted to specific models. Users learn which combinations work through benchmarks and experience.

### Output Channel Resolution

When generating for an output channel, pail resolves the full configuration:

1. **Strategy**: `output_channel.strategy` -> `pail.default_strategy` -> `"simple"`
2. **Model**: `output_channel.model` -> `opencode.default_model` -> `"opencode/glm-5-free"`
3. **Timeout**: strategy frontmatter `timeout` -> `30m`
4. **Max retries**: strategy frontmatter `max_retries` -> `1`
5. **Editorial directive**: `output_channel.prompt` (inserted into `{editorial_directive}`)
6. **opencode.json**: global base merged with strategy overlay

### CLI Commands

New `pail strategy` subcommand group for introspection and validation:

```bash
# List all available strategies (built-in + user-defined)
pail strategy list

# Show a strategy's resolved config: prompt preview, merged opencode.json, tool list
pail strategy show <name>

# Validate a user strategy directory
pail strategy validate <path>
```

`pail strategy list` shows each strategy's name, description, source (built-in / user), timeout, and tool count. `pail strategy show` renders the full resolved state: the merged opencode.json (base + overlay), the prompt with a placeholder editorial directive, and which tools will be written to the workspace.

Additionally, `pail generate` and `pail interactive` accept a `--strategy <name>` flag to override the channel/default strategy for a single run. `pail benchmark run` also accepts `--strategy`.

### Built-in Strategies

#### `simple`

Basic generation without subagents. The model reads sources, fetches full articles directly via `fetch_article`, writes the digest, then re-reads and fixes obvious link issues. Works with any model including free-tier.

- **Tools**: `fetch-article`
- **Timeout**: `30m`
- **No opencode.json overlay** (no subagents needed; inherits global base variant)

The prompt covers the same editorial rules (condensation, attribution, output format, writing style, editor's notes) but instructs the model to do everything inline rather than delegating to subagents. Key differences from the agentic prompt:
- Fetch articles directly with `fetch_article` instead of dispatching researcher Tasks
- Perform inline fact-checking via `websearch` instead of relying on researcher briefs
- Self-verify links after writing instead of dispatching a verifier Task
- No reference library building step (too complex for simple models)
- Simplified editor's notes guidance (check surprising claims, but no exhaustive verification mandate)

#### `agentic`

Full research pipeline with subagent orchestration. The current production system prompt. Requires capable models that can reliably dispatch Tasks and coordinate multi-step workflows.

- **Tools**: `fetch-article`
- **Timeout**: `30m`
- **opencode.json overlay**: sets `agent.build.variant: "max"`, defines researcher and verifier subagent configs with permission restrictions

The prompt includes researcher dispatch (parallel batches), reference library building, verifier dispatch, and a final quality pass. The detailed prompt is the current `config.example.toml` system_prompt content.

#### `brief`

Concise digest strategy optimized for quick reading. Same workflow as `simple` (no subagents) but with a prompt focused on extreme condensation — each article gets 1-3 sentences maximum, organized as a bullet-point briefing rather than prose sections.

- **Tools**: `fetch-article`
- **Timeout**: `30m`
- **No opencode.json overlay**

This strategy directly addresses the [Shorter Digests](../ideas/shorter-digests.md) idea. The prompt prioritizes:
- Bullet-point format over prose
- One key takeaway per article
- No editor's notes (brevity over depth)
- Skipped section still required (accountability for what was dropped)

### Data Model Changes

Add `strategy_used` field to the `generated_article` table:

```sql
ALTER TABLE generated_articles ADD COLUMN strategy_used TEXT NOT NULL DEFAULT 'legacy';
```

Historical articles (pre-migration) get `'legacy'`. New articles store the strategy name used for generation. This enables:
- Correlating article quality with strategy in the benchmark judge
- Filtering/querying articles by strategy
- Displaying strategy metadata in the Atom feed

### Atom Feed Metadata

The strategy name is surfaced in the Atom feed as a `<generator>` element attribute or similar metadata, so feed readers can display which strategy produced each article (e.g., "pail (agentic)"). The `strategy_used` value from the `generated_article` record is used.

### Benchmark Integration

`pail benchmark run` gains a `--strategy` flag to test strategy-model pairs:

```bash
# Test simple strategy with free models
pail benchmark run --since 3d --strategy simple --models opencode/glm-5-free,opencode/big-pickle

# Test agentic strategy with paid models
pail benchmark run --since 3d --strategy agentic --models anthropic/claude-sonnet-4-6

# Compare same model across strategies
pail benchmark run --since 3d --strategy simple --models anthropic/claude-sonnet-4-6
pail benchmark run --since 3d --strategy agentic --models anthropic/claude-sonnet-4-6
```

If `--strategy` is not provided, the benchmark uses the strategy configured on the selected channel (same as `pail generate`). The strategy name is recorded in each sample's `meta.json` and in the run summary.

Strategy-model pair benchmarking (multiple strategies x multiple models in one run) is a future enhancement — see [Agentic Benchmark spec](agentic-benchmark.md) for planned updates.

### Interactive Mode

`pail interactive` respects the channel's strategy. The strategy's opencode.json and tools are written to the workspace. The workspace context is written as `AGENTS.md` (same as today). The model launches in TUI mode with the strategy's configuration active.

### Migration

This is a **breaking config change**. The migration path:

1. `[opencode].system_prompt` is removed — users who customized it must create a user-defined strategy with their prompt.
2. `[opencode].timeout` and `[opencode].max_retries` are removed — defaults come from the strategy.
3. `[opencode.project_config]` is removed — base config is now compiled-in, strategy-specific config is in the strategy overlay.
4. `[[output_channel]]` gains an optional `strategy` field.
5. `[pail]` gains `default_strategy` and optional `strategies_dir`.

`config.example.toml` is updated to reflect the new structure. The system prompt moves from the config file to the strategy's `prompt.md`.

### Workspace Changes

The workspace preparation flow becomes:

1. Resolve strategy (channel -> default -> "simple")
2. Load strategy files (from built-in or user dir)
3. Parse `prompt.md` frontmatter for execution params
4. Deep-merge global base opencode.json + strategy overlay opencode.json
5. Write `opencode.json` to workspace
6. Collect tools: resolve built-in names from `src/opencode_tools/`, resolve user tool paths from strategy dir, merge all `package.json` dependencies into one
7. Write tools to `.opencode/tools/`, write merged `package.json` to `.opencode/package.json`
8. Write `manifest.json`, `sources/`, `output.md` (unchanged)
9. Generate workspace context dynamically (listing which tools are available based on the strategy's tool list, not hardcoded)
10. Prepend workspace context to prompt body, replace `{editorial_directive}`
11. Write `prompt.md` to workspace (for debugging/inspection)

### Validation

On startup (and `pail config validate`):
- Every referenced strategy name (channel or default) must resolve to a built-in or user strategy
- Strategy `prompt.md` must exist and contain `{editorial_directive}`
- Strategy frontmatter must have `format_version: 1`
- Strategy names must be unique (no collision between built-in and user)
- `strategies_dir` (if set) must be a valid directory path; subdirectories without a `prompt.md` are silently ignored (not every subdir needs to be a strategy)
- Frontmatter `timeout` (if set) must be a valid duration
- All tool references must resolve: built-in names must match a known tool, relative paths must point to existing files
- `shared_tools/` entries referenced from strategies must exist

### Affected Specs

When implemented, the following spec files need updating:

| Spec | What changes |
|------|-------------|
| [Generation Engine](generation-engine.md) | Workspace preparation (strategy resolution, tool writing), system prompt template section, opencode config section, context management. Remove references to `[opencode].system_prompt`. |
| [Config](config.md) | `[opencode]` section shrinks. New `[pail]` fields. New `strategy` field on output_channel. Validation rules. |
| [CLI](cli.md) | New `pail strategy` subcommand group (list, show, validate). `--strategy` flag on benchmark. |
| [Agentic Benchmark](agentic-benchmark.md) | `--strategy` flag, strategy in meta.json, strategy-model pair testing. |
| [Interactive Mode](interactive-mode.md) | Strategy-aware workspace preparation. |
| [Docker](docker.md) | Config example updates (no `[opencode].system_prompt`). |

### Future Enhancements

- **Strategy-specific post-processing:** strategies could declare pail-side post-processing steps (e.g., a link validation pass that runs after generation but before parsing). Currently all post-processing is done by the LLM. A future `post_process` field in frontmatter or a post-process script could enable cheaper, deterministic post-processing for simpler strategies.

- **Strategy inheritance:** an `extends: agentic` mechanism for user strategies that want to customize just one aspect of a built-in (e.g., swap the prompt but keep the opencode.json and tools). Currently users must copy all files. Inheritance could reduce maintenance burden as built-in strategies evolve.

- **Multi-strategy benchmarking:** test multiple (strategy, model) pairs in a single `pail benchmark run` invocation, possibly via a TOML benchmark config file. Currently requires separate runs per strategy.

## Decisions

- **Naming:** strategy (generation strategy).
  Options: pipeline / profile / preset / strategy.
  Rationale: "pipeline" is already used in the codebase (`pipeline.rs`, `run_generation`). "Strategy" emphasizes the different approach each takes and avoids ambiguity.

- **Scope of what varies per strategy:** everything except model — system prompt, opencode config, tools, timeout, max_retries.
  Options: prompt + config + tools only / everything per strategy / core + optional overrides.
  Rationale: strategies represent fundamentally different generation approaches. A simple strategy needs a shorter timeout than an agentic one. Model is excluded because model choice changes frequently and independently of strategy design.

- **Variant not in frontmatter:** `variant` (agent reasoning effort) lives exclusively in opencode.json (global base or strategy overlay), not in the prompt.md frontmatter.
  Options: frontmatter only / opencode.json only / both with precedence rule.
  Rationale: having variant in both frontmatter and opencode.json creates a dual-source-of-truth where a frontmatter default can silently override an intentional overlay value. Keeping it in one place (opencode.json) eliminates this ambiguity.

- **Storage:** built-in strategies compiled into binary + user strategies from `strategies_dir`.
  Options: all in config.toml / separate files on disk / compiled-in / compiled-in + user dir.
  Rationale: compiled-in ensures built-in strategies are always available without external files. User dir enables customization without forking the binary.

- **Built-in tool storage:** tools stay in `src/opencode_tools/` (unchanged from today), separate from strategy directories.
  Options: inside strategy dirs / separate `src/opencode_tools/` / `src/tools/`.
  Rationale: built-in tools are shared across multiple strategies. Placing them inside one strategy's directory (e.g., agentic) is architecturally misleading. The current `src/opencode_tools/` location works fine.

- **Tool inclusion:** all tools (built-in and user) must be explicitly listed in the strategy's `tools` frontmatter.
  Options: everything explicit / user tools auto-included / built-ins always available.
  Rationale: explicit listing makes the strategy self-documenting. Consistent inclusion mechanism for both built-in and user tools avoids confusion.

- **Shared user tools:** `shared_tools/` directory inside `strategies_dir` for cross-strategy tool reuse.
  Options: allow shared dir / require duplication / skip.
  Rationale: if two user strategies both need the same custom tool, duplicating it creates maintenance burden and divergence risk. A shared directory is simple to implement and avoids this.

- **npm dependency merging:** all tool `package.json` files are merged into a single `.opencode/package.json` in the workspace.
  Options: merge / user replaces built-in / each tool installs independently.
  Rationale: opencode expects a single `.opencode/package.json`. Merging ensures all tools' dependencies are available after `bun install`.

- **Directory layout:** prompt.md + optional opencode.json + optional tools/.
  Options: single file / two files / three items (with tools/).
  Rationale: separating the opencode overlay from the prompt keeps both readable. A tools/ dir allows strategy-specific tools without polluting the global tool set.

- **opencode.json layering:** global compiled-in base, then strategy overlay via deep merge. Overlay wins, null deletes, arrays replaced.
  Options: global base + strategy overlay / per-strategy complete files / config.toml base + overlay.
  Rationale: a global base avoids duplicating shared settings (share, auth, provider config) across every strategy. Deep merge lets strategies add or override just what they need. Explicit null-deletes and type-conflict rules prevent ambiguous merge behavior.

- **Prompt template:** keep `{editorial_directive}` placeholder pattern. Empty editorial directives are allowed with a warning.
  Options: keep {editorial_directive} / more placeholders / no placeholders / require non-empty.
  Rationale: the pattern works well. Some strategies may not need per-channel customization, so allowing empty directives (with a warning) provides flexibility.

- **Config migration:** remove `[opencode].system_prompt`, `timeout`, `max_retries`, and `project_config` entirely. No version bump or migration tool.
  Options: remove entirely / version bump + migration error / deprecated fallback / migrate command.
  Rationale: clean break. The config.example.toml update serves as the migration guide. The project is pre-1.0 and the user base is small enough that a simple config update is sufficient.

- **User strategy name collision:** validation error (no override of built-ins).
  Options: error / user wins / explicit override flag.
  Rationale: overriding built-ins silently would be confusing. An explicit error forces users to pick unique names.

- **Workspace context:** continues to be code-generated and prepended to the strategy prompt. Now dynamic — lists available tools based on the strategy's tool list instead of hardcoding `fetch-article`.
  Options: code-generated / part of strategy prompt / code-generated but overridable.
  Rationale: the workspace layout is defined by pail's code, not by the strategy. Dynamic generation ensures the workspace description matches the actual workspace contents.

- **Default strategy:** configurable via `[pail].default_strategy`, set to `"simple"` in config.example.toml.
  Options: require explicit / fall back to simple / configurable default.
  Rationale: a sensible default (simple) means minimal config for new users. Configurable so users can set their preferred default once rather than on every channel.

- **Model excluded from strategy:** model stays in `[opencode].default_model` + per-channel override.
  Options: include model in strategy / keep separate.
  Rationale: model choice is orthogonal to strategy design. Users switch models frequently (cost, availability, new releases). A strategy should describe *how* to generate, not *which model* to use.

- **Strategy tracking in DB:** `strategy_used` column added to `generated_article` table. Historical articles default to `'legacy'`.
  Options: add field / skip / only in meta.json.
  Rationale: correlating article quality with strategy is essential for benchmarks, analytics, and feed metadata. DB storage enables both production tracking and benchmark correlation.

- **Strategy CLI commands:** `pail strategy list`, `pail strategy show`, `pail strategy validate`.
  Options: all three / just list / defer.
  Rationale: introspection commands dramatically reduce the feedback loop for strategy authoring. `validate` catches structural issues without requiring a full generation run. Low implementation effort relative to value.

- **Format versioning:** `format_version: 1` required in strategy frontmatter.
  Options: no version / version in frontmatter / separate marker file.
  Rationale: enables clear migration guidance when the frontmatter schema evolves. Same principle as `[pail].version` for the config file.

- **Benchmark integration:** `--strategy` flag on `pail benchmark run`. Strategy recorded in `meta.json`.
  Options: --strategy flag / multi-strategy benchmark / defer.
  Rationale: strategy-model pair testing is essential for evaluating which combinations work. Multi-strategy-in-one-run is a future enhancement.

- **Simple strategy scope:** read + fetch articles + write + minimal self-verification.
  Options: read + write only / read + fetch + write / read + fetch + minimal verify + write.
  Rationale: read-only produces poor results for RSS (only summaries available). Fetching articles directly is feasible for simple models. A self-review step to fix broken links adds little complexity but catches common issues.

- **Brief strategy:** included as a third built-in strategy, implementing the Shorter Digests idea.
  Options: defer / include now / user-defined only.
  Rationale: directly enables the Shorter Digests idea with no additional effort. Bullet-point format, no editor's notes, extreme condensation. Validates the strategy system works for different output styles.

- **Feed metadata:** strategy name surfaced in Atom feed (e.g., `<generator>` element).
  Options: include / skip.
  Rationale: low effort, helps readers understand quality expectations when multiple channels use different strategies.

- **Model-strategy compatibility:** no restrictions. Documented as a known limitation.
  Options: restrict / warn / no restrictions.
  Rationale: strategies should never be restricted to specific models. Users learn which combinations work through benchmarks and experience. The documentation notes this as a design choice.

- **Effort:** Large.
  Options: Medium / Large.
  Rationale: requires config restructure, generate.rs refactor, strategy loading/embedding, three prompt designs, benchmark integration, CLI commands, DB migration, and testing. Blocks benchmark progress and other features.

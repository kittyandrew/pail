# Interactive Mode

Launch an interactive opencode TUI session with collected source data available in the workspace, for ad-hoc questions and exploration instead of batch article generation.

## Usage

```bash
pail interactive <slug> --since 1d
```

Same flags as `generate` for time window (`--since`, `--from`/`--to`).

## Implementation

1. Same pipeline as `generate` up to workspace preparation: fetch RSS, fetch TG history, write source files + manifest to `/tmp/pail-gen-<uuid>/`
2. Instead of `opencode run` (non-interactive), launch `opencode` (TUI mode) with CWD set to the workspace
3. User interacts directly with the opencode TUI — full tool access, file reading, web search, etc.
4. On exit, clean up the workspace

No article is parsed or stored. No `last_generated` update. This is purely exploratory.

## Prompt

The current system prompt is a mix of workspace context (describing the file layout, source formats, manifest structure) and generation instructions (write an article to output.md, follow editorial directive, etc.). For interactive mode, only the context portion is relevant.

**Open question:** should the system prompt be split into:
- A **context prompt** (workspace layout, source file format, manifest schema) — used by both interactive and generate modes
- An **action prompt** (write article, follow editorial directive, output format rules) — used only by generate mode

This would let interactive mode launch with just the context prompt (or no prompt at all — opencode can read the files itself). Needs prototyping to see if the AI benefits from the context prompt or if it's fine just exploring the workspace.

## Decisions

- **Approach:** launch opencode TUI (not a custom REPL).
  Options: opencode TUI / custom REPL / both.
  Rationale: simplest implementation, full opencode features. pail just builds the workspace and hands off.

- **Prompt for interactive mode:** no editorial/generation prompt; possibly just workspace context, or nothing.
  Options: full editorial prompt / stripped assistant prompt / context-only prompt / no prompt.
  Rationale: the user is driving the session — they don't need article generation instructions. Whether a context prompt (describing the workspace layout) helps or is redundant needs prototyping.

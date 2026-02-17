---
name: review-article
description: Fetch (or generate) a pail article and review it for quality and system prompt compliance.
disable-model-invocation: false
allowed-tools: Read, Glob, Grep, Bash(command:curl *), Bash(command:cargo *), Bash(command:grep *), Bash(command:wc *), Bash(command:sqlite3 *), Bash(command:opencode *), Bash(command:nix-shell *), Bash(command:gh *), WebFetch, WebSearch, AskUserQuestion
argument-hint: "[feed-slug, article-url, or empty for interactive]"
---

# Review Article Quality

You are reviewing a generated pail digest article for quality and compliance with the editorial rules.

## Phase 1: Build the Checklist

1. **Load the full project spec** — follow CLAUDE.md to locate and read the spec.
2. **Load the system prompt** — read `config.example.toml` and extract the full `system_prompt` value.
3. **Derive review criteria** from the spec and system prompt. Read them carefully and generate a comprehensive checklist of every quality rule, editorial requirement, and formatting constraint the article must satisfy. Do not rely on a hardcoded list — the spec and prompt evolve.

## Phase 2: Get the Article

- If `$ARGUMENTS` is a full URL (starts with `http`), fetch it with `curl -s` and save to `/tmp/pail-review-article.html`.
- If `$ARGUMENTS` is a feed slug (e.g., `tg-news`), read the feed token from `config.toml` (`grep '^feed_token' config.toml`), fetch the feed from the deployed instance, extract the latest article's `<link rel="alternate">` URL, then fetch that.
- If `$ARGUMENTS` is `ci`, review the latest article from the CI generate workflow:
  1. Find the latest run: `gh run list --workflow=generate.yml --limit 1 --json databaseId,status,conclusion`
  2. If still running, poll until complete: `gh run view <id> --json status,conclusion`
  3. Download the artifact: `gh run download <id> --name tech-digest-article --dir /tmp/pail-ci-article`
  4. Read `/tmp/pail-ci-article/article.md`
- If no argument is provided, ask the user interactively what they want to review. Read `config.toml` to find available output channel slugs (`grep '^slug' config.toml`) and offer these options:
  - Each available feed slug (fetch the latest article from the deployed instance)
  - Review latest CI article (same as `ci` argument above)
  - Generate a new article (ask which slug and what `--since` duration, then run `cargo run --release -- generate`)
  - Provide a URL manually

## Phase 3: Review

Evaluate the article against every criterion from Phase 1. Present findings as a numbered list with severity:
- **ISSUE** — Violates a rule from the spec or system prompt. Quote the offending passage.
- **SUGGESTION** — Could be better but not a rule violation.
- **NOTE** — Observation for awareness.

**Verification requirement:** Before flagging any factual claim in the article as wrong, you MUST
verify it via web search or another reliable method. Your training data has a knowledge cutoff and
real-world positions, names, and facts change. Never assume something is a factual error based on
your prior knowledge alone — always check first. If you cannot verify, say "unverified" rather than
"wrong."

End with a summary table rating each review area as Good / Weak / Non-compliant.

## Phase 4: Session Analysis (optional)

Investigate the opencode session to understand *why* the model made specific choices —
especially any issues found in Phase 3.

### Finding the session

1. The article file (markdown or HTML) should have an `[opencode session](https://opncd.ai/share/...)` link at the bottom. Extract the share ID.
2. Alternatively, query the DB for the generation log:
   ```bash
   sqlite3 /path/to/pail.db "SELECT generation_log FROM articles ORDER BY generated_at DESC LIMIT 1"
   ```
   Then extract the `https://opncd.ai/share/...` URL from the log.

### Exporting the session

Use `opencode export` to get the full session data as JSON (much richer than the share page):

```bash
cd /tmp && opencode export <session-id> > /tmp/pail-session-export.json
```

Find the session ID from the share URL or from `opencode session list`.

### What to analyze

Use `jq` (via `nix-shell -p jq --run "..."`) to extract:

- **Reasoning blocks**: How did the model plan? Did it identify all topics correctly?
  ```bash
  jq '.messages[] | select(.info.role=="assistant") | .parts[] | select(.type=="reasoning") | .text' export.json
  ```
- **Tool usage**: What tools did the model use? Did it verify URLs? Did it fetch external sources?
  ```bash
  jq '.messages[] | select(.info.role=="assistant") | .parts[] | select(.type=="tool-invocation") | {tool: .toolName, input: .input}' export.json
  ```
- **Token counts**: How many tokens were used per step?
  ```bash
  jq '.messages[] | select(.info.role=="assistant") | .info.tokens' export.json
  ```

Note: The `tokens.reasoning` field may show `0` even when thinking is active — this is an opencode
reporting bug. Check for actual `reasoning` parts in the message parts array instead.

### Report

Add a "Session Analysis" section to the review output summarizing:
- Whether extended thinking was active (check for reasoning parts, not token counts)
- Key decisions the model made and whether they were well-reasoned
- Any missed verification opportunities (URLs not checked, claims not fact-checked)
- Tool usage patterns (did it use WebFetch to verify external links?)

## Dataset

Known test time windows for reproducible before/after generation. Use with `--from`/`--to` flags
to regenerate articles from the same content and compare outputs across system prompt changes.

| Slug | From | To | Items |
|---|---|---|---|
| `tg-news` | `2026-02-15T21:37:32Z` | `2026-02-16T06:34:09Z` | 70 |

```bash
cargo run --release -- generate <slug> --from <from> --to <to> --output /tmp/pail-test-before.md
# make system prompt changes
cargo run --release -- generate <slug> --from <from> --to <to> --output /tmp/pail-test-after.md
```

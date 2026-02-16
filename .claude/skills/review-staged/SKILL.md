---
name: review-staged
description: Review uncommitted changes against the project spec for correctness, quality, and completeness.
disable-model-invocation: true  # user must type /review-staged explicitly
allowed-tools: Read, Glob, Grep, Bash(command:git *), Bash(command:cargo *), Edit, Write, AskUserQuestion, TaskCreate, TaskUpdate, TaskList, TaskGet
---

# Review Staged Changes

You are performing a thorough review of all uncommitted changes against the project spec. This is a two-phase process: **Review** then **Fix**. Run both phases in a single pass.

## Setup

1. **Load the full project spec** — follow the instructions in CLAUDE.md to locate and read the spec. This is non-negotiable before any review work.
2. **Gather the diff** — run `git diff` (unstaged) and `git diff --cached` (staged). Review ALL uncommitted changes together.
3. **Identify changed files** — run `git status`. Read each modified file in full to understand context around the diff hunks.

## Phase 1: Review

Produce a thorough written assessment covering:

- **Spec conformance** — Does the implementation match the spec? Are there undocumented deviations? Are there spec sections that should have been updated but weren't?
- **Completeness** — Are there gaps in the implementation, missing edge cases, or half-finished features?
- **Code quality** — Is the code clear, idiomatic, and not unnecessarily complex? Are there simpler ways to achieve the same result?
- **Security** — Any injection vectors, auth bypasses, information leaks, or OWASP top-10 issues?
- **Correctness** — Logic errors, off-by-one, race conditions, error handling gaps?
- **Documentation gaps** — Do spec sections, code comments, or config examples need updating to reflect the changes?

**Do NOT flag:**
- Dead code warnings for fields/functions that are clearly needed by upcoming implementation phases (check the spec phase list before reporting).
- Style nitpicks that linters and formatters would catch — just run those instead.

Present the assessment as a numbered list of findings, each with a severity:
- **ISSUE** — Must fix before committing.
- **SUGGESTION** — Worth considering, but not blocking.
- **NOTE** — Observation for awareness, no action needed.

## Phase 2: Fix

After presenting the review:

1. **For findings that require a decision** (multiple valid approaches, ambiguous requirements, trade-offs) — ask the user interactively using AskUserQuestion. Even simple decisions should be confirmed rather than assumed.
2. **For findings with a clear fix** (typos, missing escaping, spec text out of sync, obvious bugs) — fix them directly. Do NOT stage anything (`git add`). Just edit the files.
3. **After all fixes are applied** — update the spec and any other documentation to reflect implementation changes. The spec should always match the code.
4. **Run the project's lint and build checks** to verify fixes compile clean.
5. **Summarize** what was fixed and what decisions were made.

## Rules

- Never stage or commit. Only edit files.
- Follow all project-specific rules from CLAUDE.md (secrets, git workflow, code style, etc.).

# CLAUDE.md

## What This Is

**pail** (Personal AI Lurker) — a self-hosted Rust service that monitors RSS feeds and Telegram channels, generates AI digest articles via opencode, and publishes them as Atom feeds.

## Full Spec

The complete PRD lives in the kittyos repo (not this repo):
```
~/dev/kittyos/.notes/personal/pail/pail-prd.md
```

**Always load the PRD into context** at the start of a session (or after context compaction) before making architectural decisions, adding features, or planning implementation work. If it's not in context, read it first.

## CI / Linting

```bash
# Format Nix files
alejandra -c .

# Format Rust code (max_width = 121, see rustfmt.toml)
cargo fmt --check

# Lint
cargo clippy

# Test
cargo test
```

## Dev Environment

```bash
nix develop   # enters shell with Rust toolchain, openssl, sqlite, opencode
```

## Spec Deviations

When the implementation intentionally diverges from the PRD, **update the spec inline** in the relevant section with the rationale — don't append to a separate section. The spec should always reflect the actual implementation. Never leave undocumented differences between spec and code.

## Review Discipline

When reviewing code or auditing the project, **verify claims before reporting them.** Don't assume something is missing based on indirect evidence (e.g., git status snapshots). Check the filesystem directly — glob for files, read them, confirm they exist or don't — before listing an issue.

## Git Workflow

- **Never stage or commit without explicit user confirmation.** After making changes, show the diff and wait for the user to approve before running `git add` or `git commit`.
- **Never push to remote** unless the user explicitly asks in that moment. A one-time push approval does not carry forward to future pushes.
- **Exception: `.claude/CLAUDE.md`** — always auto-stage this file after editing it.
- **Never run git commands in the kittyos repo** (`~/dev/kittyos/`). You may edit files there (e.g., the PRD), but never stage, commit, or otherwise touch git in that repo.

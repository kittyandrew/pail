# CLAUDE.md

## What This Is

**pail** (Personal AI Lurker) — a self-hosted Rust service that monitors RSS feeds and Telegram channels, generates AI digest articles via opencode, and publishes them as Atom feeds.

## Docs

All specs and design docs live in `docs/` within this repo:
- **`docs/core.md`** — architecture, data model, technical decisions
- **`docs/specs/`** — implemented feature specs (RSS, Telegram, generation engine, Atom feed, daemon, CLI, config, Docker)
- **`docs/ideas/`** — not-yet-implemented feature ideas and designs

See `docs/README.md` for the full index with status.

**Before writing implementation code**, load the relevant spec from `docs/specs/` and `docs/core.md` into context. For new features, check `docs/ideas/` for existing design notes.

## CI / Linting

All checks run in GitHub Actions CI (`.github/workflows/ci.yml`) on push to `main` and on PRs. Run locally via `nix develop`:

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

When the implementation intentionally diverges from a spec, **update the spec inline** in the relevant `docs/specs/` file with the rationale — don't append to a separate section. The spec should always reflect the actual implementation. Never leave undocumented differences between spec and code.

## Decision Log

Every spec file (`docs/specs/`) and idea file (`docs/ideas/`) must have a `## Decisions` section at the bottom. Log **every** decision there — whether you made it yourself (even if obvious), or asked the user interactively. Always include the options that were considered, not just the outcome. Format:

```markdown
## Decisions

- **<topic>:** <chosen option>.
  Options: <option A> / <option B> / <option C>.
  Rationale: <why this was chosen>.
```

## Review Discipline

When reviewing code or auditing the project, **verify claims before reporting them.** Don't assume something is missing based on indirect evidence (e.g., git status snapshots). Check the filesystem directly — glob for files, read them, confirm they exist or don't — before listing an issue.

## Code Style

- **No imports inside functions or mid-file.** All `use` statements go at the top of the file.

## Git Workflow

- **Never stage or commit without explicit user confirmation.** After making changes, show the diff and wait for the user to approve before running `git add` or `git commit`.
- **Never push to remote** unless the user explicitly asks in that moment. A one-time push approval does not carry forward to future pushes.
- **Exception: `.claude/CLAUDE.md`** — always auto-stage this file after editing it.
- **Never run git commands in the kittyos/dotfiles repo** (`~/dev/kittyos/` or `~/dev/dotfiles/`). You may edit files there, but never stage, commit, or otherwise touch git in those repos.

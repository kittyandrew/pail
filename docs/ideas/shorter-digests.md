# Shorter Digests

Not a configurable feature — this is a prompt engineering task.

Review and improve the system prompt template (`config.example.toml`) and editorial directive patterns to ensure the AI actually respects editorial requests for brevity. If a channel's editorial directive says "be concise" or "shorter summaries," the system prompt should not fight against that with instructions that encourage verbosity.

## What to Check

- Does the system prompt's "Condensation and Fidelity" section inadvertently encourage long output when the editorial directive wants brevity?
- Are there conflicting instructions (e.g., "write thorough sections" vs user saying "less text more substance")?
- Should the system prompt explicitly say "respect the editorial directive's length/verbosity preferences"?

## Decisions

- **Approach:** prompt engineering review, not a config option.
  Options: add `style = "brief"` config / edit system prompt / both.
  Rationale: this is about making the system prompt cooperate with editorial directives that request brevity, not adding a new configuration dimension.

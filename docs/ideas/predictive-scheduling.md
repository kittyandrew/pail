# Predictive Scheduling

Generation can take minutes to tens of minutes. With naive scheduling, a channel configured for `at:08:00` will always deliver its digest *after* 08:00.

## Self-Calibrating Early Start

An optional per-channel setting that uses historical generation durations to start generation early so the article is ready closer to the scheduled time:

- Track `generation_duration_ms` on each `generated_article` (wall-clock time from pipeline start to article stored)
- Compute a rolling estimate (e.g., exponential moving average or p90 of last 10 runs) per output channel
- If the estimate is <= 30 minutes, start generation that many minutes before the scheduled tick
- If the estimate exceeds 30 minutes, do not apply early start (the content window would miss too many late-posting sources)
- The content window's `covers_to` is still the scheduled tick time, not the early-start time — so last-minute content is included if already fetched
- Disabled by default. Enabled per channel via config: `early_start = true`

This is a heuristic, not a guarantee — generation times vary with source volume, model latency, and retry behavior. The goal is "usually on time" rather than "always exactly on time."

## Decisions

No decisions made yet.

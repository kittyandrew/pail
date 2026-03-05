# Observability

pail uses [Sentry](https://sentry.io) for error tracking, performance monitoring, and breadcrumb trails.

## Setup

### Local Development

`.env` in the project root (gitignored):
```
SENTRY_DSN=https://...@....ingest.de.sentry.io/...
SENTRY_ENVIRONMENT=development
```

Without `SENTRY_DSN`, Sentry is a no-op — the guard is created but nothing is sent.

### Production (Docker)

`SENTRY_DSN`, `SENTRY_ENVIRONMENT=production`, and `GIT_SHA` are baked into the Docker image at build time via `builtins.getEnv` in `flake.nix`. The CI workflow (`.github/workflows/docker.yml`) passes these from GitHub secrets/context and builds with `nix build .#docker --impure`.

Users can override the baked-in values via `docker-compose.yml` environment variables if needed (e.g., to point at a different Sentry project or disable reporting by setting an empty DSN).

### CI

The Docker workflow injects two env vars into the `nix build --impure` step:
- `SENTRY_DSN` — from the `SENTRY_DSN` GitHub repository secret
- `GIT_SHA` — from `${{ github.sha }}`, used for Sentry release tracking

### Local Nix Build

`nix build .#docker` (without `--impure`) produces an image without Sentry — `builtins.getEnv` returns `""` in pure mode, so the DSN env var is omitted entirely. To build locally with Sentry:
```bash
SENTRY_DSN="https://..." GIT_SHA=$(git rev-parse HEAD) nix build .#docker --impure
```

## Architecture

### Sentry Init (`src/main.rs`)

Sentry is initialized **before** the tracing subscriber so the `sentry-tracing` layer can capture all events:

```rust
let _sentry_guard = sentry::init((
    std::env::var("SENTRY_DSN").ok(),
    sentry::ClientOptions {
        traces_sample_rate: 1.0,
        environment: Some(std::env::var("SENTRY_ENVIRONMENT")...),
        release: std::env::var("GIT_SHA").ok().map(Into::into),
        ..Default::default()
    },
));
```

The `_sentry_guard` must live for the entire `main()` — dropping it flushes pending events.

### Tracing Integration (`sentry-tracing`)

The tracing subscriber uses a layered setup:
- `tracing_subscriber::fmt::layer()` — console output
- `sentry::integrations::tracing::layer()` — captures tracing events as Sentry breadcrumbs/events

`tracing::error!()` calls create Sentry events (issues). `tracing::info!()` and `tracing::warn!()` create breadcrumbs attached to the current scope.

### HTTP Layer (`sentry-tower`)

Two tower layers wrap the axum router in `src/server.rs`:
- `NewSentryLayer` — binds a new Sentry hub per request (isolation)
- `SentryHttpLayer::with_transaction()` — creates performance transactions from HTTP requests

Layer order matters — `SentryHttpLayer` must come before `NewSentryLayer` in axum's `.layer()` calls (axum applies layers in reverse order).

## Adding Observability to New Code

### Automatic (just use tracing)

Any code using `tracing::error!()`, `tracing::warn!()`, or `tracing::info!()` automatically flows into Sentry:
- **Errors** become Sentry issues (grouped, alertable)
- **Warnings/info** become breadcrumbs (context on the timeline leading up to errors)

### HTTP Endpoints

New axum routes are automatically instrumented — the tower layers handle transaction creation. No per-route setup needed.

## Alerts

An alert rule (ID `443940`) creates a GitHub issue on `kittyandrew/pail` with the `sentry` label whenever a new Sentry issue appears. This was configured via the Sentry API using the GitHub integration (ID `375252`).

The GitHub integration itself (connecting GitHub to Sentry) is a one-time manual setup in the Sentry web UI — the `org:integrations` scope is [not available for internal integration tokens](https://github.com/getsentry/sentry/issues/60072).

## Sentry Project Details

- **Region:** DE (`de.sentry.io`)
- **Organization:** `kittyandrew`
- **Project:** `pail`
- **GitHub integration ID:** `375252`

## Decisions

- **sentry-tower added as direct dependency:** The `sentry` crate's `tower` feature doesn't enable the `http` sub-feature on `sentry-tower`, which is required for `SentryHttpLayer`. Adding `sentry-tower` directly with `features = ["http"]` resolves this.
  Options: patch sentry's tower feature flags / add sentry-tower directly.
  Rationale: direct dependency is cleaner and doesn't require forking sentry.

- **traces_sample_rate = 1.0:** Sample all transactions. pail is low-traffic (personal service), so 100% sampling is fine. Reduce if volume grows.
  Options: 1.0 / 0.5 / 0.1.
  Rationale: full visibility for a personal service with minimal traffic.

- **Environment fallback to "development":** When `SENTRY_ENVIRONMENT` is unset, defaults to "development" rather than the hostname. Production gets `SENTRY_ENVIRONMENT=production` baked into the Docker image.
  Options: hostname fallback / "development" default / None.
  Rationale: unset env var means local dev, and "development" is the clearest signal.

- **`--impure` Docker build:** `builtins.getEnv` requires the `--impure` flag for `nix build`. This breaks Nix caching/reproducibility, but the Docker build is already tied to CI secrets which are inherently impure. Pure builds (without the flag) simply omit the Sentry env vars — Sentry becomes a no-op.
  Options: `--impure` with `builtins.getEnv` / CI overlay (thin Dockerfile layer on top) / runtime-only injection.
  Rationale: simplest approach, keeps the DSN out of source, CI is the only place that needs it.

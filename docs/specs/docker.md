# Docker Deployment

## Docker Image

Nix-defined via `pkgs.dockerTools.buildLayeredImage` in `flake.nix` (no Dockerfile). Build with `nix build .#docker && docker load < result`. Published to [DockerHub](https://hub.docker.com/r/kittyandrew/pail) on every push to `main` via CI.

- **Binary build:** Crane (`ipetkov/crane`) with fenix (`nix-community/fenix`) toolchain
- **Image contents:** pail binary + opencode (from flake input) + CA certificates (`pkgs.cacert`) + tini (init) + `/etc/passwd` and `/etc/group`
- **No base image:** scratch-based, only Nix store paths (no Alpine layer)
- **Non-root user:** Runs as `pail` (UID 1000, GID 1000) with `HOME=/home/pail`. `/etc/passwd` and `/etc/group` are injected via `pkgs.writeTextDir`.
- **Init process:** [tini](https://github.com/krallin/tini) runs as PID 1, forwarding signals to pail and reaping zombie child processes
- **Entrypoint:** `tini -- pail --config /etc/pail/config.toml` — subcommands work naturally with `docker compose run`
- Exposes port 8080
- `HOME=/home/pail` — required for opencode to find its auth/config dirs
- `SSL_CERT_FILE` — set to the Nix store cacert path for outbound HTTPS

**Pre-created directories (via `fakeRootCommands`):**
- `/tmp` with sticky bit (for generation workspaces)
- `/home/pail` with correct ownership (HOME)
- `/var/lib/pail` with correct ownership (data dir)
- `/home/pail/.local/share/opencode` and `/home/pail/.config/opencode` (opencode dirs)

**Build note:** grammers uses MTProto's own encryption over raw TCP — no TLS. reqwest uses rustls (no runtime openssl dependency). CA certificates are provided by `pkgs.cacert` for outbound HTTPS.

## Docker Compose

```yaml
version: "3"
services:
  pail:
    container_name: pail
    image: kittyandrew/pail:latest
    restart: always
    ports:
      - "8080:8080"
    volumes:
      - ./config.toml:/etc/pail/config.toml:ro
      - pail-data:/var/lib/pail
      - opencode-auth:/home/pail/.local/share/opencode
      - opencode-config:/home/pail/.config/opencode
    environment:
      - PAIL_DATA_DIR=/var/lib/pail
      # Option A: pass API keys as env vars (no opencode auth needed)
      # - OPENAI_API_KEY=${OPENAI_API_KEY}
      # - ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}

volumes:
  pail-data:
  opencode-auth:
  opencode-config:

# Join an external Docker network for reverse proxy (e.g., Caddy).
# Uncomment and remove the `ports` section above.
# networks:
#   default:
#     name: caddynet
#     external: true
```

`PAIL_DATA_DIR` env var overrides `data_dir` from the config file so the same `config.toml` works for both local dev (default `./data`) and Docker (`/var/lib/pail`).

Pin to a specific version or commit: `kittyandrew/pail:0.1.0` (semver), `kittyandrew/pail:sha-abc1234` (commit).

## opencode Authentication in Docker

**Option A: Environment variables (simplest)**
Pass `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc. as env vars in docker-compose.

**Option B: Interactive auth (OAuth, persistent config)**
```bash
# 1. Auth opencode interactively (writes to volumes)
docker compose run --rm -it --entrypoint opencode pail auth login

# 2. Start the service (picks up auth from volume)
docker compose up -d
```

The `opencode-auth` volume persists auth across container restarts. Only needs to be done once (or when tokens expire).

## CI

### Lint & Test (`.github/workflows/ci.yml`)

Runs on push to `main` and on PRs:
1. **Nix format** — `alejandra -c .`
2. **Rust format** — `cargo fmt --check`
3. **Clippy** — `cargo clippy`
4. **Tests** — `cargo test`

Uses `nix develop` (same flake devShell as local development). Nix store cached via `DeterminateSystems/magic-nix-cache-action`; Cargo `target/` cached via `actions/cache`.

### Docker Build & Publish (`.github/workflows/docker.yml`)

Runs on push to `main` only:
1. Build image via `nix build .#docker`
2. Load with `docker load < result`
3. Tag with semver (`0.1.0`), `latest`, and `sha-<short>` (commit pinning)
4. Push all tags to `kittyandrew/pail` on DockerHub

Requires `DOCKERHUB_USERNAME` and `DOCKERHUB_TOKEN` secrets.

### Generate Test (`.github/workflows/generate.yml`)

Runs on push to `main` and on PRs:
1. Build pail, run `pail generate` with `config.example.toml` against a free model
2. Upload the generated article as a CI artifact

## Decisions

- **Image build:** Nix `dockerTools.buildLayeredImage`, not a Dockerfile.
  Options: Dockerfile (multi-stage) / Nix buildLayeredImage / Nix buildImage.
  Rationale: already using Nix for the build. Layered image is more cacheable than buildImage. No Dockerfile to maintain.

- **Base image:** scratch (no base).
  Options: scratch / Alpine / Debian slim / distroless.
  Rationale: only Nix store paths needed. No package manager, no shell, minimal attack surface.

- **Container user:** `pail` (UID 1000, GID 1000), non-root.
  Options: root / non-root with fixed UID / non-root with configurable UID.
  Rationale: non-root is security best practice. Fixed UID simplifies volume permissions and `fakeRootCommands`.

- **Init process:** tini as PID 1.
  Options: tini / dumb-init / no init (pail as PID 1).
  Rationale: PID 1 in Linux ignores signals by default unless explicitly handled. tini forwards signals to pail and reaps zombie child processes (opencode subprocesses).

- **Entrypoint:** `tini -- pail --config /etc/pail/config.toml`.
  Options: entrypoint with config baked in / entrypoint without config / CMD-based.
  Rationale: subcommands work naturally with `docker compose run` (e.g., `docker compose run pail tg login`). Config path is fixed for Docker — host config is bind-mounted to `/etc/pail/config.toml`.

- **Data dir override:** `PAIL_DATA_DIR` env var.
  Options: env var override / separate Docker config / always `/var/lib/pail`.
  Rationale: same `config.toml` works for both local dev (default `./data`) and Docker (`/var/lib/pail`) without manual edits.

- **`/etc/passwd` and `/etc/group` injection:** `pkgs.writeTextDir`.
  Options: writeTextDir / runAsRoot / fakeRootCommands.
  Rationale: writeTextDir is the simplest way to inject static files into a Nix Docker image. Just creates the text files as Nix store paths.

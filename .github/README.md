# pail — Personal AI Lurker

A self-hosted service that monitors RSS feeds and Telegram channels/groups/folders, generates AI digest articles via [opencode](https://opencode.ai), and publishes them as Atom feeds.

## Quick Start

### Prerequisites

- [Nix](https://nixos.org/download/) with flakes enabled
- An LLM provider API key (or use the free `opencode/big-pickle` model)
- For Telegram sources: API credentials from [my.telegram.org](https://my.telegram.org)

### Development

```bash
nix develop          # enters shell with Rust toolchain, opencode, etc.
cargo build          # build
cargo clippy         # lint
cargo test           # test
cargo fmt --check    # format check
alejandra -c .       # Nix format check
```

CI runs these checks automatically on push to `main` and on PRs (see `.github/workflows/ci.yml`).

### Configuration

Copy the example config and edit it:

```bash
cp config.example.toml config.toml
```

Key sections:
- `[[source]]` — define RSS feeds and Telegram channels/groups/folders to monitor
- `[[output_channel]]` — define digest channels with schedule, prompt, and source list
- `[opencode]` — LLM model, binary path, and system prompt template
- `[telegram]` — API credentials and global toggle for Telegram integration

See `config.example.toml` for all options with comments.

### Telegram Setup

To use Telegram sources, enable `[telegram]` in your config with `api_id` and `api_hash` from [my.telegram.org](https://my.telegram.org), then authenticate:

```bash
# Interactive login (phone number, verification code, optional 2FA)
pail tg login

# Verify session
pail tg status
```

The session is stored in the database and persists across restarts.

### CLI Usage

```bash
# Validate config
pail validate

# Generate a digest (one-shot, fetches RSS + TG history, invokes opencode)
pail generate tech-digest --output ./digest.md

# Re-generate with a custom time window (useful for prompt iteration)
pail generate tech-digest --since 7d --output ./digest.md
```

### Daemon Mode

Run without a subcommand to start the daemon:

```bash
pail --config config.toml
```

The daemon:
- Polls RSS feeds at configured intervals
- Listens for live Telegram messages from subscribed channels/groups
- Generates digests on schedule (e.g., `at:08:00,20:00`)
- Serves Atom feeds at `http://localhost:8080/feed/default/<slug>.atom`

### Feed Authentication

Feeds require a token. On first run, a token is auto-generated and logged once:

```
WARN feed token generated: <token> — save this, it won't be shown again
```

Access feeds with either:
- Query param: `http://localhost:8080/feed/default/tech-digest.atom?token=<token>`
- HTTP Basic Auth: `http://user:<token>@localhost:8080/feed/default/tech-digest.atom`

To set a fixed token, add `feed_token = "my-secret"` to the `[pail]` config section.

## Docker

Build the image with Nix and load it:

```bash
nix build .#docker && docker load < result
```

Run with docker-compose:

```bash
# Create data directories
mkdir -p data/pail data/opencode-auth data/opencode-config

# Copy and edit config — set data_dir to match the Docker volume mount
cp config.example.toml config.toml
# In config.toml, change: data_dir = "/var/lib/pail"

# Authenticate opencode (one-time, interactive)
docker-compose run --rm -it pail opencode auth login

# Authenticate Telegram (one-time, if using TG sources)
docker-compose run --rm -it pail tg login

# Start the service
docker-compose up -d
```

Alternatively, pass API keys as environment variables (see `docker-compose.yml`).

## Reverse Proxy

pail's built-in HTTP server is designed to sit behind a reverse proxy. [Caddy](https://caddyserver.com/) is recommended — automatic HTTPS with zero config.

**Docker network (recommended):** Uncomment the `networks` section in `docker-compose.yml` to join an external `caddynet` network, and remove the `ports` block — Caddy connects to `pail:8080` via the Docker network:

```caddyfile
pail.example.com {
	reverse_proxy pail:8080
}
```

**Host-level proxy:** If Caddy runs outside Docker, keep the `ports` section and set `listen = "127.0.0.1:8080"` in your config:

```caddyfile
pail.example.com {
	reverse_proxy localhost:8080
}
```

Feed readers can then subscribe to `https://pail.example.com/feed/default/<slug>.atom?token=<token>`.

## Schedule Formats

```toml
schedule = "at:08:00"                  # once daily at 08:00
schedule = "at:08:00,20:00"            # twice daily
schedule = "weekly:monday,08:00"       # weekly on Monday
schedule = "cron:0 8 * * *"            # 5-field cron expression (always UTC)
```

`at:` and `weekly:` times are interpreted in the configured `timezone` (default: UTC). Cron expressions always evaluate in UTC.

## License

[AGPL-3.0](../LICENSE)

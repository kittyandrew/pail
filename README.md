# pail

**Personal AI Lurker** — a self-hosted service that monitors RSS feeds and Telegram channels, generates AI
digest articles via opencode, and publishes them as Atom feeds.

Documentation lives in [`docs/`](docs/README.md): [core architecture and data model](docs/core.md), the
implemented [specs](docs/README.md), and [observability](docs/observability.md).

## Build and run

```bash
nix build .#default      # the pail binary
nix build .#docker       # Docker image tarball
nix run . -- --help
```

Copy [`config.example.toml`](config.example.toml) to `config.toml`; every option is documented inline.

## Binary cache

Builds are published to `cache.kittyandrew.dev`, so Nix can download these outputs instead of rebuilding them.

On NixOS:

```nix
nix.settings = {
  extra-substituters = ["https://cache.kittyandrew.dev/nix-cache"];
  extra-trusted-public-keys = ["cache.kittyandrew.dev-1:yy5fdErj1riKOjND10kzD5mp0L8/C8RFG3VkMizhGg4="];
};
```

Elsewhere, in `~/.config/nix/nix.conf` (or `/etc/nix/nix.conf` for all users):

```
extra-substituters = https://cache.kittyandrew.dev/nix-cache
extra-trusted-public-keys = cache.kittyandrew.dev-1:yy5fdErj1riKOjND10kzD5mp0L8/C8RFG3VkMizhGg4=
```

The `extra-` prefixes append rather than replace, so `cache.nixos.org` keeps working. The cache is read-only
and needs no credentials; it serves only what this repository's flake builds.

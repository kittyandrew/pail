# NixOS Module

A NixOS module wrapping the pail config:

```nix
services.pail = {
  enable = true;
  settings = {
    listen = "127.0.0.1:8080";
    database.path = "/var/lib/pail/pail.db";
    opencode.binary = "${pkgs.opencode}/bin/opencode";
    telegram = {
      enable = true;
      apiIdFile = config.age.secrets.pail-tg-api-id.path;
      apiHashFile = config.age.secrets.pail-tg-api-hash.path;
    };
  };
  sources = [
    { name = "HN"; type = "rss"; url = "https://hnrss.org/frontpage"; }
  ];
  outputChannels = [ ... ];
};
```

Would generate the TOML config file, set up a systemd service, and manage secrets via agenix.

## Decisions

No decisions made yet.

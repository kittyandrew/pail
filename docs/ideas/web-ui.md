# Web UI

Configuration and monitoring interface. NOT a feed reader.

## Purpose

Browser-based admin interface for managing sources, output channels, and viewing generation history.

## Features

- **Dashboard:** overview of output channels, last generation times, content store stats
- **Output Channels:** CRUD for output channels (name, sources, schedule, prompt, model)
- **Sources:** list, add, edit, disable sources
  - RSS: enter URL, set poll interval
  - Telegram: browse subscribed channels/groups/folders from connected account
  - Per-source toggle (enabled/disabled)
- **Telegram Account:** connect/disconnect, view session status, login flow
- **Generation Logs:** view past generation runs, their outputs, errors, token usage
- **Users (multi-user):** user management, per-user source/channel configuration (see [Multi-User idea](multi-user.md))

## Tech Stack

- Rust backend serves the API (same binary as the daemon)
- Frontend: TBD
- Auth: session-based, bcrypt passwords
- API: JSON REST

## API Endpoints (sketch)

```
GET    /api/sources                 # list sources
POST   /api/sources                 # create source
PATCH  /api/sources/:id             # update source
DELETE /api/sources/:id             # delete source

GET    /api/channels                # list output channels
POST   /api/channels                # create output channel
PATCH  /api/channels/:id            # update output channel
DELETE /api/channels/:id            # delete output channel
POST   /api/channels/:id/generate   # trigger immediate generation

GET    /api/articles/:channel_id    # list generated articles
GET    /api/articles/:id            # get single article

GET    /api/telegram/dialogs        # list TG channels/groups
GET    /api/telegram/folders        # list TG folders
GET    /api/telegram/status         # session status
POST   /api/telegram/connect        # start login flow

GET    /api/logs/:channel_id        # generation logs

GET    /article/:id                 # unauthenticated HTML permalink for a generated article
GET    /feed/<username>/<slug>.atom  # Atom 1.0 feed
```

## Read-Only Config Indicator

Items defined in the TOML config file are shown as locked in the UI — cannot be modified via web interface.

## Decisions

- **Frontend tech stack:** TBD — deferred until implementation.
  Options: not yet evaluated.

- **Auth:** session-based with bcrypt passwords.
  Options: session-based / JWT / OAuth only.
  Rationale: simple, well-understood. Self-hosted service doesn't need OAuth complexity.

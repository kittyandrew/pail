# Image Support

Download media (starting with TG photos) and make them available in the generation workspace so opencode's vision capabilities can process them.

## Two Layers

1. **Downloading** — platform-specific. Each source type (TG, RSS, future Discord) has its own media download logic. Starting with Telegram photos only.
2. **Workspace handling** — generic. Downloaded media files are placed in per-source folders in the workspace and referenced from source content files. opencode reads them automatically via vision.

## Telegram Implementation (first)

- Use grammers' media download API to fetch photo thumbnails/originals
- Download at content ingestion time (store in data dir) or at workspace preparation time (download on demand)
- Annotate source content files: `**Media:** photo — see [image](images/msg-1234.jpg)` instead of current `[photo — no caption, see link]`

## Workspace Layout

```
/tmp/pail-gen-<uuid>/
  sources/
    <source-slug>/
      content.md         # source content (renamed from <slug>.md)
      images/
        msg-1234.jpg
        msg-1250.jpg
  manifest.json
  prompt.md
  output.md
```

Per-source image folders keep things organized and prevent filename collisions across sources.

## Config

Configurable per source type (global defaults) with per-source overrides:

```toml
[telegram]
# Global defaults for all TG sources
media_types = ["photo"]       # which media types to download (default: photos only)
max_media_size = "500KB"      # max file size per media item

[[source]]
name = "News Channel"
type = "telegram_channel"
tg_username = "example"
# Per-source override
# media_types = ["photo", "document"]
# max_media_size = "1MB"
```

RSS image downloading (article headers, embedded images) is a separate concern — deferred.

## Decisions

- **Starting scope:** Telegram photos only.
  Options: TG only / TG + RSS / all sources.
  Rationale: TG photos are the most common use case. RSS image extraction is a different problem (HTML parsing, CDN URLs). Platform-specific downloading, generic workspace handling.

- **Size and type filtering:** configurable per source type + per-source overrides.
  Options: max file size only / media type filter only / both + per-source overrides.
  Rationale: both dimensions are needed. Photos are the useful default; documents/videos are often too large or not useful for vision. 500KB default is reasonable for compressed photos.

- **Workspace layout:** per-source `images/` subfolders.
  Options: single `images/` folder / per-source subfolders.
  Rationale: prevents filename collisions, keeps source content and its media together.

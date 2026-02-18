# OPML Endpoint

TODO: design pending

`GET /opml?token=xxx` — dynamically generated OPML 2.0 file listing all output channel feeds with embedded auth tokens, for one-click import into RSS readers. Requires a `public_url` config field (e.g., `public_url = "https://pail.example.com"`) to construct absolute feed URLs. The `public_url` field is also useful for web UI links.

## Decisions

No decisions made yet.

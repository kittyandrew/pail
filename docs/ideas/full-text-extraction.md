# Full-Text Extraction

For RSS feeds that only provide summaries/excerpts:
- Optionally follow the article link and extract full text
- Use a readability-style extractor (like Mozilla Readability)
- Configurable per source (default: off)
- Respects robots.txt and rate limits

Currently, the generation prompt tells opencode to fetch full articles from RSS links itself. This idea would move that extraction into pail's ingestion layer, making the full text available in the content store before generation.

## Decisions

No decisions made yet.

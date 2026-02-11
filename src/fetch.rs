use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::error::FetchError;
use crate::models::{ContentItem, Source};

/// Fetch RSS items from a source. Returns ContentItems ready for storage.
pub async fn fetch_rss_source(source: &Source) -> Result<Vec<ContentItem>> {
    let url = source.url.as_deref().ok_or_else(|| FetchError::Parse {
        url: source.name.clone(),
        message: "RSS source has no URL".to_string(),
    })?;

    let max_items = source.max_items as usize;

    // Build HTTP client with auth if needed
    let mut headers = HeaderMap::new();

    // Use auth from DB model fields (synced from config)
    if let Some(auth_type) = &source.auth_type {
        match auth_type.as_str() {
            "basic" => {
                if let (Some(user), Some(pass)) = (&source.auth_username, &source.auth_password) {
                    use base64::Engine;
                    let credentials = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
                    headers.insert(
                        AUTHORIZATION,
                        HeaderValue::from_str(&format!("Basic {credentials}")).map_err(|_| FetchError::Parse {
                            url: url.to_string(),
                            message: "invalid basic auth credentials".to_string(),
                        })?,
                    );
                }
            }
            "bearer" => {
                if let Some(token) = &source.auth_token {
                    headers.insert(
                        AUTHORIZATION,
                        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| FetchError::Parse {
                            url: url.to_string(),
                            message: "invalid bearer token".to_string(),
                        })?,
                    );
                }
            }
            "header" => {
                if let (Some(name), Some(value)) = (&source.auth_header_name, &source.auth_header_value) {
                    let header_name: HeaderName = name.parse().map_err(|_| FetchError::Parse {
                        url: url.to_string(),
                        message: format!("invalid header name: {name}"),
                    })?;
                    let header_value = HeaderValue::from_str(value).map_err(|_| FetchError::Parse {
                        url: url.to_string(),
                        message: format!("invalid header value for {name}"),
                    })?;
                    headers.insert(header_name, header_value);
                }
            }
            _ => {}
        }
    }

    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(concat!("pail/", env!("CARGO_PKG_VERSION"))),
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .default_headers(headers)
        .build()
        .map_err(|e| FetchError::Http {
            url: url.to_string(),
            source: e,
        })?;

    debug!(url = %url, source = %source.name, "fetching RSS feed");

    let response = client.get(url).send().await.map_err(|e| FetchError::Http {
        url: url.to_string(),
        source: e,
    })?;

    let body = response.bytes().await.map_err(|e| FetchError::Http {
        url: url.to_string(),
        source: e,
    })?;

    let feed = feed_rs::parser::parse(&body[..]).map_err(|e| FetchError::Parse {
        url: url.to_string(),
        message: e.to_string(),
    })?;

    let now = Utc::now();

    let items: Vec<ContentItem> = feed
        .entries
        .into_iter()
        .take(max_items)
        .filter_map(|entry| {
            // Get the best content: prefer content over summary
            let raw_body = entry
                .content
                .and_then(|c| c.body)
                .or_else(|| entry.summary.map(|s| s.content))
                .unwrap_or_default();

            // Convert HTML to plain text (RSS bodies are often HTML)
            let body = strip_html(&raw_body);

            if body.is_empty() && entry.title.is_none() {
                debug!(entry_id = ?entry.id, "skipping empty entry");
                return None;
            }

            let title = entry.title.map(|t| t.content);
            let url = entry.links.first().map(|l| l.href.clone());
            let author = entry.authors.first().map(|a| a.name.clone());

            let original_date: DateTime<Utc> = entry.published.or(entry.updated).unwrap_or(now);

            // Dedup key: GUID if available, else SHA-256 of URL + title (PRD §11.2)
            let dedup_key = if !entry.id.is_empty() {
                entry.id.clone()
            } else {
                let mut hasher = Sha256::new();
                hasher.update(url.as_deref().unwrap_or(""));
                hasher.update("|");
                hasher.update(title.as_deref().unwrap_or(""));
                format!("sha256:{:x}", hasher.finalize())
            };

            let content_type = if url.is_some() { "link" } else { "text" };

            Some(ContentItem {
                id: Uuid::new_v4().to_string(),
                source_id: source.id.clone(),
                ingested_at: now,
                original_date,
                content_type: content_type.to_string(),
                title,
                body,
                url,
                author,
                metadata: "{}".to_string(),
                dedup_key,
                upstream_changed: false,
            })
        })
        .collect();

    if items.is_empty() {
        warn!(source = %source.name, url = %url, "feed returned no usable items");
    }

    Ok(items)
}

/// Convert HTML to plain text. If the input doesn't look like HTML, return it as-is.
fn strip_html(text: &str) -> String {
    if !text.contains('<') {
        return text.to_string();
    }
    html2text::from_read(text.as_bytes(), 200).unwrap_or_else(|_| text.to_string())
}

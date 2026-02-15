use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use sqlx::SqlitePool;
use subtle::ConstantTimeEq;
use tracing::{debug, warn};

use crate::store;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub feed_token: String,
    pub timezone: chrono_tz::Tz,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/feed/{*path}", get(feed_handler))
        .route("/article/{id}", get(article_handler))
        .with_state(state)
}

#[derive(serde::Deserialize)]
pub struct FeedQuery {
    token: Option<String>,
}

async fn feed_handler(
    State(state): State<AppState>,
    Path(path): Path<String>,
    Query(query): Query<FeedQuery>,
    headers: HeaderMap,
) -> Response {
    // Authenticate
    if !authenticate(&state.feed_token, &query, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"pail\"")],
            "Unauthorized",
        )
            .into_response();
    }

    // Parse path: expected format is "<username>/<slug>.atom"
    let path_stripped = match path.strip_suffix(".atom") {
        Some(p) => p,
        None => return (StatusCode::NOT_FOUND, "Not found. Use /feed/default/<slug>.atom").into_response(),
    };
    let slug = match path_stripped.split_once('/') {
        Some((username, slug)) if username == "default" && !slug.is_empty() && !slug.contains('/') => slug,
        _ => return (StatusCode::NOT_FOUND, "Not found. Use /feed/default/<slug>.atom").into_response(),
    };

    // Look up channel
    let channel = match store::get_channel_by_slug(&state.pool, slug).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, format!("No feed for '{slug}'")).into_response();
        }
        Err(e) => {
            warn!(error = %e, slug = %slug, "failed to look up channel");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    // Get recent articles
    let articles = match store::get_recent_articles(&state.pool, &channel.id, 50).await {
        Ok(a) => a,
        Err(e) => {
            warn!(error = %e, "failed to query articles");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    // Build Atom feed
    let base_url = derive_base_url(&headers);
    let feed = build_atom_feed(&channel, &articles, &base_url);

    let xml = feed.to_string();

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

fn authenticate(feed_token: &str, query: &FeedQuery, headers: &HeaderMap) -> bool {
    // Method 1: query param
    if let Some(ref token) = query.token
        && constant_time_eq(token, feed_token)
    {
        debug!("authenticated via query param");
        return true;
    }

    // Method 2: HTTP Basic Auth
    if let Some(auth_header) = headers.get(header::AUTHORIZATION)
        && let Ok(auth_str) = auth_header.to_str()
        && let Some(encoded) = auth_str.strip_prefix("Basic ")
    {
        use base64::Engine;
        if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim())
            && let Ok(credentials) = String::from_utf8(decoded)
            && let Some((_user, password)) = credentials.split_once(':')
            && constant_time_eq(password, feed_token)
        {
            debug!("authenticated via HTTP Basic Auth");
            return true;
        }
    }

    false
}

/// Constant-time string comparison to prevent timing attacks on token validation.
fn constant_time_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Derive the base URL from request headers (works behind reverse proxies).
fn derive_base_url(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

/// Escape HTML special characters for safe embedding in HTML attributes/content.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

async fn article_handler(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    // Validate UUID format
    if uuid::Uuid::parse_str(&id).is_err() {
        return (StatusCode::BAD_REQUEST, "Invalid article ID").into_response();
    }

    let article = match store::get_article_by_id(&state.pool, &id).await {
        Ok(Some(a)) => a,
        Ok(None) => return (StatusCode::NOT_FOUND, "Article not found").into_response(),
        Err(e) => {
            warn!(error = %e, "failed to look up article");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response();
        }
    };

    let title = html_escape(&article.title);
    let local_time = article.generated_at.with_timezone(&state.timezone);
    let date = local_time.format("%b %-d %Y, %H:%M %Z");

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
body {{ max-width: 48rem; margin: 2rem auto; padding: 0 1rem; font-family: system-ui, sans-serif; line-height: 1.6; color: #222; }}
h1 {{ margin-bottom: 0.25rem; }}
.date {{ color: #666; margin-bottom: 2rem; }}
a {{ color: #0366d6; }}
blockquote {{ border-left: 3px solid #ddd; margin-left: 0; padding-left: 1rem; color: #555; }}
</style>
</head>
<body>
<h1>{title}</h1>
<p class="date">{date}</p>
{body}
</body>
</html>"#,
        body = article.body_html,
    );

    Html(html).into_response()
}

fn build_atom_feed(
    channel: &crate::models::OutputChannel,
    articles: &[crate::models::GeneratedArticleRow],
    base_url: &str,
) -> atom_syndication::Feed {
    use atom_syndication::{Category, Content, Entry, Feed, Link, Person, Text};
    use chrono::FixedOffset;

    let to_fixed = |dt: &chrono::DateTime<chrono::Utc>| -> chrono::DateTime<FixedOffset> {
        dt.with_timezone(&FixedOffset::east_opt(0).unwrap())
    };

    let feed_updated = articles
        .first()
        .map(|a| to_fixed(&a.generated_at))
        .unwrap_or_else(|| to_fixed(&chrono::Utc::now()));

    let entries: Vec<Entry> = articles
        .iter()
        .map(|article| {
            // Parse topics from JSON
            let topics: Vec<String> = serde_json::from_str(&article.topics).unwrap_or_default();
            let categories: Vec<Category> = topics
                .into_iter()
                .map(|t| Category {
                    term: t,
                    ..Default::default()
                })
                .collect();

            // Derive author from model_used: "anthropic/claude-sonnet-4-5" -> "pail-opencode-claude-sonnet-4-5"
            let model_short = article.model_used.split('/').next_back().unwrap_or(&article.model_used);
            let author = Person {
                name: format!("pail-opencode-{model_short}"),
                ..Default::default()
            };

            let content = Content {
                content_type: Some("html".to_string()),
                value: Some(article.body_html.clone()),
                ..Default::default()
            };

            let entry_link = Link {
                href: format!("{base_url}/article/{}", article.id),
                rel: "alternate".to_string(),
                mime_type: Some("text/html".to_string()),
                ..Default::default()
            };

            Entry {
                id: format!("urn:uuid:{}", article.id),
                title: Text::plain(&article.title),
                updated: to_fixed(&article.generated_at),
                authors: vec![author],
                content: Some(content),
                categories,
                published: Some(to_fixed(&article.generated_at)),
                links: vec![entry_link],
                ..Default::default()
            }
        })
        .collect();

    let self_link = Link {
        href: format!("{base_url}/feed/default/{}.atom", channel.slug),
        rel: "self".to_string(),
        mime_type: Some("application/atom+xml".to_string()),
        ..Default::default()
    };

    Feed {
        id: format!("urn:pail:channel:{}", channel.id),
        title: Text::plain(&channel.name),
        subtitle: Some(Text::plain(&channel.name)),
        updated: feed_updated,
        entries,
        links: vec![self_link],
        ..Default::default()
    }
}

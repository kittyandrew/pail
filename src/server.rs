use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use sqlx::SqlitePool;
use subtle::ConstantTimeEq;
use tracing::{debug, warn};

use crate::store;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub feed_token: String,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/feed/{*path}", get(feed_handler))
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
    let feed = build_atom_feed(&channel, &articles);

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

fn build_atom_feed(
    channel: &crate::models::OutputChannel,
    articles: &[crate::models::GeneratedArticleRow],
) -> atom_syndication::Feed {
    use atom_syndication::{Category, Content, Entry, Feed, Person, Text};
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

            Entry {
                id: format!("urn:uuid:{}", article.id),
                title: Text::plain(&article.title),
                updated: to_fixed(&article.generated_at),
                authors: vec![author],
                content: Some(content),
                categories,
                published: Some(to_fixed(&article.generated_at)),
                ..Default::default()
            }
        })
        .collect();

    Feed {
        id: format!("urn:pail:channel:{}", channel.id),
        title: Text::plain(&channel.name),
        subtitle: Some(Text::plain(&channel.name)),
        updated: feed_updated,
        entries,
        ..Default::default()
    }
}

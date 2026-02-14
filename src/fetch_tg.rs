use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use grammers_client::Client;
use grammers_client::media::Media;
use grammers_session::types::{PeerAuth, PeerId, PeerRef};
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::models::{ContentItem, Source};
use crate::store;

/// Convert a grammers Message to a pail ContentItem.
/// Returns None for empty messages (no text, no media).
pub fn message_to_content_item(
    msg: &grammers_client::message::Message,
    source_id: &str,
    peer_username: Option<&str>,
) -> Option<ContentItem> {
    let chat_id = msg.peer_id().bare_id();
    let message_id = msg.id();
    let text = msg.text().to_string();

    // Skip empty messages with no meaningful content
    if text.is_empty() && msg.media().is_none() {
        return None;
    }

    // Determine content type
    let content_type = if msg.forward_header().is_some() {
        "forward"
    } else if msg.media().is_some() {
        "media"
    } else {
        "text"
    };

    // Get sender info (anonymous for channels, named for groups)
    let sender_name = msg.sender().and_then(|s| s.name().map(|n| n.to_string()));

    // Construct t.me URL (PRD §10.5)
    let url = match peer_username {
        Some(username) => Some(format!("https://t.me/{username}/{message_id}")),
        None => Some(format!("https://t.me/c/{chat_id}/{message_id}")),
    };

    // Build metadata JSON
    let mut meta = serde_json::Map::new();
    meta.insert("message_id".to_string(), serde_json::json!(message_id));
    meta.insert("chat_id".to_string(), serde_json::json!(chat_id));

    if let Some(reply_to) = msg.reply_to_message_id() {
        meta.insert("reply_to_msg_id".to_string(), serde_json::json!(reply_to));
    }

    if let Some(fwd) = msg.forward_header() {
        let grammers_tl_types::enums::MessageFwdHeader::Header(h) = fwd;
        // Prefer from_name (always human-readable), fall back to from_id peer
        if let Some(name) = &h.from_name {
            meta.insert("forward_from".to_string(), serde_json::json!(name));
        } else if let Some(ref peer) = h.from_id {
            match peer {
                grammers_tl_types::enums::Peer::Channel(c) => {
                    meta.insert("forward_from_id".to_string(), serde_json::json!(c.channel_id));
                }
                grammers_tl_types::enums::Peer::User(u) => {
                    meta.insert("forward_from_id".to_string(), serde_json::json!(u.user_id));
                }
                grammers_tl_types::enums::Peer::Chat(c) => {
                    meta.insert("forward_from_id".to_string(), serde_json::json!(c.chat_id));
                }
            }
        }
        if let Some(post_author) = &h.post_author {
            meta.insert("forward_post_author".to_string(), serde_json::json!(post_author));
        }
    }

    if let Some(ref media) = msg.media() {
        let media_type = match media {
            Media::Photo(_) => "photo",
            Media::Document(_) => "document",
            Media::Sticker(_) => "sticker",
            Media::Contact(_) => "contact",
            Media::Poll(_) => "poll",
            Media::Geo(_) => "geo",
            Media::Dice(_) => "dice",
            Media::Venue(_) => "venue",
            Media::GeoLive(_) => "geo_live",
            Media::WebPage(_) => "webpage",
            _ => "other",
        };
        meta.insert("media_type".to_string(), serde_json::json!(media_type));
    }

    if let Some(username) = peer_username {
        meta.insert("chat_username".to_string(), serde_json::json!(username));
    }

    let metadata = serde_json::to_string(&meta).unwrap_or_else(|_| "{}".to_string());
    let dedup_key = format!("tg:{chat_id}:{message_id}");
    let now = Utc::now();
    let original_date = msg.date();

    Some(ContentItem {
        id: Uuid::new_v4().to_string(),
        source_id: source_id.to_string(),
        ingested_at: now,
        original_date,
        content_type: content_type.to_string(),
        title: None,
        body: text,
        url,
        author: sender_name,
        metadata,
        dedup_key,
        upstream_changed: false,
    })
}

/// Fetch recent TG message history for all TG sources in a channel (CLI mode).
/// Analogous to the RSS one-shot fetch block in pipeline.rs.
pub async fn fetch_tg_sources(
    client: &Client,
    pool: &SqlitePool,
    sources: &[Source],
    since: DateTime<Utc>,
    cancel: &CancellationToken,
) -> Result<()> {
    for (i, source) in sources.iter().enumerate() {
        if cancel.is_cancelled() {
            return Ok(());
        }
        // Brief delay between sources to avoid aggressive API bursts
        if i > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        match source.source_type.as_str() {
            "telegram_channel" | "telegram_group" => {
                let tg_id = match source.tg_id {
                    Some(id) => id,
                    None => {
                        warn!(source = %source.name, "TG source has no resolved tg_id, skipping history fetch");
                        continue;
                    }
                };
                let peer_username = source.tg_username.as_deref().map(|u| u.trim_start_matches('@'));
                match fetch_channel_history(client, pool, &source.id, tg_id, peer_username, since).await {
                    Ok(count) => info!(source = %source.name, items = count, "fetched TG history"),
                    Err(e) => warn!(source = %source.name, error = format!("{e:#}"), "failed to fetch TG history"),
                }
            }
            "telegram_folder" => {
                let channels = store::get_folder_channels_with_info(pool, &source.id)
                    .await
                    .with_context(|| format!("loading folder channels for source '{}'", source.name))?;

                if channels.is_empty() {
                    warn!(source = %source.name, "folder has no channels, skipping history fetch");
                    continue;
                }

                info!(source = %source.name, channels = channels.len(), "fetching TG folder history");

                for (i, (channel_tg_id, _channel_name, channel_username)) in channels.iter().enumerate() {
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    // Brief delay between channels to avoid aggressive API bursts
                    if i > 0 {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    match fetch_channel_history(
                        client,
                        pool,
                        &source.id,
                        *channel_tg_id,
                        channel_username.as_deref(),
                        since,
                    )
                    .await
                    {
                        Ok(count) => {
                            debug!(source = %source.name, channel_tg_id, items = count, "fetched folder channel history")
                        }
                        Err(e) => {
                            warn!(source = %source.name, channel_tg_id, error = format!("{e:#}"), "failed to fetch folder channel history")
                        }
                    }
                }
            }
            _ => {
                debug!(source = %source.name, source_type = %source.source_type, "skipping non-TG source");
            }
        }
    }

    Ok(())
}

/// Resolve a bare tg_id to a PeerRef by looking up tg_peer_info.
/// Tries channel first (most common: channels + supergroups), then basic chat.
/// Falls back to channel with access_hash 0 if the peer isn't cached.
pub async fn resolve_peer_ref(pool: &SqlitePool, tg_id: i64) -> Result<PeerRef> {
    // Try as channel/supergroup first (vast majority of cases)
    let channel_bot_api_id = PeerId::channel(tg_id).bot_api_dialog_id();
    if let Some(hash) = sqlx::query_scalar::<_, Option<i64>>("SELECT hash FROM tg_peer_info WHERE peer_id = ?")
        .bind(channel_bot_api_id)
        .fetch_optional(pool)
        .await
        .context("looking up channel peer")?
        .flatten()
    {
        return Ok(PeerRef {
            id: PeerId::channel(tg_id),
            auth: PeerAuth::from_hash(hash),
        });
    }

    // Try as basic group chat
    let chat_bot_api_id = PeerId::chat(tg_id).bot_api_dialog_id();
    if let Some(row) = sqlx::query_as::<_, (Option<i64>,)>("SELECT hash FROM tg_peer_info WHERE peer_id = ?")
        .bind(chat_bot_api_id)
        .fetch_optional(pool)
        .await
        .context("looking up chat peer")?
    {
        return Ok(PeerRef {
            id: PeerId::chat(tg_id),
            auth: row.0.map(PeerAuth::from_hash).unwrap_or(PeerAuth::from_hash(0)),
        });
    }

    // Fallback: assume channel (most common)
    Ok(PeerRef {
        id: PeerId::channel(tg_id),
        auth: PeerAuth::from_hash(0),
    })
}

/// Fetch message history for a single TG channel/group.
/// Returns the number of items stored.
async fn fetch_channel_history(
    client: &Client,
    pool: &SqlitePool,
    source_id: &str,
    tg_id: i64,
    peer_username: Option<&str>,
    since: DateTime<Utc>,
) -> Result<usize> {
    let peer_ref = resolve_peer_ref(pool, tg_id).await?;

    // No item limit — the time boundary (`since`) is the stop condition.
    let mut iter = client.iter_messages(peer_ref);
    let mut count = 0;

    while let Some(msg) = iter.next().await.context("iterating TG message history")? {
        // Messages arrive newest-first; stop when we pass the time boundary
        if msg.date() < since {
            break;
        }

        if let Some(item) = message_to_content_item(&msg, source_id, peer_username) {
            store::upsert_content_item(pool, &item)
                .await
                .context("storing TG history item")?;
            count += 1;
        }
    }

    Ok(count)
}

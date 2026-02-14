use std::collections::HashMap;
use std::sync::Arc;

use grammers_client::Client;
use grammers_client::client::UpdatesConfiguration;
use grammers_client::update::Update;
use grammers_session::updates::UpdatesLike;
use grammers_tl_types as tl;
use sqlx::SqlitePool;
use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::fetch_tg;
use crate::store;
use crate::telegram;

/// Run the Telegram event listener loop.
/// Receives live updates and stores messages from subscribed chats.
pub async fn listener_loop(
    client: Client,
    pool: SqlitePool,
    subscriptions: Arc<RwLock<HashMap<i64, Vec<String>>>>,
    updates_rx: mpsc::UnboundedReceiver<UpdatesLike>,
    cancel: CancellationToken,
) {
    info!("Telegram listener started");

    let mut update_stream = client.stream_updates(updates_rx, UpdatesConfiguration::default()).await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("Telegram listener shutting down");
                update_stream.sync_update_state().await;
                break;
            }
            update = update_stream.next() => {
                match update {
                    Ok(Update::NewMessage(msg)) if !msg.outgoing() => {
                        handle_message(&msg, &pool, &subscriptions).await;
                    }
                    Ok(Update::Raw(raw)) => {
                        // Check for folder change events (updateDialogFilter)
                        handle_raw_update(&raw, &client, &pool, &subscriptions).await;
                    }
                    Ok(_) => {
                        // MessageEdited, MessageDeleted, etc. — ignore for now
                    }
                    Err(e) => {
                        error!(error = %e, "error receiving Telegram update");
                    }
                }
            }
        }
    }

    info!("Telegram listener stopped");
}

/// Handle an incoming new message from a subscribed chat.
async fn handle_message(
    msg: &grammers_client::update::Message,
    pool: &SqlitePool,
    subscriptions: &Arc<RwLock<HashMap<i64, Vec<String>>>>,
) {
    // Get chat ID
    let chat_id = msg.peer_id().bare_id();

    // Look up in subscription map
    let source_ids = {
        let subs = subscriptions.read().await;
        match subs.get(&chat_id) {
            Some(ids) => ids.clone(),
            None => return, // Not subscribed to this chat
        }
    };

    let message_id = msg.id();

    // Get chat username for URL construction (computed once before the source_id loop)
    let peer_username: Option<String> = msg.peer().and_then(|p| p.username().map(|u| u.to_string()));

    // Store for each source that subscribes to this chat
    for source_id in &source_ids {
        if let Some(item) = fetch_tg::message_to_content_item(msg, source_id, peer_username.as_deref())
            && let Err(e) = store::upsert_content_item(pool, &item).await
        {
            warn!(
                source_id = %source_id,
                chat_id,
                message_id,
                error = %e,
                "failed to store TG message"
            );
        }
    }

    debug!(chat_id, message_id, sources = source_ids.len(), "stored TG message");
}

/// Handle raw TL updates — specifically folder changes (updateDialogFilter).
async fn handle_raw_update(
    raw: &grammers_client::update::Raw,
    client: &Client,
    pool: &SqlitePool,
    subscriptions: &Arc<RwLock<HashMap<i64, Vec<String>>>>,
) {
    // Check if this is an updateDialogFilter event
    let is_dialog_filter_update = matches!(
        &**raw,
        tl::enums::Update::DialogFilter(_) | tl::enums::Update::DialogFilterOrder(_)
    );

    if !is_dialog_filter_update {
        return;
    }

    info!("detected folder change, re-resolving folders");

    // Re-resolve all folder sources
    let folder_sources = match store::get_tg_sources(pool).await {
        Ok(sources) => sources
            .into_iter()
            .filter(|s| s.source_type == "telegram_folder")
            .collect::<Vec<_>>(),
        Err(e) => {
            error!(error = %e, "failed to load folder sources for re-resolution");
            return;
        }
    };

    if let Err(e) = telegram::resolve_folders(client, pool, &folder_sources).await {
        error!(error = %e, "failed to re-resolve folders after update");
        return;
    }

    // Rebuild subscription map
    let tg_sources = match store::get_tg_sources(pool).await {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "failed to reload TG sources");
            return;
        }
    };

    let folder_channels = match store::get_all_folder_channel_ids(pool).await {
        Ok(fc) => fc,
        Err(e) => {
            error!(error = %e, "failed to reload folder channels");
            return;
        }
    };

    let direct_sources: Vec<_> = tg_sources
        .iter()
        .filter(|s| s.source_type != "telegram_folder")
        .cloned()
        .collect();

    let new_map = telegram::build_subscription_map(&direct_sources, &folder_channels);
    let count = new_map.len();

    {
        let mut subs = subscriptions.write().await;
        *subs = new_map;
    }

    info!(subscribed_chats = count, "subscription map rebuilt after folder change");
}

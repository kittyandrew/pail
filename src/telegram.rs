use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use anyhow::{Context, Result};
use grammers_client::peer::Peer as ClientPeer;
use grammers_client::{Client, SenderPool, SignInError};
use grammers_mtsender::ConnectionParams;
use grammers_session::types::PeerId;
use grammers_session::updates::UpdatesLike;
use grammers_tl_types as tl;
use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::error::TelegramError;
use crate::models::{ContentItem, Source};
use crate::store;
use crate::tg_session::SqlxSession;

/// Holds a connected grammers client and its background runner handle.
pub struct TgConnection {
    pub client: Client,
    pub updates_rx: mpsc::UnboundedReceiver<UpdatesLike>,
    pub runner_handle: tokio::task::JoinHandle<()>,
}

/// Create a grammers Client connected to Telegram.
/// Returns the client and the updates receiver (for the listener loop).
pub async fn connect(config: &Config, pool: &SqlitePool) -> Result<TgConnection> {
    let api_id = config
        .telegram
        .api_id
        .ok_or_else(|| TelegramError::Connection("api_id not configured".to_string()))?;

    info!("loading Telegram session from database");

    let session = Arc::new(
        SqlxSession::load(pool.clone())
            .await
            .map_err(|e| TelegramError::Connection(format!("failed to load session: {e}")))?,
    );

    let sender_pool = SenderPool::with_configuration(
        session as Arc<SqlxSession>,
        api_id,
        ConnectionParams {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            device_model: "pail".to_string(),
            ..Default::default()
        },
    );

    // Destructure to get handle, runner, and updates
    let SenderPool {
        runner,
        handle: fat_handle,
        updates,
    } = sender_pool;

    let client = Client::new(fat_handle);

    // Spawn the sender pool runner (drives all MTProto I/O)
    let runner_handle = tokio::spawn(async move {
        runner.run().await;
    });

    Ok(TgConnection {
        client,
        updates_rx: updates,
        runner_handle,
    })
}

/// Interactive login flow (phone -> code -> optional 2FA).
pub async fn login(client: &Client, config: &Config) -> Result<()> {
    let api_hash = config
        .telegram
        .api_hash
        .as_deref()
        .ok_or_else(|| TelegramError::Connection("api_hash not configured".to_string()))?;

    // Check if already authorized
    if client.is_authorized().await.unwrap_or(false) {
        let me = client.get_me().await.context("getting current user")?;
        println!(
            "Already logged in as {} (@{})",
            me.full_name(),
            me.username().unwrap_or("no username")
        );
        return Ok(());
    }

    // Prompt for phone number
    print!("Phone number (with country code, e.g. +380...): ");
    std::io::stdout().flush()?;
    let mut phone = String::new();
    std::io::stdin().read_line(&mut phone)?;
    let phone = phone.trim().to_string();

    let masked_phone = if phone.len() > 4 {
        format!(
            "{}****{}",
            &phone[..phone.len() - 4].chars().take(4).collect::<String>(),
            &phone[phone.len() - 4..]
        )
    } else {
        "****".to_string()
    };
    info!(phone = %masked_phone, "requesting login code");
    let token = client.request_login_code(&phone, api_hash).await.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("API_ID_INVALID") || msg.contains("CONNECTION_API_ID_INVALID") {
            anyhow::anyhow!(
                "invalid Telegram API credentials. Check [telegram].api_id and api_hash in config.toml \
                     (get valid credentials at https://my.telegram.org)"
            )
        } else {
            anyhow::anyhow!(e).context("requesting login code")
        }
    })?;

    println!("Login code sent via Telegram.");
    print!("Enter code: ");
    std::io::stdout().flush()?;
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    let code = code.trim();

    match client.sign_in(&token, code).await {
        Ok(user) => {
            println!(
                "Logged in as {} (@{})",
                user.full_name(),
                user.username().unwrap_or("no username")
            );
        }
        Err(SignInError::PasswordRequired(password_token)) => {
            let hint = password_token.hint().unwrap_or("none");
            println!("Two-factor authentication required (hint: {hint})");
            let password = rpassword::prompt_password_stdout("Enter 2FA password: ").context("reading 2FA password")?;

            let user = client
                .check_password(password_token, password.as_bytes())
                .await
                .map_err(|e| anyhow::anyhow!("2FA check failed: {e:?}"))?;

            println!(
                "Logged in as {} (@{})",
                user.full_name(),
                user.username().unwrap_or("no username")
            );
        }
        Err(SignInError::InvalidCode) => {
            anyhow::bail!("invalid verification code");
        }
        Err(other) => {
            anyhow::bail!("sign-in failed: {other:?}");
        }
    }

    Ok(())
}

/// Print session/connection status.
pub async fn status(client: &Client) -> Result<()> {
    match client.is_authorized().await {
        Ok(true) => {
            let me = client.get_me().await.context("getting current user")?;
            println!("Status: Connected");
            println!("  Name: {}", me.full_name());
            if let Some(username) = me.username() {
                println!("  Username: @{username}");
            }
            if let Some(phone) = me.phone() {
                println!("  Phone: {phone}");
            }
        }
        Ok(false) => {
            println!("Status: Not authorized");
            println!("  Run 'pail tg login' to authenticate.");
        }
        Err(e) => {
            println!("Status: Connection error");
            println!("  Error: {e}");
        }
    }
    Ok(())
}

/// Resolve @username to numeric tg_id for sources that have a username but no tg_id.
/// Stores resolved IDs in the database.
pub async fn resolve_source_ids(client: &Client, pool: &SqlitePool, sources: &[Source]) -> Result<HashMap<String, i64>> {
    let mut resolved = HashMap::new();

    for source in sources {
        // Skip sources that already have a tg_id
        if let Some(tg_id) = source.tg_id {
            resolved.insert(source.id.clone(), tg_id);
            continue;
        }

        // Skip folder sources (they don't have a direct tg_id)
        if source.source_type == "telegram_folder" {
            continue;
        }

        let username = match &source.tg_username {
            Some(u) => u.trim_start_matches('@').to_string(),
            None => {
                warn!(source = %source.name, "TG source has neither tg_id nor tg_username, skipping");
                continue;
            }
        };

        info!(source = %source.name, username = %username, "resolving Telegram username");

        match client.resolve_username(&username).await {
            Ok(Some(peer)) => {
                let tg_id = peer.id().bare_id();
                store::update_source_tg_id(pool, &source.id, tg_id)
                    .await
                    .with_context(|| format!("storing tg_id for source '{}'", source.name))?;
                resolved.insert(source.id.clone(), tg_id);
                info!(source = %source.name, tg_id, "resolved username @{username}");
            }
            Ok(None) => {
                warn!(source = %source.name, username = %username, "username not found on Telegram");
            }
            Err(e) => {
                warn!(
                    source = %source.name,
                    username = %username,
                    error = %e,
                    "failed to resolve username"
                );
            }
        }
    }

    Ok(resolved)
}

/// Resolve folder names to channel lists.
/// For each folder source, looks up the folder by name via getDialogFilters,
/// extracts the included peers, and stores them in tg_folder_channels.
pub async fn resolve_folders(client: &Client, pool: &SqlitePool, folder_sources: &[Source]) -> Result<()> {
    if folder_sources.is_empty() {
        return Ok(());
    }

    // Get all folder definitions from Telegram
    let request = tl::functions::messages::GetDialogFilters {};
    let result = client.invoke(&request).await.context("fetching dialog filters")?;

    let filters = match result {
        tl::enums::messages::DialogFilters::Filters(f) => f.filters,
    };

    for source in folder_sources {
        let folder_name = match &source.tg_folder_name {
            Some(n) => n,
            None => continue,
        };

        // Find the matching filter by title
        let filter = filters.iter().find(|f| {
            let title = match f {
                tl::enums::DialogFilter::Filter(df) => extract_filter_title(&df.title),
                tl::enums::DialogFilter::Chatlist(df) => extract_filter_title(&df.title),
                _ => None,
            };
            title.as_deref() == Some(folder_name.as_str())
        });

        let filter = match filter {
            Some(f) => f,
            None => {
                warn!(source = %source.name, folder = %folder_name, "folder not found in Telegram");
                continue;
            }
        };

        // Extract folder ID and all peers (pinned + included)
        let (folder_id, pinned_peers, included_peers) = match filter {
            tl::enums::DialogFilter::Filter(df) => (df.id, &df.pinned_peers, &df.include_peers),
            tl::enums::DialogFilter::Chatlist(df) => (df.id, &df.pinned_peers, &df.include_peers),
            _ => continue,
        };

        // Store folder_id on the source
        store::update_source_tg_folder_id(pool, &source.id, folder_id)
            .await
            .with_context(|| format!("storing folder_id for source '{}'", source.name))?;

        // Clear existing folder channels and re-sync
        store::delete_folder_channels(pool, &source.id).await?;

        // Collect all peers and cache their access hashes
        let all_peers: Vec<&tl::enums::InputPeer> = pinned_peers.iter().chain(included_peers.iter()).collect();
        for peer in &all_peers {
            cache_input_peer(pool, peer).await;
        }

        // Batch-resolve channel peers in a single getChannels call
        let channel_info = batch_resolve_channels(client, &all_peers).await;

        for peer in &all_peers {
            let tg_id = match peer {
                tl::enums::InputPeer::Channel(c) => c.channel_id,
                tl::enums::InputPeer::Chat(c) => c.chat_id,
                tl::enums::InputPeer::User(u) => u.user_id,
                _ => continue,
            };

            let (name, username) = channel_info.get(&tg_id).cloned().unwrap_or((None, None));

            store::upsert_folder_channel(pool, &source.id, tg_id, name.as_deref(), username.as_deref()).await?;
        }

        info!(source = %source.name, folder = %folder_name, folder_id, "resolved folder");
    }

    Ok(())
}

/// Ensure all direct TG sources have their peer info cached.
///
/// Sources configured with `tg_id` only (no @username) never trigger a `resolve_username`
/// API call, so their access hashes may not be in the peer cache. Without a valid access hash,
/// `getHistory` for supergroups/channels fails with CHANNEL_INVALID.
///
/// This function checks for uncached peers and, if any are found, iterates the user's dialog
/// list to warm the cache. grammers auto-caches all peers from `getDialogs` responses via
/// the Session trait.
pub async fn ensure_peer_cache(client: &Client, pool: &SqlitePool, sources: &[Source]) -> Result<()> {
    let mut uncached_ids: Vec<i64> = Vec::new();

    for source in sources {
        // Folder channels are cached by cache_input_peer in resolve_folders
        if source.source_type == "telegram_folder" {
            continue;
        }

        let tg_id = match source.tg_id {
            Some(id) => id,
            None => continue,
        };

        // Check if this peer exists in tg_peer_info (as channel or chat)
        let channel_api_id = PeerId::channel(tg_id).bot_api_dialog_id();
        let chat_api_id = PeerId::chat(tg_id).bot_api_dialog_id();

        let found = sqlx::query_scalar::<_, i32>("SELECT 1 FROM tg_peer_info WHERE peer_id IN (?, ?) LIMIT 1")
            .bind(channel_api_id)
            .bind(chat_api_id)
            .fetch_optional(pool)
            .await
            .context("checking peer cache")?;

        if found.is_none() {
            uncached_ids.push(tg_id);
        }
    }

    if uncached_ids.is_empty() {
        return Ok(());
    }

    info!(uncached = uncached_ids.len(), "warming peer cache via dialog iteration");

    let mut dialogs = client.iter_dialogs();
    while let Some(_dialog) = dialogs.next().await.context("iterating dialogs for peer cache")? {
        // grammers auto-caches peers from the getDialogs API responses
    }

    // Verify that the previously uncached peers are now resolved
    for tg_id in &uncached_ids {
        let channel_api_id = PeerId::channel(*tg_id).bot_api_dialog_id();
        let chat_api_id = PeerId::chat(*tg_id).bot_api_dialog_id();

        let found = sqlx::query_scalar::<_, i32>("SELECT 1 FROM tg_peer_info WHERE peer_id IN (?, ?) LIMIT 1")
            .bind(channel_api_id)
            .bind(chat_api_id)
            .fetch_optional(pool)
            .await
            .context("verifying peer cache")?;

        if found.is_none() {
            warn!(
                tg_id,
                "peer not found after dialog iteration — are you a member of this chat?"
            );
        }
    }

    Ok(())
}

/// Build subscription map: chat_id -> Vec<source_id>.
/// Maps each Telegram chat ID to the list of pail source IDs that want messages from it.
pub fn build_subscription_map(
    direct_sources: &[Source],
    folder_channels: &[(String, i64)],
) -> HashMap<i64, Vec<String>> {
    let mut map: HashMap<i64, Vec<String>> = HashMap::new();

    // Add direct channel/group sources by their tg_id
    for source in direct_sources {
        if let Some(tg_id) = source.tg_id {
            map.entry(tg_id).or_default().push(source.id.clone());
        }
    }

    // Add folder channel entries
    for (source_id, channel_tg_id) in folder_channels {
        map.entry(*channel_tg_id).or_default().push(source_id.clone());
    }

    map
}

/// Mark Telegram channels/groups as read up to the latest message included in a generation.
/// This is the ONLY write operation pail performs on Telegram
/// (see docs/specs/telegram.md "Read-Only Contract" and "Mark-as-Read").
/// Best-effort: failures are logged but never fail the generation pipeline.
pub async fn mark_channels_as_read(client: &Client, pool: &SqlitePool, items: &[ContentItem]) {
    // Group TG content items by chat_id and find the max message_id per chat
    let mut max_msg_per_chat: HashMap<i64, i32> = HashMap::new();
    for item in items {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&item.metadata)
            && let (Some(chat_id), Some(msg_id)) = (
                meta.get("chat_id").and_then(|v| v.as_i64()),
                meta.get("message_id").and_then(|v| v.as_i64()),
            )
        {
            let entry = max_msg_per_chat.entry(chat_id).or_default();
            *entry = (*entry).max(msg_id as i32);
        }
    }

    if max_msg_per_chat.is_empty() {
        return;
    }

    info!(chats = max_msg_per_chat.len(), "marking Telegram channels as read");

    for (&chat_id, &max_id) in &max_msg_per_chat {
        // Resolve peer kind and access hash from the cache
        let peer_ref = match crate::fetch_tg::resolve_peer_ref(pool, chat_id).await {
            Ok(pr) => pr,
            Err(e) => {
                warn!(chat_id, error = %e, "failed to resolve peer for mark-as-read");
                continue;
            }
        };

        let is_channel = matches!(peer_ref.id.kind(), grammers_session::types::PeerKind::Channel);

        if is_channel {
            let access_hash = peer_ref.auth.hash();
            let request = tl::functions::channels::ReadHistory {
                channel: tl::enums::InputChannel::Channel(tl::types::InputChannel {
                    channel_id: chat_id,
                    access_hash,
                }),
                max_id,
            };
            match client.invoke(&request).await {
                Ok(_) => debug!(chat_id, max_id, "marked channel as read"),
                Err(e) => warn!(chat_id, max_id, error = %e, "failed to mark channel as read"),
            }
        } else {
            let request = tl::functions::messages::ReadHistory {
                peer: tl::enums::InputPeer::Chat(tl::types::InputPeerChat { chat_id }),
                max_id,
            };
            match client.invoke(&request).await {
                Ok(_) => debug!(chat_id, max_id, "marked group as read"),
                Err(e) => warn!(chat_id, max_id, error = %e, "failed to mark group as read"),
            }
        }
    }
}

// ─── Types and functions for the config editor TUI ───

/// Chat type classification for TUI display.
/// Note: grammers maps both basic groups and supergroups to `Peer::Group`,
/// so we only distinguish Channel (broadcast) vs Group (everything else).
#[derive(Clone)]
pub enum TgChatType {
    Channel,
    Group,
}

impl TgChatType {
    /// The config file `type` value for this chat type.
    pub fn config_type(&self) -> &'static str {
        match self {
            TgChatType::Channel => "telegram_channel",
            TgChatType::Group => "telegram_group",
        }
    }
}

impl std::fmt::Display for TgChatType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TgChatType::Channel => write!(f, "Channel"),
            TgChatType::Group => write!(f, "Group"),
        }
    }
}

/// A Telegram dialog (channel/group) for TUI selection.
#[derive(Clone)]
pub struct TgDialog {
    pub name: String,
    pub chat_type: TgChatType,
    pub username: Option<String>,
    pub tg_id: i64,
}

/// A Telegram folder with its resolved channel list.
pub struct TgFolder {
    pub name: String,
    pub channels: Vec<TgDialog>,
}

/// List all non-DM dialogs from the user's Telegram account.
pub async fn list_dialogs(client: &Client) -> Result<Vec<TgDialog>> {
    let mut result = Vec::new();
    let mut dialogs = client.iter_dialogs();

    while let Some(dialog) = dialogs.next().await.context("iterating dialogs")? {
        let peer = dialog.peer();

        let (chat_type, tg_id) = match peer {
            ClientPeer::User(_) => continue, // Skip DMs/bots/self
            ClientPeer::Channel(_) => (TgChatType::Channel, peer.id().bare_id()),
            ClientPeer::Group(_) => (TgChatType::Group, peer.id().bare_id()),
        };

        result.push(TgDialog {
            name: peer.name().unwrap_or("(unnamed)").to_string(),
            chat_type,
            username: peer.username().map(|s| s.to_string()),
            tg_id,
        });
    }

    Ok(result)
}

/// List all Telegram folders with their resolved channel contents.
pub async fn list_folders(client: &Client) -> Result<Vec<TgFolder>> {
    let request = tl::functions::messages::GetDialogFilters {};
    let result = client.invoke(&request).await.context("fetching dialog filters")?;

    let filters = match result {
        tl::enums::messages::DialogFilters::Filters(f) => f.filters,
    };

    let mut folders = Vec::new();

    for filter in &filters {
        let (title, pinned_peers, included_peers) = match filter {
            tl::enums::DialogFilter::Filter(df) => {
                let title = extract_filter_title(&df.title);
                (title, &df.pinned_peers, &df.include_peers)
            }
            tl::enums::DialogFilter::Chatlist(df) => {
                let title = extract_filter_title(&df.title);
                (title, &df.pinned_peers, &df.include_peers)
            }
            _ => continue,
        };

        let title = match title {
            Some(t) => t,
            None => continue,
        };

        let all_peers: Vec<&tl::enums::InputPeer> = pinned_peers.iter().chain(included_peers.iter()).collect();
        let channel_info = batch_resolve_channels(client, &all_peers).await;

        let mut channels = Vec::new();
        for peer in &all_peers {
            let tg_id = match peer {
                tl::enums::InputPeer::Channel(c) => c.channel_id,
                tl::enums::InputPeer::Chat(c) => c.chat_id,
                tl::enums::InputPeer::User(_) => continue, // skip DMs in folders
                _ => continue,
            };

            let (name, username) = channel_info.get(&tg_id).cloned().unwrap_or((None, None));

            // Determine chat type from peer kind
            let chat_type = match peer {
                tl::enums::InputPeer::Channel(_) => {
                    // We can't easily distinguish channel vs supergroup from InputPeer alone,
                    // but the batch_resolve_channels gave us Chat::Channel which has the flag.
                    // Default to Channel — the config uses telegram_folder anyway.
                    TgChatType::Channel
                }
                tl::enums::InputPeer::Chat(_) => TgChatType::Group,
                _ => continue,
            };

            channels.push(TgDialog {
                name: name.unwrap_or_else(|| format!("Unknown ({})", tg_id)),
                chat_type,
                username,
                tg_id,
            });
        }

        folders.push(TgFolder { name: title, channels });
    }

    // Synthesize an "Archived" folder from archived dialogs (folder_id=1)
    let archived = fetch_archived_folder(client).await;
    if let Ok(folder) = archived
        && !folder.channels.is_empty()
    {
        folders.push(folder);
    }

    Ok(folders)
}

/// Fetch archived dialogs (folder_id=1) and return them as a synthetic "Archived" TgFolder.
async fn fetch_archived_folder(client: &Client) -> Result<TgFolder> {
    let mut channels = Vec::new();
    // Paginate through archived dialogs
    let mut offset_date = 0;
    let mut offset_id = 0;
    let mut offset_peer = tl::enums::InputPeer::Empty;
    let mut exclude_pinned = false;

    loop {
        let request = tl::functions::messages::GetDialogs {
            exclude_pinned,
            folder_id: Some(1),
            offset_date,
            offset_id,
            offset_peer: offset_peer.clone(),
            limit: 100,
            hash: 0,
        };

        let (dialogs, messages, users, chats) = match client.invoke(&request).await? {
            tl::enums::messages::Dialogs::Dialogs(d) => (d.dialogs, d.messages, d.users, d.chats),
            tl::enums::messages::Dialogs::Slice(d) => (d.dialogs, d.messages, d.users, d.chats),
            tl::enums::messages::Dialogs::NotModified(_) => break,
        };

        // Build a map of chat_id -> (name, username) from the chats array
        let mut chat_map: HashMap<i64, (String, Option<String>)> = HashMap::new();
        for chat in &chats {
            match chat {
                tl::enums::Chat::Channel(c) => {
                    chat_map.insert(c.id, (c.title.clone(), c.username.clone()));
                }
                tl::enums::Chat::Chat(c) => {
                    chat_map.insert(c.id, (c.title.clone(), None));
                }
                _ => {}
            }
        }

        let is_final = dialogs.len() < 100;

        for dialog_enum in &dialogs {
            let peer = match dialog_enum {
                tl::enums::Dialog::Dialog(d) => &d.peer,
                _ => continue,
            };

            let (chat_type, tg_id) = match peer {
                tl::enums::Peer::User(_) => continue,
                tl::enums::Peer::Channel(c) => (TgChatType::Channel, c.channel_id),
                tl::enums::Peer::Chat(c) => (TgChatType::Group, c.chat_id),
            };

            let (name, username) = chat_map
                .get(&tg_id)
                .cloned()
                .unwrap_or_else(|| (format!("Unknown ({tg_id})"), None));

            channels.push(TgDialog {
                name,
                chat_type,
                username,
                tg_id,
            });
        }

        if is_final || dialogs.is_empty() {
            break;
        }

        // Update pagination offsets
        exclude_pinned = true;
        if let Some(last_msg) = messages.last() {
            match last_msg {
                tl::enums::Message::Message(m) => {
                    offset_date = m.date;
                    offset_id = m.id;
                }
                tl::enums::Message::Service(m) => {
                    offset_date = m.date;
                    offset_id = m.id;
                }
                _ => break,
            }
        }
        if let Some(tl::enums::Dialog::Dialog(d)) = dialogs.last() {
            offset_peer = match &d.peer {
                tl::enums::Peer::User(u) => {
                    // Find access_hash from users
                    let access_hash = users.iter().find_map(|user| match user {
                        tl::enums::User::User(u2) if u2.id == u.user_id => u2.access_hash,
                        _ => None,
                    });
                    tl::enums::InputPeer::User(tl::types::InputPeerUser {
                        user_id: u.user_id,
                        access_hash: access_hash.unwrap_or(0),
                    })
                }
                tl::enums::Peer::Chat(c) => tl::enums::InputPeer::Chat(tl::types::InputPeerChat { chat_id: c.chat_id }),
                tl::enums::Peer::Channel(c) => {
                    let access_hash = chats.iter().find_map(|chat| match chat {
                        tl::enums::Chat::Channel(ch) if ch.id == c.channel_id => Some(ch.access_hash.unwrap_or(0)),
                        _ => None,
                    });
                    tl::enums::InputPeer::Channel(tl::types::InputPeerChannel {
                        channel_id: c.channel_id,
                        access_hash: access_hash.unwrap_or(0),
                    })
                }
            };
        }
    }

    Ok(TgFolder {
        name: "Archived".to_string(),
        channels,
    })
}

/// Fetch the "about" description for a channel or group.
/// For channels, needs a username to resolve the access_hash.
/// For groups (basic chats), only needs the chat_id.
/// Returns None if the chat doesn't have a description or on error.
pub async fn fetch_chat_about(client: &Client, dialog: &TgDialog) -> Option<String> {
    let result = match dialog.chat_type {
        TgChatType::Channel => {
            // Channels need access_hash; resolve via username if available
            let username = dialog.username.as_deref()?;
            let peer = client.resolve_username(username).await.ok()?;
            let input_channel = match peer {
                Some(ClientPeer::Channel(ch)) => {
                    let peer_ref = ch.to_ref().await?;
                    let input_peer: tl::enums::InputPeer = (&peer_ref).into();
                    match input_peer {
                        tl::enums::InputPeer::Channel(c) => tl::enums::InputChannel::Channel(tl::types::InputChannel {
                            channel_id: c.channel_id,
                            access_hash: c.access_hash,
                        }),
                        _ => return None,
                    }
                }
                // Supergroups also appear as Channel in resolve_username
                _ => return None,
            };
            let request = tl::functions::channels::GetFullChannel { channel: input_channel };
            client.invoke(&request).await.ok()
        }
        TgChatType::Group => {
            let request = tl::functions::messages::GetFullChat { chat_id: dialog.tg_id };
            client.invoke(&request).await.ok()
        }
    };

    let response = result?;
    let tl::enums::messages::ChatFull::Full(full) = response;
    let about = match full.full_chat {
        tl::enums::ChatFull::Full(f) => f.about,
        tl::enums::ChatFull::ChannelFull(f) => f.about,
    };

    if about.is_empty() { None } else { Some(about) }
}

/// Extract the text title from a TextWithEntities enum.
fn extract_filter_title(title: &tl::enums::TextWithEntities) -> Option<String> {
    match title {
        tl::enums::TextWithEntities::Entities(t) => Some(t.text.clone()),
    }
}

/// Batch-resolve channel InputPeers to (name, username) via a single getChannels call.
/// Returns a map of channel_id -> (name, username). Non-channel peers are not included.
async fn batch_resolve_channels(
    client: &Client,
    peers: &[&tl::enums::InputPeer],
) -> HashMap<i64, (Option<String>, Option<String>)> {
    let mut result = HashMap::new();

    // Collect channel InputChannels for batching
    let input_channels: Vec<tl::enums::InputChannel> = peers
        .iter()
        .filter_map(|peer| {
            if let tl::enums::InputPeer::Channel(c) = peer {
                Some(tl::enums::InputChannel::Channel(tl::types::InputChannel {
                    channel_id: c.channel_id,
                    access_hash: c.access_hash,
                }))
            } else {
                None
            }
        })
        .collect();

    if input_channels.is_empty() {
        return result;
    }

    let request = tl::functions::channels::GetChannels { id: input_channels };
    let chats = match client.invoke(&request).await {
        Ok(tl::enums::messages::Chats::Chats(c)) => c.chats,
        Ok(tl::enums::messages::Chats::Slice(c)) => c.chats,
        Err(e) => {
            warn!(error = %e, "failed to batch-resolve channel peers");
            return result;
        }
    };

    for chat in &chats {
        if let tl::enums::Chat::Channel(ch) = chat {
            result.insert(ch.id, (Some(ch.title.clone()), ch.username.clone()));
        }
    }

    result
}

/// Cache the access hash from an InputPeer into tg_peer_info.
///
/// Folder definitions contain InputPeers with valid access_hashes, but grammers'
/// raw `invoke` doesn't auto-cache peers from RPC responses. Without this, subsequent
/// getHistory calls fail with CHANNEL_INVALID because the access_hash is missing.
async fn cache_input_peer(pool: &SqlitePool, peer: &tl::enums::InputPeer) {
    let (peer_id, access_hash) = match peer {
        tl::enums::InputPeer::Channel(c) => (PeerId::channel(c.channel_id), c.access_hash),
        tl::enums::InputPeer::User(u) => (PeerId::user(u.user_id), u.access_hash),
        _ => return, // Basic chats don't have access hashes
    };

    let bot_api_id = peer_id.bot_api_dialog_id();
    if let Err(e) = sqlx::query(
        "INSERT INTO tg_peer_info (peer_id, hash) VALUES (?, ?)
         ON CONFLICT(peer_id) DO UPDATE SET hash = COALESCE(excluded.hash, tg_peer_info.hash)",
    )
    .bind(bot_api_id)
    .bind(access_hash)
    .execute(pool)
    .await
    {
        warn!(error = %e, peer_id = bot_api_id, "failed to cache input peer");
    }
}

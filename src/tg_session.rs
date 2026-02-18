//! Custom grammers `Session` trait implementation backed by pail's sqlx `SqlitePool`.
//!
//! grammers' built-in `SqliteSession` uses `libsql` (a sqlite3 fork by Turso), which
//! statically bundles its own sqlite3 via `libsql-ffi`. pail uses `sqlx` with
//! `libsqlite3-sys` (upstream sqlite3). Both produce the same symbols, causing duplicate
//! symbol linker errors. This module reimplements the Session trait using sqlx to avoid
//! the conflict entirely. See docs/specs/telegram.md "Session Management" for full details.

use std::collections::HashMap;
use std::sync::Mutex;

use futures_core::future::BoxFuture;
use grammers_session::Session;
use grammers_session::types::{
    ChannelKind, ChannelState, DcOption, PeerAuth, PeerId, PeerInfo, PeerKind, UpdateState, UpdatesState,
};
use sqlx::SqlitePool;
use tracing::warn;

/// Default home DC (DC 2, same as grammers' default).
const DEFAULT_DC: i32 = 2;

/// Hardcoded known DC options (same as grammers' KNOWN_DC_OPTIONS).
const KNOWN_DC_OPTIONS: [DcOption; 5] = [
    DcOption {
        id: 1,
        ipv4: std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(149, 154, 175, 53), 443),
        ipv6: std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::new(0x2001, 0xb28, 0xf23d, 0xf001, 0, 0, 0, 0xa),
            443,
            0,
            0,
        ),
        auth_key: None,
    },
    DcOption {
        id: 2,
        ipv4: std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(149, 154, 167, 41), 443),
        ipv6: std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::new(0x2001, 0x67c, 0x4e8, 0xf002, 0, 0, 0, 0xa),
            443,
            0,
            0,
        ),
        auth_key: None,
    },
    DcOption {
        id: 3,
        ipv4: std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(149, 154, 175, 100), 443),
        ipv6: std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::new(0x2001, 0xb28, 0xf23d, 0xf003, 0, 0, 0, 0xa),
            443,
            0,
            0,
        ),
        auth_key: None,
    },
    DcOption {
        id: 4,
        ipv4: std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(149, 154, 167, 92), 443),
        ipv6: std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::new(0x2001, 0x67c, 0x4e8, 0xf004, 0, 0, 0, 0xa),
            443,
            0,
            0,
        ),
        auth_key: None,
    },
    DcOption {
        id: 5,
        ipv4: std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(91, 108, 56, 104), 443),
        ipv6: std::net::SocketAddrV6::new(
            std::net::Ipv6Addr::new(0x2001, 0xb28, 0xf23f, 0xf005, 0, 0, 0, 0xa),
            443,
            0,
            0,
        ),
        auth_key: None,
    },
];

/// In-memory cache for values that must be read synchronously (home_dc_id, dc_option).
struct Cache {
    home_dc: i32,
    dc_options: HashMap<i32, DcOption>,
}

/// Peer subtype flags (matches grammers' internal representation).
#[repr(u8)]
enum PeerSubtype {
    UserSelf = 1,
    UserBot = 2,
    UserSelfBot = 3,
    Megagroup = 4,
    Broadcast = 8,
    Gigagroup = 12,
}

/// Custom grammers Session backed by pail's sqlx SqlitePool.
pub struct SqlxSession {
    pool: SqlitePool,
    cache: Mutex<Cache>,
}

impl SqlxSession {
    /// Load or initialize a session from the database.
    /// The tg_* tables must already exist (created by the Phase 2 migration).
    pub async fn load(pool: SqlitePool) -> anyhow::Result<Self> {
        // Load home DC from DB, default to DC 2
        let home_dc: i32 = sqlx::query_scalar("SELECT dc_id FROM tg_dc_home LIMIT 1")
            .fetch_optional(&pool)
            .await?
            .unwrap_or(DEFAULT_DC);

        // Load DC options from DB
        let rows = sqlx::query_as::<_, (i32, String, String, Option<Vec<u8>>)>(
            "SELECT dc_id, ipv4, ipv6, auth_key FROM tg_dc_option",
        )
        .fetch_all(&pool)
        .await?;

        let mut dc_options = HashMap::new();
        for (dc_id, ipv4_str, ipv6_str, auth_key_bytes) in rows {
            let ipv4 = ipv4_str.parse().unwrap_or_else(|_| {
                warn!(dc_id, ipv4 = %ipv4_str, "invalid IPv4 in tg_dc_option, using default");
                std::net::SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, 443)
            });
            let ipv6 = ipv6_str.parse().unwrap_or_else(|_| {
                warn!(dc_id, ipv6 = %ipv6_str, "invalid IPv6 in tg_dc_option, using default");
                std::net::SocketAddrV6::new(std::net::Ipv6Addr::UNSPECIFIED, 443, 0, 0)
            });
            let auth_key = auth_key_bytes.and_then(|bytes| {
                let arr: Result<[u8; 256], _> = bytes.try_into();
                arr.ok()
            });
            dc_options.insert(
                dc_id,
                DcOption {
                    id: dc_id,
                    ipv4,
                    ipv6,
                    auth_key,
                },
            );
        }

        Ok(Self {
            pool,
            cache: Mutex::new(Cache { home_dc, dc_options }),
        })
    }
}

impl Session for SqlxSession {
    fn home_dc_id(&self) -> i32 {
        self.cache.lock().unwrap().home_dc
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, ()> {
        self.cache.lock().unwrap().home_dc = dc_id;
        Box::pin(async move {
            if let Err(e) = sqlx::query("DELETE FROM tg_dc_home").execute(&self.pool).await {
                warn!(error = %e, "failed to clear tg_dc_home");
            }
            if let Err(e) = sqlx::query("INSERT INTO tg_dc_home (dc_id) VALUES (?)")
                .bind(dc_id)
                .execute(&self.pool)
                .await
            {
                warn!(error = %e, dc_id, "failed to persist home DC");
            }
        })
    }

    fn dc_option(&self, dc_id: i32) -> Option<DcOption> {
        self.cache
            .lock()
            .unwrap()
            .dc_options
            .get(&dc_id)
            .cloned()
            .or_else(|| KNOWN_DC_OPTIONS.iter().find(|o| o.id == dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, ()> {
        self.cache
            .lock()
            .unwrap()
            .dc_options
            .insert(dc_option.id, dc_option.clone());
        let dc_option = dc_option.clone();
        Box::pin(async move {
            let auth_key_bytes = dc_option.auth_key.map(|k| k.to_vec());
            if let Err(e) =
                sqlx::query("INSERT OR REPLACE INTO tg_dc_option (dc_id, ipv4, ipv6, auth_key) VALUES (?, ?, ?, ?)")
                    .bind(dc_option.id)
                    .bind(dc_option.ipv4.to_string())
                    .bind(dc_option.ipv6.to_string())
                    .bind(auth_key_bytes)
                    .execute(&self.pool)
                    .await
            {
                warn!(error = %e, dc_id = dc_option.id, "failed to persist DC option");
            }
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Option<PeerInfo>> {
        Box::pin(async move {
            let row = if peer.kind() == PeerKind::UserSelf {
                match sqlx::query_as::<_, (i64, Option<i64>, Option<i64>)>(
                    "SELECT peer_id, hash, subtype FROM tg_peer_info WHERE subtype & ? != 0 LIMIT 1",
                )
                .bind(PeerSubtype::UserSelf as i64)
                .fetch_optional(&self.pool)
                .await
                {
                    Ok(row) => row,
                    Err(e) => {
                        warn!(error = %e, "failed to query self peer");
                        None
                    }
                }
            } else {
                match sqlx::query_as::<_, (i64, Option<i64>, Option<i64>)>(
                    "SELECT peer_id, hash, subtype FROM tg_peer_info WHERE peer_id = ? LIMIT 1",
                )
                .bind(peer.bot_api_dialog_id())
                .fetch_optional(&self.pool)
                .await
                {
                    Ok(row) => row,
                    Err(e) => {
                        warn!(error = %e, peer_id = peer.bot_api_dialog_id(), "failed to query peer");
                        None
                    }
                }
            };

            row.map(|(peer_id_val, hash, subtype)| {
                let subtype_u8 = subtype.map(|s| s as u8);
                match peer.kind() {
                    PeerKind::User | PeerKind::UserSelf => PeerInfo::User {
                        id: PeerId::user(peer_id_val).bare_id(),
                        auth: hash.map(PeerAuth::from_hash),
                        bot: subtype_u8.map(|s| s & PeerSubtype::UserBot as u8 != 0),
                        is_self: subtype_u8.map(|s| s & PeerSubtype::UserSelf as u8 != 0),
                    },
                    PeerKind::Chat => PeerInfo::Chat { id: peer.bare_id() },
                    PeerKind::Channel => PeerInfo::Channel {
                        id: peer.bare_id(),
                        auth: hash.map(PeerAuth::from_hash),
                        kind: subtype_u8.and_then(|s| {
                            if (s & PeerSubtype::Gigagroup as u8) == PeerSubtype::Gigagroup as u8 {
                                Some(ChannelKind::Gigagroup)
                            } else if s & PeerSubtype::Broadcast as u8 != 0 {
                                Some(ChannelKind::Broadcast)
                            } else if s & PeerSubtype::Megagroup as u8 != 0 {
                                Some(ChannelKind::Megagroup)
                            } else {
                                None
                            }
                        }),
                    },
                }
            })
        })
    }

    fn cache_peer(&self, peer: &PeerInfo) -> BoxFuture<'_, ()> {
        let peer = peer.clone();
        Box::pin(async move {
            let subtype: Option<i64> = match &peer {
                PeerInfo::User { bot, is_self, .. } => match (bot.unwrap_or_default(), is_self.unwrap_or_default()) {
                    (true, true) => Some(PeerSubtype::UserSelfBot as i64),
                    (true, false) => Some(PeerSubtype::UserBot as i64),
                    (false, true) => Some(PeerSubtype::UserSelf as i64),
                    (false, false) => None,
                },
                PeerInfo::Chat { .. } => None,
                PeerInfo::Channel { kind, .. } => kind.map(|kind| match kind {
                    ChannelKind::Megagroup => PeerSubtype::Megagroup as i64,
                    ChannelKind::Broadcast => PeerSubtype::Broadcast as i64,
                    ChannelKind::Gigagroup => PeerSubtype::Gigagroup as i64,
                }),
            };

            let peer_id = peer.id().bot_api_dialog_id();
            let hash: Option<i64> = peer.auth().map(|a| a.hash());

            if let Err(e) = sqlx::query("INSERT OR REPLACE INTO tg_peer_info (peer_id, hash, subtype) VALUES (?, ?, ?)")
                .bind(peer_id)
                .bind(hash)
                .bind(subtype)
                .execute(&self.pool)
                .await
            {
                warn!(error = %e, peer_id, "failed to cache peer");
            }
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, UpdatesState> {
        Box::pin(async move {
            let primary = match sqlx::query_as::<_, (i32, i32, i32, i32)>(
                "SELECT pts, qts, date, seq FROM tg_update_state LIMIT 1",
            )
            .fetch_optional(&self.pool)
            .await
            {
                Ok(row) => row,
                Err(e) => {
                    warn!(error = %e, "failed to load update state");
                    None
                }
            };

            let mut state = match primary {
                Some((pts, qts, date, seq)) => UpdatesState {
                    pts,
                    qts,
                    date,
                    seq,
                    channels: Vec::new(),
                },
                None => UpdatesState::default(),
            };

            let channels = match sqlx::query_as::<_, (i64, i32)>("SELECT peer_id, pts FROM tg_channel_state")
                .fetch_all(&self.pool)
                .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    warn!(error = %e, "failed to load channel states");
                    Vec::new()
                }
            };

            state.channels = channels.into_iter().map(|(id, pts)| ChannelState { id, pts }).collect();

            state
        })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            match update {
                UpdateState::All(updates_state) => {
                    if let Err(e) = sqlx::query("DELETE FROM tg_update_state").execute(&self.pool).await {
                        warn!(error = %e, "failed to clear update state");
                    }
                    if let Err(e) = sqlx::query("INSERT INTO tg_update_state (pts, qts, date, seq) VALUES (?, ?, ?, ?)")
                        .bind(updates_state.pts)
                        .bind(updates_state.qts)
                        .bind(updates_state.date)
                        .bind(updates_state.seq)
                        .execute(&self.pool)
                        .await
                    {
                        warn!(error = %e, "failed to persist update state");
                    }

                    if let Err(e) = sqlx::query("DELETE FROM tg_channel_state").execute(&self.pool).await {
                        warn!(error = %e, "failed to clear channel states");
                    }
                    for channel in updates_state.channels {
                        if let Err(e) = sqlx::query("INSERT INTO tg_channel_state (peer_id, pts) VALUES (?, ?)")
                            .bind(channel.id)
                            .bind(channel.pts)
                            .execute(&self.pool)
                            .await
                        {
                            warn!(error = %e, peer_id = channel.id, "failed to persist channel state");
                        }
                    }
                }
                UpdateState::Primary { pts, date, seq } => {
                    let exists = sqlx::query_scalar::<_, i32>("SELECT 1 FROM tg_update_state LIMIT 1")
                        .fetch_optional(&self.pool)
                        .await
                        .unwrap_or(None)
                        .is_some();

                    let result = if exists {
                        sqlx::query("UPDATE tg_update_state SET pts = ?, date = ?, seq = ?")
                            .bind(pts)
                            .bind(date)
                            .bind(seq)
                            .execute(&self.pool)
                            .await
                    } else {
                        sqlx::query("INSERT INTO tg_update_state (pts, qts, date, seq) VALUES (?, 0, ?, ?)")
                            .bind(pts)
                            .bind(date)
                            .bind(seq)
                            .execute(&self.pool)
                            .await
                    };
                    if let Err(e) = result {
                        warn!(error = %e, "failed to persist primary update state");
                    }
                }
                UpdateState::Secondary { qts } => {
                    let exists = sqlx::query_scalar::<_, i32>("SELECT 1 FROM tg_update_state LIMIT 1")
                        .fetch_optional(&self.pool)
                        .await
                        .unwrap_or(None)
                        .is_some();

                    let result = if exists {
                        sqlx::query("UPDATE tg_update_state SET qts = ?")
                            .bind(qts)
                            .execute(&self.pool)
                            .await
                    } else {
                        sqlx::query("INSERT INTO tg_update_state (pts, qts, date, seq) VALUES (0, ?, 0, 0)")
                            .bind(qts)
                            .execute(&self.pool)
                            .await
                    };
                    if let Err(e) = result {
                        warn!(error = %e, "failed to persist secondary update state");
                    }
                }
                UpdateState::Channel { id, pts } => {
                    if let Err(e) = sqlx::query("INSERT OR REPLACE INTO tg_channel_state (peer_id, pts) VALUES (?, ?)")
                        .bind(id)
                        .bind(pts)
                        .execute(&self.pool)
                        .await
                    {
                        warn!(error = %e, peer_id = id, "failed to persist channel state");
                    }
                }
            }
        })
    }
}

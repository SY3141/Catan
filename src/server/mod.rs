mod protocol;
mod redis_multiplayer;
mod session;
mod traits;

pub use protocol::{ClientMsg, MultiplayerChatMessage, SearchBudget, ServerMsg};
pub use session::GameSession;
pub use traits::GamePresenter;

use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    env,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{
    Router,
    extract::{
        Path,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::{Html, Redirect},
};
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};
use tokio_postgres::{Row, types::ToSql};
use tower_http::services::ServeDir;

use crate::eval::Evaluator;
use crate::game::Game;
use crate::game_log::GameLog;
use crate::mcts::Config;
use protocol::{
    MultiplayerLobbyRoom, MultiplayerPlayer, MultiplayerSpectator, ReplayEntry, ViewTarget,
};
use redis_multiplayer::{ActiveRedisMultiplayerRoom, RedisMultiplayerRoomStore};

/// Send a progress snapshot every N simulations.
const PROGRESS_INTERVAL: u32 = 100;
const SEARCH_PROGRESS_KEEPALIVE_MS: u64 = 5_000;
const SEARCH_INTERRUPT_INTERVAL: u32 = 8;
const MULTIPLAYER_ANALYSIS_SIMS: u32 = 200;
const MANAGED_BOT_LOBBY_REFRESH_MS: u64 = 15 * 60_000;
const MANAGED_BOT_SEARCH_DEPTH: u32 = 9;
const MANAGED_BOT_SEARCH_SIMULATIONS: u32 = 550;
const MULTIPLAYER_CHAT_HISTORY_LIMIT: usize = 100;
const MULTIPLAYER_CHAT_TEXT_LIMIT: usize = 280;
const ALL_REPLAYS_PAGE_LIMIT: usize = 500;
const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 64 * 1024;
const BOARD_FINGERPRINT_RETRIES: usize = 64;
const DATABASE_URL_ENV: &str = "DATABASE_URL";
const REPLAY_STORE_REQUIRED_ENV: &str = "HEXFISH_REPLAY_STORE_REQUIRED";
const MULTIPLAYER_BACKEND_ENV: &str = "HEXFISH_MULTIPLAYER_BACKEND";
const MULTIPLAYER_REQUIRED_ENV: &str = "HEXFISH_MULTIPLAYER_REQUIRED";
const REDIS_URL_ENV: &str = "REDIS_URL";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
enum ManagedBotLobbySlot {
    FiveThree,
    FifteenOne,
    ThirtyZero,
}

impl ManagedBotLobbySlot {
    const ALL: [Self; 3] = [Self::FiveThree, Self::FifteenOne, Self::ThirtyZero];

    fn time_control(self) -> (u32, u32) {
        match self {
            Self::FiveThree => (5, 3),
            Self::FifteenOne => (15, 1),
            Self::ThirtyZero => (30, 0),
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::FiveThree => "5+3",
            Self::FifteenOne => "15+1",
            Self::ThirtyZero => "30+0",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Default, Eq, PartialEq, Serialize)]
enum SeatOwnerKind {
    #[default]
    Human,
    ManagedBot,
}

fn managed_bot_search_budget() -> SearchBudget {
    SearchBudget::pv_depth_with_simulations(
        MANAGED_BOT_SEARCH_DEPTH,
        MANAGED_BOT_SEARCH_SIMULATIONS,
    )
}

fn managed_bot_user_id(slot: ManagedBotLobbySlot, code: &str) -> String {
    format!("managed-bot:{}:{code}", slot.key())
}

fn managed_bot_display_name() -> String {
    format!("User{:08x}", fastrand::u32(..))
}

#[derive(Clone, Copy)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

pub struct CpuLoadSampler {
    last: Option<CpuTimes>,
}

impl CpuLoadSampler {
    pub fn new() -> Self {
        Self {
            last: read_cpu_times(),
        }
    }

    pub fn sample(&mut self) -> Option<u8> {
        let current = read_cpu_times()?;
        let Some(last) = self.last.replace(current) else {
            return None;
        };
        let total = current.total.saturating_sub(last.total);
        if total == 0 {
            return None;
        }
        let idle = current.idle.saturating_sub(last.idle).min(total);
        let busy = total - idle;
        Some(((busy * 100 + total / 2) / total).min(100) as u8)
    }
}

impl Default for CpuLoadSampler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "linux")]
fn read_cpu_times() -> Option<CpuTimes> {
    let data = std::fs::read_to_string("/proc/stat").ok()?;
    let line = data.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "cpu" {
        return None;
    }

    let values = parts
        .map(|part| part.parse::<u64>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() < 4 {
        return None;
    }

    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    let total = values.iter().sum();
    Some(CpuTimes { idle, total })
}

#[cfg(windows)]
#[repr(C)]
struct FileTime {
    low: u32,
    high: u32,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetSystemTimes(
        idle_time: *mut FileTime,
        kernel_time: *mut FileTime,
        user_time: *mut FileTime,
    ) -> i32;
}

#[cfg(windows)]
fn read_cpu_times() -> Option<CpuTimes> {
    let mut idle = FileTime { low: 0, high: 0 };
    let mut kernel = FileTime { low: 0, high: 0 };
    let mut user = FileTime { low: 0, high: 0 };
    // SAFETY: All pointers refer to valid FILETIME-compatible local structs.
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
    if ok == 0 {
        return None;
    }

    let idle = file_time_to_u64(idle);
    let kernel = file_time_to_u64(kernel);
    let user = file_time_to_u64(user);
    Some(CpuTimes {
        idle,
        total: kernel.saturating_add(user),
    })
}

#[cfg(windows)]
fn file_time_to_u64(file_time: FileTime) -> u64 {
    ((file_time.high as u64) << 32) | file_time.low as u64
}

#[cfg(not(any(target_os = "linux", windows)))]
fn read_cpu_times() -> Option<CpuTimes> {
    None
}

struct LoadedReplay {
    id: String,
    log: GameLog,
    player_names: Option<[String; 2]>,
}

struct ReplayEntryWithAccount {
    account_key: String,
    entry: ReplayEntry,
}

#[derive(Clone, Debug)]
struct ReplayParticipant {
    player: usize,
    account_key: String,
    display_name: Option<String>,
    result: String,
}

#[derive(Clone)]
enum ReplayStore {
    File(FileReplayStore),
    Postgres(PostgresReplayStore),
}

impl ReplayStore {
    fn file(root: PathBuf) -> Self {
        Self::File(FileReplayStore::new(root))
    }

    async fn postgres(database_url: &str) -> Result<Self, String> {
        PostgresReplayStore::connect(database_url)
            .await
            .map(Self::Postgres)
    }

    async fn save_with_result(
        &self,
        account_key: &str,
        session_id: u64,
        counter: u64,
        log: &GameLog,
        result: Option<&str>,
    ) -> Result<ReplayEntry, String> {
        match self {
            Self::File(store) => {
                store.save_with_result(account_key, session_id, counter, log, result)
            }
            Self::Postgres(store) => store.save_with_result(account_key, log, result).await,
        }
    }

    async fn save_multiplayer_with_results(
        &self,
        session_id: u64,
        counter: u64,
        log: &GameLog,
        participants: &[ReplayParticipant],
    ) -> Result<ReplayEntry, String> {
        match self {
            Self::File(store) => {
                store.save_multiplayer_with_results(session_id, counter, log, participants)
            }
            Self::Postgres(store) => store.save_multiplayer_with_results(log, participants).await,
        }
    }

    async fn list(&self, account_key: &str) -> Result<Vec<ReplayEntry>, String> {
        match self {
            Self::File(store) => store.list(account_key),
            Self::Postgres(store) => store.list(account_key).await,
        }
    }

    async fn list_all(&self, limit: usize) -> Result<Vec<ReplayEntryWithAccount>, String> {
        match self {
            Self::File(store) => store.list_all(limit),
            Self::Postgres(store) => store.list_all(limit).await,
        }
    }

    async fn delete(&self, account_key: &str, id: &str) -> Result<(), String> {
        match self {
            Self::File(store) => store.delete(account_key, id),
            Self::Postgres(store) => store.delete(account_key, id).await,
        }
    }

    async fn set_favorite(
        &self,
        account_key: &str,
        id: &str,
        favorite: bool,
    ) -> Result<(), String> {
        match self {
            Self::File(store) => store.set_favorite(account_key, id, favorite),
            Self::Postgres(store) => store.set_favorite(account_key, id, favorite).await,
        }
    }

    async fn load(&self, account_key: &str, id: &str) -> Result<LoadedReplay, String> {
        match self {
            Self::File(store) => Ok(LoadedReplay {
                id: id.to_string(),
                log: store.load(account_key, id)?,
                player_names: store.saved_player_names(account_key, id),
            }),
            Self::Postgres(store) => store.load(account_key, id).await,
        }
    }

    async fn load_shared(&self, slug: &str) -> Result<LoadedReplay, String> {
        match self {
            Self::File(store) => store.load_shared(slug),
            Self::Postgres(store) => store.load_shared(slug).await,
        }
    }
}

#[derive(Clone)]
struct FileReplayStore {
    root: PathBuf,
}

impl FileReplayStore {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn save_with_result(
        &self,
        account_key: &str,
        session_id: u64,
        counter: u64,
        log: &GameLog,
        result: Option<&str>,
    ) -> Result<ReplayEntry, String> {
        self.save_with_metadata(account_key, session_id, counter, log, result, None)
    }

    fn save_with_metadata(
        &self,
        account_key: &str,
        session_id: u64,
        counter: u64,
        log: &GameLog,
        result: Option<&str>,
        player_names: Option<&[String; 2]>,
    ) -> Result<ReplayEntry, String> {
        let saved_at_ms = current_unix_ms();
        let id = format!(
            "{saved_at_ms}-{session_id}-{counter}-{}.log",
            random_base62(16)
        );
        let dir = self.user_dir(account_key);
        std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create replay dir: {e}"))?;
        let path = dir.join(&id);
        log.write_result(&path)
            .map_err(|e| format!("failed to write replay log: {e}"))?;
        let result = normalize_replay_result(result.unwrap_or("incomplete"));
        if result != "incomplete" {
            std::fs::write(self.result_path(account_key, &id), result.as_bytes())
                .map_err(|e| format!("failed to save replay result: {e}"))?;
        }
        if let Some(player_names) = player_names {
            let bytes = serde_json::to_vec(player_names)
                .map_err(|e| format!("failed to encode replay player names: {e}"))?;
            std::fs::write(self.player_names_path(account_key, &id), bytes)
                .map_err(|e| format!("failed to save replay player names: {e}"))?;
        }
        let share_slug = file_share_slug(&id);
        Ok(ReplayEntry {
            id,
            saved_at_ms,
            action_count: log.actions.len(),
            result,
            favorite: false,
            share_slug,
        })
    }

    fn save_multiplayer_with_results(
        &self,
        session_id: u64,
        mut counter: u64,
        log: &GameLog,
        participants: &[ReplayParticipant],
    ) -> Result<ReplayEntry, String> {
        if participants.is_empty() {
            return Err("cannot save multiplayer replay without participants".into());
        }
        let player_names = replay_player_names_from_participants(participants);
        let mut first_entry = None;
        let mut first_error = None;
        for participant in participants {
            match self.save_with_metadata(
                &participant.account_key,
                session_id,
                counter,
                log,
                Some(&participant.result),
                player_names.as_ref(),
            ) {
                Ok(entry) if first_entry.is_none() => first_entry = Some(entry),
                Ok(_) => {}
                Err(message) if first_error.is_none() => first_error = Some(message),
                Err(_) => {}
            }
            counter = counter.saturating_add(1);
        }
        if let Some(entry) = first_entry {
            Ok(entry)
        } else if let Some(message) = first_error {
            Err(message)
        } else {
            Err("failed to save multiplayer replay for any participant".into())
        }
    }

    fn list(&self, account_key: &str) -> Result<Vec<ReplayEntry>, String> {
        let dir = self.user_dir(account_key);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("failed to list replays: {e}")),
        };

        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| format!("failed to read replay entry: {e}"))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !safe_replay_id(id) {
                continue;
            }
            let saved_at_ms = replay_timestamp_from_id(id).unwrap_or_else(current_unix_ms);
            let action_count = count_log_actions(&path).unwrap_or(0);
            out.push(ReplayEntry {
                id: id.to_string(),
                saved_at_ms,
                action_count,
                result: self.saved_result(account_key, id),
                favorite: self.is_favorite(account_key, id),
                share_slug: file_share_slug(id),
            });
        }
        out.sort_by(|a, b| {
            b.saved_at_ms
                .cmp(&a.saved_at_ms)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(out)
    }

    fn list_all(&self, limit: usize) -> Result<Vec<ReplayEntryWithAccount>, String> {
        let accounts = match std::fs::read_dir(&self.root) {
            Ok(accounts) => accounts,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("failed to read replay storage: {e}")),
        };

        let mut out = Vec::new();
        for account in accounts {
            let account = account.map_err(|e| format!("failed to read replay account: {e}"))?;
            let account_path = account.path();
            if !account_path.is_dir() {
                continue;
            }
            let Some(account_key) = account_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            for entry in self.list(&account_key)? {
                out.push(ReplayEntryWithAccount {
                    account_key: account_key.clone(),
                    entry,
                });
            }
        }
        out.sort_by(|a, b| {
            b.entry
                .saved_at_ms
                .cmp(&a.entry.saved_at_ms)
                .then_with(|| b.entry.id.cmp(&a.entry.id))
        });
        out.truncate(limit);
        Ok(out)
    }

    fn delete(&self, account_key: &str, id: &str) -> Result<(), String> {
        if !safe_replay_id(id) {
            return Err("invalid replay id".into());
        }
        let path = self.user_dir(account_key).join(id);
        std::fs::remove_file(&path).map_err(|e| format!("failed to delete replay log: {e}"))?;
        match std::fs::remove_file(self.favorite_path(account_key, id)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("failed to delete replay favourite marker: {e}")),
        }
        match std::fs::remove_file(self.result_path(account_key, id)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("failed to delete replay result marker: {e}")),
        }
        match std::fs::remove_file(self.player_names_path(account_key, id)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("failed to delete replay player names: {e}")),
        }
        Ok(())
    }

    fn set_favorite(&self, account_key: &str, id: &str, favorite: bool) -> Result<(), String> {
        if !safe_replay_id(id) {
            return Err("invalid replay id".into());
        }
        let path = self.user_dir(account_key).join(id);
        if !path.is_file() {
            return Err("replay log not found".into());
        }
        let marker = self.favorite_path(account_key, id);
        if favorite {
            let dir = self.user_dir(account_key);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("failed to create replay dir: {e}"))?;
            std::fs::write(&marker, b"favorite")
                .map_err(|e| format!("failed to save replay favourite: {e}"))?;
        } else {
            match std::fs::remove_file(marker) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("failed to remove replay favourite: {e}")),
            }
        }
        Ok(())
    }

    fn load(&self, account_key: &str, id: &str) -> Result<GameLog, String> {
        if !safe_replay_id(id) {
            return Err("invalid replay id".into());
        }
        let path = self.user_dir(account_key).join(id);
        GameLog::read_result(&path)
    }

    fn load_shared(&self, slug: &str) -> Result<LoadedReplay, String> {
        if !safe_share_slug(slug) {
            return Err("invalid replay share link".into());
        }
        let accounts = match std::fs::read_dir(&self.root) {
            Ok(accounts) => accounts,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err("shared replay not found".into());
            }
            Err(e) => return Err(format!("failed to read replay storage: {e}")),
        };

        for account in accounts {
            let account = account.map_err(|e| format!("failed to read replay account: {e}"))?;
            let account_path = account.path();
            if !account_path.is_dir() {
                continue;
            }
            let Some(account_key) = account_path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let logs = match std::fs::read_dir(&account_path) {
                Ok(logs) => logs,
                Err(_) => continue,
            };
            for log in logs {
                let log = log.map_err(|e| format!("failed to read replay entry: {e}"))?;
                let path = log.path();
                if !path.is_file() {
                    continue;
                }
                let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if safe_replay_id(id) && file_share_slug(id) == slug {
                    return Ok(LoadedReplay {
                        id: id.to_string(),
                        log: GameLog::read_result(&path)?,
                        player_names: self.saved_player_names(&account_key, id),
                    });
                }
            }
        }

        Err("shared replay not found".into())
    }

    fn is_favorite(&self, account_key: &str, id: &str) -> bool {
        self.favorite_path(account_key, id).is_file()
    }

    fn favorite_path(&self, account_key: &str, id: &str) -> PathBuf {
        self.user_dir(account_key).join(format!("{id}.favorite"))
    }

    fn result_path(&self, account_key: &str, id: &str) -> PathBuf {
        self.user_dir(account_key).join(format!("{id}.result"))
    }

    fn player_names_path(&self, account_key: &str, id: &str) -> PathBuf {
        self.user_dir(account_key)
            .join(format!("{id}.players.json"))
    }

    fn saved_result(&self, account_key: &str, id: &str) -> String {
        match std::fs::read_to_string(self.result_path(account_key, id)) {
            Ok(result) => normalize_replay_result(result.trim()),
            Err(_) => "incomplete".into(),
        }
    }

    fn saved_player_names(&self, account_key: &str, id: &str) -> Option<[String; 2]> {
        let bytes = std::fs::read(self.player_names_path(account_key, id)).ok()?;
        let names: Vec<String> = serde_json::from_slice(&bytes).ok()?;
        replay_player_names_from_vec(names)
    }

    fn user_dir(&self, account_key: &str) -> PathBuf {
        self.root.join(account_key)
    }
}

#[derive(Clone)]
struct PostgresReplayStore {
    database_url: Arc<str>,
    client: Arc<Mutex<Option<Arc<tokio_postgres::Client>>>>,
}

impl PostgresReplayStore {
    async fn connect(database_url: &str) -> Result<Self, String> {
        let store = Self {
            database_url: Arc::<str>::from(database_url),
            client: Arc::new(Mutex::new(None)),
        };
        store.reconnect("initialize replay store").await?;
        store.init_schema().await?;
        Ok(store)
    }

    async fn open_client(database_url: &str) -> Result<Arc<tokio_postgres::Client>, String> {
        let tls = native_tls::TlsConnector::builder()
            .build()
            .map_err(|e| format!("failed to configure Postgres TLS: {e}"))?;
        let tls = postgres_native_tls::MakeTlsConnector::new(tls);
        let (client, connection) = tokio_postgres::connect(database_url, tls)
            .await
            .map_err(|e| format!("failed to connect to Postgres replay store: {e}"))?;

        tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::error!(%error, "Postgres replay store connection closed");
            }
        });

        Ok(Arc::new(client))
    }

    async fn client(&self) -> Result<Arc<tokio_postgres::Client>, String> {
        {
            let client = self.client.lock().await;
            if let Some(client) = client.as_ref() {
                if !client.is_closed() {
                    return Ok(Arc::clone(client));
                }
            }
        }

        self.reconnect("refresh closed replay store connection")
            .await
    }

    async fn reconnect(&self, operation: &str) -> Result<Arc<tokio_postgres::Client>, String> {
        let client = Self::open_client(&self.database_url).await?;
        let mut current = self.client.lock().await;
        if let Some(current) = current.as_ref() {
            if !current.is_closed() {
                return Ok(Arc::clone(current));
            }
        }
        *current = Some(Arc::clone(&client));
        tracing::info!(operation, "Postgres replay store connected");
        Ok(client)
    }

    async fn clear_client_if_current(&self, failed: &Arc<tokio_postgres::Client>) {
        let mut current = self.client.lock().await;
        let should_clear = current
            .as_ref()
            .map_or(false, |current| Arc::ptr_eq(current, failed));
        if should_clear {
            *current = None;
        }
    }

    async fn query_rows(
        &self,
        operation: &str,
        statement: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, String> {
        let client = self.client().await?;
        match client.query(statement, params).await {
            Ok(rows) => Ok(rows),
            Err(error) if error.is_closed() => {
                tracing::warn!(%error, operation, "Postgres replay query lost connection; reconnecting");
                self.clear_client_if_current(&client).await;
                let client = self.reconnect(operation).await?;
                client
                    .query(statement, params)
                    .await
                    .map_err(|e| format!("{operation}: {e}"))
            }
            Err(error) => Err(format!("{operation}: {error}")),
        }
    }

    async fn query_opt_row(
        &self,
        operation: &str,
        statement: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Row>, String> {
        let client = self.client().await?;
        match client.query_opt(statement, params).await {
            Ok(row) => Ok(row),
            Err(error) if error.is_closed() => {
                tracing::warn!(%error, operation, "Postgres replay query lost connection; reconnecting");
                self.clear_client_if_current(&client).await;
                let client = self.reconnect(operation).await?;
                client
                    .query_opt(statement, params)
                    .await
                    .map_err(|e| format!("{operation}: {e}"))
            }
            Err(error) => Err(format!("{operation}: {error}")),
        }
    }

    async fn query_opt_row_once(
        &self,
        operation: &str,
        statement: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Option<Row>, String> {
        let client = self.client().await?;
        match client.query_opt(statement, params).await {
            Ok(row) => Ok(row),
            Err(error) if error.is_closed() => {
                self.clear_client_if_current(&client).await;
                Err(format!("{operation}: {error}"))
            }
            Err(error) => Err(format!("{operation}: {error}")),
        }
    }

    async fn execute_once(
        &self,
        operation: &str,
        statement: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<u64, String> {
        let client = self.client().await?;
        match client.execute(statement, params).await {
            Ok(updated) => Ok(updated),
            Err(error) if error.is_closed() => {
                self.clear_client_if_current(&client).await;
                Err(format!("{operation}: {error}"))
            }
            Err(error) => Err(format!("{operation}: {error}")),
        }
    }

    async fn batch_execute(&self, operation: &str, query: &str) -> Result<(), String> {
        let client = self.client().await?;
        match client.batch_execute(query).await {
            Ok(()) => Ok(()),
            Err(error) if error.is_closed() => {
                tracing::warn!(%error, operation, "Postgres replay schema query lost connection; reconnecting");
                self.clear_client_if_current(&client).await;
                let client = self.reconnect(operation).await?;
                client
                    .batch_execute(query)
                    .await
                    .map_err(|e| format!("{operation}: {e}"))
            }
            Err(error) => Err(format!("{operation}: {error}")),
        }
    }

    async fn init_schema(&self) -> Result<(), String> {
        self.batch_execute(
            "failed to initialize Postgres replay schema",
            r#"
                CREATE TABLE IF NOT EXISTS replay_logs (
                    id TEXT PRIMARY KEY,
                    account_key TEXT NOT NULL,
                    share_slug TEXT NOT NULL UNIQUE,
                    initial_state TEXT NOT NULL,
                    actions BIGINT[] NOT NULL,
                    saved_at_ms BIGINT NOT NULL,
                    action_count BIGINT NOT NULL,
                    result TEXT NOT NULL DEFAULT 'incomplete',
                    favorite BOOLEAN NOT NULL DEFAULT FALSE,
                    player_names TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[]
                );
                ALTER TABLE replay_logs
                    ADD COLUMN IF NOT EXISTS result TEXT NOT NULL DEFAULT 'incomplete';
                ALTER TABLE replay_logs
                    ADD COLUMN IF NOT EXISTS favorite BOOLEAN NOT NULL DEFAULT FALSE;
                ALTER TABLE replay_logs
                    ADD COLUMN IF NOT EXISTS player_names TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[];
                CREATE TABLE IF NOT EXISTS replay_log_accounts (
                    replay_id TEXT NOT NULL REFERENCES replay_logs(id) ON DELETE CASCADE,
                    account_key TEXT NOT NULL,
                    player_index INTEGER,
                    display_name TEXT,
                    result TEXT NOT NULL DEFAULT 'incomplete',
                    favorite BOOLEAN NOT NULL DEFAULT FALSE,
                    PRIMARY KEY (replay_id, account_key)
                );
                INSERT INTO replay_log_accounts (
                    replay_id,
                    account_key,
                    player_index,
                    display_name,
                    result,
                    favorite
                )
                SELECT id, account_key, NULL, NULL, result, favorite
                FROM replay_logs
                ON CONFLICT (replay_id, account_key) DO NOTHING;
                CREATE INDEX IF NOT EXISTS replay_logs_account_saved_idx
                    ON replay_logs (account_key, saved_at_ms DESC, id DESC);
                CREATE INDEX IF NOT EXISTS replay_logs_share_slug_idx
                    ON replay_logs (share_slug);
                CREATE INDEX IF NOT EXISTS replay_log_accounts_account_idx
                    ON replay_log_accounts (account_key);
                CREATE INDEX IF NOT EXISTS replay_log_accounts_replay_idx
                    ON replay_log_accounts (replay_id);
                "#,
        )
        .await
    }

    async fn save_with_result(
        &self,
        account_key: &str,
        log: &GameLog,
        result: Option<&str>,
    ) -> Result<ReplayEntry, String> {
        let saved_at_ms = i64::try_from(current_unix_ms())
            .map_err(|_| "current timestamp does not fit in Postgres BIGINT".to_string())?;
        let action_count = i64::try_from(log.actions.len())
            .map_err(|_| "replay action count does not fit in Postgres BIGINT".to_string())?;
        let actions = actions_to_i64(&log.actions)?;
        let result = normalize_replay_result(result.unwrap_or("incomplete"));

        for _ in 0..32 {
            let id = random_base62(24);
            let share_slug = random_base62(18);
            let row = self
                .query_opt_row_once(
                    "failed to save replay log",
                    r#"
                    WITH inserted AS (
                        INSERT INTO replay_logs (
                            id,
                            account_key,
                            share_slug,
                            initial_state,
                            actions,
                            saved_at_ms,
                            action_count,
                            result,
                            favorite
                        )
                        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, FALSE)
                        ON CONFLICT DO NOTHING
                        RETURNING id, saved_at_ms, action_count, share_slug
                    ),
                    saved_account AS (
                        INSERT INTO replay_log_accounts (
                            replay_id,
                            account_key,
                            player_index,
                            display_name,
                            result,
                            favorite
                        )
                        SELECT id, $2, NULL, NULL, $8, FALSE
                        FROM inserted
                        ON CONFLICT (replay_id, account_key) DO NOTHING
                        RETURNING replay_id
                    )
                    SELECT
                        id,
                        saved_at_ms,
                        action_count,
                        $8::TEXT AS result,
                        FALSE AS favorite,
                        share_slug
                    FROM inserted
                    "#,
                    &[
                        &id,
                        &account_key,
                        &share_slug,
                        &log.initial_state,
                        &actions,
                        &saved_at_ms,
                        &action_count,
                        &result,
                    ],
                )
                .await?;

            if let Some(row) = row {
                return replay_entry_from_row(&row);
            }
        }

        Err("failed to generate a unique replay id".into())
    }

    async fn save_multiplayer_with_results(
        &self,
        log: &GameLog,
        participants: &[ReplayParticipant],
    ) -> Result<ReplayEntry, String> {
        let first = participants
            .first()
            .ok_or_else(|| "cannot save multiplayer replay without participants".to_string())?;
        let saved_at_ms = i64::try_from(current_unix_ms())
            .map_err(|_| "current timestamp does not fit in Postgres BIGINT".to_string())?;
        let action_count = i64::try_from(log.actions.len())
            .map_err(|_| "replay action count does not fit in Postgres BIGINT".to_string())?;
        let actions = actions_to_i64(&log.actions)?;
        let player_names = replay_player_names_vec_from_participants(participants);
        let account_keys = participants
            .iter()
            .map(|participant| participant.account_key.clone())
            .collect::<Vec<_>>();
        let player_indices = participants
            .iter()
            .map(|participant| {
                i32::try_from(participant.player)
                    .map_err(|_| "multiplayer player index does not fit in Postgres INTEGER")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let display_names = participants
            .iter()
            .map(|participant| participant.display_name.clone().unwrap_or_default())
            .collect::<Vec<_>>();
        let results = participants
            .iter()
            .map(|participant| normalize_replay_result(&participant.result))
            .collect::<Vec<_>>();
        let first_result = normalize_replay_result(&first.result);

        for _ in 0..32 {
            let id = random_base62(24);
            let share_slug = random_base62(18);
            let row = self
                .query_opt_row_once(
                    "failed to save multiplayer replay log",
                    r#"
                    WITH inserted AS (
                        INSERT INTO replay_logs (
                            id,
                            account_key,
                            share_slug,
                            initial_state,
                            actions,
                            saved_at_ms,
                            action_count,
                            result,
                            favorite,
                            player_names
                        )
                        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, FALSE, $9)
                        ON CONFLICT DO NOTHING
                        RETURNING id, saved_at_ms, action_count, share_slug
                    ),
                    saved_accounts AS (
                        INSERT INTO replay_log_accounts (
                            replay_id,
                            account_key,
                            player_index,
                            display_name,
                            result,
                            favorite
                        )
                        SELECT
                            inserted.id,
                            participant.account_key,
                            participant.player_index,
                            NULLIF(participant.display_name, ''),
                            participant.result,
                            FALSE
                        FROM inserted
                        CROSS JOIN unnest(
                            $10::TEXT[],
                            $11::INTEGER[],
                            $12::TEXT[],
                            $13::TEXT[]
                        ) AS participant(account_key, player_index, display_name, result)
                        ON CONFLICT (replay_id, account_key) DO UPDATE SET
                            player_index = EXCLUDED.player_index,
                            display_name = EXCLUDED.display_name,
                            result = EXCLUDED.result
                        RETURNING account_key, result, favorite
                    )
                    SELECT
                        inserted.id,
                        inserted.saved_at_ms,
                        inserted.action_count,
                        COALESCE(saved_accounts.result, $8::TEXT) AS result,
                        COALESCE(saved_accounts.favorite, FALSE) AS favorite,
                        inserted.share_slug
                    FROM inserted
                    LEFT JOIN saved_accounts ON saved_accounts.account_key = $14
                    "#,
                    &[
                        &id,
                        &first.account_key,
                        &share_slug,
                        &log.initial_state,
                        &actions,
                        &saved_at_ms,
                        &action_count,
                        &first_result,
                        &player_names,
                        &account_keys,
                        &player_indices,
                        &display_names,
                        &results,
                        &first.account_key,
                    ],
                )
                .await?;

            if let Some(row) = row {
                return replay_entry_from_row(&row);
            }
        }

        Err("failed to generate a unique replay id".into())
    }

    async fn list(&self, account_key: &str) -> Result<Vec<ReplayEntry>, String> {
        let rows = self
            .query_rows(
                "failed to list replays",
                r#"
                SELECT
                    replay_logs.id,
                    replay_logs.saved_at_ms,
                    replay_logs.action_count,
                    replay_log_accounts.result,
                    replay_log_accounts.favorite,
                    replay_logs.share_slug
                FROM replay_logs
                JOIN replay_log_accounts
                    ON replay_log_accounts.replay_id = replay_logs.id
                WHERE replay_log_accounts.account_key = $1
                ORDER BY replay_logs.saved_at_ms DESC, replay_logs.id DESC
                "#,
                &[&account_key],
            )
            .await?;

        rows.iter().map(replay_entry_from_row).collect()
    }

    async fn list_all(&self, limit: usize) -> Result<Vec<ReplayEntryWithAccount>, String> {
        let limit = i64::try_from(limit)
            .map_err(|_| "replay list limit does not fit in Postgres BIGINT".to_string())?;
        let rows = self
            .query_rows(
                "failed to list all replays",
                r#"
                SELECT
                    replay_log_accounts.account_key,
                    replay_logs.id,
                    replay_logs.saved_at_ms,
                    replay_logs.action_count,
                    replay_log_accounts.result,
                    replay_log_accounts.favorite,
                    replay_logs.share_slug
                FROM replay_logs
                JOIN replay_log_accounts
                    ON replay_log_accounts.replay_id = replay_logs.id
                ORDER BY replay_logs.saved_at_ms DESC, replay_logs.id DESC
                LIMIT $1
                "#,
                &[&limit],
            )
            .await?;

        rows.iter()
            .map(|row| {
                Ok(ReplayEntryWithAccount {
                    account_key: row.get("account_key"),
                    entry: replay_entry_from_row(row)?,
                })
            })
            .collect()
    }

    async fn delete(&self, account_key: &str, id: &str) -> Result<(), String> {
        if !safe_replay_token(id) {
            return Err("invalid replay id".into());
        }
        let deleted = self
            .execute_once(
                "failed to delete replay access",
                "DELETE FROM replay_log_accounts WHERE account_key = $1 AND replay_id = $2",
                &[&account_key, &id],
            )
            .await?;
        if deleted == 0 {
            return Err("replay log not found".into());
        }
        self.execute_once(
            "failed to delete orphaned replay log",
            r#"
            DELETE FROM replay_logs
            WHERE id = $1
              AND NOT EXISTS (
                  SELECT 1
                  FROM replay_log_accounts
                  WHERE replay_id = $1
              )
            "#,
            &[&id],
        )
        .await?;
        Ok(())
    }

    async fn set_favorite(
        &self,
        account_key: &str,
        id: &str,
        favorite: bool,
    ) -> Result<(), String> {
        if !safe_replay_token(id) {
            return Err("invalid replay id".into());
        }
        let updated = self
            .execute_once(
                "failed to save replay favourite",
                "UPDATE replay_log_accounts SET favorite = $3 WHERE account_key = $1 AND replay_id = $2",
                &[&account_key, &id, &favorite],
            )
            .await?;
        if updated == 0 {
            return Err("replay log not found".into());
        }
        Ok(())
    }

    async fn load(&self, account_key: &str, id: &str) -> Result<LoadedReplay, String> {
        if !safe_replay_token(id) {
            return Err("invalid replay id".into());
        }
        let row = self
            .query_opt_row(
                "failed to read replay log",
                r#"
                SELECT
                    replay_logs.id,
                    replay_logs.initial_state,
                    replay_logs.actions,
                    replay_logs.player_names
                FROM replay_logs
                JOIN replay_log_accounts
                    ON replay_log_accounts.replay_id = replay_logs.id
                WHERE replay_log_accounts.account_key = $1
                  AND replay_logs.id = $2
                "#,
                &[&account_key, &id],
            )
            .await?
            .ok_or_else(|| "replay log not found".to_string())?;
        Ok(LoadedReplay {
            id: row.get("id"),
            log: game_log_from_row(&row)?,
            player_names: replay_player_names_from_row(&row),
        })
    }

    async fn load_shared(&self, slug: &str) -> Result<LoadedReplay, String> {
        if !safe_share_slug(slug) {
            return Err("invalid replay share link".into());
        }
        tracing::info!(share_slug = %slug, "loading shared replay from Postgres");
        let row = self
            .query_opt_row(
                "failed to read shared replay",
                "SELECT id, initial_state, actions, player_names FROM replay_logs WHERE share_slug = $1",
                &[&slug],
            )
            .await?
            .ok_or_else(|| "shared replay not found".to_string())?;
        Ok(LoadedReplay {
            id: row.get("id"),
            log: game_log_from_row(&row)?,
            player_names: replay_player_names_from_row(&row),
        })
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn replay_player_names_vec_from_participants(participants: &[ReplayParticipant]) -> Vec<String> {
    let mut names = vec![String::new(), String::new()];
    for participant in participants {
        if participant.player < names.len() {
            names[participant.player] =
                normalize_display_name(participant.display_name.clone()).unwrap_or_default();
        }
    }
    names
}

fn replay_player_names_from_participants(
    participants: &[ReplayParticipant],
) -> Option<[String; 2]> {
    replay_player_names_from_vec(replay_player_names_vec_from_participants(participants))
}

fn replay_player_names_from_row(row: &Row) -> Option<[String; 2]> {
    let names: Vec<String> = row.get("player_names");
    replay_player_names_from_vec(names)
}

fn replay_player_names_from_vec(names: Vec<String>) -> Option<[String; 2]> {
    let mut out = [String::new(), String::new()];
    for (idx, name) in names.into_iter().take(2).enumerate() {
        out[idx] = normalize_display_name(Some(name)).unwrap_or_default();
    }
    out.iter().any(|name| !name.is_empty()).then_some(out)
}

fn replay_entry_from_row(row: &Row) -> Result<ReplayEntry, String> {
    let saved_at_ms: i64 = row.get("saved_at_ms");
    let action_count: i64 = row.get("action_count");
    let result: String = row.get("result");
    Ok(ReplayEntry {
        id: row.get("id"),
        saved_at_ms: u64::try_from(saved_at_ms)
            .map_err(|_| "stored replay timestamp is negative".to_string())?,
        action_count: usize::try_from(action_count)
            .map_err(|_| "stored replay action count is invalid".to_string())?,
        result: normalize_replay_result(&result),
        favorite: row.get("favorite"),
        share_slug: row.get("share_slug"),
    })
}

fn normalize_replay_result(result: &str) -> String {
    match result.trim() {
        "won" => "won".into(),
        "lost" => "lost".into(),
        "draw" => "draw".into(),
        "won_by_resignation" => "won_by_resignation".into(),
        "lost_by_resignation" => "lost_by_resignation".into(),
        "won_on_time" => "won_on_time".into(),
        "lost_on_time" => "lost_on_time".into(),
        _ => "incomplete".into(),
    }
}

fn game_log_from_row(row: &Row) -> Result<GameLog, String> {
    let actions: Vec<i64> = row.get("actions");
    Ok(GameLog {
        initial_state: row.get("initial_state"),
        actions: actions_from_i64(actions)?,
    })
}

fn actions_to_i64(actions: &[usize]) -> Result<Vec<i64>, String> {
    actions
        .iter()
        .map(|&action| {
            i64::try_from(action).map_err(|_| "replay action id does not fit in BIGINT".to_string())
        })
        .collect()
}

fn actions_from_i64(actions: Vec<i64>) -> Result<Vec<usize>, String> {
    actions
        .into_iter()
        .map(|action| {
            usize::try_from(action).map_err(|_| "stored replay action id is invalid".to_string())
        })
        .collect()
}

const REPLAY_TOKEN_ALPHABET: &[u8] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn random_base62(len: usize) -> String {
    (0..len)
        .map(|_| {
            let idx = fastrand::usize(..REPLAY_TOKEN_ALPHABET.len());
            REPLAY_TOKEN_ALPHABET[idx] as char
        })
        .collect()
}

fn safe_replay_id(id: &str) -> bool {
    !id.is_empty()
        && id.ends_with(".log")
        && !id.starts_with('.')
        && !id.contains("..")
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn safe_replay_token(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn safe_share_slug(slug: &str) -> bool {
    safe_replay_token(slug)
}

fn file_share_slug(id: &str) -> String {
    id.strip_suffix(".log").unwrap_or(id).to_string()
}

fn replay_timestamp_from_id(id: &str) -> Option<u64> {
    id.split('-').next()?.parse().ok()
}

fn count_log_actions(path: &std::path::Path) -> Result<usize, String> {
    let data =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read replay log: {e}"))?;
    Ok(data.lines().skip(1).filter(|line| !line.is_empty()).count())
}

enum SessionTemplate<G: Game> {
    New { replay: Option<(G, Arc<GameLog>)> },
    State(G),
    Timeline(Vec<(String, G)>),
}

struct SessionFactory<G: Game + 'static> {
    evaluator: Arc<dyn Evaluator<G> + Sync>,
    eval_name: String,
    presenter: Arc<dyn GamePresenter<G>>,
    human_players: [bool; 2],
    template: SessionTemplate<G>,
}

impl<G: Game + 'static> SessionFactory<G> {
    fn new_game(
        evaluator: Arc<dyn Evaluator<G> + Sync>,
        eval_name: &str,
        presenter: Arc<dyn GamePresenter<G>>,
        human_players: [bool; 2],
        replay: Option<(G, Arc<GameLog>)>,
    ) -> Self {
        Self {
            evaluator,
            eval_name: eval_name.to_string(),
            presenter,
            human_players,
            template: SessionTemplate::New { replay },
        }
    }

    fn with_state(
        state: G,
        evaluator: Arc<dyn Evaluator<G> + Sync>,
        eval_name: &str,
        presenter: Arc<dyn GamePresenter<G>>,
        human_players: [bool; 2],
    ) -> Self {
        Self {
            evaluator,
            eval_name: eval_name.to_string(),
            presenter,
            human_players,
            template: SessionTemplate::State(state),
        }
    }

    fn with_timeline(
        timeline: Vec<(String, G)>,
        evaluator: Arc<dyn Evaluator<G> + Sync>,
        presenter: Arc<dyn GamePresenter<G>>,
        human_players: [bool; 2],
    ) -> Self {
        Self {
            evaluator,
            eval_name: "unknown".into(),
            presenter,
            human_players,
            template: SessionTemplate::Timeline(timeline),
        }
    }

    fn create_session(&self) -> GameSession<G> {
        self.create_session_with_humans(self.human_players)
    }

    fn create_multiplayer_session(&self) -> GameSession<G> {
        self.create_session_with_humans([true, true])
    }

    fn create_session_with_humans(&self, human_players: [bool; 2]) -> GameSession<G> {
        match &self.template {
            SessionTemplate::New { replay } => {
                let mut session = GameSession::new(
                    Arc::clone(&self.evaluator),
                    self.eval_name.clone(),
                    Arc::clone(&self.presenter),
                    human_players,
                );
                if let Some((state, log)) = replay {
                    session.load_replay(state.clone(), log.as_ref());
                }
                session
            }
            SessionTemplate::State(state) => GameSession::with_state(
                state.clone(),
                Arc::clone(&self.evaluator),
                self.eval_name.clone(),
                Arc::clone(&self.presenter),
                human_players,
                Config::default(),
            ),
            SessionTemplate::Timeline(timeline) => {
                let mut session = GameSession::with_state(
                    timeline[0].1.clone(),
                    Arc::clone(&self.evaluator),
                    self.eval_name.clone(),
                    Arc::clone(&self.presenter),
                    human_players,
                    Config::default(),
                );
                session.load_timeline(timeline.clone());
                session
            }
        }
    }

    fn creates_random_boards(&self) -> bool {
        matches!(&self.template, SessionTemplate::New { replay: None })
    }
}

struct UserSession<G: Game + 'static> {
    session_id: u64,
    seed: u64,
    board_fingerprint: Option<u64>,
    account_key: String,
    factory: Arc<SessionFactory<G>>,
    replay_store: Option<Arc<ReplayStore>>,
    next_replay_counter: AtomicU64,
    session: Arc<Mutex<GameSession<G>>>,
    replay_session: Arc<Mutex<Option<GameSession<G>>>>,
    sockets: StdMutex<HashMap<u64, mpsc::UnboundedSender<String>>>,
    next_socket_id: AtomicU64,
}

impl<G: Game + 'static> UserSession<G> {
    fn new(
        session: GameSession<G>,
        session_id: u64,
        account_key: String,
        factory: Arc<SessionFactory<G>>,
        replay_store: Option<Arc<ReplayStore>>,
    ) -> Self {
        let seed = session.seed();
        let board_fingerprint = session.board_fingerprint();
        Self {
            session_id,
            seed,
            board_fingerprint,
            account_key,
            factory,
            replay_store,
            next_replay_counter: AtomicU64::new(1),
            session: Arc::new(Mutex::new(session)),
            replay_session: Arc::new(Mutex::new(None)),
            sockets: StdMutex::new(HashMap::new()),
            next_socket_id: AtomicU64::new(1),
        }
    }

    fn register_socket(&self, tx: mpsc::UnboundedSender<String>) -> u64 {
        let id = self.next_socket_id.fetch_add(1, Ordering::Relaxed);
        self.sockets
            .lock()
            .expect("socket registry lock poisoned")
            .insert(id, tx);
        id
    }

    fn unregister_socket(&self, id: u64) {
        self.sockets
            .lock()
            .expect("socket registry lock poisoned")
            .remove(&id);
    }

    fn broadcast_game_states_to_others(&self, sender_id: u64, msgs: &[ServerMsg]) {
        let json_msgs: Vec<String> = msgs
            .iter()
            .filter(|msg| matches!(msg, ServerMsg::GameState { .. }))
            .filter_map(|msg| serde_json::to_string(msg).ok())
            .collect();
        if json_msgs.is_empty() {
            return;
        }

        let mut sockets = self.sockets.lock().expect("socket registry lock poisoned");
        sockets.retain(|id, tx| {
            if *id == sender_id {
                return true;
            }
            json_msgs.iter().all(|json| tx.send(json.clone()).is_ok())
        });
    }
}

#[derive(Clone)]
struct SeatOwner {
    user_id: String,
    account_key: String,
    display_name: Option<String>,
    kind: SeatOwnerKind,
}

impl SeatOwner {
    fn human(user_id: &str, display_name: Option<&str>) -> Self {
        Self {
            user_id: user_id.to_string(),
            account_key: redacted_account_key(user_id),
            display_name: display_name.map(str::to_string),
            kind: SeatOwnerKind::Human,
        }
    }

    fn managed_bot(slot: ManagedBotLobbySlot, code: &str) -> Self {
        let user_id = managed_bot_user_id(slot, code);
        Self {
            account_key: redacted_account_key(&user_id),
            user_id,
            display_name: Some(managed_bot_display_name()),
            kind: SeatOwnerKind::ManagedBot,
        }
    }

    fn is_managed_bot(&self) -> bool {
        self.kind == SeatOwnerKind::ManagedBot
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MultiplayerViewer {
    Player(usize),
    Spectator,
}

impl MultiplayerViewer {
    fn player(self) -> Option<usize> {
        match self {
            Self::Player(player) => Some(player),
            Self::Spectator => None,
        }
    }

    fn role_name(self) -> &'static str {
        match self {
            Self::Player(_) => "player",
            Self::Spectator => "spectator",
        }
    }
}

#[derive(Clone)]
struct MultiplayerSocket {
    user_id: String,
    display_name: Option<String>,
    viewer: MultiplayerViewer,
    tx: mpsc::UnboundedSender<String>,
}

#[derive(Clone, Copy)]
struct RoomClockSnapshot {
    time_millis: [Option<u64>; 2],
    active_player: Option<usize>,
    winner: Option<usize>,
}

struct RoomClock {
    remaining_ms: Option<[u64; 2]>,
    active_player: Option<usize>,
    active_since_ms: Option<u64>,
    started: bool,
    winner: Option<usize>,
}

#[derive(Clone)]
struct RoomOutcome {
    winner: Option<usize>,
    reason: String,
    replay_share_slug: Option<String>,
}

impl RoomClock {
    fn new(time_minutes: Option<u32>) -> Self {
        let remaining_ms = time_minutes.map(|minutes| {
            let millis = u64::from(minutes).saturating_mul(60_000);
            [millis; 2]
        });
        Self {
            remaining_ms,
            active_player: None,
            active_since_ms: None,
            started: false,
            winner: None,
        }
    }

    fn snapshot(&self) -> RoomClockSnapshot {
        let time_millis = self
            .remaining_ms
            .map(|remaining| [Some(remaining[0]), Some(remaining[1])])
            .unwrap_or([None, None]);
        RoomClockSnapshot {
            time_millis,
            active_player: self.active_player,
            winner: self.winner,
        }
    }

    fn refresh(&mut self, now_ms: u64) -> bool {
        if self.winner.is_some() || !self.started {
            return false;
        }
        let Some(active_player) = self.active_player else {
            return false;
        };
        let Some(active_since_ms) = self.active_since_ms else {
            return false;
        };
        let Some(remaining) = self.remaining_ms.as_mut() else {
            return false;
        };

        let elapsed = now_ms.saturating_sub(active_since_ms);
        if elapsed == 0 {
            return false;
        }
        let player_remaining = &mut remaining[active_player];
        if elapsed >= *player_remaining {
            *player_remaining = 0;
            self.winner = Some(1 - active_player);
            self.active_player = None;
            self.active_since_ms = None;
            return true;
        }

        *player_remaining -= elapsed;
        self.active_since_ms = Some(now_ms);
        true
    }
}

struct MultiplayerRoom<G: Game + 'static> {
    code: String,
    room_id: u64,
    session: Arc<Mutex<GameSession<G>>>,
    seats: StdMutex<[Option<SeatOwner>; 2]>,
    sockets: StdMutex<HashMap<u64, MultiplayerSocket>>,
    chat_messages: StdMutex<Vec<MultiplayerChatMessage>>,
    clock: StdMutex<RoomClock>,
    outcome: StdMutex<Option<RoomOutcome>>,
    clock_generation: AtomicU64,
    next_socket_id: AtomicU64,
    next_chat_id: AtomicU64,
    replay_store: Option<Arc<ReplayStore>>,
    next_replay_counter: AtomicU64,
    replay_saved: AtomicBool,
    last_activity_ms: AtomicU64,
    analysis_generation: AtomicU64,
    is_public: bool,
    time_minutes: Option<u32>,
    increment_seconds: Option<u32>,
    managed_bot_slot: Option<ManagedBotLobbySlot>,
    managed_bot_player: Option<usize>,
    managed_human_ever_joined: AtomicBool,
    bot_move_running: AtomicBool,
}

impl<G: Game + 'static> MultiplayerRoom<G> {
    fn new(
        code: String,
        room_id: u64,
        session: GameSession<G>,
        creator: SeatOwner,
        creator_player: usize,
        replay_store: Option<Arc<ReplayStore>>,
        is_public: bool,
        time_minutes: Option<u32>,
        increment_seconds: Option<u32>,
    ) -> Self {
        let mut seats: [Option<SeatOwner>; 2] = [None, None];
        seats[creator_player] = Some(creator);
        Self {
            code,
            room_id,
            session: Arc::new(Mutex::new(session)),
            seats: StdMutex::new(seats),
            sockets: StdMutex::new(HashMap::new()),
            chat_messages: StdMutex::new(Vec::new()),
            clock: StdMutex::new(RoomClock::new(time_minutes)),
            outcome: StdMutex::new(None),
            clock_generation: AtomicU64::new(0),
            next_socket_id: AtomicU64::new(1),
            next_chat_id: AtomicU64::new(1),
            replay_store,
            next_replay_counter: AtomicU64::new(1),
            replay_saved: AtomicBool::new(false),
            last_activity_ms: AtomicU64::new(current_unix_ms()),
            analysis_generation: AtomicU64::new(0),
            is_public,
            time_minutes,
            increment_seconds,
            managed_bot_slot: None,
            managed_bot_player: None,
            managed_human_ever_joined: AtomicBool::new(false),
            bot_move_running: AtomicBool::new(false),
        }
    }

    fn new_managed_bot(
        code: String,
        room_id: u64,
        session: GameSession<G>,
        slot: ManagedBotLobbySlot,
        bot_player: usize,
        replay_store: Option<Arc<ReplayStore>>,
    ) -> Self {
        let (time_minutes, increment_seconds) = slot.time_control();
        let mut room = Self::new(
            code.clone(),
            room_id,
            session,
            SeatOwner::managed_bot(slot, &code),
            bot_player,
            replay_store,
            true,
            Some(time_minutes),
            Some(increment_seconds),
        );
        room.managed_bot_slot = Some(slot);
        room.managed_bot_player = Some(bot_player);
        room
    }

    fn assign_or_find_viewer(&self, owner: SeatOwner) -> MultiplayerViewer {
        let mut seats = self.seats.lock().expect("room seats lock poisoned");
        if let Some(player) = seats.iter().position(|seat| {
            seat.as_ref()
                .map_or(false, |seat| seat.user_id == owner.user_id)
        }) {
            if owner.display_name.is_some() {
                seats[player] = Some(owner);
                self.touch();
            }
            return MultiplayerViewer::Player(player);
        }
        if let Some(player) = seats.iter().position(Option::is_none) {
            if self.managed_bot_slot.is_some() && !owner.is_managed_bot() {
                self.managed_human_ever_joined
                    .store(true, Ordering::Relaxed);
            }
            seats[player] = Some(owner);
            self.touch();
            return MultiplayerViewer::Player(player);
        }
        MultiplayerViewer::Spectator
    }

    fn register_socket(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        viewer: MultiplayerViewer,
        tx: mpsc::UnboundedSender<String>,
    ) -> Result<u64, String> {
        if let Some(player) = viewer.player() {
            if self.seat_for_user(user_id) != Some(player) {
                return Err("You are not seated in this room".to_string());
            }
        }
        let socket_id = self.next_socket_id.fetch_add(1, Ordering::Relaxed);
        let mut sockets = self.sockets.lock().expect("room sockets lock poisoned");
        sockets.retain(|_, socket| socket.user_id != user_id);
        sockets.insert(
            socket_id,
            MultiplayerSocket {
                user_id: user_id.to_string(),
                display_name: display_name.map(str::to_string),
                viewer,
                tx,
            },
        );
        drop(sockets);
        if viewer.player().is_some() {
            self.touch();
        }
        Ok(socket_id)
    }

    fn socket_registered(&self, socket_id: u64) -> bool {
        self.sockets
            .lock()
            .expect("room sockets lock poisoned")
            .contains_key(&socket_id)
    }

    fn unregister_socket(&self, socket_id: u64) -> Option<MultiplayerViewer> {
        let removed = self
            .sockets
            .lock()
            .expect("room sockets lock poisoned")
            .remove(&socket_id)
            .map(|socket| socket.viewer);
        if removed
            .as_ref()
            .is_some_and(|viewer| viewer.player().is_some())
        {
            self.touch();
        }
        removed
    }

    fn connected_player_count_from_sockets(
        seats: &[Option<SeatOwner>; 2],
        sockets: &HashMap<u64, MultiplayerSocket>,
    ) -> usize {
        seats
            .iter()
            .enumerate()
            .filter(|(player, seat)| {
                seat.as_ref().is_some_and(|seat| {
                    seat.is_managed_bot()
                        || sockets.values().any(|socket| {
                            socket.user_id == seat.user_id
                                && socket.viewer == MultiplayerViewer::Player(*player)
                        })
                })
            })
            .count()
    }

    fn connected_player_count(&self) -> usize {
        let seats = self.seats.lock().expect("room seats lock poisoned");
        let sockets = self.sockets.lock().expect("room sockets lock poisoned");
        Self::connected_player_count_from_sockets(&seats, &sockets)
    }

    fn seat_for_user(&self, user_id: &str) -> Option<usize> {
        self.seats
            .lock()
            .expect("room seats lock poisoned")
            .iter()
            .position(|seat| seat.as_ref().map_or(false, |seat| seat.user_id == user_id))
    }

    fn is_full(&self) -> bool {
        self.seats
            .lock()
            .expect("room seats lock poisoned")
            .iter()
            .all(Option::is_some)
    }

    fn room_msg_for_viewer(
        &self,
        viewer: MultiplayerViewer,
        local_socket_id: Option<u64>,
    ) -> ServerMsg {
        let seats = self.seats.lock().expect("room seats lock poisoned");
        let sockets = self.sockets.lock().expect("room sockets lock poisoned");
        let clock = self.clock_snapshot();
        let outcome = self.outcome_snapshot();
        let winner = outcome
            .as_ref()
            .and_then(|outcome| outcome.winner)
            .or(clock.winner);
        let occupied = [
            seats[0].as_ref().map(|seat| seat.user_id.as_str()),
            seats[1].as_ref().map(|seat| seat.user_id.as_str()),
        ];
        let players = (0..2)
            .map(|player| {
                let connected = seats[player].as_ref().map_or(false, |seat| {
                    seat.is_managed_bot()
                        || sockets.values().any(|socket| {
                            socket.user_id == seat.user_id
                                && socket.viewer == MultiplayerViewer::Player(player)
                        })
                });
                MultiplayerPlayer {
                    occupied: occupied[player].is_some(),
                    connected,
                    you: viewer == MultiplayerViewer::Player(player),
                    name: seats[player]
                        .as_ref()
                        .and_then(|seat| seat.display_name.clone()),
                    time_millis: clock.time_millis[player],
                    clock_active: clock.active_player == Some(player),
                }
            })
            .collect::<Vec<_>>();
        let spectators = sockets
            .iter()
            .filter_map(|(socket_id, socket)| {
                (socket.viewer == MultiplayerViewer::Spectator).then(|| MultiplayerSpectator {
                    you: local_socket_id == Some(*socket_id),
                    name: socket.display_name.clone(),
                })
            })
            .collect::<Vec<_>>();
        let status = if outcome.is_some() || clock.winner.is_some() {
            "finished"
        } else if occupied.iter().all(Option::is_some) {
            "active"
        } else {
            "waiting"
        };
        ServerMsg::MultiplayerRoom {
            code: self.code.clone(),
            status: status.into(),
            viewer_role: viewer.role_name().into(),
            local_player: viewer.player().map(|player| player as u8),
            players,
            spectators,
            time_minutes: self.time_minutes,
            increment_seconds: self.increment_seconds,
            winner: winner.map(|player| player as u8),
            finish_reason: outcome.as_ref().map(|outcome| outcome.reason.clone()),
            replay_share_slug: outcome.and_then(|outcome| outcome.replay_share_slug),
        }
    }

    #[cfg(test)]
    fn room_msg_for_player(&self, local_player: Option<usize>) -> ServerMsg {
        let viewer = local_player
            .map(MultiplayerViewer::Player)
            .unwrap_or(MultiplayerViewer::Spectator);
        self.room_msg_for_viewer(viewer, None)
    }

    fn room_msg_for_active(&self, active: &ActiveMultiplayerRoom<G>) -> ServerMsg {
        self.room_msg_for_viewer(active.viewer, Some(active.socket_id))
    }

    fn chat_msg(&self) -> Option<ServerMsg> {
        let messages = self
            .chat_messages
            .lock()
            .expect("room chat lock poisoned")
            .clone();
        (!messages.is_empty()).then_some(ServerMsg::MultiplayerChat { messages })
    }

    fn player_chat_name(&self, player: usize) -> Option<String> {
        self.seats
            .lock()
            .expect("room seats lock poisoned")
            .get(player)
            .and_then(|seat| seat.as_ref())
            .and_then(|seat| seat.display_name.clone())
    }

    fn push_chat_message(&self, player: usize, text: &str) -> Result<ServerMsg, String> {
        let text = sanitize_multiplayer_chat_text(text)?;
        let id = self.next_chat_id.fetch_add(1, Ordering::Relaxed);
        let message = MultiplayerChatMessage {
            id,
            sent_at_ms: current_unix_ms(),
            player: player as u8,
            name: self.player_chat_name(player),
            text,
        };
        let messages = {
            let mut messages = self.chat_messages.lock().expect("room chat lock poisoned");
            messages.push(message);
            let overflow = messages
                .len()
                .saturating_sub(MULTIPLAYER_CHAT_HISTORY_LIMIT);
            if overflow > 0 {
                messages.drain(0..overflow);
            }
            messages.clone()
        };
        self.touch();
        Ok(ServerMsg::MultiplayerChat { messages })
    }

    fn broadcast_chat_except(&self, excluded_socket_id: Option<u64>) {
        let Some(msg) = self.chat_msg() else {
            return;
        };
        let json = serde_json::to_string(&msg).ok();
        self.broadcast_with(excluded_socket_id, |_room, _socket_id, socket| {
            socket.viewer.player().and(json.clone())
        });
    }

    fn lobby_entry(&self) -> MultiplayerLobbyRoom {
        let seats = self.seats.lock().expect("room seats lock poisoned");
        let sockets = self.sockets.lock().expect("room sockets lock poisoned");
        let occupied_count = seats.iter().filter(|seat| seat.is_some()).count() as u8;
        let connected_count = Self::connected_player_count_from_sockets(&seats, &sockets) as u8;
        let spectator_count = sockets
            .values()
            .filter(|socket| socket.viewer == MultiplayerViewer::Spectator)
            .count() as u8;
        let status = if self.outcome_snapshot().is_some() {
            "finished"
        } else if occupied_count == 2 {
            "active"
        } else {
            "waiting"
        };
        let last_activity_ms = self.last_activity_ms.load(Ordering::Relaxed);
        MultiplayerLobbyRoom {
            code: self.code.clone(),
            status: status.into(),
            occupied: occupied_count,
            connected: connected_count,
            spectator_count,
            is_public: self.is_public,
            time_minutes: self.time_minutes,
            increment_seconds: self.increment_seconds,
            last_activity_ms,
            empty_room_closes_at_ms: None,
        }
    }

    fn managed_bot_player(&self) -> Option<usize> {
        self.managed_bot_player
    }

    fn has_human_player_occupant(&self) -> bool {
        self.seats
            .lock()
            .expect("room seats lock poisoned")
            .iter()
            .flatten()
            .any(|seat| !seat.is_managed_bot())
    }

    fn vacate_human_player(&self, player: usize) -> bool {
        if self.managed_bot_slot.is_none() {
            return false;
        }
        let mut seats = self.seats.lock().expect("room seats lock poisoned");
        let Some(seat) = seats.get(player).and_then(Option::as_ref) else {
            return false;
        };
        if seat.is_managed_bot() {
            return false;
        }
        seats[player] = None;
        drop(seats);
        self.touch();
        true
    }

    fn managed_bot_should_refresh(&self, now_ms: u64) -> bool {
        if self.managed_bot_slot.is_none() {
            return false;
        }
        if self.is_finished() {
            return true;
        }
        if self.has_human_player_occupant() {
            return false;
        }
        self.managed_human_ever_joined.load(Ordering::Relaxed)
            || now_ms.saturating_sub(self.last_activity_ms.load(Ordering::Relaxed))
                >= MANAGED_BOT_LOBBY_REFRESH_MS
    }

    fn left_room_msg() -> ServerMsg {
        ServerMsg::MultiplayerRoom {
            code: String::new(),
            status: "left".into(),
            viewer_role: "player".into(),
            local_player: None,
            players: vec![
                MultiplayerPlayer {
                    occupied: false,
                    connected: false,
                    you: false,
                    name: None,
                    time_millis: None,
                    clock_active: false,
                },
                MultiplayerPlayer {
                    occupied: false,
                    connected: false,
                    you: false,
                    name: None,
                    time_millis: None,
                    clock_active: false,
                },
            ],
            spectators: Vec::new(),
            time_minutes: None,
            increment_seconds: None,
            winner: None,
            finish_reason: None,
            replay_share_slug: None,
        }
    }

    fn clock_snapshot(&self) -> RoomClockSnapshot {
        let mut clock = self.clock.lock().expect("room clock lock poisoned");
        let changed = clock.refresh(current_unix_ms());
        let snapshot = clock.snapshot();
        drop(clock);
        if changed && snapshot.winner.is_some() {
            self.clock_generation.fetch_add(1, Ordering::Relaxed);
            self.touch();
            self.mark_finished(snapshot.winner, "timeout");
        }
        snapshot
    }

    fn clock_winner(&self) -> Option<usize> {
        self.clock_snapshot().winner
    }

    fn outcome_snapshot(&self) -> Option<RoomOutcome> {
        self.outcome
            .lock()
            .expect("room outcome lock poisoned")
            .clone()
    }

    fn is_finished(&self) -> bool {
        self.outcome_snapshot().is_some() || self.clock_winner().is_some()
    }

    fn mark_finished(&self, winner: Option<usize>, reason: &str) -> bool {
        let mut outcome = self.outcome.lock().expect("room outcome lock poisoned");
        if outcome.is_some() {
            return false;
        }
        *outcome = Some(RoomOutcome {
            winner,
            reason: reason.to_string(),
            replay_share_slug: None,
        });
        drop(outcome);

        let mut clock = self.clock.lock().expect("room clock lock poisoned");
        clock.winner = winner;
        clock.active_player = None;
        clock.active_since_ms = None;
        drop(clock);

        self.clock_generation.fetch_add(1, Ordering::Relaxed);
        self.touch();
        true
    }

    fn set_replay_share_slug(&self, slug: String) {
        let mut outcome = self.outcome.lock().expect("room outcome lock poisoned");
        if let Some(outcome) = outcome.as_mut() {
            if outcome.replay_share_slug.is_none() {
                outcome.replay_share_slug = Some(slug);
            }
        }
    }

    fn finish_clock_turn(&self, moved_player: usize, next_player: usize, game_over: bool) {
        let mut clock = self.clock.lock().expect("room clock lock poisoned");
        let now_ms = current_unix_ms();
        let changed = clock.refresh(now_ms);
        if clock.winner.is_some() || clock.remaining_ms.is_none() {
            drop(clock);
            if changed {
                self.clock_generation.fetch_add(1, Ordering::Relaxed);
                self.touch();
            }
            return;
        }
        if clock.started {
            if let Some(remaining) = clock.remaining_ms.as_mut() {
                remaining[moved_player] = remaining[moved_player]
                    .saturating_add(u64::from(self.increment_seconds.unwrap_or(0)) * 1000);
            }
        } else {
            clock.started = true;
        }
        if game_over {
            clock.active_player = None;
            clock.active_since_ms = None;
        } else {
            clock.active_player = Some(next_player);
            clock.active_since_ms = Some(now_ms);
        }
        drop(clock);
        self.clock_generation.fetch_add(1, Ordering::Relaxed);
        self.touch();
    }

    fn add_clock_seconds(&self, player: usize, seconds: u32) -> bool {
        let mut clock = self.clock.lock().expect("room clock lock poisoned");
        let changed = clock.refresh(current_unix_ms());
        if clock.winner.is_some() {
            drop(clock);
            if changed {
                self.clock_generation.fetch_add(1, Ordering::Relaxed);
                self.touch();
            }
            return false;
        }
        let Some(remaining) = clock.remaining_ms.as_mut() else {
            return false;
        };
        let Some(player_clock) = remaining.get_mut(player) else {
            return false;
        };
        *player_clock = player_clock.saturating_add(u64::from(seconds) * 1000);
        drop(clock);
        self.clock_generation.fetch_add(1, Ordering::Relaxed);
        self.touch();
        true
    }

    fn clock_timeout_schedule(&self) -> Option<(u64, u64)> {
        let snapshot = self.clock_snapshot();
        let active_player = snapshot.active_player?;
        let remaining_ms = snapshot.time_millis[active_player]?;
        if remaining_ms == 0 || snapshot.winner.is_some() {
            return None;
        }
        Some((self.clock_generation.load(Ordering::Relaxed), remaining_ms))
    }

    fn mark_clock_timeout_if_current(&self, generation: u64) -> bool {
        if self.clock_generation.load(Ordering::Relaxed) != generation {
            return false;
        }
        let mut clock = self.clock.lock().expect("room clock lock poisoned");
        let before = clock.winner;
        let changed = clock.refresh(current_unix_ms());
        let timed_out = before.is_none() && clock.winner.is_some();
        let winner = clock.winner;
        drop(clock);
        if changed {
            self.clock_generation.fetch_add(1, Ordering::Relaxed);
            self.touch();
        }
        if timed_out {
            self.mark_finished(winner, "timeout");
        }
        timed_out
    }

    fn broadcast_room_info_except(&self, excluded_socket_id: Option<u64>) {
        self.broadcast_with(excluded_socket_id, |room, socket_id, socket| {
            serde_json::to_string(&room.room_msg_for_viewer(socket.viewer, Some(socket_id))).ok()
        });
    }

    fn broadcast_state_except(&self, session: &GameSession<G>, excluded_socket_id: Option<u64>) {
        let by_player = [
            serde_json::to_string(&session.state_msg_for_player(0)).ok(),
            serde_json::to_string(&session.state_msg_for_player(1)).ok(),
        ];
        let spectator = serde_json::to_string(&session.state_msg_for_spectator()).ok();
        self.broadcast_with(
            excluded_socket_id,
            |_room, _socket_id, socket| match socket.viewer {
                MultiplayerViewer::Player(player) => by_player[player].clone(),
                MultiplayerViewer::Spectator => spectator.clone(),
            },
        );
    }

    fn broadcast_analysis_except(&self, root_wdl: [f32; 3], excluded_socket_id: Option<u64>) {
        let json = serde_json::to_string(&ServerMsg::MultiplayerAnalysis { root_wdl }).ok();
        self.broadcast_with(excluded_socket_id, |_room, _socket_id, _socket| {
            json.clone()
        });
    }

    fn broadcast_with(
        &self,
        excluded_socket_id: Option<u64>,
        mut build: impl FnMut(&Self, u64, &MultiplayerSocket) -> Option<String>,
    ) {
        let recipients = {
            let sockets = self.sockets.lock().expect("room sockets lock poisoned");
            sockets
                .iter()
                .filter(|(socket_id, _)| excluded_socket_id != Some(**socket_id))
                .map(|(socket_id, socket)| (*socket_id, socket.clone()))
                .collect::<Vec<_>>()
        };
        let mut failed = Vec::new();
        for (socket_id, socket) in recipients {
            if let Some(json) = build(self, socket_id, &socket) {
                if socket.tx.send(json).is_err() {
                    failed.push(socket_id);
                }
            }
        }
        if !failed.is_empty() {
            let mut sockets = self.sockets.lock().expect("room sockets lock poisoned");
            for socket_id in failed {
                sockets.remove(&socket_id);
            }
        }
    }

    async fn save_room_replay_once(
        &self,
        session: &GameSession<G>,
        require_terminal: bool,
    ) -> Vec<ServerMsg> {
        if require_terminal && !session.current_game_ended() {
            return Vec::new();
        }
        if self.replay_saved.swap(true, Ordering::Relaxed) {
            return Vec::new();
        }
        let Some(store) = self.replay_store.as_ref() else {
            return Vec::new();
        };
        let log = if require_terminal {
            session.export_current_log()
        } else {
            session.export_current_log_allow_empty()
        };
        let Some(log) = log else {
            return Vec::new();
        };

        let seats = {
            let seats = self.seats.lock().expect("room seats lock poisoned");
            seats
                .iter()
                .enumerate()
                .filter_map(|(player, seat)| {
                    seat.as_ref()
                        .map(|seat| (player, seat.account_key.clone(), seat.display_name.clone()))
                })
                .collect::<Vec<_>>()
        };
        let outcome = self.outcome_snapshot();
        let participants = seats
            .into_iter()
            .map(|(player, account_key, display_name)| ReplayParticipant {
                player,
                account_key,
                display_name,
                result: outcome
                    .as_ref()
                    .and_then(|outcome| replay_result_for_room_outcome(outcome, player))
                    .unwrap_or("incomplete")
                    .to_string(),
            })
            .collect::<Vec<_>>();
        let counter = self.next_replay_counter.fetch_add(1, Ordering::Relaxed);
        match store
            .save_multiplayer_with_results(self.room_id, counter, &log, &participants)
            .await
        {
            Ok(entry) => {
                self.set_replay_share_slug(entry.share_slug);
                Vec::new()
            }
            Err(message) => vec![ServerMsg::Error { message }],
        }
    }

    async fn save_completed_replay_once(&self, session: &GameSession<G>) -> Vec<ServerMsg> {
        self.save_room_replay_once(session, true).await
    }

    async fn save_finished_room_replay_once(&self, session: &GameSession<G>) -> Vec<ServerMsg> {
        self.save_room_replay_once(session, false).await
    }

    fn touch(&self) {
        self.last_activity_ms
            .store(current_unix_ms(), Ordering::Relaxed);
    }

    fn next_analysis_generation(&self) -> u64 {
        self.analysis_generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn is_current_analysis_generation(&self, generation: u64) -> bool {
        self.analysis_generation.load(Ordering::Relaxed) == generation
    }
}

struct MultiplayerRoomStore<G: Game + 'static> {
    factory: Arc<SessionFactory<G>>,
    rooms: StdMutex<HashMap<String, Arc<MultiplayerRoom<G>>>>,
    lobby_sockets: StdMutex<HashMap<u64, mpsc::UnboundedSender<String>>>,
    next_room_id: AtomicU64,
    next_lobby_socket_id: AtomicU64,
    replay_store: Option<Arc<ReplayStore>>,
    managed_bot_lobbies_enabled: bool,
}

impl<G: Game + 'static> MultiplayerRoomStore<G> {
    #[cfg(test)]
    fn new(factory: Arc<SessionFactory<G>>, replay_store: Option<Arc<ReplayStore>>) -> Self {
        Self::new_with_managed_bot_lobbies(factory, replay_store, false)
    }

    fn new_with_managed_bot_lobbies(
        factory: Arc<SessionFactory<G>>,
        replay_store: Option<Arc<ReplayStore>>,
        managed_bot_lobbies_enabled: bool,
    ) -> Self {
        Self {
            factory,
            rooms: StdMutex::new(HashMap::new()),
            lobby_sockets: StdMutex::new(HashMap::new()),
            next_room_id: AtomicU64::new(1),
            next_lobby_socket_id: AtomicU64::new(1),
            replay_store,
            managed_bot_lobbies_enabled,
        }
    }

    fn register_lobby_socket(&self, tx: mpsc::UnboundedSender<String>) -> u64 {
        let id = self.next_lobby_socket_id.fetch_add(1, Ordering::Relaxed);
        self.lobby_sockets
            .lock()
            .expect("lobby socket registry lock poisoned")
            .insert(id, tx);
        id
    }

    fn unregister_lobby_socket(&self, id: u64) {
        self.lobby_sockets
            .lock()
            .expect("lobby socket registry lock poisoned")
            .remove(&id);
    }

    fn lobby_rooms(&self) -> Vec<MultiplayerLobbyRoom> {
        self.ensure_managed_bot_lobbies();
        let mut rooms = self
            .rooms
            .lock()
            .expect("room store lock poisoned")
            .values()
            .filter(|room| room.is_public)
            .map(|room| room.lobby_entry())
            .collect::<Vec<_>>();
        rooms.sort_by(|a, b| {
            b.last_activity_ms
                .cmp(&a.last_activity_ms)
                .then_with(|| a.code.cmp(&b.code))
        });
        rooms
    }

    fn lobby_msg(&self) -> ServerMsg {
        let rooms = self.lobby_rooms();
        ServerMsg::MultiplayerLobby { rooms }
    }

    fn ensure_managed_bot_lobbies(&self) -> bool {
        if !self.managed_bot_lobbies_enabled {
            return false;
        }
        let now_ms = current_unix_ms();
        let mut rooms = self.rooms.lock().expect("room store lock poisoned");
        let mut changed = false;
        for slot in ManagedBotLobbySlot::ALL {
            let existing_code = rooms.iter().find_map(|(code, room)| {
                (room.managed_bot_slot == Some(slot)).then(|| code.clone())
            });
            if let Some(code) = existing_code {
                let refresh = rooms
                    .get(&code)
                    .is_some_and(|room| room.managed_bot_should_refresh(now_ms));
                if !refresh {
                    continue;
                }
                rooms.remove(&code);
                changed = true;
            }

            for _ in 0..64 {
                let code = new_room_code();
                if rooms.contains_key(&code) {
                    continue;
                }
                let room_id = self.next_room_id.fetch_add(1, Ordering::Relaxed);
                let bot_player = usize::from(fastrand::bool());
                let room = Arc::new(MultiplayerRoom::new_managed_bot(
                    code.clone(),
                    room_id,
                    self.factory.create_multiplayer_session(),
                    slot,
                    bot_player,
                    self.replay_store.clone(),
                ));
                rooms.insert(code, room);
                changed = true;
                break;
            }
        }
        changed
    }

    fn send_lobby_to(&self, tx: &mpsc::UnboundedSender<String>) {
        if let Ok(json) = serde_json::to_string(&self.lobby_msg()) {
            let _ = tx.send(json);
        }
    }

    fn broadcast_lobby(&self) {
        self.ensure_managed_bot_lobbies();
        let Ok(json) = serde_json::to_string(&self.lobby_msg()) else {
            return;
        };
        let mut sockets = self
            .lobby_sockets
            .lock()
            .expect("lobby socket registry lock poisoned");
        sockets.retain(|_, tx| tx.send(json.clone()).is_ok());
    }

    fn start_managed_bot_lobby_worker(self: &Arc<Self>) {
        if !self.managed_bot_lobbies_enabled {
            return;
        }
        let rooms = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(60_000)).await;
                if rooms.ensure_managed_bot_lobbies() {
                    rooms.broadcast_lobby();
                }
            }
        });
    }

    #[cfg(test)]
    fn create_room(
        &self,
        user_id: &str,
        preferred_player: Option<u8>,
        requested_code: Option<String>,
        is_public: Option<bool>,
        time_minutes: Option<u32>,
        increment_seconds: Option<u32>,
    ) -> Result<(Arc<MultiplayerRoom<G>>, usize), String> {
        self.create_room_with_display_name(
            user_id,
            None,
            preferred_player,
            requested_code,
            is_public,
            time_minutes,
            increment_seconds,
        )
    }

    fn create_room_with_display_name(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        preferred_player: Option<u8>,
        requested_code: Option<String>,
        is_public: Option<bool>,
        time_minutes: Option<u32>,
        increment_seconds: Option<u32>,
    ) -> Result<(Arc<MultiplayerRoom<G>>, usize), String> {
        let player = match preferred_player {
            None => usize::from(fastrand::bool()),
            Some(1) => 1,
            _ => 0,
        };
        let owner = SeatOwner::human(user_id, display_name);
        let room_id = self.next_room_id.fetch_add(1, Ordering::Relaxed);
        let mut rooms = self.rooms.lock().expect("room store lock poisoned");
        let is_public = is_public.unwrap_or(true);
        let time_minutes = normalize_room_time_minutes(time_minutes);
        let increment_seconds = normalize_room_increment_seconds(increment_seconds);
        if let Some(code) = requested_code {
            let code = normalize_room_code(&code)?;
            if rooms.contains_key(&code) {
                return Err("Room code is already in use".into());
            }
            let room = Arc::new(MultiplayerRoom::new(
                code.clone(),
                room_id,
                self.factory.create_multiplayer_session(),
                owner.clone(),
                player,
                self.replay_store.clone(),
                is_public,
                time_minutes,
                increment_seconds,
            ));
            rooms.insert(code, Arc::clone(&room));
            return Ok((room, player));
        }
        for _ in 0..64 {
            let code = new_room_code();
            if rooms.contains_key(&code) {
                continue;
            }
            let room = Arc::new(MultiplayerRoom::new(
                code.clone(),
                room_id,
                self.factory.create_multiplayer_session(),
                owner.clone(),
                player,
                self.replay_store.clone(),
                is_public,
                time_minutes,
                increment_seconds,
            ));
            rooms.insert(code, Arc::clone(&room));
            return Ok((room, player));
        }
        Err("Could not create a unique room code".into())
    }

    #[cfg(test)]
    fn join_room(
        &self,
        user_id: &str,
        code: &str,
    ) -> Result<(Arc<MultiplayerRoom<G>>, usize), String> {
        let (room, viewer) = self.join_room_with_display_name(user_id, None, code)?;
        match viewer {
            MultiplayerViewer::Player(player) => Ok((room, player)),
            MultiplayerViewer::Spectator => Err("Room is full".into()),
        }
    }

    fn join_room_with_display_name(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        code: &str,
    ) -> Result<(Arc<MultiplayerRoom<G>>, MultiplayerViewer), String> {
        let code = normalize_room_code(code)?;
        let room = self
            .rooms
            .lock()
            .expect("room store lock poisoned")
            .get(&code)
            .cloned()
            .ok_or_else(|| "Room not found".to_string())?;
        let owner = SeatOwner::human(user_id, display_name);
        let viewer = room.assign_or_find_viewer(owner);
        Ok((room, viewer))
    }

    fn close_room_if_no_connected_players(&self, room: &Arc<MultiplayerRoom<G>>) -> bool {
        if room.managed_bot_slot.is_some() {
            return false;
        }
        if room.connected_player_count() > 0 {
            return false;
        }
        let mut rooms = self.rooms.lock().expect("room store lock poisoned");
        let should_remove = rooms
            .get(&room.code)
            .map_or(false, |stored| Arc::ptr_eq(stored, room));
        if should_remove {
            rooms.remove(&room.code);
        }
        should_remove
    }
}

fn new_room_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    (0..6)
        .map(|_| {
            let idx = fastrand::usize(..ALPHABET.len());
            ALPHABET[idx] as char
        })
        .collect()
}

fn normalize_room_code(code: &str) -> Result<String, String> {
    let code = code.trim().to_ascii_uppercase();
    if code.len() != 6
        || !code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err("Room code must be 6 letters or numbers".into());
    }
    Ok(code)
}

fn normalize_room_time_minutes(value: Option<u32>) -> Option<u32> {
    value.map(|value| value.clamp(1, 180))
}

fn normalize_room_increment_seconds(value: Option<u32>) -> Option<u32> {
    value.map(|value| value.clamp(0, 120))
}

async fn save_replay_log<G: Game + 'static>(
    user_session: &UserSession<G>,
    log: &GameLog,
) -> Option<ServerMsg> {
    match save_replay_entry(user_session, log).await {
        Ok(_) => None,
        Err(message) => Some(ServerMsg::Error { message }),
    }
}

async fn save_replay_entry<G: Game + 'static>(
    user_session: &UserSession<G>,
    log: &GameLog,
) -> Result<Option<ReplayEntry>, String> {
    save_replay_entry_with_result(user_session, log, None).await
}

async fn save_replay_entry_with_result<G: Game + 'static>(
    user_session: &UserSession<G>,
    log: &GameLog,
    result: Option<&str>,
) -> Result<Option<ReplayEntry>, String> {
    let Some(store) = user_session.replay_store.as_ref() else {
        return Ok(None);
    };
    let counter = user_session
        .next_replay_counter
        .fetch_add(1, Ordering::Relaxed);
    store
        .save_with_result(
            &user_session.account_key,
            user_session.session_id,
            counter,
            log,
            result,
        )
        .await
        .map(Some)
}

async fn save_current_replay_once<G: Game + 'static>(
    user_session: &UserSession<G>,
    session: &mut GameSession<G>,
) -> Option<ServerMsg> {
    let has_store = user_session.replay_store.is_some();
    let replay_result = session.live_replay_result().map(str::to_owned);
    let log = if replay_result.is_some() {
        session.export_unsaved_current_log_allow_empty()?
    } else {
        session.export_unsaved_current_log()?
    };
    match save_replay_entry_with_result(user_session, &log, replay_result.as_deref()).await {
        Ok(entry) => {
            if has_store {
                session.mark_current_log_saved(&log);
            }
            entry.map(|entry| ServerMsg::ReplaySaved { entry })
        }
        Err(message) => Some(ServerMsg::Error { message }),
    }
}

async fn list_replay_entries<G: Game + 'static>(
    user_session: &Arc<UserSession<G>>,
) -> Result<Vec<ReplayEntry>, String> {
    let store = user_session
        .replay_store
        .as_ref()
        .ok_or_else(|| "Replay storage is not configured".to_string())?;
    let mut entries = store.list(&user_session.account_key).await?;
    for entry in &mut entries {
        if entry.result != "incomplete" {
            continue;
        }
        if let Ok(loaded) = store.load(&user_session.account_key, &entry.id).await {
            entry.result = replay_result_for_log(user_session.factory.as_ref(), &loaded.log);
        }
    }
    Ok(entries)
}

fn replay_result_for_log<G: Game + 'static>(factory: &SessionFactory<G>, log: &GameLog) -> String {
    let mut session = factory.create_session();
    if session.load_saved_replay_log("summary", log).is_err() {
        return "incomplete".into();
    }
    session.seek_to_end();
    match session.current_result_reward() {
        Some(reward) if reward > 0.0 => "won".into(),
        Some(reward) if reward < 0.0 => "lost".into(),
        Some(_) => "draw".into(),
        None => "incomplete".into(),
    }
}

fn replay_result_for_room_outcome(outcome: &RoomOutcome, player: usize) -> Option<&'static str> {
    let won = outcome.winner? == player;
    match outcome.reason.as_str() {
        "resignation" => Some(if won {
            "won_by_resignation"
        } else {
            "lost_by_resignation"
        }),
        "timeout" => Some(if won { "won_on_time" } else { "lost_on_time" }),
        _ => None,
    }
}

fn winner_from_reward(reward: Option<f32>) -> Option<usize> {
    match reward {
        Some(reward) if reward > 0.0 => Some(0),
        Some(reward) if reward < 0.0 => Some(1),
        _ => None,
    }
}

fn responses_include_terminal_game_state(responses: &[ServerMsg]) -> bool {
    responses.iter().any(|msg| {
        matches!(
            msg,
            ServerMsg::GameState {
                is_terminal: true,
                ..
            }
        )
    })
}

struct UserSessionStore<G: Game + 'static> {
    factory: Arc<SessionFactory<G>>,
    sessions: StdMutex<HashMap<String, Arc<UserSession<G>>>>,
    usernames: StdMutex<UsernameRegistry>,
    next_session_id: AtomicU64,
    replay_store: Option<Arc<ReplayStore>>,
}

#[derive(Default)]
struct UsernameRegistry {
    by_user: HashMap<String, String>,
    by_name: HashMap<String, String>,
}

impl<G: Game + 'static> UserSessionStore<G> {
    fn new(factory: SessionFactory<G>, replay_store: Option<Arc<ReplayStore>>) -> Self {
        Self {
            factory: Arc::new(factory),
            sessions: StdMutex::new(HashMap::new()),
            usernames: StdMutex::new(UsernameRegistry::default()),
            next_session_id: AtomicU64::new(1),
            replay_store,
        }
    }

    fn get_or_create(&self, user_id: &str) -> (Arc<UserSession<G>>, bool) {
        let mut sessions = self.sessions.lock().expect("user session lock poisoned");
        if let Some(session) = sessions.get(user_id) {
            return (Arc::clone(session), false);
        }

        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let game_session = self.create_unique_session(&sessions);
        let account_key = redacted_account_key(user_id);
        let session = Arc::new(UserSession::new(
            game_session,
            session_id,
            account_key,
            Arc::clone(&self.factory),
            self.replay_store.clone(),
        ));
        sessions.insert(user_id.to_string(), Arc::clone(&session));
        (session, true)
    }

    fn create_unique_session(
        &self,
        sessions: &HashMap<String, Arc<UserSession<G>>>,
    ) -> GameSession<G> {
        if !self.factory.creates_random_boards() {
            return self.factory.create_session();
        }

        for _ in 0..BOARD_FINGERPRINT_RETRIES {
            let session = self.factory.create_session();
            let fingerprint = session.board_fingerprint();
            if fingerprint.map_or(true, |fp| !board_fingerprint_in_use(sessions, fp)) {
                return session;
            }
        }

        let session = self.factory.create_session();
        if let Some(fingerprint) = session.board_fingerprint() {
            tracing::warn!(
                board_code = fingerprint,
                "could not generate a unique board fingerprint after retries"
            );
        }
        session
    }

    fn current_username(&self, user_id: &str) -> Option<String> {
        self.usernames
            .lock()
            .expect("username registry lock poisoned")
            .by_user
            .get(user_id)
            .cloned()
    }

    fn profile_msg(&self, user_id: &str) -> ServerMsg {
        let registry = self
            .usernames
            .lock()
            .expect("username registry lock poisoned");
        let username = registry.by_user.get(user_id).cloned();
        ServerMsg::Profile {
            username_set: username.is_some(),
            username,
        }
    }

    fn set_display_name(&self, user_id: &str, username: &str) {
        let mut registry = self
            .usernames
            .lock()
            .expect("username registry lock poisoned");
        let Some(username) = normalize_display_name(Some(username.to_string())) else {
            return;
        };
        if let Some(old_username) = registry
            .by_user
            .insert(user_id.to_string(), username.clone())
        {
            let old_key = username_key(&old_username);
            if old_key != username_key(&username) {
                registry.by_name.remove(&old_key);
            }
        }
        registry
            .by_name
            .insert(username_key(&username), user_id.to_string());
    }

    fn ensure_guest_username(&self, user_id: &str, username: Option<&str>) -> String {
        let mut registry = self
            .usernames
            .lock()
            .expect("username registry lock poisoned");
        if let Some(existing) = registry.by_user.get(user_id) {
            return existing.clone();
        }

        if let Some(username) = normalize_profile_username(username) {
            let key = username_key(&username);
            if !registry
                .by_name
                .get(&key)
                .is_some_and(|owner| owner != user_id)
            {
                registry.by_name.insert(key, user_id.to_string());
                registry
                    .by_user
                    .insert(user_id.to_string(), username.clone());
                return username;
            }
        }

        let username = default_profile_username(user_id, &registry);
        let key = username_key(&username);
        registry.by_name.insert(key, user_id.to_string());
        registry
            .by_user
            .insert(user_id.to_string(), username.clone());
        username
    }
}

fn board_fingerprint_in_use<G: Game + 'static>(
    sessions: &HashMap<String, Arc<UserSession<G>>>,
    fingerprint: u64,
) -> bool {
    sessions
        .values()
        .any(|session| session.board_fingerprint == Some(fingerprint))
}

struct SocketSessionKey {
    user_id: String,
    display_name: Option<String>,
    ephemeral: bool,
    scope: &'static str,
}

struct ActiveMultiplayerRoom<G: Game + 'static> {
    room: Arc<MultiplayerRoom<G>>,
    socket_id: u64,
    viewer: MultiplayerViewer,
}

enum MultiplayerRuntime<G: Game + 'static> {
    Memory(Arc<MultiplayerRoomStore<G>>),
    Redis(Arc<RedisMultiplayerRoomStore<G>>),
}

enum ActiveMultiplayer<G: Game + 'static> {
    Memory(ActiveMultiplayerRoom<G>),
    Redis(ActiveRedisMultiplayerRoom),
}

enum ActiveLobbySocket {
    Memory(u64),
    Redis(u64),
}

impl<G: Game + 'static> UserSessionStore<G> {
    fn remove_if_same(&self, user_id: &str, session: &Arc<UserSession<G>>) {
        let mut sessions = self.sessions.lock().expect("user session lock poisoned");
        let should_remove = sessions
            .get(user_id)
            .map_or(false, |current| Arc::ptr_eq(current, session));
        if should_remove {
            sessions.remove(user_id);
        }
    }
}

async fn replay_store_from_config(web_log_dir: Option<PathBuf>) -> Option<Arc<ReplayStore>> {
    let database_url = env::var(DATABASE_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let required = env_flag(REPLAY_STORE_REQUIRED_ENV);
    replay_store_from_settings(web_log_dir, database_url.as_deref(), required).await
}

async fn replay_store_from_settings(
    web_log_dir: Option<PathBuf>,
    database_url: Option<&str>,
    required: bool,
) -> Option<Arc<ReplayStore>> {
    if let Some(database_url) = database_url {
        println!("Replay storage: Postgres {DATABASE_URL_ENV}");
        match ReplayStore::postgres(database_url).await {
            Ok(store) => return Some(Arc::new(store)),
            Err(error) if required => {
                panic!(
                    "failed to initialize required Postgres replay storage from {DATABASE_URL_ENV}: {error}"
                );
            }
            Err(error) => {
                eprintln!(
                    "Replay storage: failed to initialize Postgres {DATABASE_URL_ENV}: {error}; falling back to filesystem if configured"
                );
            }
        }
    }

    if required {
        panic!(
            "{REPLAY_STORE_REQUIRED_ENV}=true requires {DATABASE_URL_ENV} to be set for replay storage"
        );
    }

    web_log_dir.map(|dir| {
        println!("Replay storage: filesystem {}", dir.display());
        Arc::new(ReplayStore::file(dir))
    })
}

async fn multiplayer_runtime_from_config<G: Game + 'static>(
    factory: Arc<SessionFactory<G>>,
    replay_store: Option<Arc<ReplayStore>>,
) -> Arc<MultiplayerRuntime<G>> {
    let backend = env::var(MULTIPLAYER_BACKEND_ENV)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "memory".into());
    let redis_url = env::var(REDIS_URL_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let required = env_flag(MULTIPLAYER_REQUIRED_ENV) || backend == "redis";

    if backend == "redis" || redis_url.is_some() {
        let Some(redis_url) = redis_url.as_deref() else {
            if required {
                panic!(
                    "{MULTIPLAYER_BACKEND_ENV}=redis requires {REDIS_URL_ENV} to be set for multiplayer"
                );
            }
            return managed_memory_multiplayer_runtime(factory, replay_store);
        };
        println!("Multiplayer rooms: Redis {REDIS_URL_ENV}");
        match RedisMultiplayerRoomStore::connect(
            redis_url,
            Arc::clone(&factory),
            replay_store.clone(),
        )
        .await
        {
            Ok(store) => return Arc::new(MultiplayerRuntime::Redis(store)),
            Err(error) if required => {
                panic!(
                    "failed to initialize required Redis multiplayer backend from {REDIS_URL_ENV}: {error}"
                );
            }
            Err(error) => {
                eprintln!(
                    "Multiplayer rooms: failed to initialize Redis {REDIS_URL_ENV}: {error}; falling back to memory"
                );
            }
        }
    }

    println!("Multiplayer rooms: in-memory");
    managed_memory_multiplayer_runtime(factory, replay_store)
}

fn managed_memory_multiplayer_runtime<G: Game + 'static>(
    factory: Arc<SessionFactory<G>>,
    replay_store: Option<Arc<ReplayStore>>,
) -> Arc<MultiplayerRuntime<G>> {
    let rooms = Arc::new(MultiplayerRoomStore::new_with_managed_bot_lobbies(
        factory,
        replay_store,
        true,
    ));
    rooms.ensure_managed_bot_lobbies();
    rooms.start_managed_bot_lobby_worker();
    Arc::new(MultiplayerRuntime::Memory(rooms))
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| parse_env_bool(&value))
        .unwrap_or(false)
}

fn parse_env_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "t" | "yes" | "y" | "on"
    )
}

fn sanitize_multiplayer_chat_text(text: &str) -> Result<String, String> {
    let cleaned = text
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Err("Chat message cannot be empty".into());
    }
    Ok(trimmed.chars().take(MULTIPLAYER_CHAT_TEXT_LIMIT).collect())
}

async fn register_multiplayer_lobby_socket<G: Game + 'static>(
    runtime: &Arc<MultiplayerRuntime<G>>,
    tx: mpsc::UnboundedSender<String>,
) -> ActiveLobbySocket {
    match runtime.as_ref() {
        MultiplayerRuntime::Memory(rooms) => {
            ActiveLobbySocket::Memory(rooms.register_lobby_socket(tx))
        }
        MultiplayerRuntime::Redis(rooms) => {
            ActiveLobbySocket::Redis(rooms.register_lobby_socket(tx))
        }
    }
}

async fn send_multiplayer_lobby_to<G: Game + 'static>(
    runtime: &Arc<MultiplayerRuntime<G>>,
    tx: &mpsc::UnboundedSender<String>,
) {
    match runtime.as_ref() {
        MultiplayerRuntime::Memory(rooms) => rooms.send_lobby_to(tx),
        MultiplayerRuntime::Redis(rooms) => rooms.send_lobby_to(tx).await,
    }
}

async fn unregister_multiplayer_lobby_socket<G: Game + 'static>(
    runtime: &Arc<MultiplayerRuntime<G>>,
    lobby_socket: ActiveLobbySocket,
) {
    match (runtime.as_ref(), lobby_socket) {
        (MultiplayerRuntime::Memory(rooms), ActiveLobbySocket::Memory(id)) => {
            rooms.unregister_lobby_socket(id);
        }
        (MultiplayerRuntime::Redis(rooms), ActiveLobbySocket::Redis(id)) => {
            rooms.unregister_lobby_socket(id);
        }
        _ => {}
    }
}

async fn broadcast_multiplayer_lobby<G: Game + 'static>(runtime: &Arc<MultiplayerRuntime<G>>) {
    match runtime.as_ref() {
        MultiplayerRuntime::Memory(rooms) => rooms.broadcast_lobby(),
        MultiplayerRuntime::Redis(rooms) => rooms.publish_lobby_changed().await,
    }
}

async fn refresh_active_multiplayer_presence<G: Game + 'static>(
    runtime: &Arc<MultiplayerRuntime<G>>,
    active_room: &Option<ActiveMultiplayer<G>>,
) {
    if let (MultiplayerRuntime::Redis(rooms), Some(ActiveMultiplayer::Redis(active))) =
        (runtime.as_ref(), active_room.as_ref())
    {
        rooms.refresh_active_presence(active).await;
    }
}

async fn detach_active_multiplayer_room<G: Game + 'static>(
    runtime: &Arc<MultiplayerRuntime<G>>,
    active_room: &mut Option<ActiveMultiplayer<G>>,
) -> bool {
    match runtime.as_ref() {
        MultiplayerRuntime::Memory(rooms) => {
            let Some(active) = active_room.take() else {
                return false;
            };
            let ActiveMultiplayer::Memory(active) = active else {
                return false;
            };
            let mut active = Some(active);
            let detached = detach_active_room(rooms, &mut active);
            if let Some(active) = active {
                *active_room = Some(ActiveMultiplayer::Memory(active));
            }
            detached
        }
        MultiplayerRuntime::Redis(rooms) => {
            let Some(active) = active_room.take() else {
                return false;
            };
            let ActiveMultiplayer::Redis(active) = active else {
                return false;
            };
            let mut active = Some(active);
            let detached = rooms.detach_active_room(&mut active).await;
            if let Some(active) = active {
                *active_room = Some(ActiveMultiplayer::Redis(active));
            }
            detached
        }
    }
}

async fn replay_link_redirect(Path(slug): Path<String>) -> Redirect {
    if safe_share_slug(&slug) {
        Redirect::temporary(&format!("/?replay={slug}"))
    } else {
        Redirect::temporary("/")
    }
}

async fn multiplayer_lobby_rooms<G: Game + 'static>(
    runtime: &Arc<MultiplayerRuntime<G>>,
) -> Vec<MultiplayerLobbyRoom> {
    match runtime.as_ref() {
        MultiplayerRuntime::Memory(rooms) => rooms.lobby_rooms(),
        MultiplayerRuntime::Redis(rooms) => rooms.lobby_rooms().await,
    }
}

async fn all_page<G: Game + 'static>(
    replay_store: Option<Arc<ReplayStore>>,
    multiplayer: Arc<MultiplayerRuntime<G>>,
) -> Html<String> {
    let (rows, error) = match replay_store {
        Some(store) => match store.list_all(ALL_REPLAYS_PAGE_LIMIT).await {
            Ok(rows) => (rows, None),
            Err(message) => (Vec::new(), Some(message)),
        },
        None => (
            Vec::new(),
            Some("Replay storage is not configured".to_string()),
        ),
    };
    let lobbies = multiplayer_lobby_rooms(&multiplayer).await;
    Html(render_all_page(&rows, &lobbies, error.as_deref()))
}

fn render_all_page(
    rows: &[ReplayEntryWithAccount],
    lobbies: &[MultiplayerLobbyRoom],
    error: Option<&str>,
) -> String {
    let mut html = String::from(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Replay Logs</title>
<style>
:root { color-scheme: dark; font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: #111827; color: #e5e7eb; }
body { margin: 0; padding: 24px; background: #111827; }
main { max-width: 1180px; margin: 0 auto; }
header { display: flex; align-items: center; justify-content: space-between; gap: 16px; margin-bottom: 18px; }
h1 { margin: 0; font-size: 22px; font-weight: 800; }
h2 { margin: 0 0 12px; font-size: 16px; font-weight: 800; }
.meta { color: #9ca3af; font-size: 13px; }
.header-copy { display: flex; flex-direction: column; gap: 4px; }
.view-toggle { border: 1px solid #374151; border-radius: 6px; background: #1f2937; color: #e5e7eb; cursor: pointer; font: inherit; font-size: 13px; font-weight: 700; padding: 8px 12px; }
.view-toggle:hover { background: #374151; }
.panel[hidden] { display: none; }
.error { margin-bottom: 16px; border: 1px solid #7f1d1d; border-radius: 6px; background: #3f1218; color: #fecaca; padding: 10px 12px; }
table { width: 100%; border-collapse: collapse; overflow: hidden; border: 1px solid #374151; border-radius: 6px; background: #16213e; }
.summary-table { max-width: 360px; margin-bottom: 16px; }
th, td { padding: 8px 10px; border-bottom: 1px solid #283247; text-align: left; font-size: 13px; vertical-align: top; }
th { position: sticky; top: 0; background: #0f172a; color: #cbd5e1; font-size: 11px; text-transform: uppercase; letter-spacing: .04em; }
tr:last-child td { border-bottom: 0; }
td.mono { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; color: #cbd5e1; }
td.number { text-align: right; }
a { color: #93c5fd; text-decoration: none; font-weight: 700; }
a:hover { color: #bfdbfe; text-decoration: underline; }
.empty { border: 1px dashed #374151; border-radius: 6px; color: #9ca3af; padding: 18px; background: #16213e; }
@media (max-width: 760px) { body { padding: 12px; } table { display: block; overflow-x: auto; } header { align-items: flex-start; flex-direction: column; } .meta { margin-top: 6px; } }
</style>
</head>
<body>
<main>
"#,
    );
    html.push_str(
        "<header><div class=\"header-copy\"><h1>Replay Logs</h1><div class=\"meta\">Latest ",
    );
    html.push_str(&ALL_REPLAYS_PAGE_LIMIT.to_string());
    html.push_str(" saved games</div></div><button id=\"all-view-toggle\" class=\"view-toggle\" type=\"button\" aria-controls=\"replay-logs-section lobbies-section\" aria-pressed=\"false\">Show Lobbies</button></header>\n");
    html.push_str("<section id=\"replay-logs-section\" class=\"panel\">\n");
    if let Some(error) = error {
        html.push_str("<div class=\"error\">");
        html.push_str(&escape_html(error));
        html.push_str("</div>\n");
    }
    if rows.is_empty() {
        html.push_str("<div class=\"empty\">No replay logs found.</div>\n");
    } else {
        html.push_str(
            r#"<table>
<thead><tr><th>Saved</th><th>Result</th><th>Actions</th><th>Account</th><th>Replay ID</th><th>Open</th></tr></thead>
<tbody>
"#,
        );
        for row in rows {
            let replay = &row.entry;
            html.push_str("<tr><td><time data-ms=\"");
            html.push_str(&replay.saved_at_ms.to_string());
            html.push_str("\">");
            html.push_str(&replay.saved_at_ms.to_string());
            html.push_str("</time></td><td>");
            html.push_str(&escape_html(&replay.result));
            html.push_str("</td><td class=\"number\">");
            html.push_str(&replay.action_count.to_string());
            html.push_str("</td><td class=\"mono\">");
            html.push_str(&escape_html(&row.account_key));
            html.push_str("</td><td class=\"mono\">");
            html.push_str(&escape_html(&replay.id));
            html.push_str("</td><td><a href=\"/r/");
            html.push_str(&escape_html(&replay.share_slug));
            html.push_str("\">Open</a></td></tr>\n");
        }
        html.push_str(
            r#"</tbody>
</table>
"#,
        );
    }
    html.push_str(
        "</section>\n<section id=\"lobbies-section\" class=\"panel\" hidden>\n<h2>Lobbies</h2>\n",
    );
    html.push_str(
        r#"<table class="summary-table">
<thead><tr><th>Metric</th><th>Count</th></tr></thead>
<tbody><tr><td>Open lobbies</td><td class="number">"#,
    );
    html.push_str(&lobbies.len().to_string());
    html.push_str(
        r#"</td></tr></tbody>
</table>
"#,
    );
    if lobbies.is_empty() {
        html.push_str("<div class=\"empty\">No open lobbies found.</div>\n");
    } else {
        html.push_str(
            r#"<table>
<thead><tr><th>Code</th><th>Status</th><th>Occupied</th><th>Connected</th><th>Spectators</th><th>Time Control</th><th>Last Activity</th><th>Open</th></tr></thead>
<tbody>
"#,
        );
        for lobby in lobbies {
            html.push_str("<tr><td class=\"mono\">");
            html.push_str(&escape_html(&lobby.code));
            html.push_str("</td><td>");
            html.push_str(&escape_html(&lobby.status));
            html.push_str("</td><td class=\"number\">");
            html.push_str(&lobby.occupied.to_string());
            html.push_str("</td><td class=\"number\">");
            html.push_str(&lobby.connected.to_string());
            html.push_str("</td><td class=\"number\">");
            html.push_str(&lobby.spectator_count.to_string());
            html.push_str("</td><td>");
            html.push_str(&escape_html(&lobby_time_control_label(lobby)));
            html.push_str("</td><td><time data-ms=\"");
            html.push_str(&lobby.last_activity_ms.to_string());
            html.push_str("\">");
            html.push_str(&lobby.last_activity_ms.to_string());
            html.push_str("</time></td><td><a href=\"/?room=");
            html.push_str(&escape_html(&lobby.code));
            html.push_str("\">Open</a></td></tr>\n");
        }
        html.push_str(
            r#"</tbody>
</table>
"#,
        );
    }
    html.push_str("</section>\n");
    html.push_str(
        r#"</main>
<script>
const toggle = document.getElementById('all-view-toggle');
const replaySection = document.getElementById('replay-logs-section');
const lobbiesSection = document.getElementById('lobbies-section');
function setAllView(showLobbies) {
  replaySection.hidden = showLobbies;
  lobbiesSection.hidden = !showLobbies;
  toggle.textContent = showLobbies ? 'Show Replay Logs' : 'Show Lobbies';
  toggle.setAttribute('aria-pressed', showLobbies ? 'true' : 'false');
}
toggle?.addEventListener('click', () => setAllView(lobbiesSection.hidden));
for (const el of document.querySelectorAll('time[data-ms]')) {
  const ms = Number(el.dataset.ms);
  if (Number.isFinite(ms)) {
    el.textContent = new Date(ms).toLocaleString();
    el.title = String(ms);
  }
}
</script>
</body>
</html>
"#,
    );
    html
}

fn lobby_time_control_label(lobby: &MultiplayerLobbyRoom) -> String {
    match (lobby.time_minutes, lobby.increment_seconds) {
        (Some(minutes), Some(increment)) => format!("{minutes}+{increment}"),
        (Some(minutes), None) => format!("{minutes} min"),
        (None, Some(increment)) => format!("+{increment}s"),
        (None, None) => "Untimed".into(),
    }
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Launch the web analysis board server.
///
/// Serves static files from the presenter's `static_dir()` and provides
/// a WebSocket endpoint at `/ws` for real-time game interaction.
pub async fn serve<G: Game + 'static>(
    port: u16,
    evaluator: Arc<dyn Evaluator<G> + Sync>,
    eval_name: &str,
    presenter: Arc<dyn GamePresenter<G>>,
    human_players: [bool; 2],
    replay: Option<(G, GameLog)>,
    web_log_dir: Option<PathBuf>,
) {
    let static_dir = presenter.static_dir().to_path_buf();
    let replay = replay.map(|(s, l)| (s, Arc::new(l)));
    let replay_store = replay_store_from_config(web_log_dir).await;
    let store = Arc::new(UserSessionStore::new(
        SessionFactory::new_game(evaluator, eval_name, presenter, human_players, replay),
        replay_store.clone(),
    ));
    let multiplayer =
        multiplayer_runtime_from_config(Arc::clone(&store.factory), replay_store.clone()).await;
    println!("Anonymous HexFish WebSocket sessions enabled");
    tracing::info!("anonymous HexFish WebSocket sessions enabled");

    let app =
        Router::new()
            .route(
                "/ws",
                axum::routing::get({
                    let store = Arc::clone(&store);
                    let multiplayer = Arc::clone(&multiplayer);
                    move |ws: WebSocketUpgrade| {
                        let store = Arc::clone(&store);
                        let multiplayer = Arc::clone(&multiplayer);
                        async move {
                            ws.on_upgrade(move |socket| handle_socket(socket, store, multiplayer))
                        }
                    }
                }),
            )
            .route(
                "/all",
                axum::routing::get({
                    let replay_store = replay_store.clone();
                    let multiplayer = Arc::clone(&multiplayer);
                    move || {
                        let replay_store = replay_store.clone();
                        let multiplayer = Arc::clone(&multiplayer);
                        async move { all_page(replay_store, multiplayer).await }
                    }
                }),
            )
            .route("/r/{slug}", axum::routing::get(replay_link_redirect))
            .fallback_service(ServeDir::new(&static_dir));

    let addr = format!("0.0.0.0:{port}");
    println!("Analysis board: http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Launch the web analysis board with a pre-built game state.
pub async fn serve_with_state<G: Game + 'static>(
    port: u16,
    state: G,
    evaluator: Arc<dyn Evaluator<G> + Sync>,
    eval_name: &str,
    presenter: Arc<dyn GamePresenter<G>>,
    human_players: [bool; 2],
) {
    let static_dir = presenter.static_dir().to_path_buf();
    let store = Arc::new(UserSessionStore::new(
        SessionFactory::with_state(state, evaluator, eval_name, presenter, human_players),
        None,
    ));
    let multiplayer = managed_memory_multiplayer_runtime(Arc::clone(&store.factory), None);

    let app =
        Router::new()
            .route(
                "/ws",
                axum::routing::get({
                    let store = Arc::clone(&store);
                    move |ws: WebSocketUpgrade| {
                        let store = Arc::clone(&store);
                        let multiplayer = Arc::clone(&multiplayer);
                        async move {
                            ws.on_upgrade(move |socket| handle_socket(socket, store, multiplayer))
                        }
                    }
                }),
            )
            .route("/r/{slug}", axum::routing::get(replay_link_redirect))
            .fallback_service(ServeDir::new(&static_dir));

    let addr = format!("0.0.0.0:{port}");
    println!("Analysis board: http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Launch the web analysis board with a pre-built timeline of game states.
pub async fn serve_with_timeline<G: Game + 'static>(
    port: u16,
    timeline: Vec<(String, G)>,
    evaluator: Arc<dyn Evaluator<G> + Sync>,
    presenter: Arc<dyn GamePresenter<G>>,
    human_players: [bool; 2],
) {
    let static_dir = presenter.static_dir().to_path_buf();
    let store = Arc::new(UserSessionStore::new(
        SessionFactory::with_timeline(timeline, evaluator, presenter, human_players),
        None,
    ));
    let multiplayer = managed_memory_multiplayer_runtime(Arc::clone(&store.factory), None);

    let app =
        Router::new()
            .route(
                "/ws",
                axum::routing::get({
                    let store = Arc::clone(&store);
                    move |ws: WebSocketUpgrade| {
                        let store = Arc::clone(&store);
                        let multiplayer = Arc::clone(&multiplayer);
                        async move {
                            ws.on_upgrade(move |socket| handle_socket(socket, store, multiplayer))
                        }
                    }
                }),
            )
            .route("/r/{slug}", axum::routing::get(replay_link_redirect))
            .fallback_service(ServeDir::new(&static_dir));

    let addr = format!("0.0.0.0:{port}");
    println!("Analysis board: http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

/// Run a streaming MCTS search, sending progress updates over the socket.
///
/// Handles `BotMove`, `RunSims`, and `RunSearch` messages. Returns the final response
/// messages, or `Err(())` if the socket disconnected during search.
pub async fn run_search<G: Game + 'static>(
    socket: &mut WebSocket,
    session: &mut GameSession<G>,
    msg: &ClientMsg,
) -> Result<Vec<ServerMsg>, ()> {
    match session.begin_search(msg) {
        Err(msgs) => Ok(msgs),
        Ok(active_budget) => {
            let mut last_progress_sent = 0;
            let mut last_progress_sent_at = Instant::now();
            let mut ticks_since_interrupt_check = 0;
            let mut cpu_load = CpuLoadSampler::new();
            let result = loop {
                if let Some(result) = session.search_tick() {
                    break result;
                }
                ticks_since_interrupt_check += 1;
                if ticks_since_interrupt_check >= SEARCH_INTERRUPT_INTERVAL {
                    ticks_since_interrupt_check = 0;
                    tokio::task::yield_now().await;
                    if let Some(msgs) = handle_search_interrupt(socket, session, msg).await? {
                        return Ok(msgs);
                    }
                }
                if let Some((snap, labels)) = session.snapshot_with_labels() {
                    if should_send_search_progress(
                        snap.fresh_simulations,
                        last_progress_sent,
                        last_progress_sent_at.elapsed(),
                    ) {
                        last_progress_sent = snap.fresh_simulations;
                        last_progress_sent_at = Instant::now();
                        send_msg(
                            socket,
                            &ServerMsg::SearchProgress {
                                snapshot: snap,
                                action_labels: labels,
                                sims_total: active_budget.sims_total,
                                budget: active_budget.budget,
                                cpu_load: cpu_load.sample(),
                            },
                        )
                        .await?;
                        if let Some(subtree_msg) = session.explore_subtree_msg() {
                            let _ = send_msg(socket, &subtree_msg).await;
                        }
                        if let Some(msgs) = handle_search_interrupt(socket, session, msg).await? {
                            return Ok(msgs);
                        }
                    }
                }
            };
            if let Some((snap, labels)) = session.snapshot_with_labels() {
                if snap.fresh_simulations > last_progress_sent {
                    send_msg(
                        socket,
                        &ServerMsg::SearchProgress {
                            snapshot: snap,
                            action_labels: labels,
                            sims_total: active_budget.sims_total,
                            budget: active_budget.budget,
                            cpu_load: cpu_load.sample(),
                        },
                    )
                    .await?;
                }
            }
            Ok(session.finish_search(msg, result))
        }
    }
}

fn should_send_search_progress(
    fresh_simulations: u32,
    last_progress_sent: u32,
    elapsed_since_last_send: Duration,
) -> bool {
    fresh_simulations >= last_progress_sent.saturating_add(PROGRESS_INTERVAL)
        || elapsed_since_last_send >= Duration::from_millis(SEARCH_PROGRESS_KEEPALIVE_MS)
}

async fn handle_search_interrupt<G: Game + 'static>(
    socket: &mut WebSocket,
    session: &mut GameSession<G>,
    active_msg: &ClientMsg,
) -> Result<Option<Vec<ServerMsg>>, ()> {
    let Some(inbound) = socket.recv().now_or_never() else {
        return Ok(None);
    };
    let Some(inbound) = inbound else {
        return Err(());
    };
    let ws_msg = inbound.map_err(|_| ())?;
    let text = match ws_msg {
        Message::Text(text) => text,
        Message::Close(_) => return Err(()),
        _ => return Ok(None),
    };

    let msg: ClientMsg = match serde_json::from_str(&text) {
        Ok(msg) => msg,
        Err(e) => {
            send_msg(
                socket,
                &ServerMsg::Error {
                    message: format!("Invalid message: {e}"),
                },
            )
            .await?;
            return Ok(None);
        }
    };

    match msg {
        ClientMsg::Ping => {
            send_msg(socket, &ServerMsg::Pong).await?;
            Ok(None)
        }
        pause @ ClientMsg::PauseSearch { .. } => {
            if matches!(active_msg, ClientMsg::BotMove { .. }) {
                send_msg(
                    socket,
                    &ServerMsg::Error {
                        message: "Bot moves cannot be paused".into(),
                    },
                )
                .await?;
                return Ok(None);
            }

            if client_msg_target(&pause) != client_msg_target(active_msg) {
                send_msg(
                    socket,
                    &ServerMsg::Error {
                        message: "Pause target does not match the active search".into(),
                    },
                )
                .await?;
                return Ok(None);
            }

            Ok(Some(session.pause_search()))
        }
        play @ ClientMsg::PlayAction { .. } => {
            if matches!(active_msg, ClientMsg::BotMove { .. }) {
                send_msg(
                    socket,
                    &ServerMsg::Error {
                        message: "Bot moves cannot be interrupted".into(),
                    },
                )
                .await?;
                return Ok(None);
            }

            if client_msg_target(active_msg) != ViewTarget::Analysis {
                send_msg(
                    socket,
                    &ServerMsg::Error {
                        message: "Cannot play actions during a replay search".into(),
                    },
                )
                .await?;
                return Ok(None);
            }

            let mut msgs = session.pause_search();
            msgs.extend(session.handle(play));
            Ok(Some(msgs))
        }
        _ => {
            send_msg(
                socket,
                &ServerMsg::Error {
                    message: "Search is running; pause before sending another command".into(),
                },
            )
            .await?;
            Ok(None)
        }
    }
}

async fn handle_socket<G: Game + 'static>(
    mut socket: WebSocket,
    store: Arc<UserSessionStore<G>>,
    multiplayer: Arc<MultiplayerRuntime<G>>,
) {
    let session_key = match authenticate_socket(&mut socket).await {
        Ok(session_key) => session_key,
        Err(()) => return,
    };
    let user_id = session_key.user_id.clone();
    let display_name = session_key.display_name.clone();
    let session_scope = session_key.scope;
    let account_key = redacted_account_key(&user_id);
    let (user_session, created) = store.get_or_create(&user_id);
    if session_scope == "clerk" {
        if let Some(display_name) = display_name.as_deref() {
            store.set_display_name(&user_id, display_name);
        }
    } else {
        store.ensure_guest_username(&user_id, display_name.as_deref());
    }
    let board_code = format_board_fingerprint(user_session.board_fingerprint);
    if created {
        tracing::info!(
            account = %account_key,
            scope = session_scope,
            session_id = user_session.session_id,
            seed = user_session.seed,
            board_code = %board_code,
            "created HexFish game session"
        );
    } else {
        tracing::info!(
            account = %account_key,
            scope = session_scope,
            session_id = user_session.session_id,
            seed = user_session.seed,
            board_code = %board_code,
            "reused HexFish game session"
        );
    }
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();
    let socket_id = user_session.register_socket(outbound_tx.clone());

    let disconnect = handle_authenticated_socket(
        &mut socket,
        Arc::clone(&store),
        Arc::clone(&user_session),
        multiplayer,
        user_id.clone(),
        display_name,
        socket_id,
        outbound_tx,
        outbound_rx,
    )
    .await;
    user_session.unregister_socket(socket_id);
    let close_code = disconnect.close_code.as_deref().unwrap_or("");
    tracing::info!(
        account = %account_key,
        scope = session_scope,
        session_id = user_session.session_id,
        seed = user_session.seed,
        board_code = %board_code,
        socket_id,
        disconnect_kind = disconnect.kind,
        close_code,
        close_reason = %disconnect.close_reason,
        "disconnected HexFish game socket"
    );
    if session_key.ephemeral {
        store.remove_if_same(&user_id, &user_session);
    }
}

fn redacted_account_key(user_id: &str) -> String {
    let mut hasher = DefaultHasher::new();
    user_id.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn format_board_fingerprint(fingerprint: Option<u64>) -> String {
    fingerprint
        .map(|value| value.to_string())
        .unwrap_or_else(|| "none".into())
}

#[derive(Debug, Clone)]
struct SocketDisconnectInfo {
    kind: &'static str,
    close_code: Option<String>,
    close_reason: String,
}

impl SocketDisconnectInfo {
    fn recv_closed() -> Self {
        Self {
            kind: "recv_closed",
            close_code: None,
            close_reason: String::new(),
        }
    }

    fn recv_error() -> Self {
        Self {
            kind: "recv_error",
            close_code: None,
            close_reason: String::new(),
        }
    }

    fn send_failed() -> Self {
        Self {
            kind: "send_failed",
            close_code: None,
            close_reason: String::new(),
        }
    }

    fn server_closed() -> Self {
        Self {
            kind: "server_closed",
            close_code: None,
            close_reason: String::new(),
        }
    }

    fn client_close(close_code: Option<String>, close_reason: String) -> Self {
        Self {
            kind: "client_close",
            close_code,
            close_reason,
        }
    }
}

impl From<()> for SocketDisconnectInfo {
    fn from(_: ()) -> Self {
        Self::send_failed()
    }
}

async fn authenticate_socket(socket: &mut WebSocket) -> Result<SocketSessionKey, ()> {
    let Some(Ok(ws_msg)) = socket.recv().await else {
        return Err(());
    };
    let text = match ws_msg {
        Message::Text(t) => t,
        Message::Close(_) => return Err(()),
        _ => {
            send_unauthorized(socket).await;
            return Err(());
        }
    };

    let client_msg: ClientMsg = match serde_json::from_str(&text) {
        Ok(m) => m,
        Err(_) => {
            send_unauthorized(socket).await;
            return Err(());
        }
    };
    let ClientMsg::Authenticate {
        token,
        anonymous_session,
        clerk_user_id,
        username,
    } = client_msg
    else {
        send_unauthorized(socket).await;
        return Err(());
    };
    let display_name = normalize_display_name(username);

    Ok(socket_session_key_from_auth(
        &token,
        clerk_user_id,
        anonymous_session,
        display_name,
    ))
}

fn socket_session_key_from_auth(
    token: &str,
    clerk_user_id: Option<String>,
    anonymous_session: Option<String>,
    display_name: Option<String>,
) -> SocketSessionKey {
    if let Some(user_id) = clerk_user_id_from_auth(token, clerk_user_id.as_deref()) {
        return SocketSessionKey {
            user_id,
            display_name,
            ephemeral: false,
            scope: "clerk",
        };
    }

    if let Some(user_id) = anonymous_user_id_from_token(token) {
        return SocketSessionKey {
            user_id,
            display_name,
            ephemeral: false,
            scope: "anonymous",
        };
    }

    if let Some(user_id) = anonymous_session
        .as_deref()
        .and_then(anonymous_user_id_from_id)
    {
        return SocketSessionKey {
            user_id,
            display_name,
            ephemeral: false,
            scope: "anonymous",
        };
    }

    SocketSessionKey {
        user_id: format!("anonymous:{}", fastrand::u64(..)),
        display_name,
        ephemeral: true,
        scope: "anonymous-ephemeral",
    }
}

fn normalize_display_name(name: Option<String>) -> Option<String> {
    const MAX_DISPLAY_NAME_CHARS: usize = 24;
    let name = name?;
    let mut normalized = String::new();
    let mut last_was_space = false;
    let mut chars = 0;

    for ch in name.trim().chars() {
        if chars >= MAX_DISPLAY_NAME_CHARS {
            break;
        }
        if ch.is_control() {
            continue;
        }
        if ch.is_whitespace() {
            if normalized.is_empty() || last_was_space {
                continue;
            }
            normalized.push(' ');
            last_was_space = true;
        } else {
            normalized.push(ch);
            last_was_space = false;
        }
        chars += 1;
    }

    let normalized = normalized.trim().to_string();
    (!normalized.is_empty()).then_some(normalized)
}

fn normalize_profile_username(name: Option<&str>) -> Option<String> {
    const MIN_USERNAME_CHARS: usize = 3;
    const MAX_USERNAME_CHARS: usize = 24;
    let mut normalized = String::new();

    for ch in name?.trim().chars() {
        if normalized.len() >= MAX_USERNAME_CHARS {
            break;
        }
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            normalized.push(ch);
        }
    }

    (normalized.len() >= MIN_USERNAME_CHARS).then_some(normalized)
}

fn default_profile_username(user_id: &str, registry: &UsernameRegistry) -> String {
    for salt in 0_u64.. {
        let mut hasher = DefaultHasher::new();
        user_id.hash(&mut hasher);
        salt.hash(&mut hasher);
        let unique_key = format!("{:016x}", hasher.finish());
        let username = format!("User{}", &unique_key[..8]);
        if !registry.by_name.contains_key(&username_key(&username)) {
            return username;
        }
    }
    unreachable!("default username key space is exhausted")
}

fn username_key(username: &str) -> String {
    username.to_ascii_lowercase()
}

fn clerk_user_id_from_auth(token: &str, clerk_user_id: Option<&str>) -> Option<String> {
    if token.trim().is_empty() {
        return None;
    }
    let id = clerk_user_id?.trim();
    if id.is_empty() || id.len() > 128 {
        return None;
    }
    let safe = id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    safe.then(|| format!("clerk:{id}"))
}

fn anonymous_user_id_from_token(token: &str) -> Option<String> {
    let id = token.strip_prefix("anon:")?;
    anonymous_user_id_from_id(id)
}

fn anonymous_user_id_from_id(id: &str) -> Option<String> {
    if id.is_empty() || id.len() > 128 {
        return None;
    }
    let safe = id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    safe.then(|| format!("anonymous:{id}"))
}

async fn handle_authenticated_socket<G: Game + 'static>(
    socket: &mut WebSocket,
    store: Arc<UserSessionStore<G>>,
    user_session: Arc<UserSession<G>>,
    multiplayer: Arc<MultiplayerRuntime<G>>,
    user_id: String,
    display_name: Option<String>,
    socket_id: u64,
    outbound_tx: mpsc::UnboundedSender<String>,
    mut outbound_rx: mpsc::UnboundedReceiver<String>,
) -> SocketDisconnectInfo {
    let mut auto_search: Option<SearchBudget> = None;
    let mut active_room: Option<ActiveMultiplayer<G>> = None;

    // Send initial state.
    {
        let mut session = user_session.session.lock().await;
        let init_msgs = session.handle(ClientMsg::GetState);
        for msg in init_msgs {
            if send_msg(socket, &msg).await.is_err() {
                return SocketDisconnectInfo::send_failed();
            }
        }
    }
    if send_msg(socket, &store.profile_msg(&user_id))
        .await
        .is_err()
    {
        return SocketDisconnectInfo::send_failed();
    }

    let lobby_socket_id =
        register_multiplayer_lobby_socket(&multiplayer, outbound_tx.clone()).await;
    send_multiplayer_lobby_to(&multiplayer, &outbound_tx).await;

    let disconnect = loop {
        tokio::select! {
            outbound = outbound_rx.recv() => {
                let Some(json) = outbound else {
                    break SocketDisconnectInfo::server_closed();
                };
                if send_raw_msg(socket, json).await.is_err() {
                    break SocketDisconnectInfo::send_failed();
                }
            }
            inbound = socket.recv() => {
                let ws_msg = match inbound {
                    Some(Ok(Message::Close(frame))) => {
                        let (close_code, close_reason) = frame
                            .map(|frame| (Some(frame.code.to_string()), frame.reason.to_string()))
                            .unwrap_or((None, String::new()));
                        break SocketDisconnectInfo::client_close(close_code, close_reason);
                    }
                    Some(Ok(ws_msg)) => ws_msg,
                    Some(Err(_)) => break SocketDisconnectInfo::recv_error(),
                    None => break SocketDisconnectInfo::recv_closed(),
                };
                if let Err(disconnect) = handle_authenticated_message(
                    socket,
                    &store,
                    &user_session,
                    &multiplayer,
                    &user_id,
                    display_name.as_deref(),
                    socket_id,
                    &outbound_tx,
                    &mut active_room,
                    &mut auto_search,
                    ws_msg,
                )
                .await
                {
                    break disconnect;
                }
            }
        }
    };

    let detached = detach_active_multiplayer_room(&multiplayer, &mut active_room).await;
    unregister_multiplayer_lobby_socket(&multiplayer, lobby_socket_id).await;
    if detached {
        broadcast_multiplayer_lobby(&multiplayer).await;
    }
    disconnect
}

async fn handle_authenticated_message<G: Game + 'static>(
    socket: &mut WebSocket,
    store: &Arc<UserSessionStore<G>>,
    user_session: &Arc<UserSession<G>>,
    multiplayer: &Arc<MultiplayerRuntime<G>>,
    user_id: &str,
    display_name: Option<&str>,
    socket_id: u64,
    outbound_tx: &mpsc::UnboundedSender<String>,
    active_room: &mut Option<ActiveMultiplayer<G>>,
    auto_search: &mut Option<SearchBudget>,
    ws_msg: Message,
) -> Result<(), SocketDisconnectInfo> {
    let text = match ws_msg {
        Message::Text(t) => t,
        Message::Close(frame) => {
            let (close_code, close_reason) = frame
                .map(|frame| (Some(frame.code.to_string()), frame.reason.to_string()))
                .unwrap_or((None, String::new()));
            return Err(SocketDisconnectInfo::client_close(close_code, close_reason));
        }
        _ => return Ok(()),
    };

    if text.len() > MAX_WEBSOCKET_MESSAGE_BYTES {
        send_msg(
            socket,
            &ServerMsg::Error {
                message: "WebSocket message is too large".into(),
            },
        )
        .await?;
        return Ok(());
    }

    let client_msg: ClientMsg = match serde_json::from_str(&text) {
        Ok(m) => m,
        Err(e) => {
            send_msg(
                socket,
                &ServerMsg::Error {
                    message: format!("Invalid message: {e}"),
                },
            )
            .await?;
            return Ok(());
        }
    };

    if matches!(client_msg, ClientMsg::Ping) {
        refresh_active_multiplayer_presence(multiplayer, active_room).await;
        send_msg(socket, &ServerMsg::Pong).await?;
        return Ok(());
    }

    if matches!(
        &client_msg,
        ClientMsg::GetProfile | ClientMsg::SetUsername { .. }
    ) {
        let responses = handle_profile_message(store, user_id, client_msg);
        for msg in responses {
            send_msg(socket, &msg).await?;
        }
        return Ok(());
    }

    if is_multiplayer_msg(&client_msg)
        || (active_room.is_some() && matches!(&client_msg, ClientMsg::Undo | ClientMsg::Redo))
    {
        let profile_display_name = store
            .current_username(user_id)
            .or_else(|| display_name.map(str::to_string));
        let responses = handle_multiplayer_runtime_message(
            multiplayer,
            user_id,
            profile_display_name.as_deref(),
            outbound_tx,
            active_room,
            client_msg,
        )
        .await;
        for msg in responses {
            send_msg(socket, &msg).await?;
        }
        return Ok(());
    }

    if active_room.is_some() && is_blocked_while_in_multiplayer(&client_msg) {
        send_msg(
            socket,
            &ServerMsg::Error {
                message: "Leave multiplayer before using analysis game controls".into(),
            },
        )
        .await?;
        return Ok(());
    }

    let message_target = client_msg_target(&client_msg);

    // Track auto-search state at the connection level.
    if let ClientMsg::SetAutoSearch {
        enabled,
        target,
        budget,
    } = &client_msg
    {
        let budget = budget.unwrap_or_else(|| SearchBudget::simulations(target.unwrap_or(0)));
        *auto_search = if *enabled { Some(budget) } else { None };
    }

    let responses = if message_target == ViewTarget::Replay {
        handle_replay_message(socket, user_session, client_msg).await?
    } else {
        let mut session = user_session.session.lock().await;
        let was_terminal = session.current_game_ended();
        let mut responses = match client_msg {
            ClientMsg::ListReplays => match &user_session.replay_store {
                Some(_) => match list_replay_entries(user_session).await {
                    Ok(entries) => vec![ServerMsg::ReplayList { entries }],
                    Err(message) => vec![ServerMsg::Error { message }],
                },
                None => vec![ServerMsg::Error {
                    message: "Replay storage is not configured".into(),
                }],
            },
            ClientMsg::DeleteReplay { id } => match &user_session.replay_store {
                Some(store) => match store.delete(&user_session.account_key, &id).await {
                    Ok(()) => match list_replay_entries(user_session).await {
                        Ok(entries) => vec![ServerMsg::ReplayList { entries }],
                        Err(message) => vec![ServerMsg::Error { message }],
                    },
                    Err(message) => vec![ServerMsg::Error { message }],
                },
                None => vec![ServerMsg::Error {
                    message: "Replay storage is not configured".into(),
                }],
            },
            ClientMsg::SetReplayFavorite { id, favorite } => match &user_session.replay_store {
                Some(store) => match store
                    .set_favorite(&user_session.account_key, &id, favorite)
                    .await
                {
                    Ok(()) => match list_replay_entries(user_session).await {
                        Ok(entries) => vec![ServerMsg::ReplayList { entries }],
                        Err(message) => vec![ServerMsg::Error { message }],
                    },
                    Err(message) => vec![ServerMsg::Error { message }],
                },
                None => vec![ServerMsg::Error {
                    message: "Replay storage is not configured".into(),
                }],
            },
            ClientMsg::SaveReplay => {
                let mut responses = Vec::new();
                if let Some(msg) = save_current_replay_once(user_session, &mut session).await {
                    responses.push(msg);
                }
                responses
            }
            msg @ ClientMsg::NewGame { .. } => {
                let pending_log = if was_terminal {
                    None
                } else {
                    session.export_unsaved_current_log()
                };
                let mut msgs = session.handle(msg);
                let started_game = msgs
                    .iter()
                    .any(|msg| matches!(msg, ServerMsg::GameState { .. }));
                if started_game {
                    if let Some(log) = pending_log {
                        if let Some(error) = save_replay_log(user_session, &log).await {
                            msgs.insert(0, error);
                        }
                    }
                }
                msgs
            }
            msg @ ClientMsg::StartEditedGame { .. } => session.handle(msg),
            msg @ (ClientMsg::BotMove { .. }
            | ClientMsg::RunSims { .. }
            | ClientMsg::RunSearch { .. }) => run_search(socket, &mut session, &msg).await?,
            msg => session.handle(msg),
        };
        if !was_terminal && responses_include_terminal_game_state(&responses) {
            if let Some(error) = save_current_replay_once(user_session, &mut session).await {
                responses.insert(0, error);
            }
        }
        responses
    };

    // Check if any response is a state update that should trigger auto-search.
    let has_state_update = responses
        .iter()
        .any(|m| matches!(m, ServerMsg::GameState { .. }));

    for msg in &responses {
        send_msg(socket, msg).await?;
    }
    if has_state_update && message_target == ViewTarget::Analysis {
        user_session.broadcast_game_states_to_others(socket_id, &responses);
    }

    // Auto-search: trigger RunSims after state changes (e.g. PlayAction, BotMove).
    if has_state_update && message_target == ViewTarget::Analysis {
        if let Some(budget) = *auto_search {
            let mut session = user_session.session.lock().await;
            if session.should_auto_search() {
                let auto_msg = ClientMsg::RunSearch {
                    budget,
                    target: Some(ViewTarget::Analysis),
                };
                let msgs = run_search(socket, &mut session, &auto_msg).await?;
                for msg in msgs {
                    send_msg(socket, &msg).await?;
                }
            }
        }
    }

    Ok(())
}

fn is_multiplayer_msg(msg: &ClientMsg) -> bool {
    matches!(
        msg,
        ClientMsg::CreateMultiplayerRoom { .. }
            | ClientMsg::ListMultiplayerRooms
            | ClientMsg::GetMultiplayerRoom
            | ClientMsg::JoinMultiplayerRoom { .. }
            | ClientMsg::LeaveMultiplayerRoom
            | ClientMsg::PlayMultiplayerAction { .. }
            | ClientMsg::SendMultiplayerChat { .. }
            | ClientMsg::ResignMultiplayerGame
            | ClientMsg::AddMultiplayerOpponentTime
    )
}

fn is_blocked_while_in_multiplayer(msg: &ClientMsg) -> bool {
    matches!(
        msg,
        ClientMsg::NewGame { .. }
            | ClientMsg::StartEditedGame { .. }
            | ClientMsg::SaveReplay
            | ClientMsg::PlayAction { .. }
            | ClientMsg::ResignGame
            | ClientMsg::BotMove { .. }
            | ClientMsg::RunSims { .. }
            | ClientMsg::RunSearch { .. }
            | ClientMsg::PauseSearch { .. }
            | ClientMsg::GetSnapshot
            | ClientMsg::ExploreSubtree { .. }
            | ClientMsg::TakeOver { .. }
            | ClientMsg::ReleaseControl { .. }
            | ClientMsg::SetAutoplay { .. }
            | ClientMsg::SetSingleplayer { .. }
            | ClientMsg::Undo
            | ClientMsg::Redo
            | ClientMsg::SetLogCursor { .. }
            | ClientMsg::SetConfig { .. }
            | ClientMsg::SetAutoSearch { .. }
    )
}

fn handle_profile_message<G: Game + 'static>(
    store: &Arc<UserSessionStore<G>>,
    user_id: &str,
    msg: ClientMsg,
) -> Vec<ServerMsg> {
    match msg {
        ClientMsg::GetProfile => vec![store.profile_msg(user_id)],
        ClientMsg::SetUsername { .. } => vec![ServerMsg::Error {
            message: "Username is managed by Clerk".into(),
        }],
        _ => vec![ServerMsg::Error {
            message: "Unsupported profile message".into(),
        }],
    }
}

async fn handle_multiplayer_runtime_message<G: Game + 'static>(
    runtime: &Arc<MultiplayerRuntime<G>>,
    user_id: &str,
    display_name: Option<&str>,
    outbound_tx: &mpsc::UnboundedSender<String>,
    active_room: &mut Option<ActiveMultiplayer<G>>,
    msg: ClientMsg,
) -> Vec<ServerMsg> {
    match runtime.as_ref() {
        MultiplayerRuntime::Memory(rooms) => {
            let mut memory_active = match active_room.take() {
                Some(ActiveMultiplayer::Memory(active)) => Some(active),
                Some(ActiveMultiplayer::Redis(_)) | None => None,
            };
            let responses = handle_multiplayer_message_for_user(
                rooms,
                user_id,
                display_name,
                outbound_tx,
                &mut memory_active,
                msg,
            )
            .await;
            *active_room = memory_active.map(ActiveMultiplayer::Memory);
            responses
        }
        MultiplayerRuntime::Redis(rooms) => {
            let mut redis_active = match active_room.take() {
                Some(ActiveMultiplayer::Redis(active)) => Some(active),
                Some(ActiveMultiplayer::Memory(_)) | None => None,
            };
            let responses = rooms
                .handle_message_for_user(user_id, display_name, outbound_tx, &mut redis_active, msg)
                .await;
            *active_room = redis_active.map(ActiveMultiplayer::Redis);
            responses
        }
    }
}

#[cfg(test)]
async fn handle_multiplayer_message<G: Game + 'static>(
    rooms: &Arc<MultiplayerRoomStore<G>>,
    user_id: &str,
    outbound_tx: &mpsc::UnboundedSender<String>,
    active_room: &mut Option<ActiveMultiplayerRoom<G>>,
    msg: ClientMsg,
) -> Vec<ServerMsg> {
    handle_multiplayer_message_for_user(rooms, user_id, None, outbound_tx, active_room, msg).await
}

async fn handle_multiplayer_message_for_user<G: Game + 'static>(
    rooms: &Arc<MultiplayerRoomStore<G>>,
    user_id: &str,
    display_name: Option<&str>,
    outbound_tx: &mpsc::UnboundedSender<String>,
    active_room: &mut Option<ActiveMultiplayerRoom<G>>,
    msg: ClientMsg,
) -> Vec<ServerMsg> {
    if active_room
        .as_ref()
        .is_some_and(|active| !active.room.socket_registered(active.socket_id))
    {
        *active_room = None;
    }

    match msg {
        ClientMsg::ListMultiplayerRooms => vec![rooms.lobby_msg()],
        ClientMsg::GetMultiplayerRoom => {
            let Some(active) = active_room.as_ref() else {
                return vec![ServerMsg::Error {
                    message: "Join a multiplayer room before refreshing it".into(),
                }];
            };
            let mut messages = vec![active.room.room_msg_for_active(active)];
            if active.viewer.player().is_some() {
                if let Some(chat) = active.room.chat_msg() {
                    messages.push(chat);
                }
            }
            messages
        }
        ClientMsg::CreateMultiplayerRoom {
            preferred_player,
            code,
            is_public,
            time_minutes,
            increment_seconds,
        } => {
            if detach_active_room_and_maybe_vacate(rooms, active_room, true) {
                rooms.broadcast_lobby();
            }
            let (room, player) = match rooms.create_room_with_display_name(
                user_id,
                display_name,
                preferred_player,
                code,
                is_public,
                time_minutes,
                increment_seconds,
            ) {
                Ok(room) => room,
                Err(message) => return vec![ServerMsg::Error { message }],
            };
            let responses = activate_multiplayer_room(
                Arc::clone(rooms),
                room,
                user_id,
                display_name,
                MultiplayerViewer::Player(player),
                outbound_tx,
                active_room,
            )
            .await;
            rooms.broadcast_lobby();
            responses
        }
        ClientMsg::JoinMultiplayerRoom { code } => {
            if detach_active_room_and_maybe_vacate(rooms, active_room, true) {
                rooms.broadcast_lobby();
            }
            let (room, viewer) =
                match rooms.join_room_with_display_name(user_id, display_name, &code) {
                    Ok(room) => room,
                    Err(message) => return vec![ServerMsg::Error { message }],
                };
            let responses = activate_multiplayer_room(
                Arc::clone(rooms),
                Arc::clone(&room),
                user_id,
                display_name,
                viewer,
                outbound_tx,
                active_room,
            )
            .await;
            rooms.broadcast_lobby();
            maybe_schedule_managed_bot_move(Arc::clone(rooms), room);
            responses
        }
        ClientMsg::LeaveMultiplayerRoom => {
            if detach_active_room_and_maybe_vacate(rooms, active_room, true) {
                rooms.broadcast_lobby();
            }
            vec![MultiplayerRoom::<G>::left_room_msg()]
        }
        ClientMsg::PlayMultiplayerAction { action } => {
            let Some(active) = active_room.as_ref() else {
                return vec![ServerMsg::Error {
                    message: "Join a multiplayer room before playing".into(),
                }];
            };
            let Some(player) = active.viewer.player() else {
                return vec![ServerMsg::Error {
                    message: "Spectators cannot play multiplayer actions".into(),
                }];
            };
            let room = Arc::clone(&active.room);
            let socket_id = active.socket_id;
            if !room.is_full() {
                return vec![ServerMsg::Error {
                    message: "Room is waiting for an opponent".into(),
                }];
            }
            if room.is_finished() {
                let _ = save_finished_multiplayer_replay(Arc::clone(&room)).await;
                room.broadcast_room_info_except(Some(socket_id));
                return vec![room.room_msg_for_active(active)];
            }
            let (responses, analysis_generation, analysis_session) = {
                let mut session = room.session.lock().await;
                if let Err(message) = session.play_human_action(player, action) {
                    return vec![ServerMsg::Error { message }];
                }
                let game_over = session.current_game_ended();
                if game_over {
                    room.mark_finished(winner_from_reward(session.current_result_reward()), "game");
                }
                let next_player = session.current_player_idx();
                room.finish_clock_turn(player, next_player, game_over);
                room.touch();
                let save_errors = room.save_completed_replay_once(&session).await;
                room.broadcast_state_except(&session, Some(socket_id));
                room.broadcast_room_info_except(Some(socket_id));
                let mut responses = vec![
                    room.room_msg_for_active(active),
                    session.state_msg_for_player(player),
                ];
                responses.extend(save_errors);
                (
                    responses,
                    room.next_analysis_generation(),
                    session.fork_analysis_session(),
                )
            };
            schedule_lobby_broadcast(Arc::clone(rooms));
            schedule_multiplayer_clock_timeout(Arc::clone(rooms), Arc::clone(&room));
            schedule_multiplayer_analysis(room, analysis_generation, analysis_session);
            if let Some(active) = active_room.as_ref() {
                maybe_schedule_managed_bot_move(Arc::clone(rooms), Arc::clone(&active.room));
            }
            responses
        }
        ClientMsg::SendMultiplayerChat { text } => {
            let Some(active) = active_room.as_ref() else {
                return vec![ServerMsg::Error {
                    message: "Join a multiplayer room before chatting".into(),
                }];
            };
            let Some(player) = active.viewer.player() else {
                return vec![ServerMsg::Error {
                    message: "Spectators cannot send multiplayer chat".into(),
                }];
            };
            let room = Arc::clone(&active.room);
            let msg = match room.push_chat_message(player, &text) {
                Ok(msg) => msg,
                Err(message) => return vec![ServerMsg::Error { message }],
            };
            room.broadcast_chat_except(Some(active.socket_id));
            vec![msg]
        }
        history_msg @ (ClientMsg::Undo | ClientMsg::Redo) => {
            let Some(active) = active_room.as_ref() else {
                return vec![ServerMsg::Error {
                    message: "Join a multiplayer room before using history controls".into(),
                }];
            };
            let Some(player) = active.viewer.player() else {
                return vec![ServerMsg::Error {
                    message: "Spectators cannot use multiplayer history controls".into(),
                }];
            };
            let room = Arc::clone(&active.room);
            let socket_id = active.socket_id;
            if !room.is_full() {
                return vec![ServerMsg::Error {
                    message: "Room is waiting for an opponent".into(),
                }];
            }
            if room.is_finished() {
                let _ = save_finished_multiplayer_replay(Arc::clone(&room)).await;
                room.broadcast_room_info_except(Some(socket_id));
                return vec![room.room_msg_for_active(active)];
            }
            let (responses, analysis_generation, analysis_session) = {
                let mut session = room.session.lock().await;
                let result = match history_msg {
                    ClientMsg::Undo => session.undo_multiplayer_action(player),
                    ClientMsg::Redo => session.redo_multiplayer_action(player),
                    _ => unreachable!(),
                };
                if let Err(message) = result {
                    return vec![ServerMsg::Error { message }];
                }
                room.touch();
                room.broadcast_state_except(&session, Some(socket_id));
                (
                    vec![
                        room.room_msg_for_active(active),
                        session.state_msg_for_player(player),
                    ],
                    room.next_analysis_generation(),
                    session.fork_analysis_session(),
                )
            };
            schedule_multiplayer_analysis(room, analysis_generation, analysis_session);
            if let Some(active) = active_room.as_ref() {
                maybe_schedule_managed_bot_move(Arc::clone(rooms), Arc::clone(&active.room));
            }
            responses
        }
        ClientMsg::ResignMultiplayerGame => {
            let Some(active) = active_room.as_ref() else {
                return vec![ServerMsg::Error {
                    message: "Join a multiplayer room before resigning".into(),
                }];
            };
            let Some(player) = active.viewer.player() else {
                return vec![ServerMsg::Error {
                    message: "Spectators cannot resign multiplayer games".into(),
                }];
            };
            let room = Arc::clone(&active.room);
            if !room.is_full() {
                return vec![ServerMsg::Error {
                    message: "Room is waiting for an opponent".into(),
                }];
            }
            if !room.is_finished() {
                room.mark_finished(Some(1 - player), "resignation");
            }
            let save_errors = save_finished_multiplayer_replay(Arc::clone(&room)).await;
            room.broadcast_room_info_except(Some(active.socket_id));
            schedule_lobby_broadcast(Arc::clone(rooms));
            let mut responses = vec![room.room_msg_for_active(active)];
            responses.extend(save_errors);
            responses
        }
        ClientMsg::AddMultiplayerOpponentTime => {
            let Some(active) = active_room.as_ref() else {
                return vec![ServerMsg::Error {
                    message: "Join a multiplayer room before adjusting clocks".into(),
                }];
            };
            let Some(player) = active.viewer.player() else {
                return vec![ServerMsg::Error {
                    message: "Spectators cannot adjust multiplayer clocks".into(),
                }];
            };
            let room = Arc::clone(&active.room);
            let opponent = if player == 0 { 1 } else { 0 };
            if !room.add_clock_seconds(opponent, 15) {
                if room.is_finished() {
                    let _ = save_finished_multiplayer_replay(Arc::clone(&room)).await;
                    room.broadcast_room_info_except(Some(active.socket_id));
                    schedule_lobby_broadcast(Arc::clone(rooms));
                    return vec![room.room_msg_for_active(active)];
                }
                return vec![ServerMsg::Error {
                    message: "This room does not have clocks enabled".into(),
                }];
            }
            room.broadcast_room_info_except(Some(active.socket_id));
            schedule_multiplayer_clock_timeout(Arc::clone(rooms), Arc::clone(&room));
            vec![room.room_msg_for_active(active)]
        }
        _ => vec![ServerMsg::Error {
            message: "Unsupported multiplayer message".into(),
        }],
    }
}

async fn activate_multiplayer_room<G: Game + 'static>(
    rooms: Arc<MultiplayerRoomStore<G>>,
    room: Arc<MultiplayerRoom<G>>,
    user_id: &str,
    display_name: Option<&str>,
    viewer: MultiplayerViewer,
    outbound_tx: &mpsc::UnboundedSender<String>,
    active_room: &mut Option<ActiveMultiplayerRoom<G>>,
) -> Vec<ServerMsg> {
    let socket_id = match room.register_socket(user_id, display_name, viewer, outbound_tx.clone()) {
        Ok(active) => active,
        Err(message) => return vec![ServerMsg::Error { message }],
    };
    *active_room = Some(ActiveMultiplayerRoom {
        room: Arc::clone(&room),
        socket_id,
        viewer,
    });
    room.broadcast_room_info_except(Some(socket_id));
    let (responses, analysis_job) = {
        let session = room.session.lock().await;
        let state_msg = match viewer {
            MultiplayerViewer::Player(player) => session.state_msg_for_player(player),
            MultiplayerViewer::Spectator => session.state_msg_for_spectator(),
        };
        let mut responses = vec![room.room_msg_for_viewer(viewer, Some(socket_id)), state_msg];
        if viewer.player().is_some() {
            if let Some(chat) = room.chat_msg() {
                responses.push(chat);
            }
        }
        let analysis_job = room.is_full().then(|| {
            (
                room.next_analysis_generation(),
                session.fork_analysis_session(),
            )
        });
        (responses, analysis_job)
    };
    if let Some((analysis_generation, analysis_session)) = analysis_job {
        schedule_multiplayer_analysis(Arc::clone(&room), analysis_generation, analysis_session);
    }
    schedule_multiplayer_clock_timeout(rooms, Arc::clone(&room));
    schedule_room_info_broadcast(Arc::clone(&room), 300);
    schedule_room_info_broadcast(Arc::clone(&room), 1500);
    responses
}

fn detach_active_room<G: Game + 'static>(
    rooms: &Arc<MultiplayerRoomStore<G>>,
    active_room: &mut Option<ActiveMultiplayerRoom<G>>,
) -> bool {
    detach_active_room_and_maybe_vacate(rooms, active_room, false)
}

fn detach_active_room_and_maybe_vacate<G: Game + 'static>(
    rooms: &Arc<MultiplayerRoomStore<G>>,
    active_room: &mut Option<ActiveMultiplayerRoom<G>>,
    vacate_managed_human_seat: bool,
) -> bool {
    if let Some(active) = active_room.take() {
        let vacated_managed_seat = active.viewer.player().is_some_and(|player| {
            vacate_managed_human_seat && active.room.vacate_human_player(player)
        });
        let removed_viewer = active.room.unregister_socket(active.socket_id);
        let closed_room = removed_viewer.is_some_and(|viewer| viewer.player().is_some())
            && rooms.close_room_if_no_connected_players(&active.room);
        active
            .room
            .broadcast_room_info_except(Some(active.socket_id));
        schedule_room_info_broadcast(Arc::clone(&active.room), 300);
        if closed_room {
            rooms.broadcast_lobby();
        }
        if vacated_managed_seat {
            rooms.ensure_managed_bot_lobbies();
        }
        true
    } else {
        false
    }
}

fn schedule_lobby_broadcast<G: Game + 'static>(rooms: Arc<MultiplayerRoomStore<G>>) {
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        rooms.broadcast_lobby();
    });
}

fn schedule_room_info_broadcast<G: Game + 'static>(room: Arc<MultiplayerRoom<G>>, delay_ms: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        room.broadcast_room_info_except(None);
    });
}

fn schedule_multiplayer_clock_timeout<G: Game + 'static>(
    rooms: Arc<MultiplayerRoomStore<G>>,
    room: Arc<MultiplayerRoom<G>>,
) {
    let Some((generation, remaining_ms)) = room.clock_timeout_schedule() else {
        return;
    };
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(remaining_ms)).await;
        if room.mark_clock_timeout_if_current(generation) {
            let _ = save_finished_multiplayer_replay(Arc::clone(&room)).await;
            room.broadcast_room_info_except(None);
            schedule_lobby_broadcast(rooms);
        }
    });
}

fn maybe_schedule_managed_bot_move<G: Game + 'static>(
    rooms: Arc<MultiplayerRoomStore<G>>,
    room: Arc<MultiplayerRoom<G>>,
) {
    let Some(bot_player) = room.managed_bot_player() else {
        return;
    };
    if !room.is_full() || room.is_finished() {
        return;
    }
    if room
        .bot_move_running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    tokio::spawn(async move {
        let mut schedule_again = false;
        'run: {
            if !room.is_full() || room.is_finished() {
                break 'run;
            }

            let (analysis_generation, analysis_session, game_over) = {
                let mut session = room.session.lock().await;
                if session.current_player_idx() != bot_player {
                    break 'run;
                }

                let bot_msg = ClientMsg::BotMove {
                    simulations: None,
                    budget: Some(managed_bot_search_budget()),
                };
                if let Err(messages) = session.begin_search(&bot_msg) {
                    tracing::warn!(
                        code = %room.code,
                        messages = ?messages,
                        "managed bot move could not start"
                    );
                    break 'run;
                }

                let mut ticks_since_yield = 0;
                let result = loop {
                    if let Some(result) = session.search_tick() {
                        break result;
                    }
                    ticks_since_yield += 1;
                    if ticks_since_yield >= SEARCH_INTERRUPT_INTERVAL {
                        ticks_since_yield = 0;
                        tokio::task::yield_now().await;
                    }
                };
                let messages = session.finish_search(&bot_msg, result);
                if messages
                    .iter()
                    .any(|msg| matches!(msg, ServerMsg::Error { .. }))
                {
                    tracing::warn!(
                        code = %room.code,
                        messages = ?messages,
                        "managed bot move failed"
                    );
                    break 'run;
                }

                let game_over = session.current_game_ended();
                if game_over {
                    room.mark_finished(winner_from_reward(session.current_result_reward()), "game");
                }
                let next_player = session.current_player_idx();
                room.finish_clock_turn(bot_player, next_player, game_over);
                room.touch();
                let save_errors = room.save_completed_replay_once(&session).await;
                if !save_errors.is_empty() {
                    tracing::warn!(
                        code = %room.code,
                        errors = ?save_errors,
                        "managed bot replay save failed"
                    );
                }
                room.broadcast_state_except(&session, None);
                room.broadcast_room_info_except(None);
                schedule_again = !game_over && next_player == bot_player && room.is_full();
                (
                    room.next_analysis_generation(),
                    (!game_over).then(|| session.fork_analysis_session()),
                    game_over,
                )
            };

            schedule_lobby_broadcast(Arc::clone(&rooms));
            schedule_multiplayer_clock_timeout(Arc::clone(&rooms), Arc::clone(&room));
            if let Some(analysis_session) = analysis_session {
                schedule_multiplayer_analysis(
                    Arc::clone(&room),
                    analysis_generation,
                    analysis_session,
                );
            }
            if game_over {
                let _ = save_finished_multiplayer_replay(Arc::clone(&room)).await;
            }
        }

        room.bot_move_running.store(false, Ordering::Release);
        if schedule_again {
            maybe_schedule_managed_bot_move(rooms, room);
        }
    });
}

async fn save_finished_multiplayer_replay<G: Game + 'static>(
    room: Arc<MultiplayerRoom<G>>,
) -> Vec<ServerMsg> {
    let session = room.session.lock().await;
    room.save_finished_room_replay_once(&session).await
}

fn schedule_multiplayer_analysis<G: Game + 'static>(
    room: Arc<MultiplayerRoom<G>>,
    generation: u64,
    mut session: GameSession<G>,
) {
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        let Ok(root_wdl) =
            tokio::task::spawn_blocking(move || multiplayer_analysis_root_wdl(&mut session)).await
        else {
            return;
        };
        if room.is_current_analysis_generation(generation) {
            room.broadcast_analysis_except(root_wdl, None);
        }
    });
}

fn multiplayer_analysis_root_wdl<G: Game + 'static>(session: &mut GameSession<G>) -> [f32; 3] {
    session
        .run_analysis_bar_search(SearchBudget::simulations(MULTIPLAYER_ANALYSIS_SIMS))
        .unwrap_or_else(|_| session.root_wdl())
}

fn client_msg_target(msg: &ClientMsg) -> ViewTarget {
    match msg {
        ClientMsg::LoadReplay { .. }
        | ClientMsg::LoadSharedReplay { .. }
        | ClientMsg::SetReplayCursor { .. } => ViewTarget::Replay,
        ClientMsg::RunSims { target, .. }
        | ClientMsg::RunSearch { target, .. }
        | ClientMsg::PauseSearch { target, .. }
        | ClientMsg::ExploreSubtree { target, .. } => target.unwrap_or(ViewTarget::Analysis),
        _ => ViewTarget::Analysis,
    }
}

async fn handle_replay_message<G: Game + 'static>(
    socket: &mut WebSocket,
    user_session: &Arc<UserSession<G>>,
    client_msg: ClientMsg,
) -> Result<Vec<ServerMsg>, ()> {
    match client_msg {
        ClientMsg::LoadReplay { id } => match &user_session.replay_store {
            Some(store) => match store.load(&user_session.account_key, &id).await {
                Ok(loaded) => {
                    let mut replay_session = user_session.factory.create_session();
                    match replay_session.load_saved_replay_log_with_player_names(
                        loaded.id.clone(),
                        &loaded.log,
                        loaded.player_names,
                    ) {
                        Ok(()) => {
                            let state_msg = replay_session.state_msg();
                            *user_session.replay_session.lock().await = Some(replay_session);
                            Ok(vec![state_msg])
                        }
                        Err(message) => Ok(vec![ServerMsg::Error { message }]),
                    }
                }
                Err(message) => Ok(vec![ServerMsg::Error { message }]),
            },
            None => Ok(vec![ServerMsg::Error {
                message: "Replay storage is not configured".into(),
            }]),
        },
        ClientMsg::LoadSharedReplay { slug } => match &user_session.replay_store {
            Some(store) => match store.load_shared(&slug).await {
                Ok(loaded) => {
                    tracing::info!(
                        share_slug = %slug,
                        replay_id = %loaded.id,
                        "loaded shared replay"
                    );
                    let mut replay_session = user_session.factory.create_session();
                    match replay_session.load_saved_replay_log_with_player_names(
                        loaded.id.clone(),
                        &loaded.log,
                        loaded.player_names,
                    ) {
                        Ok(()) => {
                            let state_msg = replay_session.state_msg();
                            *user_session.replay_session.lock().await = Some(replay_session);
                            Ok(vec![state_msg])
                        }
                        Err(message) => Ok(vec![ServerMsg::Error { message }]),
                    }
                }
                Err(message) => {
                    tracing::warn!(share_slug = %slug, error = %message, "failed to load shared replay");
                    Ok(vec![ServerMsg::Error { message }])
                }
            },
            None => {
                tracing::warn!(share_slug = %slug, "shared replay requested without replay storage");
                Ok(vec![ServerMsg::Error {
                    message: "Replay storage is not configured".into(),
                }])
            }
        },
        msg @ (ClientMsg::RunSims { .. } | ClientMsg::RunSearch { .. }) => {
            let mut replay_session = user_session.replay_session.lock().await;
            match replay_session.as_mut() {
                Some(session) => run_search(socket, session, &msg).await,
                None => Ok(vec![ServerMsg::Error {
                    message: "No replay is loaded".into(),
                }]),
            }
        }
        msg @ (ClientMsg::SetReplayCursor { .. }
        | ClientMsg::PauseSearch { .. }
        | ClientMsg::ExploreSubtree { .. }) => {
            let mut replay_session = user_session.replay_session.lock().await;
            match replay_session.as_mut() {
                Some(session) => Ok(session.handle(msg)),
                None => Ok(vec![ServerMsg::Error {
                    message: "No replay is loaded".into(),
                }]),
            }
        }
        msg => Ok(vec![ServerMsg::Error {
            message: format!("Message is not supported in replay view: {msg:?}"),
        }]),
    }
}

async fn send_unauthorized(socket: &mut WebSocket) {
    let _ = send_msg(
        socket,
        &ServerMsg::Error {
            message: "Unauthorized".into(),
        },
    )
    .await;
}

async fn send_raw_msg(socket: &mut WebSocket, json: String) -> Result<(), ()> {
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|_| ())
}

pub async fn send_msg(socket: &mut WebSocket, msg: &ServerMsg) -> Result<(), ()> {
    let json = serde_json::to_string(msg).map_err(|_| ())?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use crate::{
        eval::{Evaluation, Evaluator},
        game::{Game, Status},
        game_log::GameLog,
    };
    use tokio::{sync::mpsc, time::timeout};

    use super::{
        ActiveMultiplayerRoom, ClientMsg, FileReplayStore, GamePresenter,
        MANAGED_BOT_LOBBY_REFRESH_MS, MULTIPLAYER_CHAT_HISTORY_LIMIT, ManagedBotLobbySlot,
        MultiplayerLobbyRoom, MultiplayerRoom, MultiplayerRoomStore, MultiplayerViewer,
        ReplayEntry, ReplayEntryWithAccount, ReplayParticipant, ReplayStore, RoomClock,
        RoomOutcome, SearchBudget, SeatOwner, ServerMsg, SessionFactory, UserSessionStore,
        ViewTarget, anonymous_user_id_from_id, anonymous_user_id_from_token, client_msg_target,
        current_unix_ms, detach_active_room, handle_multiplayer_message, handle_profile_message,
        list_replay_entries, maybe_schedule_managed_bot_move, parse_env_bool, redacted_account_key,
        render_all_page, replay_result_for_room_outcome, safe_replay_id, safe_share_slug,
        save_current_replay_once, should_send_search_progress, socket_session_key_from_auth,
    };

    #[derive(Clone)]
    struct TestGame {
        board_id: u64,
        moves: u8,
    }

    impl Game for TestGame {
        const NUM_ACTIONS: usize = 1;

        fn status(&self) -> Status {
            if self.moves == u8::MAX {
                Status::Terminal(-1.0)
            } else {
                Status::Decision(1.0)
            }
        }

        fn legal_actions(&self, buf: &mut Vec<usize>) {
            buf.push(0);
        }

        fn apply_action(&mut self, _action: usize) {
            self.moves += 1;
        }
    }

    struct TestEvaluator;

    impl Evaluator<TestGame> for TestEvaluator {
        fn evaluate(&self, _state: &TestGame, _rng: &mut fastrand::Rng) -> Evaluation {
            Evaluation::uniform(TestGame::NUM_ACTIONS, 0.0)
        }
    }

    #[derive(Clone)]
    struct AlternatingGame {
        moves: u8,
    }

    impl Game for AlternatingGame {
        const NUM_ACTIONS: usize = 1;

        fn status(&self) -> Status {
            if self.moves >= 2 {
                Status::Terminal(1.0)
            } else if self.moves % 2 == 0 {
                Status::Decision(1.0)
            } else {
                Status::Decision(-1.0)
            }
        }

        fn legal_actions(&self, buf: &mut Vec<usize>) {
            buf.push(0);
        }

        fn apply_action(&mut self, _action: usize) {
            self.moves = self.moves.saturating_add(1);
        }
    }

    struct AlternatingEvaluator;

    impl Evaluator<AlternatingGame> for AlternatingEvaluator {
        fn evaluate(&self, _state: &AlternatingGame, _rng: &mut fastrand::Rng) -> Evaluation {
            Evaluation::uniform(AlternatingGame::NUM_ACTIONS, 0.0)
        }
    }

    struct AlternatingPresenter;

    impl GamePresenter<AlternatingGame> for AlternatingPresenter {
        fn serialize_state(&self, state: &AlternatingGame) -> serde_json::Value {
            serde_json::json!({ "moves": state.moves })
        }

        fn serialize_state_for_player(
            &self,
            state: &AlternatingGame,
            player: usize,
        ) -> serde_json::Value {
            serde_json::json!({ "moves": state.moves, "viewer": player })
        }

        fn serialize_state_for_spectator(&self, state: &AlternatingGame) -> serde_json::Value {
            serde_json::json!({ "moves": state.moves, "viewer": "spectator" })
        }

        fn action_label(&self, _state: &AlternatingGame, action: usize) -> String {
            format!("Action {action}")
        }

        fn phase_label(&self, _state: &AlternatingGame) -> String {
            "alternating".into()
        }

        fn serialize_log_state(&self, state: &AlternatingGame) -> Option<String> {
            Some(state.moves.to_string())
        }

        fn deserialize_log_state(&self, text: &str) -> Result<AlternatingGame, String> {
            Ok(AlternatingGame {
                moves: text.parse().map_err(|e| format!("bad moves: {e}"))?,
            })
        }

        fn static_dir(&self) -> &Path {
            Path::new(".")
        }

        fn new_game(&self, _seed: u64) -> AlternatingGame {
            AlternatingGame { moves: 0 }
        }
    }

    #[test]
    fn parse_env_bool_accepts_enabled_values() {
        for value in ["1", "true", "TRUE", "t", "yes", "Y", "on", " on "] {
            assert!(parse_env_bool(value), "{value:?} should be enabled");
        }
    }

    #[test]
    fn parse_env_bool_rejects_other_values() {
        for value in ["", "0", "false", "no", "off", "required", "enabled"] {
            assert!(!parse_env_bool(value), "{value:?} should be disabled");
        }
    }

    #[test]
    fn search_progress_predicate_uses_sim_delta_or_keepalive_elapsed() {
        assert!(!should_send_search_progress(
            99,
            0,
            Duration::from_millis(4_999)
        ));
        assert!(should_send_search_progress(
            100,
            0,
            Duration::from_millis(1)
        ));
        assert!(!should_send_search_progress(
            64,
            64,
            Duration::from_millis(4_999)
        ));
        assert!(should_send_search_progress(
            64,
            64,
            Duration::from_millis(5_000)
        ));
        assert!(should_send_search_progress(
            164,
            64,
            Duration::from_millis(1)
        ));
    }

    #[test]
    fn socket_auth_falls_back_to_browser_session_for_unrecognized_token() {
        let key = socket_session_key_from_auth(
            "clerk-session-token",
            None,
            Some("stable_browser_session".into()),
            Some("Alice".into()),
        );

        assert_eq!(key.user_id, "anonymous:stable_browser_session");
        assert_eq!(key.display_name.as_deref(), Some("Alice"));
        assert!(!key.ephemeral);
        assert_eq!(key.scope, "anonymous");
    }

    #[test]
    fn socket_auth_prefers_recognized_token_over_browser_session() {
        let key = socket_session_key_from_auth(
            "anon:token_session",
            None,
            Some("browser_session".into()),
            None,
        );

        assert_eq!(key.user_id, "anonymous:token_session");
        assert!(!key.ephemeral);
        assert_eq!(key.scope, "anonymous");
    }

    #[test]
    fn socket_auth_uses_clerk_user_id_when_token_is_present() {
        let key = socket_session_key_from_auth(
            "clerk-session-token",
            Some("user_2abc123".into()),
            Some("browser_session".into()),
            Some("Alice".into()),
        );

        assert_eq!(key.user_id, "clerk:user_2abc123");
        assert_eq!(key.display_name.as_deref(), Some("Alice"));
        assert!(!key.ephemeral);
        assert_eq!(key.scope, "clerk");
    }

    #[test]
    fn socket_auth_ignores_clerk_user_id_without_token() {
        let key = socket_session_key_from_auth(
            "",
            Some("user_2abc123".into()),
            Some("browser_session".into()),
            None,
        );

        assert_eq!(key.user_id, "anonymous:browser_session");
        assert!(!key.ephemeral);
        assert_eq!(key.scope, "anonymous");
    }

    struct CountingPresenter {
        created_games: AtomicUsize,
    }

    impl GamePresenter<TestGame> for CountingPresenter {
        fn serialize_state(&self, state: &TestGame) -> serde_json::Value {
            serde_json::json!({
                "board_id": state.board_id,
                "moves": state.moves,
            })
        }

        fn serialize_state_for_player(&self, state: &TestGame, player: usize) -> serde_json::Value {
            serde_json::json!({
                "board_id": state.board_id,
                "moves": state.moves,
                "viewer": player,
            })
        }

        fn serialize_state_for_spectator(&self, state: &TestGame) -> serde_json::Value {
            serde_json::json!({
                "board_id": state.board_id,
                "moves": state.moves,
                "viewer": "spectator",
            })
        }

        fn action_label(&self, _state: &TestGame, action: usize) -> String {
            format!("Action {action}")
        }

        fn phase_label(&self, _state: &TestGame) -> String {
            "test".into()
        }

        fn serialize_log_state(&self, state: &TestGame) -> Option<String> {
            Some(state.board_id.to_string())
        }

        fn deserialize_log_state(&self, text: &str) -> Result<TestGame, String> {
            Ok(TestGame {
                board_id: text.parse().map_err(|e| format!("bad board id: {e}"))?,
                moves: 0,
            })
        }

        fn static_dir(&self) -> &Path {
            Path::new(".")
        }

        fn board_fingerprint(&self, state: &TestGame) -> Option<u64> {
            Some(state.board_id)
        }

        fn new_game(&self, _seed: u64) -> TestGame {
            let board_id = self.created_games.fetch_add(1, Ordering::SeqCst) as u64 + 1;
            TestGame { board_id, moves: 0 }
        }

        fn resign(&self, state: &mut TestGame, _player: usize) -> Result<(), String> {
            state.moves = u8::MAX;
            Ok(())
        }
    }

    fn test_store() -> (UserSessionStore<TestGame>, Arc<CountingPresenter>) {
        let presenter = Arc::new(CountingPresenter {
            created_games: AtomicUsize::new(0),
        });
        let evaluator: Arc<dyn Evaluator<TestGame> + Sync> = Arc::new(TestEvaluator);
        let factory =
            SessionFactory::new_game(evaluator, "test", presenter.clone(), [true, true], None);

        (UserSessionStore::new(factory, None), presenter)
    }

    fn test_room_store() -> (MultiplayerRoomStore<TestGame>, Arc<CountingPresenter>) {
        let (store, presenter) = test_store();
        (
            MultiplayerRoomStore::new(Arc::clone(&store.factory), None),
            presenter,
        )
    }

    fn test_managed_room_store() -> (MultiplayerRoomStore<TestGame>, Arc<CountingPresenter>) {
        let (store, presenter) = test_store();
        (
            MultiplayerRoomStore::new_with_managed_bot_lobbies(
                Arc::clone(&store.factory),
                None,
                true,
            ),
            presenter,
        )
    }

    fn temp_replay_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hexfish-{name}-{}-{}",
            current_unix_ms(),
            fastrand::u64(..)
        ))
    }

    fn sample_all_page_replay_row() -> ReplayEntryWithAccount {
        ReplayEntryWithAccount {
            account_key: "account:test".into(),
            entry: ReplayEntry {
                id: "replay-1".into(),
                saved_at_ms: 1_700_000_000_000,
                action_count: 42,
                result: "win".into(),
                favorite: false,
                share_slug: "share-1".into(),
            },
        }
    }

    fn sample_all_page_lobby(code: &str) -> MultiplayerLobbyRoom {
        MultiplayerLobbyRoom {
            code: code.into(),
            status: "waiting".into(),
            occupied: 1,
            connected: 1,
            spectator_count: 0,
            is_public: true,
            time_minutes: Some(15),
            increment_seconds: Some(1),
            last_activity_ms: 1_700_000_001_000,
            empty_room_closes_at_ms: None,
        }
    }

    async fn recv_multiplayer_analysis(
        rx: &mut mpsc::UnboundedReceiver<String>,
    ) -> serde_json::Value {
        for _ in 0..4 {
            let raw = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("multiplayer analysis timeout")
                .expect("multiplayer analysis broadcast");
            let msg: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if msg["type"] == "MultiplayerAnalysis" {
                return msg;
            }
        }
        panic!("expected multiplayer analysis broadcast");
    }

    async fn recv_multiplayer_chat(rx: &mut mpsc::UnboundedReceiver<String>) -> serde_json::Value {
        for _ in 0..4 {
            let raw = timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("multiplayer chat timeout")
                .expect("multiplayer chat broadcast");
            let msg: serde_json::Value = serde_json::from_str(&raw).unwrap();
            if msg["type"] == "MultiplayerChat" {
                return msg;
            }
        }
        panic!("expected multiplayer chat broadcast");
    }

    #[test]
    fn all_page_renderer_labels_replay_logs_and_includes_toggle() {
        let replay_rows = [sample_all_page_replay_row()];
        let lobbies = [sample_all_page_lobby("LOBBY1")];
        let html = render_all_page(&replay_rows, &lobbies, None);

        assert!(html.contains("<title>Replay Logs</title>"));
        assert!(html.contains("<h1>Replay Logs</h1>"));
        assert!(!html.contains("All Replay Logs"));
        assert!(html.contains("id=\"all-view-toggle\""));
        assert!(html.contains(">Show Lobbies</button>"));
        assert!(html.contains("Show Replay Logs"));
        assert!(html.contains("id=\"replay-logs-section\""));
        assert!(html.contains("id=\"lobbies-section\" class=\"panel\" hidden"));
    }

    #[test]
    fn all_page_renderer_shows_lobby_count_and_details_table() {
        let lobbies = [
            sample_all_page_lobby("LOBBY1"),
            MultiplayerLobbyRoom {
                code: "LOBBY2".into(),
                status: "active".into(),
                occupied: 2,
                connected: 2,
                spectator_count: 3,
                is_public: true,
                time_minutes: None,
                increment_seconds: None,
                last_activity_ms: 1_700_000_002_000,
                empty_room_closes_at_ms: None,
            },
        ];
        let html = render_all_page(&[], &lobbies, None);

        assert!(html.contains("<td>Open lobbies</td><td class=\"number\">2</td>"));
        assert!(html.contains("<th>Code</th><th>Status</th><th>Occupied</th><th>Connected</th><th>Spectators</th><th>Time Control</th><th>Last Activity</th><th>Open</th>"));
        assert!(html.contains("<td class=\"mono\">LOBBY1</td>"));
        assert!(html.contains("<td>15+1</td>"));
        assert!(html.contains("<td><a href=\"/?room=LOBBY2\">Open</a></td>"));
    }

    #[test]
    fn all_page_renderer_empty_states_are_clear() {
        let html = render_all_page(&[], &[], None);

        assert!(html.contains("No replay logs found."));
        assert!(html.contains("<td>Open lobbies</td><td class=\"number\">0</td>"));
        assert!(html.contains("No open lobbies found."));
    }

    #[test]
    fn all_page_lobby_snapshot_matches_public_lobby_list() {
        let (rooms, _) = test_room_store();
        rooms
            .create_room(
                "user_a",
                Some(0),
                Some("PUB123".into()),
                Some(true),
                None,
                None,
            )
            .unwrap();
        rooms
            .create_room(
                "user_b",
                Some(0),
                Some("PRIV12".into()),
                Some(false),
                None,
                None,
            )
            .unwrap();

        let snapshot = rooms.lobby_rooms();
        match rooms.lobby_msg() {
            ServerMsg::MultiplayerLobby { rooms: lobby } => {
                assert_eq!(snapshot.len(), 1);
                assert_eq!(lobby.len(), snapshot.len());
                assert_eq!(snapshot[0].code, "PUB123");
                assert_eq!(lobby[0].code, snapshot[0].code);
            }
            other => panic!("expected lobby message, got {other:?}"),
        }
    }

    #[test]
    fn anonymous_tokens_are_stable_when_safe() {
        assert_eq!(
            anonymous_user_id_from_token("anon:local-session_123"),
            Some("anonymous:local-session_123".to_string())
        );
        assert_eq!(
            anonymous_user_id_from_id("local-session_123"),
            Some("anonymous:local-session_123".to_string())
        );
        assert_eq!(anonymous_user_id_from_token(""), None);
        assert_eq!(anonymous_user_id_from_token("anon:"), None);
        assert_eq!(anonymous_user_id_from_token("anon:not safe"), None);
        assert_eq!(anonymous_user_id_from_id("not safe"), None);
    }

    #[test]
    fn same_user_reuses_same_session() {
        let (store, presenter) = test_store();

        let (first, first_created) = store.get_or_create("user_a");
        let (second, second_created) = store.get_or_create("user_a");

        assert!(first_created);
        assert!(!second_created);
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.session_id, second.session_id);
        assert_eq!(first.board_fingerprint, second.board_fingerprint);
        assert_eq!(presenter.created_games.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn different_users_get_distinct_sessions() {
        let (store, presenter) = test_store();

        let (first, first_created) = store.get_or_create("user_a");
        let (second, second_created) = store.get_or_create("user_b");

        assert!(first_created);
        assert!(second_created);
        assert!(!Arc::ptr_eq(&first, &second));
        assert_ne!(first.session_id, second.session_id);
        assert_ne!(first.board_fingerprint, second.board_fingerprint);
        assert_eq!(presenter.created_games.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn clerk_user_without_username_has_unset_profile() {
        let (store, _) = test_store();

        match store.profile_msg("clerk:user_a") {
            ServerMsg::Profile {
                username,
                username_set,
            } => {
                assert_eq!(username, None);
                assert!(!username_set);
            }
            other => panic!("unexpected profile message: {other:?}"),
        }
    }

    #[test]
    fn clerk_username_is_cached_as_profile_display_name() {
        let (store, _) = test_store();

        store.set_display_name("clerk:user_a", "Alice");
        match store.profile_msg("clerk:user_a") {
            ServerMsg::Profile {
                username,
                username_set,
            } => {
                assert_eq!(username.as_deref(), Some("Alice"));
                assert!(username_set);
            }
            other => panic!("unexpected profile message: {other:?}"),
        }
    }

    #[test]
    fn guest_users_receive_unique_default_usernames() {
        let (store, _) = test_store();

        let first = store.ensure_guest_username("anonymous:a", None);
        let second = store.ensure_guest_username("anonymous:b", None);
        let first_again = store.ensure_guest_username("anonymous:a", Some("Alice"));

        assert!(first.starts_with("User"));
        assert!(second.starts_with("User"));
        assert_ne!(first, second);
        assert_eq!(first_again, first);
    }

    #[test]
    fn set_username_is_rejected_because_clerk_manages_usernames() {
        let (store, _) = test_store();
        let store = Arc::new(store);

        let responses = handle_profile_message(
            &store,
            "clerk:user_a",
            ClientMsg::SetUsername {
                username: "Alice".into(),
            },
        );

        assert!(matches!(
            responses.as_slice(),
            [ServerMsg::Error { message }] if message == "Username is managed by Clerk"
        ));
    }

    #[test]
    fn multiplayer_room_invite_flow_assigns_fixed_seats() {
        let (rooms, presenter) = test_room_store();

        let (room, creator_player) = rooms
            .create_room("user_a", Some(1), None, None, None, None)
            .unwrap();
        assert_eq!(creator_player, 1);
        assert_eq!(presenter.created_games.load(Ordering::SeqCst), 1);

        let (same_room, same_player) = rooms.join_room("user_a", &room.code).unwrap();
        assert!(Arc::ptr_eq(&room, &same_room));
        assert_eq!(same_player, 1);

        let (joined_room, joined_player) = rooms.join_room("user_b", &room.code).unwrap();
        assert!(Arc::ptr_eq(&room, &joined_room));
        assert_eq!(joined_player, 0);

        let err = match rooms.join_room("user_c", &room.code) {
            Ok(_) => panic!("third user should not join a full room"),
            Err(err) => err,
        };
        assert!(err.contains("full"));
    }

    #[test]
    fn managed_bot_lobbies_expose_three_guest_like_public_rooms() {
        let (rooms, _) = test_managed_room_store();

        match rooms.lobby_msg() {
            ServerMsg::MultiplayerLobby { rooms: lobby } => {
                assert_eq!(lobby.len(), 3);
                let mut controls = lobby
                    .iter()
                    .map(|room| (room.time_minutes, room.increment_seconds))
                    .collect::<Vec<_>>();
                controls.sort();
                assert_eq!(
                    controls,
                    vec![(Some(5), Some(3)), (Some(15), Some(1)), (Some(30), Some(0))]
                );
                for room in &lobby {
                    assert_eq!(room.status, "waiting");
                    assert_eq!(room.occupied, 1);
                    assert_eq!(room.connected, 1);
                    assert_eq!(room.spectator_count, 0);
                    assert!(room.is_public);
                    assert!(room.empty_room_closes_at_ms.is_none());
                }
            }
            other => panic!("expected lobby message, got {other:?}"),
        }

        let stored = rooms.rooms.lock().expect("room store lock poisoned");
        assert_eq!(stored.len(), 3);
        for room in stored.values() {
            let bot_player = room.managed_bot_player().expect("managed bot player");
            match room.room_msg_for_player(None) {
                ServerMsg::MultiplayerRoom { players, .. } => {
                    assert!(players[bot_player].occupied);
                    assert!(players[bot_player].connected);
                    assert!(
                        players[bot_player]
                            .name
                            .as_deref()
                            .is_some_and(|name| name.starts_with("User") && name.len() == 12)
                    );
                    assert!(!players[1 - bot_player].occupied);
                }
                other => panic!("expected room message, got {other:?}"),
            }
        }
    }

    #[test]
    fn managed_bot_lobby_refresh_respects_human_occupancy_until_finished() {
        let (rooms, _) = test_managed_room_store();
        rooms.ensure_managed_bot_lobbies();
        let (slot, code, room) = {
            let stored = rooms.rooms.lock().expect("room store lock poisoned");
            let room = stored.values().next().expect("managed room").clone();
            (
                room.managed_bot_slot.expect("managed slot"),
                room.code.clone(),
                room,
            )
        };
        room.last_activity_ms.store(
            current_unix_ms().saturating_sub(MANAGED_BOT_LOBBY_REFRESH_MS + 1),
            Ordering::Relaxed,
        );

        assert!(rooms.ensure_managed_bot_lobbies());
        let refreshed_code = {
            let stored = rooms.rooms.lock().expect("room store lock poisoned");
            assert!(!stored.contains_key(&code));
            stored
                .values()
                .find(|room| room.managed_bot_slot == Some(slot))
                .expect("refreshed slot")
                .code
                .clone()
        };
        assert_ne!(refreshed_code, code);

        let (room, human_player) = rooms
            .join_room("user_human", &refreshed_code)
            .expect("human joins managed room");
        room.last_activity_ms.store(
            current_unix_ms().saturating_sub(MANAGED_BOT_LOBBY_REFRESH_MS + 1),
            Ordering::Relaxed,
        );
        assert!(!rooms.ensure_managed_bot_lobbies());
        assert!(
            rooms
                .rooms
                .lock()
                .expect("room store lock poisoned")
                .contains_key(&refreshed_code)
        );

        assert!(room.mark_finished(
            Some(room.managed_bot_player().unwrap_or(1 - human_player)),
            "game"
        ));
        assert!(rooms.ensure_managed_bot_lobbies());
        let stored = rooms.rooms.lock().expect("room store lock poisoned");
        assert!(!stored.contains_key(&refreshed_code));
        assert!(
            stored
                .values()
                .any(|room| room.managed_bot_slot == Some(slot))
        );
    }

    #[test]
    fn managed_bot_move_is_applied_once_after_human_action() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let factory = Arc::new(SessionFactory::new_game(
                Arc::new(AlternatingEvaluator),
                "alternating",
                Arc::new(AlternatingPresenter),
                [true, true],
                None,
            ));
            let rooms = Arc::new(MultiplayerRoomStore::new_with_managed_bot_lobbies(
                Arc::clone(&factory),
                None,
                false,
            ));
            let room = Arc::new(MultiplayerRoom::new_managed_bot(
                "BOT123".into(),
                1,
                factory.create_multiplayer_session(),
                ManagedBotLobbySlot::FiveThree,
                1,
                None,
            ));
            rooms
                .rooms
                .lock()
                .expect("room store lock poisoned")
                .insert(room.code.clone(), Arc::clone(&room));
            assert_eq!(
                room.assign_or_find_viewer(SeatOwner::human("user_human", Some("Human"))),
                MultiplayerViewer::Player(0)
            );

            let (tx, mut rx) = mpsc::unbounded_channel();
            let socket_id = room
                .register_socket(
                    "user_human",
                    Some("Human"),
                    MultiplayerViewer::Player(0),
                    tx,
                )
                .unwrap();
            let mut active = Some(ActiveMultiplayerRoom {
                room: Arc::clone(&room),
                socket_id,
                viewer: MultiplayerViewer::Player(0),
            });

            let (reply_tx, _reply_rx) = mpsc::unbounded_channel();
            let responses = handle_multiplayer_message(
                &rooms,
                "user_human",
                &reply_tx,
                &mut active,
                ClientMsg::PlayMultiplayerAction { action: 0 },
            )
            .await;
            assert!(responses.iter().any(|msg| {
                matches!(
                    msg,
                    ServerMsg::GameState {
                        current_player: 1,
                        ..
                    }
                )
            }));

            maybe_schedule_managed_bot_move(Arc::clone(&rooms), Arc::clone(&room));
            let mut saw_terminal_bot_state = false;
            for _ in 0..8 {
                let raw = timeout(Duration::from_secs(2), rx.recv())
                    .await
                    .expect("bot state broadcast timeout")
                    .expect("bot state broadcast");
                let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
                if value["type"] == serde_json::json!("GameState")
                    && value["state"]["moves"] == serde_json::json!(2)
                    && value["is_terminal"] == serde_json::json!(true)
                {
                    saw_terminal_bot_state = true;
                    break;
                }
            }
            assert!(saw_terminal_bot_state);

            tokio::time::sleep(Duration::from_millis(25)).await;
            let session = room.session.lock().await;
            match session.state_msg_for_player(0) {
                ServerMsg::GameState { state, .. } => {
                    assert_eq!(state["moves"], serde_json::json!(2));
                }
                other => panic!("expected final game state, got {other:?}"),
            }
        });
    }

    #[test]
    fn multiplayer_room_exposes_display_names() {
        let (rooms, _) = test_room_store();

        let (room, _) = rooms
            .create_room_with_display_name(
                "user_a",
                Some("Alice"),
                Some(0),
                Some("NAMES1".into()),
                None,
                None,
                None,
            )
            .unwrap();
        rooms
            .join_room_with_display_name("user_b", Some("Bob"), "NAMES1")
            .unwrap();

        match room.room_msg_for_player(Some(0)) {
            ServerMsg::MultiplayerRoom { players, .. } => {
                assert_eq!(players[0].name.as_deref(), Some("Alice"));
                assert_eq!(players[1].name.as_deref(), Some("Bob"));
            }
            other => panic!("expected multiplayer room message, got {other:?}"),
        }
    }

    #[test]
    fn multiplayer_chat_broadcasts_to_players_and_survives_reconnect() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (tx_a, mut rx_a) = mpsc::unbounded_channel();
            let (tx_b, mut rx_b) = mpsc::unbounded_channel();
            let mut active_a = None;
            let mut active_b = None;

            let _ = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx_a,
                &mut active_a,
                ClientMsg::CreateMultiplayerRoom {
                    preferred_player: Some(0),
                    code: Some("CHAT01".into()),
                    is_public: Some(true),
                    time_minutes: None,
                    increment_seconds: None,
                },
            )
            .await;
            let _ = handle_multiplayer_message(
                &rooms,
                "user_b",
                &tx_b,
                &mut active_b,
                ClientMsg::JoinMultiplayerRoom {
                    code: "CHAT01".into(),
                },
            )
            .await;
            while rx_a.try_recv().is_ok() {}

            let responses = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx_a,
                &mut active_a,
                ClientMsg::SendMultiplayerChat {
                    text: " hello opponent ".into(),
                },
            )
            .await;
            match responses.as_slice() {
                [ServerMsg::MultiplayerChat { messages }] => {
                    assert_eq!(messages.len(), 1);
                    assert_eq!(messages[0].player, 0);
                    assert_eq!(messages[0].text, "hello opponent");
                }
                other => panic!("expected chat response, got {other:?}"),
            }

            let broadcast = recv_multiplayer_chat(&mut rx_b).await;
            assert_eq!(broadcast["messages"][0]["text"], "hello opponent");

            let refresh = handle_multiplayer_message(
                &rooms,
                "user_b",
                &tx_b,
                &mut active_b,
                ClientMsg::GetMultiplayerRoom,
            )
            .await;
            assert!(refresh.iter().any(|msg| {
                matches!(
                    msg,
                    ServerMsg::MultiplayerChat { messages }
                        if messages.first().is_some_and(|message| message.text == "hello opponent")
                )
            }));

            assert!(detach_active_room(&rooms, &mut active_b));
            let (tx_b_rejoin, _rx_b_rejoin) = mpsc::unbounded_channel();
            let rejoin = handle_multiplayer_message(
                &rooms,
                "user_b",
                &tx_b_rejoin,
                &mut active_b,
                ClientMsg::JoinMultiplayerRoom {
                    code: "CHAT01".into(),
                },
            )
            .await;
            assert!(rejoin.iter().any(|msg| {
                matches!(
                    msg,
                    ServerMsg::MultiplayerChat { messages }
                        if messages.first().is_some_and(|message| message.text == "hello opponent")
                )
            }));
        });
    }

    #[test]
    fn multiplayer_chat_sanitizes_and_caps_messages() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut active_room = None;
            let _ = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::CreateMultiplayerRoom {
                    preferred_player: Some(0),
                    code: Some("CHAT02".into()),
                    is_public: Some(true),
                    time_minutes: None,
                    increment_seconds: None,
                },
            )
            .await;

            let empty = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::SendMultiplayerChat {
                    text: " \t ".into(),
                },
            )
            .await;
            assert!(matches!(
                empty.as_slice(),
                [ServerMsg::Error { message }] if message.contains("empty")
            ));

            let long = "x".repeat(300);
            let capped = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::SendMultiplayerChat {
                    text: format!("hi\u{0007}{long}"),
                },
            )
            .await;
            match capped.as_slice() {
                [ServerMsg::MultiplayerChat { messages }] => {
                    let text = &messages[0].text;
                    assert!(text.starts_with("hix"));
                    assert_eq!(text.chars().count(), 280);
                    assert!(!text.chars().any(char::is_control));
                }
                other => panic!("expected capped chat, got {other:?}"),
            }
        });
    }

    #[test]
    fn multiplayer_chat_history_is_capped_to_latest_messages() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut active_room = None;
            let _ = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::CreateMultiplayerRoom {
                    preferred_player: Some(0),
                    code: Some("CHAT03".into()),
                    is_public: Some(true),
                    time_minutes: None,
                    increment_seconds: None,
                },
            )
            .await;

            let mut last_len = 0;
            let mut first_text = String::new();
            let mut last_text = String::new();
            for i in 0..(MULTIPLAYER_CHAT_HISTORY_LIMIT + 2) {
                let responses = handle_multiplayer_message(
                    &rooms,
                    "user_a",
                    &tx,
                    &mut active_room,
                    ClientMsg::SendMultiplayerChat {
                        text: format!("msg {i}"),
                    },
                )
                .await;
                match responses.as_slice() {
                    [ServerMsg::MultiplayerChat { messages }] => {
                        last_len = messages.len();
                        first_text = messages.first().unwrap().text.clone();
                        last_text = messages.last().unwrap().text.clone();
                    }
                    other => panic!("expected chat response, got {other:?}"),
                }
            }

            assert_eq!(last_len, MULTIPLAYER_CHAT_HISTORY_LIMIT);
            assert_eq!(first_text, "msg 2");
            assert_eq!(
                last_text,
                format!("msg {}", MULTIPLAYER_CHAT_HISTORY_LIMIT + 1)
            );
        });
    }

    #[test]
    fn multiplayer_full_room_closes_when_last_player_disconnects_and_spectator_can_refresh() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let code = "CLOSE1".to_string();
            let (tx_a, _rx_a) = mpsc::unbounded_channel();
            let (tx_b, _rx_b) = mpsc::unbounded_channel();
            let (tx_c, _rx_c) = mpsc::unbounded_channel();
            let (tx_d, _rx_d) = mpsc::unbounded_channel();
            let mut active_a = None;
            let mut active_b = None;
            let mut active_c = None;

            let created = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx_a,
                &mut active_a,
                ClientMsg::CreateMultiplayerRoom {
                    preferred_player: Some(0),
                    code: Some(code.clone()),
                    is_public: Some(true),
                    time_minutes: None,
                    increment_seconds: None,
                },
            )
            .await;
            assert!(
                created
                    .iter()
                    .any(|msg| matches!(msg, ServerMsg::MultiplayerRoom { .. }))
            );

            let joined = handle_multiplayer_message(
                &rooms,
                "user_b",
                &tx_b,
                &mut active_b,
                ClientMsg::JoinMultiplayerRoom { code: code.clone() },
            )
            .await;
            assert!(
                joined
                    .iter()
                    .any(|msg| matches!(msg, ServerMsg::MultiplayerRoom { .. }))
            );

            let spectator_joined = handle_multiplayer_message(
                &rooms,
                "user_c",
                &tx_c,
                &mut active_c,
                ClientMsg::JoinMultiplayerRoom { code: code.clone() },
            )
            .await;
            assert!(spectator_joined.iter().any(|msg| {
                matches!(
                    msg,
                    ServerMsg::MultiplayerRoom { viewer_role, .. }
                        if viewer_role == "spectator"
                )
            }));

            assert!(detach_active_room(&rooms, &mut active_a));
            match rooms.lobby_msg() {
                ServerMsg::MultiplayerLobby { rooms } => {
                    assert_eq!(rooms.len(), 1);
                    assert_eq!(rooms[0].code, code);
                    assert_eq!(rooms[0].connected, 1);
                    assert_eq!(rooms[0].spectator_count, 1);
                    assert!(rooms[0].empty_room_closes_at_ms.is_none());
                }
                other => panic!("expected lobby message, got {other:?}"),
            }

            assert!(detach_active_room(&rooms, &mut active_b));
            match rooms.lobby_msg() {
                ServerMsg::MultiplayerLobby { rooms } => assert!(rooms.is_empty()),
                other => panic!("expected lobby message, got {other:?}"),
            }

            let mut active_d = None;
            let rejected = handle_multiplayer_message(
                &rooms,
                "user_d",
                &tx_d,
                &mut active_d,
                ClientMsg::JoinMultiplayerRoom { code: code.clone() },
            )
            .await;
            match rejected.as_slice() {
                [ServerMsg::Error { message }] => assert!(message.contains("not found")),
                other => panic!("expected closed room to reject join, got {other:?}"),
            }

            let refreshed = handle_multiplayer_message(
                &rooms,
                "user_c",
                &tx_c,
                &mut active_c,
                ClientMsg::GetMultiplayerRoom,
            )
            .await;
            assert!(refreshed.iter().any(|msg| {
                matches!(
                    msg,
                    ServerMsg::MultiplayerRoom {
                        code: refreshed_code,
                        viewer_role,
                        ..
                    } if refreshed_code == &code && viewer_role == "spectator"
                )
            }));
        });
    }

    #[test]
    fn multiplayer_waiting_room_closes_when_only_player_disconnects() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let code = "EMPTY1".to_string();
            let (tx_a, _rx_a) = mpsc::unbounded_channel();
            let mut active_a = None;

            let created = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx_a,
                &mut active_a,
                ClientMsg::CreateMultiplayerRoom {
                    preferred_player: Some(0),
                    code: Some(code.clone()),
                    is_public: Some(true),
                    time_minutes: None,
                    increment_seconds: None,
                },
            )
            .await;
            assert!(
                created
                    .iter()
                    .any(|msg| matches!(msg, ServerMsg::MultiplayerRoom { .. }))
            );

            match rooms.lobby_msg() {
                ServerMsg::MultiplayerLobby { rooms } => {
                    assert_eq!(rooms.len(), 1);
                    assert_eq!(rooms[0].code, code);
                    assert_eq!(rooms[0].connected, 1);
                    assert_eq!(rooms[0].spectator_count, 0);
                    assert!(rooms[0].empty_room_closes_at_ms.is_none());
                }
                other => panic!("expected lobby message, got {other:?}"),
            }

            assert!(detach_active_room(&rooms, &mut active_a));
            match rooms.lobby_msg() {
                ServerMsg::MultiplayerLobby { rooms } => assert!(rooms.is_empty()),
                other => panic!("expected lobby message, got {other:?}"),
            }
            match rooms.join_room("user_b", &code) {
                Ok(_) => panic!("expected closed room to reject join"),
                Err(err) => assert!(err.contains("not found")),
            }
        });
    }

    #[test]
    fn multiplayer_reconnect_replaces_previous_player_socket() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (room, player_a) = rooms
                .create_room("user_a", Some(0), Some("REJOIN".into()), None, None, None)
                .unwrap();
            rooms.join_room("user_b", "REJOIN").unwrap();
            let (old_tx, _old_rx) = mpsc::unbounded_channel();
            let (new_tx, _new_rx) = mpsc::unbounded_channel();
            let old_socket = room
                .register_socket("user_a", None, MultiplayerViewer::Player(player_a), old_tx)
                .unwrap();
            let new_socket = room
                .register_socket("user_a", None, MultiplayerViewer::Player(player_a), new_tx)
                .unwrap();

            assert!(!room.socket_registered(old_socket));
            assert!(room.socket_registered(new_socket));

            let mut old_active = Some(ActiveMultiplayerRoom {
                room: Arc::clone(&room),
                socket_id: old_socket,
                viewer: MultiplayerViewer::Player(player_a),
            });
            let (tx, _rx) = mpsc::unbounded_channel();
            let responses = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut old_active,
                ClientMsg::PlayMultiplayerAction { action: 0 },
            )
            .await;

            assert!(old_active.is_none());
            match responses.as_slice() {
                [ServerMsg::Error { message }] => {
                    assert!(message.contains("Join a multiplayer room"));
                }
                other => panic!("expected stale socket error, got {other:?}"),
            }
        });
    }

    #[test]
    fn multiplayer_get_room_refresh_reports_current_presence() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (tx_a, _rx_a) = mpsc::unbounded_channel();
            let (tx_b, _rx_b) = mpsc::unbounded_channel();
            let mut active_a = None;
            let mut active_b = None;

            let _ = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx_a,
                &mut active_a,
                ClientMsg::CreateMultiplayerRoom {
                    preferred_player: Some(0),
                    code: Some("SYNC12".into()),
                    is_public: Some(true),
                    time_minutes: None,
                    increment_seconds: None,
                },
            )
            .await;
            let _ = handle_multiplayer_message(
                &rooms,
                "user_b",
                &tx_b,
                &mut active_b,
                ClientMsg::JoinMultiplayerRoom {
                    code: "SYNC12".into(),
                },
            )
            .await;

            let responses = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx_a,
                &mut active_a,
                ClientMsg::GetMultiplayerRoom,
            )
            .await;

            match responses.as_slice() {
                [ServerMsg::MultiplayerRoom { players, .. }] => {
                    assert!(players[0].connected);
                    assert!(players[1].connected);
                }
                other => panic!("expected room refresh response, got {other:?}"),
            }
        });
    }

    #[test]
    fn multiplayer_full_room_join_returns_spectator_without_legal_actions() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (room, _) = rooms
                .create_room(
                    "user_a",
                    Some(0),
                    Some("SPEC12".into()),
                    Some(true),
                    None,
                    None,
                )
                .unwrap();
            rooms.join_room("user_b", &room.code).unwrap();

            let (tx_c, _rx_c) = mpsc::unbounded_channel();
            let mut active_c = None;
            let responses = handle_multiplayer_message(
                &rooms,
                "user_c",
                &tx_c,
                &mut active_c,
                ClientMsg::JoinMultiplayerRoom {
                    code: "SPEC12".into(),
                },
            )
            .await;

            match responses.as_slice() {
                [
                    ServerMsg::MultiplayerRoom {
                        viewer_role,
                        local_player,
                        spectators,
                        ..
                    },
                    ServerMsg::GameState {
                        state,
                        legal_actions,
                        ..
                    },
                ] => {
                    assert_eq!(viewer_role, "spectator");
                    assert_eq!(*local_player, None);
                    assert_eq!(spectators.len(), 1);
                    assert!(spectators[0].you);
                    assert!(legal_actions.is_empty());
                    assert_eq!(state["viewer"], serde_json::json!("spectator"));
                }
                other => panic!("expected spectator join response, got {other:?}"),
            }
            assert!(matches!(
                active_c.as_ref().map(|active| active.viewer),
                Some(MultiplayerViewer::Spectator)
            ));
        });
    }

    #[test]
    fn multiplayer_seated_user_rejoins_full_room_as_player() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (room, player_a) = rooms
                .create_room(
                    "user_a",
                    Some(0),
                    Some("PLAYER".into()),
                    Some(true),
                    None,
                    None,
                )
                .unwrap();
            rooms.join_room("user_b", &room.code).unwrap();

            let (tx_a, _rx_a) = mpsc::unbounded_channel();
            let mut active_a = None;
            let responses = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx_a,
                &mut active_a,
                ClientMsg::JoinMultiplayerRoom {
                    code: "PLAYER".into(),
                },
            )
            .await;

            match responses.as_slice() {
                [
                    ServerMsg::MultiplayerRoom {
                        viewer_role,
                        local_player,
                        spectators,
                        ..
                    },
                    ServerMsg::GameState { legal_actions, .. },
                ] => {
                    assert_eq!(viewer_role, "player");
                    assert_eq!(*local_player, Some(player_a as u8));
                    assert!(spectators.is_empty());
                    assert!(!legal_actions.is_empty());
                }
                other => panic!("expected seated player rejoin response, got {other:?}"),
            }
            assert!(matches!(
                active_a.as_ref().map(|active| active.viewer),
                Some(MultiplayerViewer::Player(0))
            ));
        });
    }

    #[test]
    fn multiplayer_spectator_receives_state_updates_and_cannot_act() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (tx_c, mut rx_c) = mpsc::unbounded_channel();
            let (room, player_a) = rooms
                .create_room(
                    "user_a",
                    Some(0),
                    Some("WATCH1".into()),
                    Some(true),
                    Some(10),
                    Some(0),
                )
                .unwrap();
            rooms.join_room("user_b", &room.code).unwrap();
            let socket_id = room
                .register_socket(
                    "user_c",
                    Some("Spec"),
                    MultiplayerViewer::Spectator,
                    tx_c.clone(),
                )
                .unwrap();
            let mut active_c = Some(ActiveMultiplayerRoom {
                room: Arc::clone(&room),
                socket_id,
                viewer: MultiplayerViewer::Spectator,
            });

            for msg in [
                ClientMsg::PlayMultiplayerAction { action: 0 },
                ClientMsg::SendMultiplayerChat {
                    text: "watching".into(),
                },
                ClientMsg::ResignMultiplayerGame,
                ClientMsg::AddMultiplayerOpponentTime,
            ] {
                let responses =
                    handle_multiplayer_message(&rooms, "user_c", &tx_c, &mut active_c, msg).await;
                match responses.as_slice() {
                    [ServerMsg::Error { message }] => {
                        assert!(message.contains("Spectators cannot"));
                    }
                    other => panic!("expected spectator command rejection, got {other:?}"),
                }
            }

            {
                let mut session = room.session.lock().await;
                session.play_human_action(player_a, 0).unwrap();
                room.broadcast_state_except(&session, None);
            }

            let mut spectator_state = None;
            for _ in 0..3 {
                let raw = rx_c.try_recv().expect("spectator state broadcast");
                let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
                if value["type"] == serde_json::json!("GameState") {
                    spectator_state = Some(value);
                    break;
                }
            }
            let state = spectator_state.expect("spectator should receive game state update");
            assert_eq!(state["state"]["viewer"], serde_json::json!("spectator"));
            assert_eq!(state["legal_actions"], serde_json::json!([]));
        });
    }

    #[test]
    fn multiplayer_room_creation_broadcasts_lobby() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (lobby_tx, mut lobby_rx) = mpsc::unbounded_channel();
            rooms.register_lobby_socket(lobby_tx);
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut active_room = None;

            let responses = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::CreateMultiplayerRoom {
                    preferred_player: Some(0),
                    code: Some("ROOM42".into()),
                    is_public: Some(true),
                    time_minutes: Some(30),
                    increment_seconds: Some(5),
                },
            )
            .await;

            let code = responses
                .iter()
                .find_map(|msg| match msg {
                    ServerMsg::MultiplayerRoom { code, .. } => Some(code.clone()),
                    _ => None,
                })
                .expect("created room response");
            let lobby_msg: serde_json::Value =
                serde_json::from_str(&lobby_rx.try_recv().expect("lobby broadcast")).unwrap();
            assert_eq!(lobby_msg["type"], serde_json::json!("MultiplayerLobby"));
            let rooms_json = lobby_msg["rooms"].as_array().expect("lobby rooms");
            assert_eq!(rooms_json.len(), 1);
            assert_eq!(code, "ROOM42");
            assert_eq!(rooms_json[0]["code"], serde_json::json!("ROOM42"));
            assert_eq!(rooms_json[0]["status"], serde_json::json!("waiting"));
            assert_eq!(rooms_json[0]["occupied"], serde_json::json!(1));
            assert_eq!(rooms_json[0]["connected"], serde_json::json!(1));
            assert_eq!(rooms_json[0]["is_public"], serde_json::json!(true));
            assert_eq!(rooms_json[0]["time_minutes"], serde_json::json!(30));
            assert_eq!(rooms_json[0]["increment_seconds"], serde_json::json!(5));
        });
    }

    #[test]
    fn private_multiplayer_room_is_joinable_but_hidden_from_lobby() {
        let (rooms, _) = test_room_store();
        let (room, _) = rooms
            .create_room(
                "user_a",
                Some(0),
                Some("HIDDEN".into()),
                Some(false),
                None,
                None,
            )
            .unwrap();

        match rooms.lobby_msg() {
            ServerMsg::MultiplayerLobby { rooms } => assert!(rooms.is_empty()),
            other => panic!("expected lobby message, got {other:?}"),
        }

        let (joined_room, player) = rooms.join_room("user_b", "HIDDEN").unwrap();
        assert!(Arc::ptr_eq(&room, &joined_room));
        assert_eq!(player, 1);
    }

    #[test]
    fn multiplayer_finished_room_replay_is_saved_for_both_players() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (store, _) = test_store();
            let replay_store = Arc::new(ReplayStore::File(FileReplayStore::new(temp_replay_dir(
                "multiplayer-finished-replay",
            ))));
            let rooms = MultiplayerRoomStore::new(
                Arc::clone(&store.factory),
                Some(Arc::clone(&replay_store)),
            );
            let (room, _) = rooms
                .create_room(
                    "user_a",
                    Some(0),
                    Some("DONE12".into()),
                    Some(true),
                    None,
                    None,
                )
                .unwrap();
            rooms.join_room("user_b", "DONE12").unwrap();

            let errors = {
                let mut session = room.session.lock().await;
                session.play_human_action(0, 0).unwrap();
                room.save_finished_room_replay_once(&session).await
            };
            assert!(errors.is_empty());

            let entries_a = replay_store
                .list(&redacted_account_key("user_a"))
                .await
                .unwrap();
            let entries_b = replay_store
                .list(&redacted_account_key("user_b"))
                .await
                .unwrap();
            assert_eq!(entries_a.len(), 1);
            assert_eq!(entries_b.len(), 1);
            assert_eq!(entries_a[0].action_count, 1);
            assert_eq!(entries_b[0].action_count, 1);
        });
    }

    #[test]
    fn multiplayer_resign_finishes_room_and_exposes_replay_share_slug() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (store, _) = test_store();
            let replay_store = Arc::new(ReplayStore::File(FileReplayStore::new(temp_replay_dir(
                "multiplayer-resign-replay",
            ))));
            let rooms = Arc::new(MultiplayerRoomStore::new(
                Arc::clone(&store.factory),
                Some(Arc::clone(&replay_store)),
            ));
            let (room, player_a) = rooms
                .create_room(
                    "user_a",
                    Some(0),
                    Some("QUIT12".into()),
                    Some(true),
                    None,
                    None,
                )
                .unwrap();
            rooms.join_room("user_b", "QUIT12").unwrap();

            let (tx, _rx) = mpsc::unbounded_channel();
            let socket_id = room
                .register_socket(
                    "user_a",
                    None,
                    MultiplayerViewer::Player(player_a),
                    tx.clone(),
                )
                .unwrap();
            let mut active_room = Some(ActiveMultiplayerRoom {
                room,
                socket_id,
                viewer: MultiplayerViewer::Player(player_a),
            });

            let responses = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::ResignMultiplayerGame,
            )
            .await;

            let share_slug = match responses.as_slice() {
                [
                    ServerMsg::MultiplayerRoom {
                        status,
                        winner,
                        finish_reason,
                        replay_share_slug,
                        ..
                    },
                ] => {
                    assert_eq!(status, "finished");
                    assert_eq!(*winner, Some(1));
                    assert_eq!(finish_reason.as_deref(), Some("resignation"));
                    replay_share_slug
                        .clone()
                        .expect("resign should expose replay slug")
                }
                other => panic!("expected resign room response, got {other:?}"),
            };
            assert!(safe_share_slug(&share_slug));

            let loaded = replay_store.load_shared(&share_slug).await.unwrap();
            assert_eq!(loaded.log.actions.len(), 0);

            let entries_a = replay_store
                .list(&redacted_account_key("user_a"))
                .await
                .unwrap();
            let entries_b = replay_store
                .list(&redacted_account_key("user_b"))
                .await
                .unwrap();
            assert_eq!(entries_a[0].result, "lost_by_resignation");
            assert_eq!(entries_b[0].result, "won_by_resignation");
        });
    }

    #[test]
    fn multiplayer_adds_clock_time_to_opponent_only() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut active_room = None;

            let _ = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::CreateMultiplayerRoom {
                    preferred_player: Some(0),
                    code: Some("TIME15".into()),
                    is_public: Some(true),
                    time_minutes: Some(10),
                    increment_seconds: Some(3),
                },
            )
            .await;

            let responses = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::AddMultiplayerOpponentTime,
            )
            .await;

            match responses.as_slice() {
                [ServerMsg::MultiplayerRoom { players, .. }] => {
                    assert_eq!(players[0].time_millis, Some(600_000));
                    assert_eq!(players[1].time_millis, Some(615_000));
                }
                other => panic!("expected room clock response, got {other:?}"),
            }
        });
    }

    #[test]
    fn room_clock_is_wall_clock_driven_after_start() {
        let mut clock = RoomClock::new(Some(1));
        assert!(!clock.refresh(10_000));
        assert_eq!(
            clock.snapshot().time_millis,
            [Some(60_000_u64), Some(60_000_u64)]
        );
        assert_eq!(clock.snapshot().winner, None);

        clock.started = true;
        clock.active_player = Some(1);
        clock.active_since_ms = Some(10_000);

        assert!(clock.refresh(25_500));
        assert_eq!(
            clock.snapshot().time_millis,
            [Some(60_000_u64), Some(44_500_u64)]
        );
        assert_eq!(clock.snapshot().winner, None);

        assert!(clock.refresh(70_000));
        assert_eq!(
            clock.snapshot().time_millis,
            [Some(60_000_u64), Some(0_u64)]
        );
        assert_eq!(clock.snapshot().active_player, None);
        assert_eq!(clock.snapshot().winner, Some(0));
    }

    #[test]
    fn multiplayer_room_state_is_local_to_each_player() {
        let (rooms, _) = test_room_store();
        let (room, _) = rooms
            .create_room("user_a", Some(0), None, None, None, None)
            .unwrap();
        rooms.join_room("user_b", &room.code).unwrap();

        let session = room.session.blocking_lock();
        match session.state_msg_for_player(0) {
            ServerMsg::GameState {
                state,
                legal_actions,
                can_undo,
                can_redo,
                ..
            } => {
                assert_eq!(state["viewer"], serde_json::json!(0));
                assert_eq!(legal_actions.len(), 1);
                assert!(!can_undo);
                assert!(!can_redo);
            }
            other => panic!("expected p1 GameState, got {other:?}"),
        }
        match session.state_msg_for_player(1) {
            ServerMsg::GameState {
                state,
                legal_actions,
                ..
            } => {
                assert_eq!(state["viewer"], serde_json::json!(1));
                assert!(legal_actions.is_empty());
            }
            other => panic!("expected p2 GameState, got {other:?}"),
        }
    }

    #[test]
    fn multiplayer_room_routes_undo_and_redo_to_room_session() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (room, player_a) = rooms
                .create_room("user_a", Some(0), None, None, None, None)
                .unwrap();
            rooms.join_room("user_b", &room.code).unwrap();
            let (tx, _rx) = mpsc::unbounded_channel();
            let socket_id = room
                .register_socket(
                    "user_a",
                    None,
                    MultiplayerViewer::Player(player_a),
                    tx.clone(),
                )
                .unwrap();
            let mut active_room = Some(ActiveMultiplayerRoom {
                room,
                socket_id,
                viewer: MultiplayerViewer::Player(player_a),
            });

            let played = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::PlayMultiplayerAction { action: 0 },
            )
            .await;
            assert!(played.iter().any(|msg| match msg {
                ServerMsg::GameState { can_undo, .. } => *can_undo,
                _ => false,
            }));

            let undone = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::Undo,
            )
            .await;
            match undone.as_slice() {
                [
                    ServerMsg::MultiplayerRoom { .. },
                    ServerMsg::GameState {
                        state,
                        history_cursor,
                        can_undo,
                        can_redo,
                        ..
                    },
                ] => {
                    assert_eq!(state["moves"], serde_json::json!(0));
                    assert_eq!(*history_cursor, 0);
                    assert!(!*can_undo);
                    assert!(*can_redo);
                }
                other => panic!("expected multiplayer undo state, got {other:?}"),
            }

            let redone = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::Redo,
            )
            .await;
            match redone.as_slice() {
                [
                    ServerMsg::MultiplayerRoom { .. },
                    ServerMsg::GameState {
                        state,
                        history_cursor,
                        can_undo,
                        can_redo,
                        ..
                    },
                ] => {
                    assert_eq!(state["moves"], serde_json::json!(1));
                    assert_eq!(*history_cursor, 1);
                    assert!(*can_undo);
                    assert!(!*can_redo);
                }
                other => panic!("expected multiplayer redo state, got {other:?}"),
            }
        });
    }

    #[test]
    fn anonymous_multiplayer_can_create_room() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut active_room = None;

            let responses = handle_multiplayer_message(
                &rooms,
                "anonymous:a",
                &tx,
                &mut active_room,
                ClientMsg::CreateMultiplayerRoom {
                    preferred_player: Some(0),
                    code: None,
                    is_public: None,
                    time_minutes: None,
                    increment_seconds: None,
                },
            )
            .await;

            match responses.as_slice() {
                [
                    ServerMsg::MultiplayerRoom {
                        status,
                        local_player,
                        ..
                    },
                    ServerMsg::GameState { .. },
                    ..,
                ] => {
                    assert_eq!(status, "waiting");
                    assert_eq!(*local_player, Some(0));
                }
                other => panic!("expected anonymous room creation, got {other:?}"),
            }
        });
    }

    #[test]
    fn multiplayer_rejects_play_before_opponent_joins() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (room, player_a) = rooms
                .create_room("user_a", Some(0), None, None, None, None)
                .unwrap();
            let (tx, _rx) = mpsc::unbounded_channel();
            let socket_id = room
                .register_socket(
                    "user_a",
                    None,
                    MultiplayerViewer::Player(player_a),
                    tx.clone(),
                )
                .unwrap();
            let mut active_room = Some(ActiveMultiplayerRoom {
                room,
                socket_id,
                viewer: MultiplayerViewer::Player(player_a),
            });

            let responses = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx,
                &mut active_room,
                ClientMsg::PlayMultiplayerAction { action: 0 },
            )
            .await;

            match responses.as_slice() {
                [ServerMsg::Error { message }] => {
                    assert!(message.contains("waiting"));
                }
                other => panic!("expected waiting error, got {other:?}"),
            }
        });
    }

    #[test]
    fn multiplayer_player_move_catches_up_disconnected_opponent_on_rejoin() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (room, player_a) = rooms
                .create_room("user_a", Some(0), Some("CATCH1".into()), None, None, None)
                .unwrap();
            let (_, player_b) = rooms.join_room("user_b", "CATCH1").unwrap();
            let (tx_a, _rx_a) = mpsc::unbounded_channel();
            let (tx_b, _rx_b) = mpsc::unbounded_channel();
            let socket_a = room
                .register_socket(
                    "user_a",
                    None,
                    MultiplayerViewer::Player(player_a),
                    tx_a.clone(),
                )
                .unwrap();
            let socket_b = room
                .register_socket("user_b", None, MultiplayerViewer::Player(player_b), tx_b)
                .unwrap();
            room.unregister_socket(socket_b);

            let mut active_a = Some(ActiveMultiplayerRoom {
                room: Arc::clone(&room),
                socket_id: socket_a,
                viewer: MultiplayerViewer::Player(player_a),
            });
            let responses = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx_a,
                &mut active_a,
                ClientMsg::PlayMultiplayerAction { action: 0 },
            )
            .await;

            assert!(responses.iter().any(|msg| {
                matches!(
                    msg,
                    ServerMsg::GameState { state, .. }
                        if state["moves"] == serde_json::json!(1)
                )
            }));

            let (tx_b_rejoin, _rx_b_rejoin) = mpsc::unbounded_channel();
            let mut active_b = None;
            let rejoin = handle_multiplayer_message(
                &rooms,
                "user_b",
                &tx_b_rejoin,
                &mut active_b,
                ClientMsg::JoinMultiplayerRoom {
                    code: "CATCH1".into(),
                },
            )
            .await;

            match rejoin.as_slice() {
                [
                    ServerMsg::MultiplayerRoom {
                        viewer_role,
                        local_player,
                        players,
                        ..
                    },
                    ServerMsg::GameState { state, .. },
                ] => {
                    assert_eq!(viewer_role, "player");
                    assert_eq!(*local_player, Some(player_b as u8));
                    assert!(players[player_a].connected);
                    assert!(players[player_b].connected);
                    assert_eq!(state["moves"], serde_json::json!(1));
                    assert_eq!(state["viewer"], serde_json::json!(player_b));
                }
                other => panic!("expected caught-up player rejoin response, got {other:?}"),
            }
        });
    }

    #[test]
    fn multiplayer_room_rejects_wrong_player_and_broadcasts_by_perspective() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let (room, player_a) = rooms
                .create_room("user_a", Some(0), None, None, None, None)
                .unwrap();
            let (_, player_b) = rooms.join_room("user_b", &room.code).unwrap();
            let (tx_a, mut rx_a) = mpsc::unbounded_channel();
            let (tx_b, mut rx_b) = mpsc::unbounded_channel();
            room.register_socket("user_a", None, MultiplayerViewer::Player(player_a), tx_a)
                .unwrap();
            room.register_socket("user_b", None, MultiplayerViewer::Player(player_b), tx_b)
                .unwrap();

            let mut session = room.session.lock().await;
            assert!(session.play_human_action(1, 0).is_err());
            session.play_human_action(0, 0).unwrap();
            room.broadcast_state_except(&session, None);
            drop(session);

            let msg_a: serde_json::Value =
                serde_json::from_str(&rx_a.try_recv().expect("p1 broadcast")).unwrap();
            let msg_b: serde_json::Value =
                serde_json::from_str(&rx_b.try_recv().expect("p2 broadcast")).unwrap();
            assert_eq!(msg_a["state"]["viewer"], serde_json::json!(0));
            assert_eq!(msg_a["state"]["moves"], serde_json::json!(1));
            assert_eq!(msg_b["state"]["viewer"], serde_json::json!(1));
            assert_eq!(msg_b["state"]["moves"], serde_json::json!(1));
        });
    }

    #[test]
    fn multiplayer_move_defers_analysis_bar_broadcast() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (room, player_a) = rooms
                .create_room("user_a", Some(0), None, None, None, None)
                .unwrap();
            let (_, player_b) = rooms.join_room("user_b", &room.code).unwrap();
            let (tx_a, mut rx_a) = mpsc::unbounded_channel();
            let (tx_b, mut rx_b) = mpsc::unbounded_channel();
            let socket_id = room
                .register_socket(
                    "user_a",
                    None,
                    MultiplayerViewer::Player(player_a),
                    tx_a.clone(),
                )
                .unwrap();
            room.register_socket("user_b", None, MultiplayerViewer::Player(player_b), tx_b)
                .unwrap();
            let mut active_room = Some(ActiveMultiplayerRoom {
                room,
                socket_id,
                viewer: MultiplayerViewer::Player(player_a),
            });

            let responses = handle_multiplayer_message(
                &rooms,
                "user_a",
                &tx_a,
                &mut active_room,
                ClientMsg::PlayMultiplayerAction { action: 0 },
            )
            .await;

            assert!(
                responses
                    .iter()
                    .any(|msg| matches!(msg, ServerMsg::GameState { .. }))
            );
            assert!(
                !responses
                    .iter()
                    .any(|msg| matches!(msg, ServerMsg::MultiplayerAnalysis { .. }))
            );

            let _state_msg: serde_json::Value =
                serde_json::from_str(&rx_b.try_recv().expect("p2 state broadcast")).unwrap();
            let analysis_msg = recv_multiplayer_analysis(&mut rx_b).await;
            let sender_analysis_msg = recv_multiplayer_analysis(&mut rx_a).await;
            assert_eq!(analysis_msg["type"], "MultiplayerAnalysis");
            assert_eq!(sender_analysis_msg["type"], "MultiplayerAnalysis");
            assert!(analysis_msg["root_wdl"].as_array().is_some());
            assert!(analysis_msg.get("snapshot").is_none());
            assert!(analysis_msg.get("action_labels").is_none());
        });
    }

    #[test]
    fn replay_store_lists_only_the_current_account() {
        let dir = temp_replay_dir("replay-scope");
        let store = FileReplayStore::new(dir.clone());
        let log = GameLog {
            initial_state: "1".into(),
            actions: vec![0, 0],
        };

        let saved_a = store
            .save_with_result("account_a", 11, 1, &log, None)
            .unwrap();
        store
            .save_with_result("account_b", 22, 1, &log, None)
            .unwrap();

        let entries_a = store.list("account_a").unwrap();
        let entries_b = store.list("account_b").unwrap();

        assert_eq!(entries_a.len(), 1);
        assert_eq!(entries_a[0].id, saved_a.id);
        assert_eq!(entries_a[0].action_count, 2);
        assert!(safe_share_slug(&entries_a[0].share_slug));
        assert_eq!(entries_b.len(), 1);
        assert_ne!(entries_a[0].id, entries_b[0].id);

        let all_entries = store.list_all(50).unwrap();
        assert_eq!(all_entries.len(), 2);
        assert!(all_entries.iter().any(|entry| {
            entry.account_key == "account_a" && entry.entry.id == entries_a[0].id
        }));
        assert!(all_entries.iter().any(|entry| {
            entry.account_key == "account_b" && entry.entry.id == entries_b[0].id
        }));
        assert_eq!(store.list_all(1).unwrap().len(), 1);

        let shared = store
            .load_shared(&entries_a[0].share_slug)
            .expect("shared replay load");
        assert_eq!(shared.id, entries_a[0].id);
        assert_eq!(shared.log.actions, vec![0, 0]);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn replay_store_favorites_and_deletes_logs() {
        let dir = temp_replay_dir("replay-favorite-delete");
        let store = FileReplayStore::new(dir.clone());
        let log = GameLog {
            initial_state: "1".into(),
            actions: vec![0],
        };

        let saved = store
            .save_with_result("account_a", 11, 1, &log, None)
            .unwrap();
        assert!(!store.list("account_a").unwrap()[0].favorite);

        store
            .set_favorite("account_a", &saved.id, true)
            .expect("favorite saved replay");
        let entries = store.list("account_a").unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].favorite);

        store
            .set_favorite("account_a", &saved.id, false)
            .expect("unfavorite saved replay");
        assert!(!store.list("account_a").unwrap()[0].favorite);

        store.delete("account_a", &saved.id).expect("delete replay");
        assert!(store.list("account_a").unwrap().is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn replay_store_persists_special_result() {
        let dir = temp_replay_dir("replay-special-result");
        let store = FileReplayStore::new(dir.clone());
        let log = GameLog {
            initial_state: "1".into(),
            actions: Vec::new(),
        };

        let saved = store
            .save_with_result("account_a", 11, 1, &log, Some("won_on_time"))
            .unwrap();
        let entries = store.list("account_a").unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, saved.id);
        assert_eq!(entries[0].result, "won_on_time");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_multiplayer_replay_persists_player_names() {
        let dir = temp_replay_dir("file-multiplayer-player-names");
        let store = FileReplayStore::new(dir.clone());
        let log = GameLog {
            initial_state: "1".into(),
            actions: vec![0],
        };

        let saved = store
            .save_multiplayer_with_results(
                33,
                1,
                &log,
                &[
                    ReplayParticipant {
                        player: 0,
                        account_key: "account_a".into(),
                        display_name: Some("Alice".into()),
                        result: "lost_by_resignation".into(),
                    },
                    ReplayParticipant {
                        player: 1,
                        account_key: "account_b".into(),
                        display_name: Some("Bob".into()),
                        result: "won_by_resignation".into(),
                    },
                ],
            )
            .expect("save multiplayer replay");

        let shared = store
            .load_shared(&saved.share_slug)
            .expect("load shared multiplayer replay");
        assert_eq!(shared.player_names, Some(["Alice".into(), "Bob".into()]));

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn list_replay_entries_preserves_stored_resignation_result() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let replay_store = Arc::new(ReplayStore::File(FileReplayStore::new(temp_replay_dir(
                "replay-list-resignation-result",
            ))));
            let presenter = Arc::new(CountingPresenter {
                created_games: AtomicUsize::new(0),
            });
            let evaluator: Arc<dyn Evaluator<TestGame> + Sync> = Arc::new(TestEvaluator);
            let factory =
                SessionFactory::new_game(evaluator, "test", presenter, [true, true], None);
            let store = UserSessionStore::new(factory, Some(Arc::clone(&replay_store)));
            let (user_session, _) = store.get_or_create("user_a");
            let log = GameLog {
                initial_state: "1".into(),
                actions: Vec::new(),
            };

            replay_store
                .save_with_result(
                    &user_session.account_key,
                    user_session.session_id,
                    0,
                    &log,
                    Some("lost_by_resignation"),
                )
                .await
                .expect("save resignation replay result");
            let entries = list_replay_entries(&user_session)
                .await
                .expect("list replay entries");

            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].result, "lost_by_resignation");
        });
    }

    #[test]
    fn save_current_replay_once_after_singleplayer_resign_saves_result() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let replay_store = Arc::new(ReplayStore::File(FileReplayStore::new(temp_replay_dir(
                "singleplayer-resign-save-replay",
            ))));
            let presenter = Arc::new(CountingPresenter {
                created_games: AtomicUsize::new(0),
            });
            let evaluator: Arc<dyn Evaluator<TestGame> + Sync> = Arc::new(TestEvaluator);
            let factory =
                SessionFactory::new_game(evaluator, "test", presenter, [true, true], None);
            let store = UserSessionStore::new(factory, Some(Arc::clone(&replay_store)));
            let (user_session, _) = store.get_or_create("user_a");

            let saved = {
                let mut session = user_session.session.lock().await;
                session.handle(ClientMsg::SetSingleplayer {
                    human_player: Some(0),
                });
                match session.handle(ClientMsg::ResignGame).as_slice() {
                    [ServerMsg::GameState { is_terminal, .. }] => assert!(*is_terminal),
                    other => panic!("expected terminal GameState after resign, got {other:?}"),
                }
                save_current_replay_once(&user_session, &mut session)
                    .await
                    .expect("resign should save a replay")
            };

            match saved {
                ServerMsg::ReplaySaved { entry } => {
                    assert_eq!(entry.action_count, 0);
                    assert_eq!(entry.result, "lost_by_resignation");
                }
                other => panic!("expected ReplaySaved after resign, got {other:?}"),
            }
            let entries = list_replay_entries(&user_session)
                .await
                .expect("list replay entries");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].result, "lost_by_resignation");
        });
    }

    #[test]
    fn room_outcome_replay_results_describe_resignation_and_timeout() {
        let resignation = RoomOutcome {
            winner: Some(1),
            reason: "resignation".into(),
            replay_share_slug: None,
        };
        assert_eq!(
            replay_result_for_room_outcome(&resignation, 0),
            Some("lost_by_resignation")
        );
        assert_eq!(
            replay_result_for_room_outcome(&resignation, 1),
            Some("won_by_resignation")
        );

        let timeout = RoomOutcome {
            winner: Some(0),
            reason: "timeout".into(),
            replay_share_slug: None,
        };
        assert_eq!(
            replay_result_for_room_outcome(&timeout, 0),
            Some("won_on_time")
        );
        assert_eq!(
            replay_result_for_room_outcome(&timeout, 1),
            Some("lost_on_time")
        );
    }

    #[test]
    fn postgres_replay_store_integration() {
        let Ok(database_url) = std::env::var("HEXFISH_TEST_DATABASE_URL") else {
            return;
        };
        if database_url.trim().is_empty() {
            return;
        }

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let store = ReplayStore::postgres(database_url.trim())
                .await
                .expect("Postgres replay store");
            let account = format!("account_{}_{}", current_unix_ms(), fastrand::u64(..));
            let other_account = format!("other_{}_{}", current_unix_ms(), fastrand::u64(..));
            let log = GameLog {
                initial_state: "1".into(),
                actions: vec![0, 1, 2],
            };

            let saved = store
                .save_with_result(&account, 11, 1, &log, None)
                .await
                .expect("save replay");
            assert!(safe_share_slug(&saved.share_slug));
            assert!(store.list(&other_account).await.unwrap().is_empty());

            let entries = store.list(&account).await.expect("list owner replays");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, saved.id);
            assert_eq!(entries[0].action_count, 3);
            assert!(!entries[0].favorite);

            let all_entries = store.list_all(500).await.expect("list all replays");
            assert!(
                all_entries
                    .iter()
                    .any(|entry| { entry.account_key == account && entry.entry.id == saved.id })
            );

            let loaded = store
                .load(&account, &saved.id)
                .await
                .expect("load owner replay");
            assert_eq!(loaded.log.initial_state, log.initial_state);
            assert_eq!(loaded.log.actions, log.actions);
            assert_eq!(loaded.player_names, None);

            let shared = store
                .load_shared(&saved.share_slug)
                .await
                .expect("load shared replay");
            assert_eq!(shared.id, saved.id);
            assert_eq!(shared.log.actions, log.actions);

            let multiplayer_a =
                format!("multiplayer_a_{}_{}", current_unix_ms(), fastrand::u64(..));
            let multiplayer_b =
                format!("multiplayer_b_{}_{}", current_unix_ms(), fastrand::u64(..));
            let multiplayer_log = GameLog {
                initial_state: "1".into(),
                actions: vec![0],
            };
            let multiplayer_saved = store
                .save_multiplayer_with_results(
                    77,
                    1,
                    &multiplayer_log,
                    &[
                        ReplayParticipant {
                            player: 0,
                            account_key: multiplayer_a.clone(),
                            display_name: Some("Alice".into()),
                            result: "lost_by_resignation".into(),
                        },
                        ReplayParticipant {
                            player: 1,
                            account_key: multiplayer_b.clone(),
                            display_name: Some("Bob".into()),
                            result: "won_by_resignation".into(),
                        },
                    ],
                )
                .await
                .expect("save multiplayer replay");
            let multiplayer_entries_a = store
                .list(&multiplayer_a)
                .await
                .expect("list multiplayer replay for player A");
            let multiplayer_entries_b = store
                .list(&multiplayer_b)
                .await
                .expect("list multiplayer replay for player B");
            assert_eq!(multiplayer_entries_a.len(), 1);
            assert_eq!(multiplayer_entries_b.len(), 1);
            assert_eq!(multiplayer_entries_a[0].id, multiplayer_saved.id);
            assert_eq!(multiplayer_entries_b[0].id, multiplayer_saved.id);
            assert_eq!(multiplayer_entries_a[0].result, "lost_by_resignation");
            assert_eq!(multiplayer_entries_b[0].result, "won_by_resignation");

            let ReplayStore::Postgres(postgres) = &store else {
                panic!("expected Postgres replay store");
            };
            let count_rows = postgres
                .query_rows(
                    "count saved multiplayer replay rows",
                    "SELECT COUNT(*)::BIGINT AS count FROM replay_logs WHERE id = $1",
                    &[&multiplayer_saved.id],
                )
                .await
                .expect("count multiplayer replay rows");
            let row_count: i64 = count_rows[0].get("count");
            assert_eq!(row_count, 1);

            let multiplayer_loaded = store
                .load(&multiplayer_a, &multiplayer_saved.id)
                .await
                .expect("load multiplayer replay");
            assert_eq!(multiplayer_loaded.log.actions, multiplayer_log.actions);
            assert_eq!(
                multiplayer_loaded.player_names,
                Some(["Alice".into(), "Bob".into()])
            );
            let multiplayer_shared = store
                .load_shared(&multiplayer_saved.share_slug)
                .await
                .expect("load shared multiplayer replay");
            assert_eq!(
                multiplayer_shared.player_names,
                Some(["Alice".into(), "Bob".into()])
            );

            store
                .set_favorite(&multiplayer_a, &multiplayer_saved.id, true)
                .await
                .expect("favorite player A multiplayer replay");
            assert!(store.list(&multiplayer_a).await.unwrap()[0].favorite);
            assert!(!store.list(&multiplayer_b).await.unwrap()[0].favorite);

            store
                .delete(&multiplayer_a, &multiplayer_saved.id)
                .await
                .expect("delete player A multiplayer replay access");
            assert!(
                store
                    .load(&multiplayer_a, &multiplayer_saved.id)
                    .await
                    .is_err()
            );
            assert!(
                store
                    .load(&multiplayer_b, &multiplayer_saved.id)
                    .await
                    .is_ok()
            );
            assert!(
                store
                    .load_shared(&multiplayer_saved.share_slug)
                    .await
                    .is_ok()
            );
            store
                .delete(&multiplayer_b, &multiplayer_saved.id)
                .await
                .expect("delete player B multiplayer replay access");
            assert!(
                store
                    .load_shared(&multiplayer_saved.share_slug)
                    .await
                    .is_err()
            );

            store
                .set_favorite(&account, &saved.id, true)
                .await
                .expect("favorite replay");
            assert!(store.list(&account).await.unwrap()[0].favorite);

            store
                .delete(&account, &saved.id)
                .await
                .expect("delete replay");
            assert!(store.list(&account).await.unwrap().is_empty());
            assert!(store.load_shared(&saved.share_slug).await.is_err());
        });
    }

    #[test]
    fn replay_store_rejects_unsafe_ids() {
        assert!(!safe_replay_id("../game.log"));
        assert!(!safe_replay_id("..game.log"));
        assert!(!safe_replay_id(".hidden.log"));
        assert!(!safe_replay_id("nested/game.log"));
        assert!(!safe_replay_id("nested\\game.log"));
        assert!(!safe_replay_id("game.txt"));
        assert!(safe_replay_id("123-1-1.log"));

        let store = FileReplayStore::new(temp_replay_dir("unsafe-replay-id"));
        let err = store.load("account_a", "../game.log").unwrap_err();
        assert!(err.contains("invalid replay id"));
    }

    #[test]
    fn replay_messages_route_to_replay_session() {
        assert_eq!(
            client_msg_target(&super::ClientMsg::LoadReplay { id: "1.log".into() }),
            ViewTarget::Replay
        );
        assert_eq!(
            client_msg_target(&super::ClientMsg::LoadSharedReplay {
                slug: "abc123".into()
            }),
            ViewTarget::Replay
        );
        assert_eq!(
            client_msg_target(&super::ClientMsg::SetReplayCursor { cursor: 0 }),
            ViewTarget::Replay
        );
        assert_eq!(
            client_msg_target(&super::ClientMsg::RunSims {
                count: 1,
                target: Some(ViewTarget::Replay),
            }),
            ViewTarget::Replay
        );
        assert_eq!(
            client_msg_target(&super::ClientMsg::RunSearch {
                budget: SearchBudget::pv_depth(8),
                target: Some(ViewTarget::Replay),
            }),
            ViewTarget::Replay
        );
        assert_eq!(
            client_msg_target(&super::ClientMsg::PauseSearch {
                target: Some(ViewTarget::Replay),
            }),
            ViewTarget::Replay
        );
        assert_eq!(
            client_msg_target(&super::ClientMsg::PauseSearch { target: None }),
            ViewTarget::Analysis
        );
        assert_eq!(
            client_msg_target(&super::ClientMsg::ExploreSubtree {
                action_path: Vec::new(),
                depth: 1,
                target: Some(ViewTarget::Replay),
            }),
            ViewTarget::Replay
        );
        assert_eq!(
            client_msg_target(&super::ClientMsg::PlayAction { action: 0 }),
            ViewTarget::Analysis
        );
    }
}

mod protocol;
mod session;
mod traits;

pub use protocol::{ClientMsg, SearchBudget, ServerMsg};
pub use session::GameSession;
pub use traits::GamePresenter;

use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    env,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use axum::{
    Router,
    extract::{
        Path,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::Redirect,
};
use futures_util::FutureExt;
use tokio::sync::{Mutex, mpsc};
use tokio_postgres::Row;
use tower_http::services::ServeDir;

use crate::eval::Evaluator;
use crate::game::Game;
use crate::game_log::GameLog;
use crate::mcts::Config;
use protocol::{MultiplayerLobbyRoom, MultiplayerPlayer, ReplayEntry, ViewTarget};

/// Send a progress snapshot every N simulations.
const PROGRESS_INTERVAL: u32 = 100;
const SEARCH_INTERRUPT_INTERVAL: u32 = 8;
const MULTIPLAYER_ANALYSIS_SIMS: u32 = 200;
const MULTIPLAYER_EMPTY_ROOM_GRACE_MS: u64 = 5 * 60_000;
const BOARD_FINGERPRINT_RETRIES: usize = 64;
const DATABASE_URL_ENV: &str = "DATABASE_URL";
const REPLAY_STORE_REQUIRED_ENV: &str = "HEXFISH_REPLAY_STORE_REQUIRED";

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

    async fn save(
        &self,
        account_key: &str,
        session_id: u64,
        counter: u64,
        log: &GameLog,
    ) -> Result<ReplayEntry, String> {
        match self {
            Self::File(store) => store.save(account_key, session_id, counter, log),
            Self::Postgres(store) => store.save(account_key, log).await,
        }
    }

    async fn list(&self, account_key: &str) -> Result<Vec<ReplayEntry>, String> {
        match self {
            Self::File(store) => store.list(account_key),
            Self::Postgres(store) => store.list(account_key).await,
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

    async fn load(&self, account_key: &str, id: &str) -> Result<GameLog, String> {
        match self {
            Self::File(store) => store.load(account_key, id),
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

    fn save(
        &self,
        account_key: &str,
        session_id: u64,
        counter: u64,
        log: &GameLog,
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
        let share_slug = file_share_slug(&id);
        Ok(ReplayEntry {
            id,
            saved_at_ms,
            action_count: log.actions.len(),
            result: "incomplete".into(),
            favorite: false,
            share_slug,
        })
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
                result: "incomplete".into(),
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

    fn user_dir(&self, account_key: &str) -> PathBuf {
        self.root.join(account_key)
    }
}

#[derive(Clone)]
struct PostgresReplayStore {
    client: Arc<tokio_postgres::Client>,
}

impl PostgresReplayStore {
    async fn connect(database_url: &str) -> Result<Self, String> {
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

        let store = Self {
            client: Arc::new(client),
        };
        store.init_schema().await?;
        Ok(store)
    }

    async fn init_schema(&self) -> Result<(), String> {
        self.client
            .batch_execute(
                r#"
                CREATE TABLE IF NOT EXISTS replay_logs (
                    id TEXT PRIMARY KEY,
                    account_key TEXT NOT NULL,
                    share_slug TEXT NOT NULL UNIQUE,
                    initial_state TEXT NOT NULL,
                    actions BIGINT[] NOT NULL,
                    saved_at_ms BIGINT NOT NULL,
                    action_count BIGINT NOT NULL,
                    favorite BOOLEAN NOT NULL DEFAULT FALSE
                );
                CREATE INDEX IF NOT EXISTS replay_logs_account_saved_idx
                    ON replay_logs (account_key, saved_at_ms DESC, id DESC);
                CREATE INDEX IF NOT EXISTS replay_logs_share_slug_idx
                    ON replay_logs (share_slug);
                "#,
            )
            .await
            .map_err(|e| format!("failed to initialize Postgres replay schema: {e}"))
    }

    async fn save(&self, account_key: &str, log: &GameLog) -> Result<ReplayEntry, String> {
        let saved_at_ms = i64::try_from(current_unix_ms())
            .map_err(|_| "current timestamp does not fit in Postgres BIGINT".to_string())?;
        let action_count = i64::try_from(log.actions.len())
            .map_err(|_| "replay action count does not fit in Postgres BIGINT".to_string())?;
        let actions = actions_to_i64(&log.actions)?;

        for _ in 0..32 {
            let id = random_base62(24);
            let share_slug = random_base62(18);
            let row = self
                .client
                .query_opt(
                    r#"
                    INSERT INTO replay_logs (
                        id,
                        account_key,
                        share_slug,
                        initial_state,
                        actions,
                        saved_at_ms,
                        action_count,
                        favorite
                    )
                    VALUES ($1, $2, $3, $4, $5, $6, $7, FALSE)
                    ON CONFLICT DO NOTHING
                    RETURNING id, saved_at_ms, action_count, favorite, share_slug
                    "#,
                    &[
                        &id,
                        &account_key,
                        &share_slug,
                        &log.initial_state,
                        &actions,
                        &saved_at_ms,
                        &action_count,
                    ],
                )
                .await
                .map_err(|e| format!("failed to save replay log: {e}"))?;

            if let Some(row) = row {
                return replay_entry_from_row(&row);
            }
        }

        Err("failed to generate a unique replay id".into())
    }

    async fn list(&self, account_key: &str) -> Result<Vec<ReplayEntry>, String> {
        let rows = self
            .client
            .query(
                r#"
                SELECT id, saved_at_ms, action_count, favorite, share_slug
                FROM replay_logs
                WHERE account_key = $1
                ORDER BY saved_at_ms DESC, id DESC
                "#,
                &[&account_key],
            )
            .await
            .map_err(|e| format!("failed to list replays: {e}"))?;

        rows.iter().map(replay_entry_from_row).collect()
    }

    async fn delete(&self, account_key: &str, id: &str) -> Result<(), String> {
        if !safe_replay_token(id) {
            return Err("invalid replay id".into());
        }
        let deleted = self
            .client
            .execute(
                "DELETE FROM replay_logs WHERE account_key = $1 AND id = $2",
                &[&account_key, &id],
            )
            .await
            .map_err(|e| format!("failed to delete replay log: {e}"))?;
        if deleted == 0 {
            return Err("replay log not found".into());
        }
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
            .client
            .execute(
                "UPDATE replay_logs SET favorite = $3 WHERE account_key = $1 AND id = $2",
                &[&account_key, &id, &favorite],
            )
            .await
            .map_err(|e| format!("failed to save replay favourite: {e}"))?;
        if updated == 0 {
            return Err("replay log not found".into());
        }
        Ok(())
    }

    async fn load(&self, account_key: &str, id: &str) -> Result<GameLog, String> {
        if !safe_replay_token(id) {
            return Err("invalid replay id".into());
        }
        let row = self
            .client
            .query_opt(
                "SELECT initial_state, actions FROM replay_logs WHERE account_key = $1 AND id = $2",
                &[&account_key, &id],
            )
            .await
            .map_err(|e| format!("failed to read replay log: {e}"))?
            .ok_or_else(|| "replay log not found".to_string())?;
        game_log_from_row(&row)
    }

    async fn load_shared(&self, slug: &str) -> Result<LoadedReplay, String> {
        if !safe_share_slug(slug) {
            return Err("invalid replay share link".into());
        }
        tracing::info!(share_slug = %slug, "loading shared replay from Postgres");
        let row = self
            .client
            .query_opt(
                "SELECT id, initial_state, actions FROM replay_logs WHERE share_slug = $1",
                &[&slug],
            )
            .await
            .map_err(|e| format!("failed to read shared replay: {e}"))?
            .ok_or_else(|| "shared replay not found".to_string())?;
        Ok(LoadedReplay {
            id: row.get("id"),
            log: game_log_from_row(&row)?,
        })
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn replay_entry_from_row(row: &Row) -> Result<ReplayEntry, String> {
    let saved_at_ms: i64 = row.get("saved_at_ms");
    let action_count: i64 = row.get("action_count");
    Ok(ReplayEntry {
        id: row.get("id"),
        saved_at_ms: u64::try_from(saved_at_ms)
            .map_err(|_| "stored replay timestamp is negative".to_string())?,
        action_count: usize::try_from(action_count)
            .map_err(|_| "stored replay action count is invalid".to_string())?,
        result: "incomplete".into(),
        favorite: row.get("favorite"),
        share_slug: row.get("share_slug"),
    })
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
}

struct MultiplayerSocket {
    user_id: String,
    player: usize,
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
    clock: StdMutex<RoomClock>,
    outcome: StdMutex<Option<RoomOutcome>>,
    clock_generation: AtomicU64,
    next_socket_id: AtomicU64,
    replay_store: Option<Arc<ReplayStore>>,
    next_replay_counter: AtomicU64,
    replay_saved: AtomicBool,
    last_activity_ms: AtomicU64,
    analysis_generation: AtomicU64,
    is_public: bool,
    time_minutes: Option<u32>,
    increment_seconds: Option<u32>,
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
            clock: StdMutex::new(RoomClock::new(time_minutes)),
            outcome: StdMutex::new(None),
            clock_generation: AtomicU64::new(0),
            next_socket_id: AtomicU64::new(1),
            replay_store,
            next_replay_counter: AtomicU64::new(1),
            replay_saved: AtomicBool::new(false),
            last_activity_ms: AtomicU64::new(current_unix_ms()),
            analysis_generation: AtomicU64::new(0),
            is_public,
            time_minutes,
            increment_seconds,
        }
    }

    fn assign_or_find_seat(&self, owner: SeatOwner) -> Result<usize, String> {
        let mut seats = self.seats.lock().expect("room seats lock poisoned");
        if let Some(player) = seats.iter().position(|seat| {
            seat.as_ref()
                .map_or(false, |seat| seat.user_id == owner.user_id)
        }) {
            if owner.display_name.is_some() {
                seats[player] = Some(owner);
                self.touch();
            }
            return Ok(player);
        }
        if let Some(player) = seats.iter().position(Option::is_none) {
            seats[player] = Some(owner);
            self.touch();
            return Ok(player);
        }
        Err("Room is full".into())
    }

    fn register_socket(
        &self,
        user_id: &str,
        tx: mpsc::UnboundedSender<String>,
    ) -> Result<(u64, usize), String> {
        let player = self
            .seat_for_user(user_id)
            .ok_or_else(|| "You are not seated in this room".to_string())?;
        let socket_id = self.next_socket_id.fetch_add(1, Ordering::Relaxed);
        self.sockets
            .lock()
            .expect("room sockets lock poisoned")
            .insert(
                socket_id,
                MultiplayerSocket {
                    user_id: user_id.to_string(),
                    player,
                    tx,
                },
            );
        self.touch();
        Ok((socket_id, player))
    }

    fn unregister_socket(&self, socket_id: u64) {
        self.sockets
            .lock()
            .expect("room sockets lock poisoned")
            .remove(&socket_id);
        self.touch();
    }

    fn has_registered_sockets(&self) -> bool {
        !self
            .sockets
            .lock()
            .expect("room sockets lock poisoned")
            .is_empty()
    }

    fn seat_for_user(&self, user_id: &str) -> Option<usize> {
        self.seats
            .lock()
            .expect("room seats lock poisoned")
            .iter()
            .position(|seat| seat.as_ref().map_or(false, |seat| seat.user_id == user_id))
    }

    fn update_seat_display_name(&self, user_id: &str, display_name: Option<String>) -> bool {
        let mut seats = self.seats.lock().expect("room seats lock poisoned");
        let Some(player) = seats
            .iter()
            .position(|seat| seat.as_ref().map_or(false, |seat| seat.user_id == user_id))
        else {
            return false;
        };
        if let Some(seat) = seats[player].as_mut() {
            seat.display_name = display_name;
        }
        self.touch();
        true
    }

    fn is_full(&self) -> bool {
        self.seats
            .lock()
            .expect("room seats lock poisoned")
            .iter()
            .all(Option::is_some)
    }

    fn room_msg_for_player(&self, local_player: Option<usize>) -> ServerMsg {
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
                let connected = occupied[player].map_or(false, |user_id| {
                    sockets
                        .values()
                        .any(|socket| socket.user_id == user_id && socket.player == player)
                });
                MultiplayerPlayer {
                    occupied: occupied[player].is_some(),
                    connected,
                    you: local_player == Some(player),
                    name: seats[player]
                        .as_ref()
                        .and_then(|seat| seat.display_name.clone()),
                    time_millis: clock.time_millis[player],
                    clock_active: clock.active_player == Some(player),
                }
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
            local_player: local_player.map(|player| player as u8),
            players,
            time_minutes: self.time_minutes,
            increment_seconds: self.increment_seconds,
            winner: winner.map(|player| player as u8),
            finish_reason: outcome.as_ref().map(|outcome| outcome.reason.clone()),
            replay_share_slug: outcome.and_then(|outcome| outcome.replay_share_slug),
        }
    }

    fn lobby_entry(&self) -> MultiplayerLobbyRoom {
        let seats = self.seats.lock().expect("room seats lock poisoned");
        let sockets = self.sockets.lock().expect("room sockets lock poisoned");
        let occupied = [
            seats[0].as_ref().map(|seat| seat.user_id.as_str()),
            seats[1].as_ref().map(|seat| seat.user_id.as_str()),
        ];
        let occupied_count = occupied.iter().filter(|seat| seat.is_some()).count() as u8;
        let mut connected_count = 0;
        for (player, user_id) in occupied.iter().enumerate() {
            let Some(user_id) = user_id else {
                continue;
            };
            if sockets
                .values()
                .any(|socket| socket.user_id == *user_id && socket.player == player)
            {
                connected_count += 1;
            }
        }
        let status = if self.outcome_snapshot().is_some() {
            "finished"
        } else if occupied_count == 2 {
            "active"
        } else {
            "waiting"
        };
        MultiplayerLobbyRoom {
            code: self.code.clone(),
            status: status.into(),
            occupied: occupied_count,
            connected: connected_count,
            is_public: self.is_public,
            time_minutes: self.time_minutes,
            increment_seconds: self.increment_seconds,
            last_activity_ms: self.last_activity_ms.load(Ordering::Relaxed),
        }
    }

    fn left_room_msg() -> ServerMsg {
        ServerMsg::MultiplayerRoom {
            code: String::new(),
            status: "left".into(),
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
        let by_player = [
            serde_json::to_string(&self.room_msg_for_player(Some(0))).ok(),
            serde_json::to_string(&self.room_msg_for_player(Some(1))).ok(),
        ];
        self.broadcast_with(excluded_socket_id, |_room, player| {
            by_player[player].clone()
        });
    }

    fn broadcast_state_except(&self, session: &GameSession<G>, excluded_socket_id: Option<u64>) {
        let by_player = [
            serde_json::to_string(&session.state_msg_for_player(0)).ok(),
            serde_json::to_string(&session.state_msg_for_player(1)).ok(),
        ];
        self.broadcast_with(excluded_socket_id, |_room, player| {
            by_player[player].clone()
        });
    }

    fn broadcast_analysis_except(&self, root_wdl: [f32; 3], excluded_socket_id: Option<u64>) {
        let json = serde_json::to_string(&ServerMsg::MultiplayerAnalysis { root_wdl }).ok();
        self.broadcast_with(excluded_socket_id, |_room, _player| json.clone());
    }

    fn broadcast_with(
        &self,
        excluded_socket_id: Option<u64>,
        mut build: impl FnMut(&Self, usize) -> Option<String>,
    ) {
        let mut sockets = self.sockets.lock().expect("room sockets lock poisoned");
        sockets.retain(|socket_id, socket| {
            if excluded_socket_id == Some(*socket_id) {
                return true;
            }
            match build(self, socket.player) {
                Some(json) => socket.tx.send(json).is_ok(),
                None => true,
            }
        });
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

        let account_keys = {
            let seats = self.seats.lock().expect("room seats lock poisoned");
            seats
                .iter()
                .flatten()
                .map(|seat| seat.account_key.clone())
                .collect::<Vec<_>>()
        };
        let mut errors = Vec::new();
        let mut first_share_slug = None;
        for account_key in account_keys {
            let counter = self.next_replay_counter.fetch_add(1, Ordering::Relaxed);
            match store.save(&account_key, self.room_id, counter, &log).await {
                Ok(entry) => {
                    if first_share_slug.is_none() {
                        first_share_slug = Some(entry.share_slug);
                    }
                }
                Err(message) => errors.push(ServerMsg::Error { message }),
            }
        }
        if let Some(slug) = first_share_slug {
            self.set_replay_share_slug(slug);
        }
        errors
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
}

impl<G: Game + 'static> MultiplayerRoomStore<G> {
    fn new(factory: Arc<SessionFactory<G>>, replay_store: Option<Arc<ReplayStore>>) -> Self {
        Self {
            factory,
            rooms: StdMutex::new(HashMap::new()),
            lobby_sockets: StdMutex::new(HashMap::new()),
            next_room_id: AtomicU64::new(1),
            next_lobby_socket_id: AtomicU64::new(1),
            replay_store,
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

    fn lobby_msg(&self) -> ServerMsg {
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
        ServerMsg::MultiplayerLobby { rooms }
    }

    fn send_lobby_to(&self, tx: &mpsc::UnboundedSender<String>) {
        if let Ok(json) = serde_json::to_string(&self.lobby_msg()) {
            let _ = tx.send(json);
        }
    }

    fn broadcast_lobby(&self) {
        let Ok(json) = serde_json::to_string(&self.lobby_msg()) else {
            return;
        };
        let mut sockets = self
            .lobby_sockets
            .lock()
            .expect("lobby socket registry lock poisoned");
        sockets.retain(|_, tx| tx.send(json.clone()).is_ok());
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
        let owner = SeatOwner {
            user_id: user_id.to_string(),
            account_key: redacted_account_key(user_id),
            display_name: display_name.map(str::to_string),
        };
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
        self.join_room_with_display_name(user_id, None, code)
    }

    fn join_room_with_display_name(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        code: &str,
    ) -> Result<(Arc<MultiplayerRoom<G>>, usize), String> {
        let code = normalize_room_code(code)?;
        let room = self
            .rooms
            .lock()
            .expect("room store lock poisoned")
            .get(&code)
            .cloned()
            .ok_or_else(|| "Room not found".to_string())?;
        let owner = SeatOwner {
            user_id: user_id.to_string(),
            account_key: redacted_account_key(user_id),
            display_name: display_name.map(str::to_string),
        };
        let player = room.assign_or_find_seat(owner)?;
        Ok((room, player))
    }

    fn schedule_room_cleanup_if_empty(self: &Arc<Self>, room: Arc<MultiplayerRoom<G>>) -> bool {
        if room.has_registered_sockets() {
            return false;
        }
        let rooms = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(
                MULTIPLAYER_EMPTY_ROOM_GRACE_MS,
            ))
            .await;
            if rooms.close_room_if_still_empty_after_grace(&room) {
                rooms.broadcast_lobby();
            }
        });
        true
    }

    fn close_room_if_still_empty_after_grace(&self, room: &Arc<MultiplayerRoom<G>>) -> bool {
        let idle_ms =
            current_unix_ms().saturating_sub(room.last_activity_ms.load(Ordering::Relaxed));
        if idle_ms < MULTIPLAYER_EMPTY_ROOM_GRACE_MS {
            return false;
        }
        self.close_room_if_still_empty(room)
    }

    fn close_room_if_still_empty(&self, room: &Arc<MultiplayerRoom<G>>) -> bool {
        if room.has_registered_sockets() {
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
    let Some(store) = user_session.replay_store.as_ref() else {
        return Ok(None);
    };
    let counter = user_session
        .next_replay_counter
        .fetch_add(1, Ordering::Relaxed);
    store
        .save(
            &user_session.account_key,
            user_session.session_id,
            counter,
            log,
        )
        .await
        .map(Some)
}

async fn save_current_replay_once<G: Game + 'static>(
    user_session: &UserSession<G>,
    session: &mut GameSession<G>,
) -> Option<ServerMsg> {
    let has_store = user_session.replay_store.is_some();
    let log = session.export_unsaved_current_log()?;
    match save_replay_entry(user_session, &log).await {
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
        if let Ok(log) = store.load(&user_session.account_key, &entry.id).await {
            entry.result = replay_result_for_log(user_session.factory.as_ref(), &log);
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
    explicit_users: HashSet<String>,
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
        ServerMsg::Profile {
            username: registry.by_user.get(user_id).cloned(),
            username_set: registry.explicit_users.contains(user_id),
        }
    }

    fn ensure_username(&self, user_id: &str, username: Option<&str>) -> String {
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
                let default_username = default_profile_username(user_id, &registry);
                registry.by_name.insert(key, user_id.to_string());
                registry
                    .by_user
                    .insert(user_id.to_string(), username.clone());
                if username != default_username {
                    registry.explicit_users.insert(user_id.to_string());
                }
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

    fn set_username(&self, user_id: &str, username: &str) -> Result<String, String> {
        let username = normalize_profile_username(Some(username)).ok_or_else(|| {
            "Username must be 3-24 letters, numbers, dashes, or underscores".to_string()
        })?;
        let key = username_key(&username);
        let mut registry = self
            .usernames
            .lock()
            .expect("username registry lock poisoned");
        if registry
            .by_name
            .get(&key)
            .is_some_and(|owner| owner != user_id)
        {
            return Err("Username is already taken".into());
        }
        if let Some(old_username) = registry
            .by_user
            .insert(user_id.to_string(), username.clone())
        {
            let old_key = username_key(&old_username);
            if old_key != key {
                registry.by_name.remove(&old_key);
            }
        }
        registry.by_name.insert(key, user_id.to_string());
        registry.explicit_users.insert(user_id.to_string());
        Ok(username)
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
    player: usize,
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

async fn replay_link_redirect(Path(slug): Path<String>) -> Redirect {
    if safe_share_slug(&slug) {
        Redirect::temporary(&format!("/?replay={slug}"))
    } else {
        Redirect::temporary("/")
    }
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
    let rooms = Arc::new(MultiplayerRoomStore::new(
        Arc::clone(&store.factory),
        replay_store,
    ));
    println!("Anonymous HexFish WebSocket sessions enabled");
    tracing::info!("anonymous HexFish WebSocket sessions enabled");

    let app = Router::new()
        .route(
            "/ws",
            axum::routing::get({
                let store = Arc::clone(&store);
                move |ws: WebSocketUpgrade| {
                    let store = Arc::clone(&store);
                    let rooms = Arc::clone(&rooms);
                    async move { ws.on_upgrade(move |socket| handle_socket(socket, store, rooms)) }
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
    let rooms = Arc::new(MultiplayerRoomStore::new(Arc::clone(&store.factory), None));

    let app = Router::new()
        .route(
            "/ws",
            axum::routing::get({
                let store = Arc::clone(&store);
                move |ws: WebSocketUpgrade| {
                    let store = Arc::clone(&store);
                    let rooms = Arc::clone(&rooms);
                    async move { ws.on_upgrade(move |socket| handle_socket(socket, store, rooms)) }
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
    let rooms = Arc::new(MultiplayerRoomStore::new(Arc::clone(&store.factory), None));

    let app = Router::new()
        .route(
            "/ws",
            axum::routing::get({
                let store = Arc::clone(&store);
                move |ws: WebSocketUpgrade| {
                    let store = Arc::clone(&store);
                    let rooms = Arc::clone(&rooms);
                    async move { ws.on_upgrade(move |socket| handle_socket(socket, store, rooms)) }
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
            let mut last_progress = 0;
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
                    if snap.fresh_simulations >= last_progress + PROGRESS_INTERVAL {
                        last_progress = snap.fresh_simulations;
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
                if snap.fresh_simulations > last_progress {
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
    rooms: Arc<MultiplayerRoomStore<G>>,
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
    store.ensure_username(&user_id, display_name.as_deref());
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

    let _ = handle_authenticated_socket(
        &mut socket,
        Arc::clone(&store),
        Arc::clone(&user_session),
        rooms,
        user_id.clone(),
        display_name,
        socket_id,
        outbound_tx,
        outbound_rx,
    )
    .await;
    user_session.unregister_socket(socket_id);
    tracing::info!(
        account = %account_key,
        scope = session_scope,
        session_id = user_session.session_id,
        seed = user_session.seed,
        board_code = %board_code,
        socket_id,
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
    rooms: Arc<MultiplayerRoomStore<G>>,
    user_id: String,
    display_name: Option<String>,
    socket_id: u64,
    outbound_tx: mpsc::UnboundedSender<String>,
    mut outbound_rx: mpsc::UnboundedReceiver<String>,
) -> Result<(), ()> {
    let mut auto_search: Option<SearchBudget> = None;
    let mut active_room: Option<ActiveMultiplayerRoom<G>> = None;

    // Send initial state.
    {
        let mut session = user_session.session.lock().await;
        let init_msgs = session.handle(ClientMsg::GetState);
        for msg in init_msgs {
            send_msg(socket, &msg).await?;
        }
    }
    send_msg(socket, &store.profile_msg(&user_id)).await?;

    let lobby_socket_id = rooms.register_lobby_socket(outbound_tx.clone());
    rooms.send_lobby_to(&outbound_tx);

    let result = loop {
        tokio::select! {
            outbound = outbound_rx.recv() => {
                let Some(json) = outbound else {
                    break Ok(());
                };
                if send_raw_msg(socket, json).await.is_err() {
                    break Err(());
                }
            }
            inbound = socket.recv() => {
                let Some(Ok(ws_msg)) = inbound else {
                    break Ok(());
                };
                if handle_authenticated_message(
                    socket,
                    &store,
                    &user_session,
                    &rooms,
                    &user_id,
                    display_name.as_deref(),
                    socket_id,
                    &outbound_tx,
                    &mut active_room,
                    &mut auto_search,
                    ws_msg,
                )
                .await
                .is_err() {
                    break Err(());
                }
            }
        }
    };

    let detached = detach_active_room(&rooms, &mut active_room);
    rooms.unregister_lobby_socket(lobby_socket_id);
    if detached {
        rooms.broadcast_lobby();
    }
    result
}

async fn handle_authenticated_message<G: Game + 'static>(
    socket: &mut WebSocket,
    store: &Arc<UserSessionStore<G>>,
    user_session: &Arc<UserSession<G>>,
    rooms: &Arc<MultiplayerRoomStore<G>>,
    user_id: &str,
    display_name: Option<&str>,
    socket_id: u64,
    outbound_tx: &mpsc::UnboundedSender<String>,
    active_room: &mut Option<ActiveMultiplayerRoom<G>>,
    auto_search: &mut Option<SearchBudget>,
    ws_msg: Message,
) -> Result<(), ()> {
    let text = match ws_msg {
        Message::Text(t) => t,
        Message::Close(_) => return Err(()),
        _ => return Ok(()),
    };

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

    if matches!(
        &client_msg,
        ClientMsg::GetProfile | ClientMsg::SetUsername { .. }
    ) {
        let responses = handle_profile_message(store, user_id, active_room, client_msg);
        for msg in responses {
            send_msg(socket, &msg).await?;
        }
        return Ok(());
    }

    if is_multiplayer_msg(&client_msg) {
        let profile_display_name = store
            .current_username(user_id)
            .or_else(|| display_name.map(str::to_string));
        let responses = handle_multiplayer_message_for_user(
            rooms,
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
            | ClientMsg::JoinMultiplayerRoom { .. }
            | ClientMsg::LeaveMultiplayerRoom
            | ClientMsg::PlayMultiplayerAction { .. }
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
    active_room: &Option<ActiveMultiplayerRoom<G>>,
    msg: ClientMsg,
) -> Vec<ServerMsg> {
    match msg {
        ClientMsg::GetProfile => vec![store.profile_msg(user_id)],
        ClientMsg::SetUsername { username } => {
            let username = match store.set_username(user_id, &username) {
                Ok(username) => username,
                Err(message) => return vec![ServerMsg::Error { message }],
            };
            let mut responses = vec![ServerMsg::Profile {
                username: Some(username.clone()),
                username_set: true,
            }];
            if let Some(active) = active_room.as_ref() {
                if active
                    .room
                    .update_seat_display_name(user_id, Some(username.clone()))
                {
                    active
                        .room
                        .broadcast_room_info_except(Some(active.socket_id));
                    responses.push(active.room.room_msg_for_player(Some(active.player)));
                }
            }
            responses
        }
        _ => vec![ServerMsg::Error {
            message: "Unsupported profile message".into(),
        }],
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
    match msg {
        ClientMsg::ListMultiplayerRooms => vec![rooms.lobby_msg()],
        ClientMsg::CreateMultiplayerRoom {
            preferred_player,
            code,
            is_public,
            time_minutes,
            increment_seconds,
        } => {
            if detach_active_room(rooms, active_room) {
                rooms.broadcast_lobby();
            }
            let (room, _) = match rooms.create_room_with_display_name(
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
            let responses =
                activate_multiplayer_room(room, user_id, outbound_tx, active_room).await;
            rooms.broadcast_lobby();
            responses
        }
        ClientMsg::JoinMultiplayerRoom { code } => {
            if detach_active_room(rooms, active_room) {
                rooms.broadcast_lobby();
            }
            let (room, _) = match rooms.join_room_with_display_name(user_id, display_name, &code) {
                Ok(room) => room,
                Err(message) => return vec![ServerMsg::Error { message }],
            };
            let responses =
                activate_multiplayer_room(room, user_id, outbound_tx, active_room).await;
            rooms.broadcast_lobby();
            responses
        }
        ClientMsg::LeaveMultiplayerRoom => {
            if detach_active_room(rooms, active_room) {
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
            let room = Arc::clone(&active.room);
            let socket_id = active.socket_id;
            let player = active.player;
            if !room.is_full() {
                return vec![ServerMsg::Error {
                    message: "Room is waiting for an opponent".into(),
                }];
            }
            if room.is_finished() {
                let _ = save_finished_multiplayer_replay(Arc::clone(&room)).await;
                room.broadcast_room_info_except(Some(socket_id));
                return vec![room.room_msg_for_player(Some(player))];
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
                    room.room_msg_for_player(Some(player)),
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
            schedule_multiplayer_clock_timeout(Arc::clone(&room));
            schedule_multiplayer_analysis(room, analysis_generation, analysis_session);
            responses
        }
        ClientMsg::ResignMultiplayerGame => {
            let Some(active) = active_room.as_ref() else {
                return vec![ServerMsg::Error {
                    message: "Join a multiplayer room before resigning".into(),
                }];
            };
            let room = Arc::clone(&active.room);
            if !room.is_full() {
                return vec![ServerMsg::Error {
                    message: "Room is waiting for an opponent".into(),
                }];
            }
            if !room.is_finished() {
                room.mark_finished(Some(1 - active.player), "resignation");
            }
            let save_errors = save_finished_multiplayer_replay(Arc::clone(&room)).await;
            room.broadcast_room_info_except(Some(active.socket_id));
            schedule_lobby_broadcast(Arc::clone(rooms));
            let mut responses = vec![room.room_msg_for_player(Some(active.player))];
            responses.extend(save_errors);
            responses
        }
        ClientMsg::AddMultiplayerOpponentTime => {
            let Some(active) = active_room.as_ref() else {
                return vec![ServerMsg::Error {
                    message: "Join a multiplayer room before adjusting clocks".into(),
                }];
            };
            let room = Arc::clone(&active.room);
            let opponent = if active.player == 0 { 1 } else { 0 };
            if !room.add_clock_seconds(opponent, 15) {
                if room.is_finished() {
                    let _ = save_finished_multiplayer_replay(Arc::clone(&room)).await;
                    room.broadcast_room_info_except(Some(active.socket_id));
                    return vec![room.room_msg_for_player(Some(active.player))];
                }
                return vec![ServerMsg::Error {
                    message: "This room does not have clocks enabled".into(),
                }];
            }
            room.broadcast_room_info_except(Some(active.socket_id));
            schedule_multiplayer_clock_timeout(Arc::clone(&room));
            vec![room.room_msg_for_player(Some(active.player))]
        }
        _ => vec![ServerMsg::Error {
            message: "Unsupported multiplayer message".into(),
        }],
    }
}

async fn activate_multiplayer_room<G: Game + 'static>(
    room: Arc<MultiplayerRoom<G>>,
    user_id: &str,
    outbound_tx: &mpsc::UnboundedSender<String>,
    active_room: &mut Option<ActiveMultiplayerRoom<G>>,
) -> Vec<ServerMsg> {
    let (socket_id, player) = match room.register_socket(user_id, outbound_tx.clone()) {
        Ok(active) => active,
        Err(message) => return vec![ServerMsg::Error { message }],
    };
    *active_room = Some(ActiveMultiplayerRoom {
        room: Arc::clone(&room),
        socket_id,
        player,
    });
    room.broadcast_room_info_except(Some(socket_id));
    let (responses, analysis_job) = {
        let session = room.session.lock().await;
        let responses = vec![
            room.room_msg_for_player(Some(player)),
            session.state_msg_for_player(player),
        ];
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
    schedule_multiplayer_clock_timeout(Arc::clone(&room));
    responses
}

fn detach_active_room<G: Game + 'static>(
    rooms: &Arc<MultiplayerRoomStore<G>>,
    active_room: &mut Option<ActiveMultiplayerRoom<G>>,
) -> bool {
    if let Some(active) = active_room.take() {
        active.room.unregister_socket(active.socket_id);
        if !rooms.schedule_room_cleanup_if_empty(Arc::clone(&active.room)) {
            active
                .room
                .broadcast_room_info_except(Some(active.socket_id));
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

fn schedule_multiplayer_clock_timeout<G: Game + 'static>(room: Arc<MultiplayerRoom<G>>) {
    let Some((generation, remaining_ms)) = room.clock_timeout_schedule() else {
        return;
    };
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(remaining_ms)).await;
        if room.mark_clock_timeout_if_current(generation) {
            let _ = save_finished_multiplayer_replay(Arc::clone(&room)).await;
            room.broadcast_room_info_except(None);
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
                Ok(log) => {
                    let mut replay_session = user_session.factory.create_session();
                    match replay_session.load_saved_replay_log(id.clone(), &log) {
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
                    match replay_session.load_saved_replay_log(loaded.id.clone(), &loaded.log) {
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
        ActiveMultiplayerRoom, ClientMsg, FileReplayStore, GamePresenter, MultiplayerRoomStore,
        ReplayStore, RoomClock, SearchBudget, ServerMsg, SessionFactory, UserSessionStore,
        ViewTarget, anonymous_user_id_from_id, anonymous_user_id_from_token, client_msg_target,
        current_unix_ms, detach_active_room, handle_multiplayer_message, parse_env_bool,
        redacted_account_key, safe_replay_id, safe_share_slug, socket_session_key_from_auth,
    };

    #[derive(Clone)]
    struct TestGame {
        board_id: u64,
        moves: u8,
    }

    impl Game for TestGame {
        const NUM_ACTIONS: usize = 1;

        fn status(&self) -> Status {
            Status::Decision(1.0)
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

    fn temp_replay_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hexfish-{name}-{}-{}",
            current_unix_ms(),
            fastrand::u64(..)
        ))
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
    fn users_receive_unique_default_usernames() {
        let (store, _) = test_store();

        let first = store.ensure_username("user_a", None);
        let second = store.ensure_username("user_b", None);
        let first_again = store.ensure_username("user_a", Some("Alice"));

        assert!(first.starts_with("User"));
        assert!(second.starts_with("User"));
        assert_ne!(first, second);
        assert_eq!(first_again, first);
    }

    #[test]
    fn profile_reports_whether_username_was_explicitly_set() {
        let (store, _) = test_store();

        let default_username = store.ensure_username("user_a", None);
        match store.profile_msg("user_a") {
            ServerMsg::Profile {
                username,
                username_set,
            } => {
                assert_eq!(username.as_deref(), Some(default_username.as_str()));
                assert!(!username_set);
            }
            other => panic!("unexpected profile message: {other:?}"),
        }

        store.set_username("user_a", "Alice").unwrap();
        match store.profile_msg("user_a") {
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
    fn restored_default_username_is_not_treated_as_explicit() {
        let (first_store, _) = test_store();
        let default_username = first_store.ensure_username("user_a", None);
        let (restored_store, _) = test_store();

        assert_eq!(
            restored_store.ensure_username("user_a", Some(&default_username)),
            default_username
        );
        match restored_store.profile_msg("user_a") {
            ServerMsg::Profile { username_set, .. } => assert!(!username_set),
            other => panic!("unexpected profile message: {other:?}"),
        }
    }

    #[test]
    fn username_restore_collision_falls_back_to_default() {
        let (store, _) = test_store();

        let first = store.ensure_username("user_a", Some("Alice"));
        let second = store.ensure_username("user_b", Some("Alice"));

        assert_eq!(first, "Alice");
        assert!(second.starts_with("User"));
        assert_ne!(second, first);
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
    fn multiplayer_room_stays_rejoinable_after_all_players_disconnect() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let rooms = Arc::new(rooms);
            let (room, player_a) = rooms
                .create_room("user_a", Some(0), None, None, None, None)
                .unwrap();
            let cleanup_room = Arc::clone(&room);
            let code = room.code.clone();
            let (_, player_b) = rooms.join_room("user_b", &code).unwrap();
            let (tx_a, _rx_a) = mpsc::unbounded_channel();
            let (tx_b, _rx_b) = mpsc::unbounded_channel();
            let (socket_a, socket_player_a) = room.register_socket("user_a", tx_a).unwrap();
            let (socket_b, socket_player_b) = room.register_socket("user_b", tx_b).unwrap();
            assert_eq!(socket_player_a, player_a);
            assert_eq!(socket_player_b, player_b);

            let mut active_a = Some(ActiveMultiplayerRoom {
                room: Arc::clone(&room),
                socket_id: socket_a,
                player: player_a,
            });
            let mut active_b = Some(ActiveMultiplayerRoom {
                room: Arc::clone(&room),
                socket_id: socket_b,
                player: player_b,
            });

            assert!(detach_active_room(&rooms, &mut active_a));
            match rooms.lobby_msg() {
                ServerMsg::MultiplayerLobby { rooms } => {
                    assert_eq!(rooms.len(), 1);
                    assert_eq!(rooms[0].code, code);
                    assert_eq!(rooms[0].connected, 1);
                }
                other => panic!("expected lobby message, got {other:?}"),
            }

            assert!(detach_active_room(&rooms, &mut active_b));
            match rooms.lobby_msg() {
                ServerMsg::MultiplayerLobby { rooms } => {
                    assert_eq!(rooms.len(), 1);
                    assert_eq!(rooms[0].code, code);
                    assert_eq!(rooms[0].connected, 0);
                }
                other => panic!("expected lobby message, got {other:?}"),
            }
            let (rejoined_room, rejoined_player) = rooms.join_room("user_a", &code).unwrap();
            assert!(Arc::ptr_eq(&room, &rejoined_room));
            assert_eq!(rejoined_player, player_a);

            let (tx_c, _rx_c) = mpsc::unbounded_channel();
            let (socket_c, socket_player_c) =
                rejoined_room.register_socket("user_a", tx_c).unwrap();
            assert_eq!(socket_player_c, player_a);
            assert!(!rooms.close_room_if_still_empty(&cleanup_room));
            rejoined_room.unregister_socket(socket_c);
            assert!(rooms.close_room_if_still_empty(&cleanup_room));
            match rooms.join_room("user_a", &code) {
                Ok(_) => panic!("expected closed room to reject join"),
                Err(err) => assert!(err.contains("not found")),
            }
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
            let (socket_id, socket_player) = room.register_socket("user_a", tx.clone()).unwrap();
            assert_eq!(socket_player, player_a);
            let mut active_room = Some(ActiveMultiplayerRoom {
                room,
                socket_id,
                player: player_a,
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
            let (room, _) = rooms
                .create_room("user_a", Some(0), None, None, None, None)
                .unwrap();
            let (tx, _rx) = mpsc::unbounded_channel();
            let (socket_id, player) = room.register_socket("user_a", tx.clone()).unwrap();
            let mut active_room = Some(ActiveMultiplayerRoom {
                room,
                socket_id,
                player,
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
    fn multiplayer_room_rejects_wrong_player_and_broadcasts_by_perspective() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (rooms, _) = test_room_store();
            let (room, _) = rooms
                .create_room("user_a", Some(0), None, None, None, None)
                .unwrap();
            rooms.join_room("user_b", &room.code).unwrap();
            let (tx_a, mut rx_a) = mpsc::unbounded_channel();
            let (tx_b, mut rx_b) = mpsc::unbounded_channel();
            room.register_socket("user_a", tx_a).unwrap();
            room.register_socket("user_b", tx_b).unwrap();

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
            let (room, _) = rooms
                .create_room("user_a", Some(0), None, None, None, None)
                .unwrap();
            rooms.join_room("user_b", &room.code).unwrap();
            let (tx_a, mut rx_a) = mpsc::unbounded_channel();
            let (tx_b, mut rx_b) = mpsc::unbounded_channel();
            let (socket_id, player) = room.register_socket("user_a", tx_a.clone()).unwrap();
            room.register_socket("user_b", tx_b).unwrap();
            let mut active_room = Some(ActiveMultiplayerRoom {
                room,
                socket_id,
                player,
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

        let saved_a = store.save("account_a", 11, 1, &log).unwrap();
        store.save("account_b", 22, 1, &log).unwrap();

        let entries_a = store.list("account_a").unwrap();
        let entries_b = store.list("account_b").unwrap();

        assert_eq!(entries_a.len(), 1);
        assert_eq!(entries_a[0].id, saved_a.id);
        assert_eq!(entries_a[0].action_count, 2);
        assert!(safe_share_slug(&entries_a[0].share_slug));
        assert_eq!(entries_b.len(), 1);
        assert_ne!(entries_a[0].id, entries_b[0].id);

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

        let saved = store.save("account_a", 11, 1, &log).unwrap();
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
                .save(&account, 11, 1, &log)
                .await
                .expect("save replay");
            assert!(safe_share_slug(&saved.share_slug));
            assert!(store.list(&other_account).await.unwrap().is_empty());

            let entries = store.list(&account).await.expect("list owner replays");
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, saved.id);
            assert_eq!(entries[0].action_count, 3);
            assert!(!entries[0].favorite);

            let loaded = store
                .load(&account, &saved.id)
                .await
                .expect("load owner replay");
            assert_eq!(loaded.initial_state, log.initial_state);
            assert_eq!(loaded.actions, log.actions);

            let shared = store
                .load_shared(&saved.share_slug)
                .await
                .expect("load shared replay");
            assert_eq!(shared.id, saved.id);
            assert_eq!(shared.log.actions, log.actions);

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

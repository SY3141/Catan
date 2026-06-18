mod auth;
mod protocol;
mod session;
mod traits;

pub use protocol::{ClientMsg, SearchBudget, ServerMsg};
pub use session::GameSession;
pub use traits::GamePresenter;

use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
};
use futures_util::FutureExt;
use tokio::sync::{Mutex, mpsc};
use tower_http::services::ServeDir;

use crate::eval::Evaluator;
use crate::game::Game;
use crate::game_log::GameLog;
use crate::mcts::Config;
use protocol::{ReplayEntry, ViewTarget};

pub use auth::ClerkAuth;

/// Send a progress snapshot every N simulations.
const PROGRESS_INTERVAL: u32 = 100;
const SEARCH_INTERRUPT_INTERVAL: u32 = 8;
const BOARD_FINGERPRINT_RETRIES: usize = 64;

#[derive(Clone)]
struct ReplayStore {
    root: PathBuf,
}

impl ReplayStore {
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
        let id = format!("{saved_at_ms}-{session_id}-{counter}.log");
        let dir = self.user_dir(account_key);
        std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create replay dir: {e}"))?;
        let path = dir.join(&id);
        log.write_result(&path)
            .map_err(|e| format!("failed to write replay log: {e}"))?;
        Ok(ReplayEntry {
            id,
            saved_at_ms,
            action_count: log.actions.len(),
            favorite: false,
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
                favorite: self.is_favorite(account_key, id),
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

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
        match &self.template {
            SessionTemplate::New { replay } => {
                let mut session = GameSession::new(
                    Arc::clone(&self.evaluator),
                    self.eval_name.clone(),
                    Arc::clone(&self.presenter),
                    self.human_players,
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
                self.human_players,
                Config::default(),
            ),
            SessionTemplate::Timeline(timeline) => {
                let mut session = GameSession::with_state(
                    timeline[0].1.clone(),
                    Arc::clone(&self.evaluator),
                    self.eval_name.clone(),
                    Arc::clone(&self.presenter),
                    self.human_players,
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

    fn register_socket(&self) -> (u64, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.next_socket_id.fetch_add(1, Ordering::Relaxed);
        self.sockets
            .lock()
            .expect("socket registry lock poisoned")
            .insert(id, tx);
        (id, rx)
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

fn save_replay_log<G: Game + 'static>(
    user_session: &UserSession<G>,
    log: &GameLog,
) -> Option<ServerMsg> {
    let store = user_session.replay_store.as_ref()?;
    let counter = user_session
        .next_replay_counter
        .fetch_add(1, Ordering::Relaxed);
    match store.save(
        &user_session.account_key,
        user_session.session_id,
        counter,
        log,
    ) {
        Ok(_) => None,
        Err(message) => Some(ServerMsg::Error { message }),
    }
}

fn save_current_replay_once<G: Game + 'static>(
    user_session: &UserSession<G>,
    session: &mut GameSession<G>,
) -> Option<ServerMsg> {
    let has_store = user_session.replay_store.is_some();
    let log = session.export_unsaved_current_log()?;
    let error = save_replay_log(user_session, &log);
    if has_store && error.is_none() {
        session.mark_current_log_saved(&log);
    }
    error
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
    next_session_id: AtomicU64,
    replay_store: Option<Arc<ReplayStore>>,
}

impl<G: Game + 'static> UserSessionStore<G> {
    fn new(factory: SessionFactory<G>, replay_store: Option<Arc<ReplayStore>>) -> Self {
        Self {
            factory: Arc::new(factory),
            sessions: StdMutex::new(HashMap::new()),
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
}

fn board_fingerprint_in_use<G: Game + 'static>(
    sessions: &HashMap<String, Arc<UserSession<G>>>,
    fingerprint: u64,
) -> bool {
    sessions
        .values()
        .any(|session| session.board_fingerprint == Some(fingerprint))
}

#[derive(Clone)]
enum SocketAuth {
    Clerk(Arc<ClerkAuth>),
    Anonymous,
}

struct SocketSessionKey {
    user_id: String,
    ephemeral: bool,
    scope: &'static str,
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

fn socket_auth_from_env() -> SocketAuth {
    match ClerkAuth::optional_from_env() {
        Ok(Some(auth)) => SocketAuth::Clerk(Arc::new(auth)),
        Ok(None) => SocketAuth::Anonymous,
        Err(e) => panic!("Clerk authentication is misconfigured: {e}"),
    }
}

fn log_socket_auth_mode(auth: &SocketAuth) {
    match auth {
        SocketAuth::Clerk(_) => {
            println!("Per-account HexFish sessions enabled");
            tracing::info!("per-account HexFish sessions enabled");
        }
        SocketAuth::Anonymous => {
            println!("Anonymous HexFish sessions enabled (CLERK_JWT_KEY not set)");
            tracing::warn!(
                "CLERK_JWT_KEY not set; browser reconnects reuse a local anonymous session token"
            );
        }
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
    let replay_store = web_log_dir.map(|dir| Arc::new(ReplayStore::new(dir)));
    let store = Arc::new(UserSessionStore::new(
        SessionFactory::new_game(evaluator, eval_name, presenter, human_players, replay),
        replay_store,
    ));
    let auth = Arc::new(socket_auth_from_env());
    log_socket_auth_mode(auth.as_ref());

    let app = Router::new()
        .route(
            "/ws",
            axum::routing::get({
                let store = Arc::clone(&store);
                let auth = Arc::clone(&auth);
                move |ws: WebSocketUpgrade| {
                    let store = Arc::clone(&store);
                    let auth = Arc::clone(&auth);
                    async move { ws.on_upgrade(move |socket| handle_socket(socket, store, auth)) }
                }
            }),
        )
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
    let auth = Arc::new(socket_auth_from_env());

    let app = Router::new()
        .route(
            "/ws",
            axum::routing::get({
                let store = Arc::clone(&store);
                let auth = Arc::clone(&auth);
                move |ws: WebSocketUpgrade| {
                    let store = Arc::clone(&store);
                    let auth = Arc::clone(&auth);
                    async move { ws.on_upgrade(move |socket| handle_socket(socket, store, auth)) }
                }
            }),
        )
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
    let auth = Arc::new(socket_auth_from_env());

    let app = Router::new()
        .route(
            "/ws",
            axum::routing::get({
                let store = Arc::clone(&store);
                let auth = Arc::clone(&auth);
                move |ws: WebSocketUpgrade| {
                    let store = Arc::clone(&store);
                    let auth = Arc::clone(&auth);
                    async move { ws.on_upgrade(move |socket| handle_socket(socket, store, auth)) }
                }
            }),
        )
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
    auth: Arc<SocketAuth>,
) {
    let session_key = match authenticate_socket(&mut socket, auth.as_ref()).await {
        Ok(session_key) => session_key,
        Err(()) => return,
    };
    let user_id = session_key.user_id;
    let session_scope = session_key.scope;
    let account_key = redacted_account_key(&user_id);
    let (user_session, created) = store.get_or_create(&user_id);
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
    let (socket_id, outbound_rx) = user_session.register_socket();

    let _ = handle_authenticated_socket(
        &mut socket,
        Arc::clone(&user_session),
        socket_id,
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

async fn authenticate_socket(
    socket: &mut WebSocket,
    auth: &SocketAuth,
) -> Result<SocketSessionKey, ()> {
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
    } = client_msg
    else {
        send_unauthorized(socket).await;
        return Err(());
    };

    match auth {
        SocketAuth::Clerk(auth) => match auth.verify_user_id(&token) {
            Ok(user_id) => Ok(SocketSessionKey {
                user_id,
                ephemeral: false,
                scope: "clerk",
            }),
            Err(_) => {
                send_unauthorized(socket).await;
                Err(())
            }
        },
        SocketAuth::Anonymous => {
            if let Some(user_id) = anonymous_session
                .as_deref()
                .and_then(anonymous_user_id_from_id)
            {
                Ok(SocketSessionKey {
                    user_id,
                    ephemeral: false,
                    scope: "anonymous",
                })
            } else if let Some(user_id) = anonymous_user_id_from_token(&token) {
                Ok(SocketSessionKey {
                    user_id,
                    ephemeral: false,
                    scope: "anonymous",
                })
            } else {
                Ok(SocketSessionKey {
                    user_id: format!("anonymous:{}", fastrand::u64(..)),
                    ephemeral: true,
                    scope: "anonymous-ephemeral",
                })
            }
        }
    }
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
    user_session: Arc<UserSession<G>>,
    socket_id: u64,
    mut outbound_rx: mpsc::UnboundedReceiver<String>,
) -> Result<(), ()> {
    let mut auto_search: Option<SearchBudget> = None;

    // Send initial state.
    {
        let mut session = user_session.session.lock().await;
        let init_msgs = session.handle(ClientMsg::GetState);
        for msg in init_msgs {
            send_msg(socket, &msg).await?;
        }
    }

    loop {
        tokio::select! {
            outbound = outbound_rx.recv() => {
                let Some(json) = outbound else {
                    return Ok(());
                };
                send_raw_msg(socket, json).await?;
            }
            inbound = socket.recv() => {
                let Some(Ok(ws_msg)) = inbound else {
                    return Ok(());
                };
                handle_authenticated_message(
                    socket,
                    &user_session,
                    socket_id,
                    &mut auto_search,
                    ws_msg,
                )
                .await?;
            }
        }
    }
}

async fn handle_authenticated_message<G: Game + 'static>(
    socket: &mut WebSocket,
    user_session: &Arc<UserSession<G>>,
    socket_id: u64,
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
                Some(store) => match store.list(&user_session.account_key) {
                    Ok(entries) => vec![ServerMsg::ReplayList { entries }],
                    Err(message) => vec![ServerMsg::Error { message }],
                },
                None => vec![ServerMsg::Error {
                    message: "Replay storage is not configured".into(),
                }],
            },
            ClientMsg::DeleteReplay { id } => match &user_session.replay_store {
                Some(store) => match store
                    .delete(&user_session.account_key, &id)
                    .and_then(|()| store.list(&user_session.account_key))
                {
                    Ok(entries) => vec![ServerMsg::ReplayList { entries }],
                    Err(message) => vec![ServerMsg::Error { message }],
                },
                None => vec![ServerMsg::Error {
                    message: "Replay storage is not configured".into(),
                }],
            },
            ClientMsg::SetReplayFavorite { id, favorite } => match &user_session.replay_store {
                Some(store) => match store
                    .set_favorite(&user_session.account_key, &id, favorite)
                    .and_then(|()| store.list(&user_session.account_key))
                {
                    Ok(entries) => vec![ServerMsg::ReplayList { entries }],
                    Err(message) => vec![ServerMsg::Error { message }],
                },
                None => vec![ServerMsg::Error {
                    message: "Replay storage is not configured".into(),
                }],
            },
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
                        if let Some(error) = save_replay_log(user_session, &log) {
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
            if let Some(error) = save_current_replay_once(user_session, &mut session) {
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

fn client_msg_target(msg: &ClientMsg) -> ViewTarget {
    match msg {
        ClientMsg::LoadReplay { .. } | ClientMsg::SetReplayCursor { .. } => ViewTarget::Replay,
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
            Some(store) => match store.load(&user_session.account_key, &id) {
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
    };

    use crate::{
        eval::{Evaluation, Evaluator},
        game::{Game, Status},
        game_log::GameLog,
    };

    use super::{
        GamePresenter, ReplayStore, SearchBudget, SessionFactory, UserSessionStore, ViewTarget,
        anonymous_user_id_from_id, anonymous_user_id_from_token, client_msg_target,
        current_unix_ms, safe_replay_id,
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

        fn action_label(&self, _state: &TestGame, action: usize) -> String {
            format!("Action {action}")
        }

        fn phase_label(&self, _state: &TestGame) -> String {
            "test".into()
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

    fn temp_replay_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "hexfish-{name}-{}-{}",
            current_unix_ms(),
            fastrand::u64(..)
        ))
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
    fn replay_store_lists_only_the_current_account() {
        let dir = temp_replay_dir("replay-scope");
        let store = ReplayStore::new(dir.clone());
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
        assert_eq!(entries_b.len(), 1);
        assert_ne!(entries_a[0].id, entries_b[0].id);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn replay_store_favorites_and_deletes_logs() {
        let dir = temp_replay_dir("replay-favorite-delete");
        let store = ReplayStore::new(dir.clone());
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
    fn replay_store_rejects_unsafe_ids() {
        assert!(!safe_replay_id("../game.log"));
        assert!(!safe_replay_id("..game.log"));
        assert!(!safe_replay_id(".hidden.log"));
        assert!(!safe_replay_id("nested/game.log"));
        assert!(!safe_replay_id("nested\\game.log"));
        assert!(!safe_replay_id("game.txt"));
        assert!(safe_replay_id("123-1-1.log"));

        let store = ReplayStore::new(temp_replay_dir("unsafe-replay-id"));
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

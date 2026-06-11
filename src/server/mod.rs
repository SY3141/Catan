mod auth;
mod protocol;
mod session;
mod traits;

pub use protocol::{ClientMsg, ServerMsg};
pub use session::GameSession;
pub use traits::GamePresenter;

use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
};
use tokio::sync::{Mutex, mpsc};
use tower_http::services::ServeDir;

use crate::eval::Evaluator;
use crate::game::Game;
use crate::game_log::GameLog;
use crate::mcts::Config;

pub use auth::ClerkAuth;

/// Send a progress snapshot every N simulations.
const PROGRESS_INTERVAL: u32 = 100;
const BOARD_FINGERPRINT_RETRIES: usize = 64;

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
    session: Arc<Mutex<GameSession<G>>>,
    sockets: StdMutex<HashMap<u64, mpsc::UnboundedSender<String>>>,
    next_socket_id: AtomicU64,
}

impl<G: Game + 'static> UserSession<G> {
    fn new(session: GameSession<G>, session_id: u64) -> Self {
        let seed = session.seed();
        let board_fingerprint = session.board_fingerprint();
        Self {
            session_id,
            seed,
            board_fingerprint,
            session: Arc::new(Mutex::new(session)),
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

struct UserSessionStore<G: Game + 'static> {
    factory: SessionFactory<G>,
    sessions: StdMutex<HashMap<String, Arc<UserSession<G>>>>,
    next_session_id: AtomicU64,
}

impl<G: Game + 'static> UserSessionStore<G> {
    fn new(factory: SessionFactory<G>) -> Self {
        Self {
            factory,
            sessions: StdMutex::new(HashMap::new()),
            next_session_id: AtomicU64::new(1),
        }
    }

    fn get_or_create(&self, user_id: &str) -> (Arc<UserSession<G>>, bool) {
        let mut sessions = self.sessions.lock().expect("user session lock poisoned");
        if let Some(session) = sessions.get(user_id) {
            return (Arc::clone(session), false);
        }

        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let game_session = self.create_unique_session(&sessions);
        let session = Arc::new(UserSession::new(game_session, session_id));
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
) {
    let static_dir = presenter.static_dir().to_path_buf();
    let replay = replay.map(|(s, l)| (s, Arc::new(l)));
    let store = Arc::new(UserSessionStore::new(SessionFactory::new_game(
        evaluator,
        eval_name,
        presenter,
        human_players,
        replay,
    )));
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
    let store = Arc::new(UserSessionStore::new(SessionFactory::with_state(
        state,
        evaluator,
        eval_name,
        presenter,
        human_players,
    )));
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
    let store = Arc::new(UserSessionStore::new(SessionFactory::with_timeline(
        timeline,
        evaluator,
        presenter,
        human_players,
    )));
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
/// Handles `BotMove` and `RunSims` messages. Returns the final response
/// messages, or `Err(())` if the socket disconnected during search.
pub async fn run_search<G: Game + 'static>(
    socket: &mut WebSocket,
    session: &mut GameSession<G>,
    msg: &ClientMsg,
) -> Result<Vec<ServerMsg>, ()> {
    match session.begin_search(msg) {
        Err(msgs) => Ok(msgs),
        Ok(sims_total) => {
            let mut last_progress = 0;
            let result = loop {
                if let Some(result) = session.search_tick() {
                    break result;
                }
                if let Some((snap, labels)) = session.snapshot_with_labels() {
                    if snap.total_simulations >= last_progress + PROGRESS_INTERVAL {
                        last_progress = snap.total_simulations;
                        send_msg(
                            socket,
                            &ServerMsg::SearchProgress {
                                snapshot: snap,
                                action_labels: labels,
                                sims_total,
                            },
                        )
                        .await?;
                        if let Some(subtree_msg) = session.explore_subtree_msg() {
                            let _ = send_msg(socket, &subtree_msg).await;
                        }
                    }
                }
            };
            Ok(session.finish_search(msg, result))
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
            if let Some(user_id) =
                anonymous_session.as_deref().and_then(anonymous_user_id_from_id)
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
    let mut auto_search: Option<u32> = None;

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
    auto_search: &mut Option<u32>,
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

    // Track auto-search state at the connection level.
    if let ClientMsg::SetAutoSearch { enabled, target } = &client_msg {
        *auto_search = if *enabled { Some(*target) } else { None };
    }

    let mut session = user_session.session.lock().await;
    let responses = match &client_msg {
        ClientMsg::BotMove { .. } | ClientMsg::RunSims { .. } => {
            run_search(socket, &mut session, &client_msg).await?
        }
        _ => session.handle(client_msg),
    };

    // Check if any response is a state update that should trigger auto-search.
    let has_state_update = responses
        .iter()
        .any(|m| matches!(m, ServerMsg::GameState { .. }));

    for msg in &responses {
        send_msg(socket, msg).await?;
    }
    if has_state_update {
        user_session.broadcast_game_states_to_others(socket_id, &responses);
    }

    // Auto-search: trigger RunSims after state changes (e.g. PlayAction, BotMove).
    if has_state_update {
        if let Some(target) = *auto_search {
            if session.should_auto_search() {
                let auto_msg = ClientMsg::RunSims { count: target };
                let msgs = run_search(socket, &mut session, &auto_msg).await?;
                for msg in msgs {
                    send_msg(socket, &msg).await?;
                }
            }
        }
    }

    Ok(())
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
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use crate::{
        eval::{Evaluation, Evaluator},
        game::{Game, Status},
    };

    use super::{
        GamePresenter, SessionFactory, UserSessionStore, anonymous_user_id_from_id,
        anonymous_user_id_from_token,
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

        (UserSessionStore::new(factory), presenter)
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
}

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::mpsc,
};

use crate::{game::Game, game_log::GameLog};

use super::{
    GameSession, MULTIPLAYER_ANALYSIS_SIMS, MULTIPLAYER_EMPTY_ROOM_GRACE_MS, MultiplayerLobbyRoom,
    MultiplayerPlayer, MultiplayerSpectator, ReplayStore, RoomOutcome, SearchBudget, ServerMsg,
    SessionFactory, current_unix_ms, new_room_code, normalize_room_code,
    normalize_room_increment_seconds, normalize_room_time_minutes, replay_result_for_room_outcome,
    winner_from_reward,
};

const DEFAULT_REDIS_PORT: u16 = 6379;
const REDIS_PRESENCE_TTL_MS: u64 = 35_000;
const REDIS_KEY_PREFIX: &str = "hexfish:multiplayer";
const REDIS_CAS_RETRIES: usize = 12;

#[derive(Clone, Debug)]
struct RedisConfig {
    host: String,
    port: u16,
    password: Option<String>,
    db: Option<u32>,
}

impl RedisConfig {
    fn parse(url: &str) -> Result<Self, String> {
        let rest = url
            .trim()
            .strip_prefix("redis://")
            .ok_or_else(|| "REDIS_URL must start with redis://".to_string())?;
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (auth, host_port) = authority
            .rsplit_once('@')
            .map_or((None, authority), |(auth, host_port)| {
                (Some(auth), host_port)
            });
        let password = auth.and_then(|auth| {
            let password = auth.rsplit_once(':').map_or(auth, |(_, password)| password);
            (!password.is_empty()).then(|| password.to_string())
        });
        let (host, port) = host_port
            .rsplit_once(':')
            .map_or((host_port, DEFAULT_REDIS_PORT), |(host, port)| {
                (host, port.parse().unwrap_or(DEFAULT_REDIS_PORT))
            });
        if host.trim().is_empty() {
            return Err("REDIS_URL is missing a host".into());
        }
        let db = path
            .split(['?', '#'])
            .next()
            .filter(|db| !db.is_empty())
            .map(|db| db.parse::<u32>())
            .transpose()
            .map_err(|e| format!("invalid Redis database in REDIS_URL: {e}"))?;
        Ok(Self {
            host: host.to_string(),
            port,
            password,
            db,
        })
    }
}

#[derive(Debug)]
enum RedisValue {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Option<Vec<u8>>),
    Array(Option<Vec<RedisValue>>),
}

impl RedisValue {
    fn bulk_string(self) -> Result<Option<String>, String> {
        match self {
            RedisValue::Bulk(Some(bytes)) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|e| e.to_string()),
            RedisValue::Bulk(None) => Ok(None),
            RedisValue::Simple(value) => Ok(Some(value)),
            RedisValue::Error(message) => Err(message),
            other => Err(format!("unexpected Redis value: {other:?}")),
        }
    }

    fn string(self) -> Result<String, String> {
        self.bulk_string()?
            .ok_or_else(|| "missing Redis value".to_string())
    }

    fn integer(self) -> Result<i64, String> {
        match self {
            RedisValue::Integer(value) => Ok(value),
            RedisValue::Error(message) => Err(message),
            other => Err(format!("unexpected Redis integer value: {other:?}")),
        }
    }

    fn array(self) -> Result<Option<Vec<RedisValue>>, String> {
        match self {
            RedisValue::Array(values) => Ok(values),
            RedisValue::Bulk(None) => Ok(None),
            RedisValue::Error(message) => Err(message),
            other => Err(format!("unexpected Redis array value: {other:?}")),
        }
    }
}

struct RedisConnection {
    reader: BufReader<TcpStream>,
}

impl RedisConnection {
    async fn connect(config: RedisConfig) -> Result<Self, String> {
        let stream = TcpStream::connect((config.host.as_str(), config.port))
            .await
            .map_err(|e| format!("failed to connect to Redis: {e}"))?;
        let mut conn = Self {
            reader: BufReader::new(stream),
        };
        if let Some(password) = config.password.as_deref() {
            conn.command_owned(vec!["AUTH".into(), password.into()])
                .await?;
        }
        if let Some(db) = config.db {
            conn.command_owned(vec!["SELECT".into(), db.to_string()])
                .await?;
        }
        Ok(conn)
    }

    async fn command(&mut self, args: &[&str]) -> Result<RedisValue, String> {
        self.command_owned(args.iter().map(|arg| (*arg).to_string()).collect())
            .await
    }

    async fn command_owned(&mut self, args: Vec<String>) -> Result<RedisValue, String> {
        let mut data = format!("*{}\r\n", args.len()).into_bytes();
        for arg in args {
            data.extend_from_slice(format!("${}\r\n", arg.as_bytes().len()).as_bytes());
            data.extend_from_slice(arg.as_bytes());
            data.extend_from_slice(b"\r\n");
        }
        let stream = self.reader.get_mut();
        stream
            .write_all(&data)
            .await
            .map_err(|e| format!("failed to write Redis command: {e}"))?;
        stream
            .flush()
            .await
            .map_err(|e| format!("failed to flush Redis command: {e}"))?;
        self.read_value().await
    }

    fn read_value<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<RedisValue, String>> + Send + 'a>> {
        Box::pin(async move {
            let mut prefix = [0_u8; 1];
            self.reader
                .read_exact(&mut prefix)
                .await
                .map_err(|e| format!("failed to read Redis response: {e}"))?;
            match prefix[0] {
                b'+' => Ok(RedisValue::Simple(self.read_line().await?)),
                b'-' => Ok(RedisValue::Error(self.read_line().await?)),
                b':' => {
                    let value = self
                        .read_line()
                        .await?
                        .parse::<i64>()
                        .map_err(|e| format!("invalid Redis integer: {e}"))?;
                    Ok(RedisValue::Integer(value))
                }
                b'$' => {
                    let len = self
                        .read_line()
                        .await?
                        .parse::<isize>()
                        .map_err(|e| format!("invalid Redis bulk length: {e}"))?;
                    if len < 0 {
                        return Ok(RedisValue::Bulk(None));
                    }
                    let mut bytes = vec![0_u8; len as usize];
                    self.reader
                        .read_exact(&mut bytes)
                        .await
                        .map_err(|e| format!("failed to read Redis bulk value: {e}"))?;
                    let mut crlf = [0_u8; 2];
                    self.reader
                        .read_exact(&mut crlf)
                        .await
                        .map_err(|e| format!("failed to read Redis bulk terminator: {e}"))?;
                    Ok(RedisValue::Bulk(Some(bytes)))
                }
                b'*' => {
                    let len = self
                        .read_line()
                        .await?
                        .parse::<isize>()
                        .map_err(|e| format!("invalid Redis array length: {e}"))?;
                    if len < 0 {
                        return Ok(RedisValue::Array(None));
                    }
                    let mut values = Vec::with_capacity(len as usize);
                    for _ in 0..len {
                        values.push(self.read_value().await?);
                    }
                    Ok(RedisValue::Array(Some(values)))
                }
                other => Err(format!(
                    "unexpected Redis response prefix: {}",
                    other as char
                )),
            }
        })
    }

    async fn read_line(&mut self) -> Result<String, String> {
        let mut bytes = Vec::new();
        self.reader
            .read_until(b'\n', &mut bytes)
            .await
            .map_err(|e| format!("failed to read Redis line: {e}"))?;
        if bytes.ends_with(b"\r\n") {
            bytes.truncate(bytes.len() - 2);
        } else if bytes.ends_with(b"\n") {
            bytes.truncate(bytes.len() - 1);
        }
        String::from_utf8(bytes).map_err(|e| format!("invalid Redis UTF-8 line: {e}"))
    }
}

#[derive(Clone)]
struct RedisClient {
    config: RedisConfig,
}

impl RedisClient {
    fn new(config: RedisConfig) -> Self {
        Self { config }
    }

    async fn ping(&self) -> Result<(), String> {
        let mut conn = RedisConnection::connect(self.config.clone()).await?;
        match conn.command(&["PING"]).await? {
            RedisValue::Simple(value) if value == "PONG" => Ok(()),
            value => Err(format!("unexpected Redis PING response: {value:?}")),
        }
    }

    async fn command_owned(&self, args: Vec<String>) -> Result<RedisValue, String> {
        let mut conn = RedisConnection::connect(self.config.clone()).await?;
        conn.command_owned(args).await
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RedisSeatOwner {
    user_id: String,
    account_key: String,
    display_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RedisSpectator {
    user_id: String,
    display_name: Option<String>,
    connection_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RedisRoomOutcome {
    winner: Option<usize>,
    reason: String,
    replay_share_slug: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RedisRoomClock {
    remaining_ms: Option<[u64; 2]>,
    active_player: Option<usize>,
    active_since_ms: Option<u64>,
    winner: Option<usize>,
}

impl RedisRoomClock {
    fn new(time_minutes: Option<u32>) -> Self {
        let remaining_ms = time_minutes.map(|minutes| {
            let millis = u64::from(minutes) * 60_000;
            [millis, millis]
        });
        Self {
            remaining_ms,
            active_player: None,
            active_since_ms: None,
            winner: None,
        }
    }

    fn snapshot(&self) -> RedisRoomClockSnapshot {
        let now_ms = current_unix_ms();
        let mut time_millis = self.remaining_ms.map_or([None, None], |remaining| {
            [Some(remaining[0]), Some(remaining[1])]
        });
        if let (Some(player), Some(active_since_ms), Some(remaining)) = (
            self.active_player,
            self.active_since_ms,
            self.remaining_ms.as_ref(),
        ) {
            let elapsed = now_ms.saturating_sub(active_since_ms);
            time_millis[player] = Some(remaining[player].saturating_sub(elapsed));
        }
        RedisRoomClockSnapshot {
            time_millis,
            active_player: self.active_player,
            winner: self.winner,
        }
    }

    fn mark_active(&mut self, player: usize) {
        if self.remaining_ms.is_none() || self.winner.is_some() {
            return;
        }
        self.active_player = Some(player);
        self.active_since_ms = Some(current_unix_ms());
    }

    fn tick_active(&mut self) -> bool {
        let (Some(player), Some(active_since_ms)) = (self.active_player, self.active_since_ms)
        else {
            return false;
        };
        let Some(remaining) = self.remaining_ms.as_mut() else {
            return false;
        };
        let elapsed = current_unix_ms().saturating_sub(active_since_ms);
        if elapsed >= remaining[player] {
            remaining[player] = 0;
            self.winner = Some(1 - player);
            self.active_player = None;
            self.active_since_ms = None;
            return true;
        }
        remaining[player] -= elapsed;
        self.active_since_ms = Some(current_unix_ms());
        false
    }

    fn finish_turn(
        &mut self,
        finished_player: usize,
        next_player: usize,
        game_over: bool,
        increment_seconds: Option<u32>,
    ) {
        if self.remaining_ms.is_none() || self.winner.is_some() {
            return;
        }
        self.tick_active();
        if game_over || self.winner.is_some() {
            self.active_player = None;
            self.active_since_ms = None;
            return;
        }
        if let (Some(remaining), Some(increment_seconds)) =
            (self.remaining_ms.as_mut(), increment_seconds)
        {
            remaining[finished_player] =
                remaining[finished_player].saturating_add(u64::from(increment_seconds) * 1000);
        }
        self.active_player = Some(next_player);
        self.active_since_ms = Some(current_unix_ms());
    }

    fn add_seconds(&mut self, player: usize, seconds: u32) -> bool {
        if self.remaining_ms.is_none() || self.winner.is_some() {
            return false;
        }
        self.tick_active();
        let Some(remaining) = self.remaining_ms.as_mut() else {
            return false;
        };
        remaining[player] = remaining[player].saturating_add(u64::from(seconds) * 1000);
        true
    }

    fn timeout_deadline_ms(&self) -> Option<u64> {
        let player = self.active_player?;
        let active_since_ms = self.active_since_ms?;
        let remaining = self.remaining_ms.as_ref()?;
        Some(active_since_ms.saturating_add(remaining[player]))
    }
}

#[derive(Clone, Copy)]
struct RedisRoomClockSnapshot {
    time_millis: [Option<u64>; 2],
    active_player: Option<usize>,
    winner: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RedisRoomSnapshot {
    code: String,
    room_id: u64,
    version: u64,
    seats: [Option<RedisSeatOwner>; 2],
    active_connections: [Option<String>; 2],
    #[serde(default)]
    spectators: Vec<RedisSpectator>,
    initial_state: String,
    actions: Vec<usize>,
    clock: RedisRoomClock,
    outcome: Option<RedisRoomOutcome>,
    replay_saved: bool,
    next_replay_counter: u64,
    last_activity_ms: u64,
    is_public: bool,
    time_minutes: Option<u32>,
    increment_seconds: Option<u32>,
}

impl RedisRoomSnapshot {
    fn new(
        code: String,
        room_id: u64,
        initial_state: String,
        creator: RedisSeatOwner,
        creator_player: usize,
        is_public: bool,
        time_minutes: Option<u32>,
        increment_seconds: Option<u32>,
    ) -> Self {
        let mut seats: [Option<RedisSeatOwner>; 2] = [None, None];
        seats[creator_player] = Some(creator);
        Self {
            code,
            room_id,
            version: 1,
            seats,
            active_connections: [None, None],
            spectators: Vec::new(),
            initial_state,
            actions: Vec::new(),
            clock: RedisRoomClock::new(time_minutes),
            outcome: None,
            replay_saved: false,
            next_replay_counter: 1,
            last_activity_ms: current_unix_ms(),
            is_public,
            time_minutes,
            increment_seconds,
        }
    }

    fn touch(&mut self) {
        self.last_activity_ms = current_unix_ms();
    }

    fn is_full(&self) -> bool {
        self.seats.iter().all(Option::is_some)
    }

    fn is_finished(&self) -> bool {
        self.outcome.is_some() || self.clock.winner.is_some()
    }

    fn seat_for_user(&self, user_id: &str) -> Option<usize> {
        self.seats
            .iter()
            .position(|seat| seat.as_ref().map_or(false, |seat| seat.user_id == user_id))
    }

    fn assign_or_find_viewer(&mut self, owner: RedisSeatOwner) -> Option<usize> {
        let owner_user_id = owner.user_id.clone();
        if let Some(player) = self.seat_for_user(&owner.user_id) {
            if owner.display_name.is_some() {
                self.seats[player] = Some(owner);
                self.touch();
            }
            self.remove_spectator_user(&owner_user_id);
            return Some(player);
        }
        if let Some(player) = self.seats.iter().position(Option::is_none) {
            self.remove_spectator_user(&owner_user_id);
            self.seats[player] = Some(owner);
            self.touch();
            return Some(player);
        }
        None
    }

    fn remove_spectator_connection(&mut self, connection_id: &str) {
        let before = self.spectators.len();
        self.spectators
            .retain(|spectator| spectator.connection_id != connection_id);
        if self.spectators.len() != before {
            self.touch();
        }
    }

    fn remove_spectator_user(&mut self, user_id: &str) -> Option<String> {
        let mut old_connection = None;
        let before = self.spectators.len();
        self.spectators.retain(|spectator| {
            let remove = spectator.user_id == user_id;
            if remove && old_connection.is_none() {
                old_connection = Some(spectator.connection_id.clone());
            }
            !remove
        });
        if self.spectators.len() != before {
            self.touch();
        }
        old_connection
    }
}

#[derive(Clone, Debug)]
pub(super) struct ActiveRedisMultiplayerRoom {
    pub code: String,
    pub player: Option<usize>,
    pub connection_id: String,
}

#[derive(Clone)]
struct RedisLocalSocket {
    room_code: String,
    player: Option<usize>,
    connection_id: String,
    tx: mpsc::UnboundedSender<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum RedisMultiplayerEvent {
    LobbyChanged,
    RoomChanged {
        code: String,
        excluded_connection_id: Option<String>,
    },
    GameChanged {
        code: String,
        excluded_connection_id: Option<String>,
    },
    KickConnection {
        connection_id: String,
    },
    MultiplayerAnalysis {
        code: String,
        version: u64,
        root_wdl: [f32; 3],
    },
}

pub(super) struct RedisMultiplayerRoomStore<G: Game + 'static> {
    factory: Arc<SessionFactory<G>>,
    replay_store: Option<Arc<ReplayStore>>,
    client: RedisClient,
    key_prefix: String,
    process_id: String,
    local_sockets: StdMutex<HashMap<String, RedisLocalSocket>>,
    lobby_sockets: StdMutex<HashMap<u64, mpsc::UnboundedSender<String>>>,
    next_lobby_socket_id: AtomicU64,
}

impl<G: Game + 'static> RedisMultiplayerRoomStore<G> {
    pub(super) async fn connect(
        redis_url: &str,
        factory: Arc<SessionFactory<G>>,
        replay_store: Option<Arc<ReplayStore>>,
    ) -> Result<Arc<Self>, String> {
        let config = RedisConfig::parse(redis_url)?;
        let client = RedisClient::new(config);
        client.ping().await?;
        let store = Arc::new(Self {
            factory,
            replay_store,
            client,
            key_prefix: REDIS_KEY_PREFIX.into(),
            process_id: format!("{}-{}", current_unix_ms(), fastrand::u64(..)),
            local_sockets: StdMutex::new(HashMap::new()),
            lobby_sockets: StdMutex::new(HashMap::new()),
            next_lobby_socket_id: AtomicU64::new(1),
        });
        Self::start_subscriber(Arc::clone(&store));
        Self::start_clock_worker(Arc::clone(&store));
        Self::start_cleanup_worker(Arc::clone(&store));
        Ok(store)
    }

    pub(super) fn register_lobby_socket(&self, tx: mpsc::UnboundedSender<String>) -> u64 {
        let id = self.next_lobby_socket_id.fetch_add(1, Ordering::Relaxed);
        self.lobby_sockets
            .lock()
            .expect("redis lobby socket lock poisoned")
            .insert(id, tx);
        id
    }

    pub(super) fn unregister_lobby_socket(&self, id: u64) {
        self.lobby_sockets
            .lock()
            .expect("redis lobby socket lock poisoned")
            .remove(&id);
    }

    pub(super) async fn send_lobby_to(&self, tx: &mpsc::UnboundedSender<String>) {
        if let Ok(json) = serde_json::to_string(&self.lobby_msg().await) {
            let _ = tx.send(json);
        }
    }

    pub(super) async fn publish_lobby_changed(&self) {
        let _ = self
            .publish_event(RedisMultiplayerEvent::LobbyChanged)
            .await;
    }

    pub(super) async fn refresh_active_presence(&self, active: &ActiveRedisMultiplayerRoom) {
        let _ = self.set_presence(&active.connection_id).await;
    }

    pub(super) async fn detach_active_room(
        &self,
        active_room: &mut Option<ActiveRedisMultiplayerRoom>,
    ) -> bool {
        let Some(active) = active_room.take() else {
            return false;
        };
        self.local_sockets
            .lock()
            .expect("redis local socket lock poisoned")
            .remove(&active.connection_id);
        let _ = self
            .client
            .command_owned(vec!["DEL".into(), self.presence_key(&active.connection_id)])
            .await;
        let code = active.code.clone();
        let connection_id = active.connection_id.clone();
        let _ = self
            .update_room(&code, |room| {
                if let Some(player) = active.player {
                    if room.active_connections[player].as_deref() == Some(&connection_id) {
                        room.active_connections[player] = None;
                        room.touch();
                    }
                } else {
                    room.remove_spectator_connection(&connection_id);
                }
                Ok(())
            })
            .await;
        let _ = self
            .publish_event(RedisMultiplayerEvent::RoomChanged {
                code: code.clone(),
                excluded_connection_id: Some(connection_id),
            })
            .await;
        let _ = self
            .publish_event(RedisMultiplayerEvent::LobbyChanged)
            .await;
        true
    }

    pub(super) async fn handle_message_for_user(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        outbound_tx: &mpsc::UnboundedSender<String>,
        active_room: &mut Option<ActiveRedisMultiplayerRoom>,
        msg: super::ClientMsg,
    ) -> Vec<ServerMsg> {
        if let Some(active) = active_room.as_ref() {
            if self.refresh_current_connection(active).await.is_err() {
                self.local_sockets
                    .lock()
                    .expect("redis local socket lock poisoned")
                    .remove(&active.connection_id);
                *active_room = None;
            }
        }

        match msg {
            super::ClientMsg::ListMultiplayerRooms => vec![self.lobby_msg().await],
            super::ClientMsg::GetMultiplayerRoom => {
                let Some(active) = active_room.as_ref() else {
                    return vec![ServerMsg::Error {
                        message: "Join a multiplayer room before refreshing it".into(),
                    }];
                };
                match self.load_room(&active.code).await {
                    Ok(Some(room)) => {
                        self.room_messages_for_viewer(
                            &room,
                            active.player,
                            Some(&active.connection_id),
                            true,
                        )
                        .await
                    }
                    Ok(None) => vec![ServerMsg::Error {
                        message: "Room not found".into(),
                    }],
                    Err(message) => vec![ServerMsg::Error { message }],
                }
            }
            super::ClientMsg::CreateMultiplayerRoom {
                preferred_player,
                code,
                is_public,
                time_minutes,
                increment_seconds,
            } => {
                self.detach_active_room(active_room).await;
                self.create_room(
                    user_id,
                    display_name,
                    preferred_player,
                    code,
                    is_public,
                    time_minutes,
                    increment_seconds,
                    outbound_tx,
                    active_room,
                )
                .await
            }
            super::ClientMsg::JoinMultiplayerRoom { code } => {
                self.detach_active_room(active_room).await;
                self.join_room(user_id, display_name, &code, outbound_tx, active_room)
                    .await
            }
            super::ClientMsg::LeaveMultiplayerRoom => {
                self.detach_active_room(active_room).await;
                vec![super::MultiplayerRoom::<G>::left_room_msg()]
            }
            super::ClientMsg::PlayMultiplayerAction { action } => {
                self.play_action(active_room.as_ref(), action).await
            }
            super::ClientMsg::ResignMultiplayerGame => self.resign_game(active_room.as_ref()).await,
            super::ClientMsg::AddMultiplayerOpponentTime => {
                self.add_opponent_time(active_room.as_ref()).await
            }
            _ => vec![ServerMsg::Error {
                message: "Unsupported multiplayer message".into(),
            }],
        }
    }

    async fn create_room(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        preferred_player: Option<u8>,
        requested_code: Option<String>,
        is_public: Option<bool>,
        time_minutes: Option<u32>,
        increment_seconds: Option<u32>,
        outbound_tx: &mpsc::UnboundedSender<String>,
        active_room: &mut Option<ActiveRedisMultiplayerRoom>,
    ) -> Vec<ServerMsg> {
        let player = match preferred_player {
            None => usize::from(fastrand::bool()),
            Some(1) => 1,
            _ => 0,
        };
        let initial_state = match self.initial_log_state() {
            Ok(initial_state) => initial_state,
            Err(message) => return vec![ServerMsg::Error { message }],
        };
        let owner = self.seat_owner(user_id, display_name);
        let is_public = is_public.unwrap_or(true);
        let time_minutes = normalize_room_time_minutes(time_minutes);
        let increment_seconds = normalize_room_increment_seconds(increment_seconds);
        let room_id = current_unix_ms().saturating_mul(1000) ^ fastrand::u64(..);

        let mut codes = Vec::new();
        if let Some(code) = requested_code {
            match normalize_room_code(&code) {
                Ok(code) => codes.push(code),
                Err(message) => return vec![ServerMsg::Error { message }],
            }
        } else {
            for _ in 0..64 {
                codes.push(new_room_code());
            }
        }

        for code in codes {
            let room = RedisRoomSnapshot::new(
                code.clone(),
                room_id,
                initial_state.clone(),
                owner.clone(),
                player,
                is_public,
                time_minutes,
                increment_seconds,
            );
            let json = match serde_json::to_string(&room) {
                Ok(json) => json,
                Err(e) => {
                    return vec![ServerMsg::Error {
                        message: format!("failed to serialize Redis room: {e}"),
                    }];
                }
            };
            let set = self
                .client
                .command_owned(vec!["SET".into(), self.room_key(&code), json, "NX".into()])
                .await;
            match set {
                Ok(RedisValue::Simple(ok)) if ok == "OK" => {
                    let _ = self
                        .client
                        .command_owned(vec!["SADD".into(), self.rooms_key(), code.clone()])
                        .await;
                    let _ = self
                        .publish_event(RedisMultiplayerEvent::LobbyChanged)
                        .await;
                    return self
                        .activate_room(&code, user_id, display_name, outbound_tx, active_room)
                        .await;
                }
                Ok(RedisValue::Bulk(None)) => continue,
                Ok(other) => {
                    return vec![ServerMsg::Error {
                        message: format!("unexpected Redis room create response: {other:?}"),
                    }];
                }
                Err(message) => return vec![ServerMsg::Error { message }],
            }
        }

        vec![ServerMsg::Error {
            message: "Could not create a unique room code".into(),
        }]
    }

    async fn join_room(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        code: &str,
        outbound_tx: &mpsc::UnboundedSender<String>,
        active_room: &mut Option<ActiveRedisMultiplayerRoom>,
    ) -> Vec<ServerMsg> {
        let code = match normalize_room_code(code) {
            Ok(code) => code,
            Err(message) => return vec![ServerMsg::Error { message }],
        };
        self.activate_room(&code, user_id, display_name, outbound_tx, active_room)
            .await
    }

    async fn activate_room(
        &self,
        code: &str,
        user_id: &str,
        display_name: Option<&str>,
        outbound_tx: &mpsc::UnboundedSender<String>,
        active_room: &mut Option<ActiveRedisMultiplayerRoom>,
    ) -> Vec<ServerMsg> {
        let mut old_connection = None;
        let connection_id = self.next_connection_id();
        let owner = self.seat_owner(user_id, display_name);
        let activated = self
            .update_room(code, |room| {
                let player = room.assign_or_find_viewer(owner.clone());
                if let Some(player) = player {
                    old_connection = room.active_connections[player].clone();
                    room.active_connections[player] = Some(connection_id.clone());
                } else {
                    old_connection = room.remove_spectator_user(user_id);
                    room.spectators.push(RedisSpectator {
                        user_id: user_id.to_string(),
                        display_name: display_name.map(str::to_string),
                        connection_id: connection_id.clone(),
                    });
                }
                if room.is_full() && room.clock.active_player.is_none() && !room.is_finished() {
                    room.clock.mark_active(0);
                }
                room.touch();
                Ok(player)
            })
            .await;
        let (player, room) = match activated {
            Ok((player, room)) => (player, room),
            Err(message) => return vec![ServerMsg::Error { message }],
        };

        self.local_sockets
            .lock()
            .expect("redis local socket lock poisoned")
            .insert(
                connection_id.clone(),
                RedisLocalSocket {
                    room_code: code.to_string(),
                    player,
                    connection_id: connection_id.clone(),
                    tx: outbound_tx.clone(),
                },
            );
        let _ = self.set_presence(&connection_id).await;
        *active_room = Some(ActiveRedisMultiplayerRoom {
            code: code.to_string(),
            player,
            connection_id: connection_id.clone(),
        });
        if let Some(old_connection) = old_connection.filter(|old| old != &connection_id) {
            let _ = self
                .publish_event(RedisMultiplayerEvent::KickConnection {
                    connection_id: old_connection,
                })
                .await;
        }
        let _ = self.schedule_clock_deadline(&room).await;
        let _ = self
            .publish_event(RedisMultiplayerEvent::RoomChanged {
                code: code.to_string(),
                excluded_connection_id: Some(connection_id.clone()),
            })
            .await;
        let _ = self
            .publish_event(RedisMultiplayerEvent::LobbyChanged)
            .await;
        self.room_messages_for_viewer(&room, player, Some(&connection_id), true)
            .await
    }

    async fn play_action(
        &self,
        active: Option<&ActiveRedisMultiplayerRoom>,
        action: usize,
    ) -> Vec<ServerMsg> {
        let Some(active) = active else {
            return vec![ServerMsg::Error {
                message: "Join a multiplayer room before playing".into(),
            }];
        };
        if let Err(message) = self.refresh_current_connection(active).await {
            return vec![ServerMsg::Error { message }];
        }
        let Some(player) = active.player else {
            return vec![ServerMsg::Error {
                message: "Spectators cannot play multiplayer actions".into(),
            }];
        };
        let connection_id = active.connection_id.clone();
        let mut direct_state = None;
        let mut analysis_session = None;
        let mut finished = false;
        let updated = self
            .update_room(&active.code, |room| {
                self.ensure_active_connection(room, player, &connection_id)?;
                if !room.is_full() {
                    return Err("Room is waiting for an opponent".into());
                }
                if room.is_finished() {
                    finished = true;
                    return Ok(());
                }
                let mut session = self.session_from_snapshot(room)?;
                session.play_human_action(player, action)?;
                let game_over = session.current_game_ended();
                if game_over {
                    room.outcome = Some(RedisRoomOutcome {
                        winner: winner_from_reward(session.current_result_reward()),
                        reason: "game".into(),
                        replay_share_slug: room
                            .outcome
                            .as_ref()
                            .and_then(|outcome| outcome.replay_share_slug.clone()),
                    });
                }
                let log = session
                    .export_current_log_allow_empty()
                    .ok_or_else(|| "Could not export multiplayer game log".to_string())?;
                room.actions = log.actions;
                room.clock.finish_turn(
                    player,
                    session.current_player_idx(),
                    game_over,
                    room.increment_seconds,
                );
                room.touch();
                direct_state = Some(session.state_msg_for_player(player));
                analysis_session = (!game_over).then(|| session.fork_analysis_session());
                finished = game_over;
                Ok(())
            })
            .await;
        let (_, room) = match updated {
            Ok(updated) => updated,
            Err(message) => return vec![ServerMsg::Error { message }],
        };
        let _ = self.schedule_clock_deadline(&room).await;
        if let Some(session) = analysis_session {
            self.schedule_analysis(room.code.clone(), room.version, session);
        }
        let _ = self
            .publish_event(RedisMultiplayerEvent::GameChanged {
                code: room.code.clone(),
                excluded_connection_id: Some(connection_id.clone()),
            })
            .await;
        let _ = self
            .publish_event(RedisMultiplayerEvent::LobbyChanged)
            .await;
        let mut responses = vec![
            self.room_msg_for_viewer(&room, Some(player), Some(&connection_id))
                .await,
        ];
        if let Some(state) = direct_state {
            responses.push(state);
        }
        if finished {
            responses.extend(self.save_finished_room_replay_once(&room.code, false).await);
        }
        responses
    }

    async fn resign_game(&self, active: Option<&ActiveRedisMultiplayerRoom>) -> Vec<ServerMsg> {
        let Some(active) = active else {
            return vec![ServerMsg::Error {
                message: "Join a multiplayer room before resigning".into(),
            }];
        };
        if let Err(message) = self.refresh_current_connection(active).await {
            return vec![ServerMsg::Error { message }];
        }
        let Some(player) = active.player else {
            return vec![ServerMsg::Error {
                message: "Spectators cannot resign multiplayer games".into(),
            }];
        };
        let connection_id = active.connection_id.clone();
        let updated = self
            .update_room(&active.code, |room| {
                self.ensure_active_connection(room, player, &connection_id)?;
                if !room.is_full() {
                    return Err("Room is waiting for an opponent".into());
                }
                if !room.is_finished() {
                    room.outcome = Some(RedisRoomOutcome {
                        winner: Some(1 - player),
                        reason: "resignation".into(),
                        replay_share_slug: None,
                    });
                    room.clock.active_player = None;
                    room.clock.active_since_ms = None;
                    room.touch();
                }
                Ok(())
            })
            .await;
        let (_, room) = match updated {
            Ok(updated) => updated,
            Err(message) => return vec![ServerMsg::Error { message }],
        };
        let _ = self.schedule_clock_deadline(&room).await;
        let mut responses = vec![
            self.room_msg_for_viewer(&room, Some(player), Some(&connection_id))
                .await,
        ];
        responses.extend(self.save_finished_room_replay_once(&room.code, false).await);
        let _ = self
            .publish_event(RedisMultiplayerEvent::RoomChanged {
                code: room.code,
                excluded_connection_id: Some(connection_id),
            })
            .await;
        let _ = self
            .publish_event(RedisMultiplayerEvent::LobbyChanged)
            .await;
        responses
    }

    async fn add_opponent_time(
        &self,
        active: Option<&ActiveRedisMultiplayerRoom>,
    ) -> Vec<ServerMsg> {
        let Some(active) = active else {
            return vec![ServerMsg::Error {
                message: "Join a multiplayer room before adjusting clocks".into(),
            }];
        };
        if let Err(message) = self.refresh_current_connection(active).await {
            return vec![ServerMsg::Error { message }];
        }
        let Some(player) = active.player else {
            return vec![ServerMsg::Error {
                message: "Spectators cannot adjust multiplayer clocks".into(),
            }];
        };
        let opponent = 1 - player;
        let connection_id = active.connection_id.clone();
        let updated = self
            .update_room(&active.code, |room| {
                self.ensure_active_connection(room, player, &connection_id)?;
                if !room.clock.add_seconds(opponent, 15) {
                    if room.is_finished() {
                        return Ok(());
                    }
                    return Err("This room does not have clocks enabled".into());
                }
                room.touch();
                Ok(())
            })
            .await;
        let (_, room) = match updated {
            Ok(updated) => updated,
            Err(message) => return vec![ServerMsg::Error { message }],
        };
        let _ = self.schedule_clock_deadline(&room).await;
        let _ = self
            .publish_event(RedisMultiplayerEvent::RoomChanged {
                code: room.code.clone(),
                excluded_connection_id: Some(connection_id.clone()),
            })
            .await;
        vec![
            self.room_msg_for_viewer(&room, Some(player), Some(&connection_id))
                .await,
        ]
    }

    fn ensure_active_connection(
        &self,
        room: &RedisRoomSnapshot,
        player: usize,
        connection_id: &str,
    ) -> Result<(), String> {
        if room.active_connections[player].as_deref() != Some(connection_id) {
            return Err("This multiplayer connection has been replaced. Rejoin the room.".into());
        }
        Ok(())
    }

    fn spectator_connection_exists(room: &RedisRoomSnapshot, connection_id: &str) -> bool {
        room.spectators
            .iter()
            .any(|spectator| spectator.connection_id == connection_id)
    }

    async fn refresh_current_connection(
        &self,
        active: &ActiveRedisMultiplayerRoom,
    ) -> Result<(), String> {
        let room = self
            .load_room(&active.code)
            .await?
            .ok_or_else(|| "Room not found".to_string())?;
        if let Some(player) = active.player {
            if room.active_connections[player].as_deref() != Some(&active.connection_id) {
                return Err(
                    "This multiplayer connection has been replaced. Rejoin the room.".into(),
                );
            }
        } else if !Self::spectator_connection_exists(&room, &active.connection_id) {
            return Err("This spectator connection has been replaced. Rejoin the room.".into());
        }
        self.set_presence(&active.connection_id).await
    }

    async fn update_room<F, R>(
        &self,
        code: &str,
        mut mutate: F,
    ) -> Result<(R, RedisRoomSnapshot), String>
    where
        F: FnMut(&mut RedisRoomSnapshot) -> Result<R, String> + Send,
        R: Send,
    {
        let key = self.room_key(code);
        for _ in 0..REDIS_CAS_RETRIES {
            let mut conn = RedisConnection::connect(self.client.config.clone()).await?;
            conn.command_owned(vec!["WATCH".into(), key.clone()])
                .await?;
            let current = conn.command_owned(vec!["GET".into(), key.clone()]).await?;
            let Some(json) = current.bulk_string()? else {
                let _ = conn.command(&["UNWATCH"]).await;
                return Err("Room not found".into());
            };
            let mut room: RedisRoomSnapshot =
                serde_json::from_str(&json).map_err(|e| format!("invalid Redis room: {e}"))?;
            let result = mutate(&mut room)?;
            room.version = room.version.saturating_add(1);
            let next_json =
                serde_json::to_string(&room).map_err(|e| format!("serialize Redis room: {e}"))?;
            conn.command(&["MULTI"]).await?;
            conn.command_owned(vec!["SET".into(), key.clone(), next_json])
                .await?;
            let exec = conn.command(&["EXEC"]).await?;
            if exec.array()?.is_some() {
                return Ok((result, room));
            }
        }
        Err("Room was updated concurrently; try again".into())
    }

    async fn load_room(&self, code: &str) -> Result<Option<RedisRoomSnapshot>, String> {
        let value = self
            .client
            .command_owned(vec!["GET".into(), self.room_key(code)])
            .await?;
        let Some(json) = value.bulk_string()? else {
            return Ok(None);
        };
        serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("invalid Redis room: {e}"))
    }

    async fn lobby_msg(&self) -> ServerMsg {
        let mut entries = Vec::new();
        if let Ok(Some(codes)) = self.room_codes().await {
            for code in codes {
                if let Ok(Some(room)) = self.load_room(&code).await {
                    if room.is_public {
                        entries.push(self.lobby_entry(&room).await);
                    }
                }
            }
        }
        entries.sort_by(|a, b| {
            b.last_activity_ms
                .cmp(&a.last_activity_ms)
                .then_with(|| a.code.cmp(&b.code))
        });
        ServerMsg::MultiplayerLobby { rooms: entries }
    }

    async fn room_codes(&self) -> Result<Option<Vec<String>>, String> {
        let values = self
            .client
            .command_owned(vec!["SMEMBERS".into(), self.rooms_key()])
            .await?
            .array()?;
        let Some(values) = values else {
            return Ok(None);
        };
        values
            .into_iter()
            .map(|value| value.string())
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    async fn lobby_entry(&self, room: &RedisRoomSnapshot) -> MultiplayerLobbyRoom {
        let occupied = room.seats.iter().filter(|seat| seat.is_some()).count() as u8;
        let connected = self.connected_players(room).await as u8;
        let spectator_count = self.connected_spectators(room).await.len() as u8;
        let total_connected = usize::from(connected) + usize::from(spectator_count);
        let status = self.room_status(room);
        MultiplayerLobbyRoom {
            code: room.code.clone(),
            status,
            occupied,
            connected,
            spectator_count,
            is_public: room.is_public,
            time_minutes: room.time_minutes,
            increment_seconds: room.increment_seconds,
            last_activity_ms: room.last_activity_ms,
            empty_room_closes_at_ms: (total_connected == 0).then_some(
                room.last_activity_ms
                    .saturating_add(MULTIPLAYER_EMPTY_ROOM_GRACE_MS),
            ),
        }
    }

    async fn room_msg_for_viewer(
        &self,
        room: &RedisRoomSnapshot,
        local_player: Option<usize>,
        local_connection_id: Option<&str>,
    ) -> ServerMsg {
        let clock = room.clock.snapshot();
        let winner = room
            .outcome
            .as_ref()
            .and_then(|outcome| outcome.winner)
            .or(clock.winner);
        let mut players = Vec::new();
        for player in 0..2 {
            let connected = match room.active_connections[player].as_deref() {
                Some(connection_id) => self.presence_exists(connection_id).await,
                None => false,
            };
            players.push(MultiplayerPlayer {
                occupied: room.seats[player].is_some(),
                connected,
                you: local_player == Some(player),
                name: room.seats[player]
                    .as_ref()
                    .and_then(|seat| seat.display_name.clone()),
                time_millis: clock.time_millis[player],
                clock_active: clock.active_player == Some(player),
            });
        }
        let spectators = self
            .connected_spectators(room)
            .await
            .into_iter()
            .map(|spectator| MultiplayerSpectator {
                you: local_connection_id == Some(spectator.connection_id.as_str()),
                name: spectator.display_name,
            })
            .collect::<Vec<_>>();
        ServerMsg::MultiplayerRoom {
            code: room.code.clone(),
            status: self.room_status(room),
            viewer_role: if local_player.is_some() {
                "player".into()
            } else {
                "spectator".into()
            },
            local_player: local_player.map(|player| player as u8),
            players,
            spectators,
            time_minutes: room.time_minutes,
            increment_seconds: room.increment_seconds,
            winner: winner.map(|player| player as u8),
            finish_reason: room.outcome.as_ref().map(|outcome| outcome.reason.clone()),
            replay_share_slug: room
                .outcome
                .as_ref()
                .and_then(|outcome| outcome.replay_share_slug.clone()),
        }
    }

    async fn room_messages_for_viewer(
        &self,
        room: &RedisRoomSnapshot,
        player: Option<usize>,
        connection_id: Option<&str>,
        include_state: bool,
    ) -> Vec<ServerMsg> {
        let mut messages = vec![self.room_msg_for_viewer(room, player, connection_id).await];
        if include_state {
            match self.session_from_snapshot(room) {
                Ok(session) => messages.push(match player {
                    Some(player) => session.state_msg_for_player(player),
                    None => session.state_msg_for_spectator(),
                }),
                Err(message) => messages.push(ServerMsg::Error { message }),
            }
        }
        messages
    }

    fn room_status(&self, room: &RedisRoomSnapshot) -> String {
        if room.is_finished() {
            "finished"
        } else if room.is_full() {
            "active"
        } else {
            "waiting"
        }
        .into()
    }

    async fn connected_players(&self, room: &RedisRoomSnapshot) -> usize {
        let mut connected = 0;
        for connection_id in room.active_connections.iter().flatten() {
            if self.presence_exists(connection_id).await {
                connected += 1;
            }
        }
        connected
    }

    async fn connected_spectators(&self, room: &RedisRoomSnapshot) -> Vec<RedisSpectator> {
        let mut connected = Vec::new();
        for spectator in &room.spectators {
            if self.presence_exists(&spectator.connection_id).await {
                connected.push(spectator.clone());
            }
        }
        connected
    }

    async fn connected_viewers(&self, room: &RedisRoomSnapshot) -> usize {
        self.connected_players(room).await + self.connected_spectators(room).await.len()
    }

    async fn presence_exists(&self, connection_id: &str) -> bool {
        self.client
            .command_owned(vec!["EXISTS".into(), self.presence_key(connection_id)])
            .await
            .and_then(RedisValue::integer)
            .map_or(false, |value| value > 0)
    }

    async fn set_presence(&self, connection_id: &str) -> Result<(), String> {
        self.client
            .command_owned(vec![
                "SET".into(),
                self.presence_key(connection_id),
                "1".into(),
                "PX".into(),
                REDIS_PRESENCE_TTL_MS.to_string(),
            ])
            .await?;
        Ok(())
    }

    fn initial_log_state(&self) -> Result<String, String> {
        self.factory
            .create_multiplayer_session()
            .export_current_log_allow_empty()
            .map(|log| log.initial_state)
            .ok_or_else(|| {
                "Redis multiplayer requires this game to support replay log serialization".into()
            })
    }

    fn session_from_snapshot(&self, room: &RedisRoomSnapshot) -> Result<GameSession<G>, String> {
        let initial_state = self
            .factory
            .presenter
            .deserialize_log_state(&room.initial_state)?;
        let log = GameLog {
            initial_state: room.initial_state.clone(),
            actions: self
                .factory
                .presenter
                .normalize_replay_actions(&initial_state, &room.actions),
        };
        let mut session = self.factory.create_multiplayer_session();
        session.load_replay(initial_state, &log);
        session.seek_to_end();
        Ok(session)
    }

    fn seat_owner(&self, user_id: &str, display_name: Option<&str>) -> RedisSeatOwner {
        RedisSeatOwner {
            user_id: user_id.to_string(),
            account_key: super::redacted_account_key(user_id),
            display_name: display_name.map(str::to_string),
        }
    }

    async fn save_finished_room_replay_once(
        &self,
        code: &str,
        require_terminal: bool,
    ) -> Vec<ServerMsg> {
        let Some(store) = self.replay_store.as_ref() else {
            return Vec::new();
        };
        let mut errors = Vec::new();
        let save = self
            .update_room(code, |room| {
                if room.replay_saved {
                    return Ok(None);
                }
                let session = self.session_from_snapshot(room)?;
                if require_terminal && !session.current_game_ended() {
                    return Ok(None);
                }
                let log = if require_terminal {
                    session.export_current_log()
                } else {
                    session.export_current_log_allow_empty()
                };
                let Some(log) = log else {
                    return Ok(None);
                };
                let seats = room
                    .seats
                    .iter()
                    .enumerate()
                    .filter_map(|(player, seat)| {
                        seat.as_ref().map(|seat| (player, seat.account_key.clone()))
                    })
                    .collect::<Vec<_>>();
                let outcome = room.outcome.clone();
                room.replay_saved = true;
                Ok(Some((
                    log,
                    seats,
                    outcome,
                    room.room_id,
                    room.next_replay_counter,
                )))
            })
            .await;
        let (payload, room) = match save {
            Ok(save) => save,
            Err(message) => return vec![ServerMsg::Error { message }],
        };
        let Some((log, seats, outcome, room_id, mut counter)) = payload else {
            return Vec::new();
        };
        let mut first_share_slug = None;
        for (player, account_key) in seats {
            let result = outcome.as_ref().and_then(|outcome| {
                let outcome = RoomOutcome {
                    winner: outcome.winner,
                    reason: outcome.reason.clone(),
                    replay_share_slug: outcome.replay_share_slug.clone(),
                };
                replay_result_for_room_outcome(&outcome, player)
            });
            match store
                .save_with_result(&account_key, room_id, counter, &log, result)
                .await
            {
                Ok(entry) => {
                    if first_share_slug.is_none() {
                        first_share_slug = Some(entry.share_slug);
                    }
                }
                Err(message) => errors.push(ServerMsg::Error { message }),
            }
            counter = counter.saturating_add(1);
        }
        if let Some(slug) = first_share_slug {
            let code = room.code.clone();
            let _ = self
                .update_room(&code, |room| {
                    if let Some(outcome) = room.outcome.as_mut() {
                        outcome.replay_share_slug = Some(slug.clone());
                    }
                    room.next_replay_counter = counter;
                    Ok(())
                })
                .await;
            let _ = self
                .publish_event(RedisMultiplayerEvent::RoomChanged {
                    code,
                    excluded_connection_id: None,
                })
                .await;
        }
        errors
    }

    async fn schedule_clock_deadline(&self, room: &RedisRoomSnapshot) -> Result<(), String> {
        if let Some(deadline) = room.clock.timeout_deadline_ms() {
            self.client
                .command_owned(vec![
                    "ZADD".into(),
                    self.deadlines_key(),
                    deadline.to_string(),
                    room.code.clone(),
                ])
                .await?;
        } else {
            self.client
                .command_owned(vec!["ZREM".into(), self.deadlines_key(), room.code.clone()])
                .await?;
        }
        Ok(())
    }

    fn start_clock_worker(store: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(1000)).await;
                let now = current_unix_ms().to_string();
                let codes = store
                    .client
                    .command_owned(vec![
                        "ZRANGEBYSCORE".into(),
                        store.deadlines_key(),
                        "-inf".into(),
                        now,
                        "LIMIT".into(),
                        "0".into(),
                        "16".into(),
                    ])
                    .await
                    .and_then(|value| value.array())
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                for code in codes {
                    if let Ok(code) = code.string() {
                        store.handle_clock_deadline(&code).await;
                    }
                }
            }
        });
    }

    async fn handle_clock_deadline(&self, code: &str) {
        let updated = self
            .update_room(code, |room| {
                if room.is_finished() {
                    return Ok(false);
                }
                if !room.clock.tick_active() {
                    return Ok(false);
                }
                if let Some(winner) = room.clock.winner {
                    room.outcome = Some(RedisRoomOutcome {
                        winner: Some(winner),
                        reason: "timeout".into(),
                        replay_share_slug: None,
                    });
                    room.touch();
                    return Ok(true);
                }
                Ok(false)
            })
            .await;
        let Ok((timed_out, room)) = updated else {
            let _ = self
                .client
                .command_owned(vec!["ZREM".into(), self.deadlines_key(), code.into()])
                .await;
            return;
        };
        let _ = self.schedule_clock_deadline(&room).await;
        if timed_out {
            let _ = self.save_finished_room_replay_once(&room.code, false).await;
            let _ = self
                .publish_event(RedisMultiplayerEvent::RoomChanged {
                    code: room.code.clone(),
                    excluded_connection_id: None,
                })
                .await;
            let _ = self
                .publish_event(RedisMultiplayerEvent::LobbyChanged)
                .await;
        }
    }

    fn start_cleanup_worker(store: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(30_000)).await;
                store.cleanup_empty_rooms().await;
            }
        });
    }

    async fn cleanup_empty_rooms(&self) {
        let Ok(Some(codes)) = self.room_codes().await else {
            return;
        };
        for code in codes {
            let Ok(Some(room)) = self.load_room(&code).await else {
                let _ = self
                    .client
                    .command_owned(vec!["SREM".into(), self.rooms_key(), code])
                    .await;
                continue;
            };
            let idle_ms = current_unix_ms().saturating_sub(room.last_activity_ms);
            if idle_ms < MULTIPLAYER_EMPTY_ROOM_GRACE_MS {
                continue;
            }
            if self.connected_viewers(&room).await > 0 {
                continue;
            }
            let _ = self
                .client
                .command_owned(vec!["DEL".into(), self.room_key(&room.code)])
                .await;
            let _ = self
                .client
                .command_owned(vec!["SREM".into(), self.rooms_key(), room.code.clone()])
                .await;
            let _ = self
                .client
                .command_owned(vec!["ZREM".into(), self.deadlines_key(), room.code.clone()])
                .await;
            let _ = self
                .publish_event(RedisMultiplayerEvent::LobbyChanged)
                .await;
        }
    }

    fn schedule_analysis(&self, code: String, version: u64, mut session: GameSession<G>) {
        let client = self.client.clone();
        let channel = self.events_channel();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let Ok(root_wdl) = tokio::task::spawn_blocking(move || {
                session
                    .run_analysis_bar_search(SearchBudget::simulations(MULTIPLAYER_ANALYSIS_SIMS))
                    .unwrap_or_else(|_| session.root_wdl())
            })
            .await
            else {
                return;
            };
            let event = RedisMultiplayerEvent::MultiplayerAnalysis {
                code,
                version,
                root_wdl,
            };
            if let Ok(payload) = serde_json::to_string(&event) {
                let _ = client
                    .command_owned(vec!["PUBLISH".into(), channel, payload])
                    .await;
            }
        });
    }

    fn start_subscriber(store: Arc<Self>) {
        tokio::spawn(async move {
            loop {
                if let Err(message) = Self::subscriber_loop(Arc::clone(&store)).await {
                    tracing::warn!("Redis multiplayer subscriber disconnected: {message}");
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                }
            }
        });
    }

    async fn subscriber_loop(store: Arc<Self>) -> Result<(), String> {
        let mut conn = RedisConnection::connect(store.client.config.clone()).await?;
        conn.command_owned(vec!["SUBSCRIBE".into(), store.events_channel()])
            .await?;
        loop {
            let value = conn.read_value().await?;
            let Some(values) = value.array()? else {
                continue;
            };
            if values.len() < 3 {
                continue;
            }
            let mut iter = values.into_iter();
            let kind = iter.next().unwrap().string().unwrap_or_default();
            let _channel = iter.next();
            let payload = iter.next().and_then(|value| value.string().ok());
            if kind != "message" {
                continue;
            }
            let Some(payload) = payload else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<RedisMultiplayerEvent>(&payload) else {
                continue;
            };
            store.handle_event(event).await;
        }
    }

    async fn handle_event(&self, event: RedisMultiplayerEvent) {
        match event {
            RedisMultiplayerEvent::LobbyChanged => self.broadcast_lobby().await,
            RedisMultiplayerEvent::RoomChanged {
                code,
                excluded_connection_id,
            } => {
                self.broadcast_room_snapshot(&code, excluded_connection_id.as_deref(), false)
                    .await
            }
            RedisMultiplayerEvent::GameChanged {
                code,
                excluded_connection_id,
            } => {
                self.broadcast_room_snapshot(&code, excluded_connection_id.as_deref(), true)
                    .await
            }
            RedisMultiplayerEvent::KickConnection { connection_id } => {
                let socket = self
                    .local_sockets
                    .lock()
                    .expect("redis local socket lock poisoned")
                    .remove(&connection_id);
                if let Some(socket) = socket {
                    if let Ok(json) = serde_json::to_string(&ServerMsg::Error {
                        message: "This multiplayer connection was replaced by a newer login".into(),
                    }) {
                        let _ = socket.tx.send(json);
                    }
                }
            }
            RedisMultiplayerEvent::MultiplayerAnalysis {
                code,
                version,
                root_wdl,
            } => {
                let Ok(Some(room)) = self.load_room(&code).await else {
                    return;
                };
                if room.version != version {
                    return;
                }
                if let Ok(json) =
                    serde_json::to_string(&ServerMsg::MultiplayerAnalysis { root_wdl })
                {
                    let sockets = self.local_room_sockets(&code, None);
                    for socket in sockets {
                        let _ = socket.tx.send(json.clone());
                    }
                }
            }
        }
    }

    async fn broadcast_lobby(&self) {
        let Ok(json) = serde_json::to_string(&self.lobby_msg().await) else {
            return;
        };
        let mut sockets = self
            .lobby_sockets
            .lock()
            .expect("redis lobby socket lock poisoned");
        sockets.retain(|_, tx| tx.send(json.clone()).is_ok());
    }

    async fn broadcast_room_snapshot(
        &self,
        code: &str,
        excluded_connection_id: Option<&str>,
        include_state: bool,
    ) {
        let Ok(Some(room)) = self.load_room(code).await else {
            return;
        };
        let sockets = self.local_room_sockets(code, excluded_connection_id);
        for socket in sockets {
            let messages = self
                .room_messages_for_viewer(
                    &room,
                    socket.player,
                    Some(&socket.connection_id),
                    include_state,
                )
                .await;
            for msg in messages {
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.tx.send(json);
                }
            }
        }
    }

    fn local_room_sockets(
        &self,
        code: &str,
        excluded_connection_id: Option<&str>,
    ) -> Vec<RedisLocalSocket> {
        self.local_sockets
            .lock()
            .expect("redis local socket lock poisoned")
            .iter()
            .filter(|(connection_id, socket)| {
                socket.room_code == code && excluded_connection_id != Some(connection_id.as_str())
            })
            .map(|(_, socket)| socket.clone())
            .collect()
    }

    async fn publish_event(&self, event: RedisMultiplayerEvent) -> Result<(), String> {
        let payload = serde_json::to_string(&event).map_err(|e| format!("Redis event: {e}"))?;
        self.client
            .command_owned(vec!["PUBLISH".into(), self.events_channel(), payload])
            .await?;
        Ok(())
    }

    fn next_connection_id(&self) -> String {
        format!(
            "{}:{}:{}",
            self.process_id,
            current_unix_ms(),
            fastrand::u64(..)
        )
    }

    fn room_key(&self, code: &str) -> String {
        format!("{}:room:{code}", self.key_prefix)
    }

    fn rooms_key(&self) -> String {
        format!("{}:rooms", self.key_prefix)
    }

    fn deadlines_key(&self) -> String {
        format!("{}:clock_deadlines", self.key_prefix)
    }

    fn presence_key(&self, connection_id: &str) -> String {
        format!("{}:presence:{connection_id}", self.key_prefix)
    }

    fn events_channel(&self) -> String {
        format!("{}:events", self.key_prefix)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use crate::{
        eval::{Evaluation, Evaluator},
        game::{Game, Status},
    };

    use super::super::GamePresenter;
    use super::*;

    #[derive(Clone)]
    struct TestGame {
        moves: usize,
    }

    impl Game for TestGame {
        const NUM_ACTIONS: usize = 1;

        fn status(&self) -> Status {
            if self.moves >= 2 {
                Status::Terminal(1.0)
            } else {
                Status::Decision(if self.moves % 2 == 0 { 1.0 } else { -1.0 })
            }
        }

        fn legal_actions(&self, actions: &mut Vec<usize>) {
            if self.moves < 2 {
                actions.push(0);
            }
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

    struct TestPresenter;

    impl GamePresenter<TestGame> for TestPresenter {
        fn serialize_state(&self, state: &TestGame) -> serde_json::Value {
            json!({ "moves": state.moves })
        }

        fn action_label(&self, _state: &TestGame, action: usize) -> String {
            format!("Action {action}")
        }

        fn phase_label(&self, _state: &TestGame) -> String {
            "test".into()
        }

        fn serialize_log_state(&self, state: &TestGame) -> Option<String> {
            Some(state.moves.to_string())
        }

        fn deserialize_log_state(&self, text: &str) -> Result<TestGame, String> {
            Ok(TestGame {
                moves: text.parse().map_err(|e| format!("{e}"))?,
            })
        }

        fn static_dir(&self) -> &std::path::Path {
            std::path::Path::new(".")
        }

        fn new_game(&self, _seed: u64) -> TestGame {
            TestGame { moves: 0 }
        }
    }

    #[test]
    fn redis_snapshot_rebuilds_game_session() {
        let factory = Arc::new(SessionFactory::new_game(
            Arc::new(TestEvaluator),
            "test",
            Arc::new(TestPresenter),
            [true, true],
            None,
        ));
        let store = RedisMultiplayerRoomStore {
            factory,
            replay_store: None,
            client: RedisClient::new(RedisConfig {
                host: "127.0.0.1".into(),
                port: 6379,
                password: None,
                db: None,
            }),
            key_prefix: REDIS_KEY_PREFIX.into(),
            process_id: "test".into(),
            local_sockets: StdMutex::new(HashMap::new()),
            lobby_sockets: StdMutex::new(HashMap::new()),
            next_lobby_socket_id: AtomicU64::new(1),
        };
        let room = RedisRoomSnapshot::new(
            "ABCD".into(),
            1,
            "0".into(),
            RedisSeatOwner {
                user_id: "user_a".into(),
                account_key: "a".into(),
                display_name: Some("A".into()),
            },
            0,
            true,
            None,
            None,
        );
        let mut room = room;
        room.actions = vec![0];
        let session = store.session_from_snapshot(&room).unwrap();
        match session.state_msg_for_player(1) {
            ServerMsg::GameState { state, .. } => {
                assert_eq!(state["moves"], json!(1));
            }
            _ => panic!("expected game state"),
        }
    }

    #[test]
    fn redis_multiplayer_integration_create_join_across_stores() {
        let Ok(redis_url) = std::env::var("HEXFISH_TEST_REDIS_URL") else {
            return;
        };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let factory = Arc::new(SessionFactory::new_game(
                Arc::new(TestEvaluator),
                "test",
                Arc::new(TestPresenter),
                [true, true],
                None,
            ));
            let store_a =
                RedisMultiplayerRoomStore::connect(&redis_url, Arc::clone(&factory), None)
                    .await
                    .unwrap();
            let store_b =
                RedisMultiplayerRoomStore::connect(&redis_url, Arc::clone(&factory), None)
                    .await
                    .unwrap();
            let code = format!("T{:05}", fastrand::u32(0..100_000));
            let (tx_a, _rx_a) = mpsc::unbounded_channel();
            let (tx_b, _rx_b) = mpsc::unbounded_channel();
            let mut active_a = None;
            let mut active_b = None;

            let created = store_a
                .handle_message_for_user(
                    "user_a",
                    Some("Alice"),
                    &tx_a,
                    &mut active_a,
                    super::super::ClientMsg::CreateMultiplayerRoom {
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

            let joined = store_b
                .handle_message_for_user(
                    "user_b",
                    Some("Bob"),
                    &tx_b,
                    &mut active_b,
                    super::super::ClientMsg::JoinMultiplayerRoom { code: code.clone() },
                )
                .await;
            match joined.as_slice() {
                [
                    ServerMsg::MultiplayerRoom { players, .. },
                    ServerMsg::GameState { .. },
                    ..,
                ] => {
                    assert_eq!(players.iter().filter(|player| player.occupied).count(), 2);
                }
                other => panic!("expected room + state, got {other:?}"),
            }

            let room = store_a.load_room(&code).await.unwrap().unwrap();
            assert_eq!(room.seat_for_user("user_a"), Some(0));
            assert_eq!(room.seat_for_user("user_b"), Some(1));

            let _ = store_a
                .client
                .command_owned(vec!["DEL".into(), store_a.room_key(&code)])
                .await;
            let _ = store_a
                .client
                .command_owned(vec!["SREM".into(), store_a.rooms_key(), code])
                .await;
        });
    }
}

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Snapshot types (wire format for the JS frontend) ────────────────

/// Root-level search snapshot sent to the frontend.
#[derive(Debug, Serialize)]
pub struct SearchSnapshot {
    pub total_simulations: u32,
    pub fresh_simulations: u32,
    pub pv_depth: u32,
    pub root_wdl: [f32; 3],
    pub network_value: f32,
    pub edges: Vec<EdgeSnapshot>,
}

/// Per-edge data in a search snapshot.
#[derive(Debug, Serialize)]
pub struct EdgeSnapshot {
    pub action: usize,
    pub visits: u32,
    pub fresh_visits: u32,
    pub q: Option<f32>,
    pub improved_policy: f32,
    pub depth: Option<u32>,
}

/// Recursive tree node for the subtree explorer.
#[derive(Debug, Serialize)]
pub struct TreeNodeSnapshot {
    pub action: Option<usize>,
    pub label: Option<String>,
    pub kind: String,
    pub visits: u32,
    pub wdl: [f32; 3],
    pub player: Option<u8>,
    pub children: Vec<TreeNodeSnapshot>,
}

/// A saved replay available to the current web user.
#[derive(Debug, Serialize)]
pub struct ReplayEntry {
    pub id: String,
    pub saved_at_ms: u64,
    pub action_count: usize,
    pub result: String,
    pub favorite: bool,
    pub share_slug: String,
}

/// Replay metadata for the currently loaded game state.
#[derive(Clone, Debug, Serialize)]
pub struct ReplayState {
    pub id: String,
    pub cursor: usize,
    pub len: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub player_names: Option<[String; 2]>,
}

/// Seat summary for an invite-code multiplayer room.
#[derive(Clone, Debug, Serialize)]
pub struct MultiplayerPlayer {
    pub occupied: bool,
    pub connected: bool,
    pub you: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub time_millis: Option<u64>,
    pub clock_active: bool,
}

/// Spectator summary for an invite-code multiplayer room.
#[derive(Clone, Debug, Serialize)]
pub struct MultiplayerSpectator {
    pub you: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One player-to-player chat message scoped to a live multiplayer room.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MultiplayerChatMessage {
    pub id: u64,
    pub sent_at_ms: u64,
    pub player: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub text: String,
}

/// Public lobby summary for an invite-code multiplayer room.
#[derive(Clone, Debug, Serialize)]
pub struct MultiplayerLobbyRoom {
    pub code: String,
    pub status: String,
    pub occupied: u8,
    pub connected: u8,
    pub spectator_count: u8,
    pub is_public: bool,
    pub time_minutes: Option<u32>,
    pub increment_seconds: Option<u32>,
    pub last_activity_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_room_closes_at_ms: Option<u64>,
}

/// Which board state a read-only analysis request should run against.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ViewTarget {
    Analysis,
    Replay,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum SearchBudget {
    Simulations {
        value: u32,
    },
    PvDepth {
        value: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        simulations: Option<u32>,
    },
}

impl SearchBudget {
    pub fn simulations(value: u32) -> Self {
        Self::Simulations { value }
    }

    pub fn pv_depth(value: u32) -> Self {
        Self::PvDepth {
            value,
            simulations: None,
        }
    }

    pub fn pv_depth_with_simulations(value: u32, simulations: u32) -> Self {
        Self::PvDepth {
            value,
            simulations: Some(simulations),
        }
    }
}

// ── Client → Server ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMsg {
    /// Authenticate the WebSocket before any game state is sent.
    Authenticate {
        token: String,
        anonymous_session: Option<String>,
        #[serde(default)]
        clerk_user_id: Option<String>,
        #[serde(default)]
        username: Option<String>,
    },
    /// Lightweight keepalive used by the browser to detect stale sockets.
    Ping,
    /// Start a new game (optionally with a seed).
    NewGame { seed: Option<u64> },
    /// Start a game from a browser-edited board layout.
    StartEditedGame {
        terrains: Vec<String>,
        numbers: Vec<Option<u8>>,
        port_layout: Option<String>,
        ports: Option<Vec<String>>,
    },
    /// List saved game logs for this authenticated web session.
    ListReplays,
    /// Load a saved replay by id.
    LoadReplay { id: String },
    /// Load an unlisted shared replay by slug.
    LoadSharedReplay { slug: String },
    /// Delete a saved replay by id.
    DeleteReplay { id: String },
    /// Toggle whether a saved replay is favourited.
    SetReplayFavorite { id: String, favorite: bool },
    /// Save the current live game replay without starting a new game.
    SaveReplay,
    /// Read the current profile for this web session.
    GetProfile,
    /// Legacy username setter. Usernames are managed by Clerk.
    SetUsername { username: String },
    /// Jump to a replay cursor (0..=len).
    SetReplayCursor { cursor: usize },
    /// Human plays an action.
    PlayAction { action: usize },
    /// Resign the current singleplayer game and award the win to the opponent.
    ResignGame,
    /// Create an invite-code multiplayer room.
    CreateMultiplayerRoom {
        preferred_player: Option<u8>,
        code: Option<String>,
        is_public: Option<bool>,
        time_minutes: Option<u32>,
        increment_seconds: Option<u32>,
    },
    /// List visible invite-code multiplayer rooms.
    ListMultiplayerRooms,
    /// Refresh this socket's current multiplayer room state.
    GetMultiplayerRoom,
    /// Join an existing invite-code multiplayer room.
    JoinMultiplayerRoom { code: String },
    /// Leave the current multiplayer room on this socket.
    LeaveMultiplayerRoom,
    /// Human plays an action in the current multiplayer room.
    PlayMultiplayerAction { action: usize },
    /// Send a player-only chat message in the current multiplayer room.
    SendMultiplayerChat { text: String },
    /// Resign the current multiplayer game and award the win to the opponent.
    ResignMultiplayerGame,
    /// Add a small clock bonus to the opponent in the current multiplayer room.
    AddMultiplayerOpponentTime,
    /// Request the bot to play an action.
    BotMove {
        simulations: Option<u32>,
        budget: Option<SearchBudget>,
    },
    /// Run N additional simulations on current state (step debugger).
    RunSims {
        count: u32,
        target: Option<ViewTarget>,
    },
    /// Run a search with an explicit budget mode.
    RunSearch {
        budget: SearchBudget,
        target: Option<ViewTarget>,
    },
    /// Stop an in-progress analysis search, preserving partial results.
    PauseSearch { target: Option<ViewTarget> },
    /// Request current search snapshot.
    GetSnapshot,
    /// Explore a subtree by following an action path.
    ExploreSubtree {
        action_path: Vec<usize>,
        depth: usize,
        target: Option<ViewTarget>,
    },
    /// Take over control of a player (human overrides bot).
    TakeOver { player: u8 },
    /// Release control back to bot.
    ReleaseControl { player: u8 },
    /// Toggle autoplay mode.
    SetAutoplay {
        enabled: bool,
        delay_ms: Option<u64>,
    },
    /// Configure singleplayer mode. `None` disables singleplayer-specific UI rules.
    SetSingleplayer {
        human_player: Option<u8>,
        #[serde(default)]
        bot_level: Option<u8>,
    },
    /// Poll external state (e.g. colonist.io CDP). Default: returns current state.
    PollState,
    /// Request current game state.
    GetState,
    /// Undo last action.
    Undo,
    /// Redo previously undone action.
    Redo,
    /// Jump to a raw history cursor (0..=history.len()) from a log entry.
    SetLogCursor { cursor: usize },
    /// Configure per-player settings.
    SetConfig { player: u8, simulations: u32 },
    /// Enable/disable continuous background search with a search budget.
    SetAutoSearch {
        enabled: bool,
        target: Option<u32>,
        budget: Option<SearchBudget>,
    },
}

// ── Server → Client ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ServerMsg {
    /// Full game state update.
    GameState {
        state: Value,
        legal_actions: Vec<ActionInfo>,
        current_player: u8,
        phase: String,
        is_chance: bool,
        is_terminal: bool,
        result: Option<String>,
        action_log: Vec<String>,
        action_log_sound_kinds: Vec<String>,
        history_cursor: usize,
        action_log_cursors: Vec<usize>,
        can_undo: bool,
        can_redo: bool,
        replay: Option<ReplayState>,
    },
    /// Saved replays for the current web user.
    ReplayList { entries: Vec<ReplayEntry> },
    /// Current user profile for this web session.
    Profile {
        username: Option<String>,
        username_set: bool,
    },
    /// A live game replay was saved and can be shared.
    ReplaySaved { entry: ReplayEntry },
    /// Current invite-code multiplayer room state for this socket.
    MultiplayerRoom {
        code: String,
        status: String,
        viewer_role: String,
        local_player: Option<u8>,
        players: Vec<MultiplayerPlayer>,
        spectators: Vec<MultiplayerSpectator>,
        time_minutes: Option<u32>,
        increment_seconds: Option<u32>,
        winner: Option<u8>,
        finish_reason: Option<String>,
        replay_share_slug: Option<String>,
    },
    /// Public multiplayer lobby room list.
    MultiplayerLobby { rooms: Vec<MultiplayerLobbyRoom> },
    /// Public multiplayer analysis bar update. Contains no action policy details.
    MultiplayerAnalysis { root_wdl: [f32; 3] },
    /// Player-only chat history for the current multiplayer room.
    MultiplayerChat {
        messages: Vec<MultiplayerChatMessage>,
    },
    /// MCTS search snapshot.
    Snapshot {
        snapshot: SearchSnapshot,
        action_labels: Vec<String>,
    },
    /// Subtree exploration result (labels embedded in tree nodes).
    Subtree { tree: TreeNodeSnapshot },
    /// Bot played an action.
    BotAction {
        action: usize,
        label: String,
        snapshot: Option<SearchSnapshot>,
        action_labels: Vec<String>,
    },
    /// Live search progress update.
    SearchProgress {
        snapshot: SearchSnapshot,
        action_labels: Vec<String>,
        sims_total: u32,
        budget: SearchBudget,
        cpu_load: Option<u8>,
    },
    /// Lightweight keepalive response.
    Pong,
    /// Error message.
    Error { message: String },
}

#[derive(Debug, Serialize)]
pub struct ActionInfo {
    pub action: usize,
    pub label: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplayer_chat_protocol_round_trips_client_and_server_shapes() {
        let client: ClientMsg =
            serde_json::from_str(r#"{"type":"SendMultiplayerChat","text":"hello"}"#)
                .expect("chat client message");
        match client {
            ClientMsg::SendMultiplayerChat { text } => assert_eq!(text, "hello"),
            other => panic!("expected SendMultiplayerChat, got {other:?}"),
        }

        let server = ServerMsg::MultiplayerChat {
            messages: vec![MultiplayerChatMessage {
                id: 7,
                sent_at_ms: 1234,
                player: 1,
                name: Some("Bob".into()),
                text: "hi".into(),
            }],
        };
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&server).unwrap()).unwrap();
        assert_eq!(value["type"], "MultiplayerChat");
        assert_eq!(value["messages"][0]["id"], 7);
        assert_eq!(value["messages"][0]["player"], 1);
        assert_eq!(value["messages"][0]["name"], "Bob");
        assert_eq!(value["messages"][0]["text"], "hi");
    }
}

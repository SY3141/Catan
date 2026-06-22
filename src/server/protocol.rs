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
    pub favorite: bool,
    pub share_slug: String,
}

/// Replay metadata for the currently loaded game state.
#[derive(Clone, Debug, Serialize)]
pub struct ReplayState {
    pub id: String,
    pub cursor: usize,
    pub len: usize,
}

/// Seat summary for an invite-code multiplayer room.
#[derive(Clone, Debug, Serialize)]
pub struct MultiplayerPlayer {
    pub occupied: bool,
    pub connected: bool,
    pub you: bool,
}

/// Public lobby summary for an invite-code multiplayer room.
#[derive(Clone, Debug, Serialize)]
pub struct MultiplayerLobbyRoom {
    pub code: String,
    pub status: String,
    pub occupied: u8,
    pub connected: u8,
    pub last_activity_ms: u64,
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
    },
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
    /// Jump to a replay cursor (0..=len).
    SetReplayCursor { cursor: usize },
    /// Human plays an action.
    PlayAction { action: usize },
    /// Create an invite-code multiplayer room.
    CreateMultiplayerRoom { preferred_player: Option<u8> },
    /// List visible invite-code multiplayer rooms.
    ListMultiplayerRooms,
    /// Join an existing invite-code multiplayer room.
    JoinMultiplayerRoom { code: String },
    /// Leave the current multiplayer room on this socket.
    LeaveMultiplayerRoom,
    /// Human plays an action in the current multiplayer room.
    PlayMultiplayerAction { action: usize },
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
    SetSingleplayer { human_player: Option<u8> },
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
        history_cursor: usize,
        action_log_cursors: Vec<usize>,
        can_undo: bool,
        can_redo: bool,
        replay: Option<ReplayState>,
    },
    /// Saved replays for the current web user.
    ReplayList { entries: Vec<ReplayEntry> },
    /// Current invite-code multiplayer room state for this socket.
    MultiplayerRoom {
        code: String,
        status: String,
        local_player: Option<u8>,
        players: Vec<MultiplayerPlayer>,
    },
    /// Public multiplayer lobby room list.
    MultiplayerLobby { rooms: Vec<MultiplayerLobbyRoom> },
    /// Public multiplayer analysis bar update. Contains no action policy details.
    MultiplayerAnalysis { root_wdl: [f32; 3] },
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
    /// Error message.
    Error { message: String },
}

#[derive(Debug, Serialize)]
pub struct ActionInfo {
    pub action: usize,
    pub label: String,
}

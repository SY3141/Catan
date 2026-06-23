use std::path::Path;

use crate::game::Game;

/// Game-specific presentation layer for the web analysis board.
///
/// The framework doesn't require `Game` to implement `Serialize` — instead,
/// each game provides a `GamePresenter` that knows how to produce JSON state,
/// human-readable action labels, and phase descriptions.
pub trait GamePresenter<G: Game>: Send + Sync {
    /// Serialize the game state to a JSON value for the frontend.
    fn serialize_state(&self, state: &G) -> serde_json::Value;

    /// Serialize state for a specific player perspective.
    ///
    /// Defaults to the full analysis view. Games with private information can
    /// override this to redact opponent-only data for multiplayer.
    fn serialize_state_for_player(&self, state: &G, _player: usize) -> serde_json::Value {
        self.serialize_state(state)
    }

    /// Human-readable label for an action in the given state (includes player prefix).
    fn action_label(&self, state: &G, action: usize) -> String;

    /// Legal actions exposed to a singleplayer human.
    ///
    /// Defaults to the engine action generator. Games may override this for
    /// UI-only relaxations that should not affect MCTS/training legal actions.
    fn human_legal_actions(&self, state: &G, actions: &mut Vec<usize>) {
        state.legal_actions(actions);
    }

    /// Game-specific actions that should not be undone in singleplayer.
    fn is_singleplayer_undo_barrier(&self, _state: &G, _action: usize) -> bool {
        false
    }

    /// Action description without player prefix (for tree explorer where
    /// the acting player varies by depth).
    fn action_description(&self, state: &G, action: usize) -> String {
        self.action_label(state, action)
    }

    /// Human-readable label for a chance outcome (dice roll, random steal, etc.).
    /// Return empty string to omit from the game log.
    fn chance_label(&self, _state: &G, _outcome: usize) -> String {
        String::new()
    }

    /// Redact or rewrite an existing game-log label for a player perspective.
    ///
    /// The default preserves the analysis/replay label. Multiplayer presenters
    /// with private information can hide opponent-only card identities here.
    fn action_log_label_for_player(
        &self,
        _state: &G,
        _action: usize,
        _is_chance: bool,
        label: &str,
        _player: usize,
    ) -> String {
        label.to_string()
    }

    /// Human-readable label for the current phase.
    fn phase_label(&self, state: &G) -> String;

    /// Stable fingerprint for the initial board layout, if the game has one.
    fn board_fingerprint(&self, _state: &G) -> Option<u64> {
        None
    }

    /// String form to place in the first line of a replay log.
    fn serialize_log_state(&self, _state: &G) -> Option<String> {
        None
    }

    /// Parse the first line of a replay log into an initial state.
    fn deserialize_log_state(&self, _text: &str) -> Result<G, String> {
        Err("this game presenter does not support web replay logs".into())
    }

    /// Rewrite a replay action list into the engine's canonical replay order.
    ///
    /// The default preserves actions exactly. Games with UI-relaxed action
    /// ordering can override this so saved logs validate against search legal
    /// actions when they are loaded later.
    fn normalize_replay_actions(&self, _initial_state: &G, actions: &[usize]) -> Vec<usize> {
        actions.to_vec()
    }

    /// Directory containing static web assets (index.html, JS, CSS).
    fn static_dir(&self) -> &Path;

    /// Create a new game from a seed.
    fn new_game(&self, seed: u64) -> G;

    /// Create a new game from a browser-edited board layout.
    fn new_game_from_editor(
        &self,
        _terrains: &[String],
        _numbers: &[Option<u8>],
        _port_layout: Option<&str>,
        _ports: Option<&[String]>,
    ) -> Result<G, String> {
        Err("this game presenter does not support edited boards".into())
    }
}

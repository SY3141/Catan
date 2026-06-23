use std::sync::Arc;

use crate::eval::Evaluator;
use crate::game::{Game, Status};
use crate::game_log::GameLog;
use crate::mcts::{Config, NodeId, NodeKind, Search, SearchResult, Select, Tree};

use super::protocol::{
    ActionInfo, ClientMsg, EdgeSnapshot, ReplayState, SearchBudget, SearchSnapshot, ServerMsg,
    TreeNodeSnapshot,
};
use super::traits::GamePresenter;

pub const MAX_PV_DEPTH_BUDGET: u32 = 30;
pub const PV_DEPTH_SIM_SAFETY_CAP: u32 = 100_000;

#[derive(Clone, Copy, Debug)]
pub struct ActiveSearchBudget {
    pub budget: SearchBudget,
    pub sims_total: u32,
}

/// Per-player configuration.
struct PlayerConfig {
    simulations: u32,
    /// Whether this player is human-controlled.
    human: bool,
}

/// A checkpoint in the game timeline (player decision or chance outcome).
struct HistoryEntry<G> {
    state: G,        // state before this action
    action: usize,   // action taken from this state
    label: String,   // human-readable label (empty for unlabeled)
    is_chance: bool, // true for auto-resolved chance outcomes
    /// Pre-computed next state for timeline entries where the action can't be
    /// replayed through `apply_action` (e.g. colonist replay snapshots).
    next_state: Option<G>,
}

struct ReplayMode {
    id: String,
}

/// Owns the game state, search tree, evaluator, and presenter.
/// Processes client messages and produces server responses.
pub struct GameSession<G: Game> {
    search: Search<G>,
    evaluator: Arc<dyn Evaluator<G> + Sync>,
    #[allow(dead_code)]
    eval_name: String,
    presenter: Arc<dyn GamePresenter<G>>,
    rng: fastrand::Rng,
    configs: [PlayerConfig; 2],
    history: Vec<HistoryEntry<G>>,
    cursor: usize, // 0..=history.len(), points past the last applied entry
    seed: u64,
    /// Last ExploreSubtree path, used to proactively send tree updates during search.
    last_explore: Option<(Vec<usize>, usize)>,
    replay: Option<ReplayMode>,
    saved_live_log: Option<GameLog>,
    singleplayer_human_player: Option<usize>,
}

impl<G: Game + 'static> GameSession<G> {
    pub fn new(
        evaluator: Arc<dyn Evaluator<G> + Sync>,
        eval_name: impl Into<String>,
        presenter: Arc<dyn GamePresenter<G>>,
        human_players: [bool; 2],
    ) -> Self {
        Self::with_config(
            evaluator,
            eval_name,
            presenter,
            human_players,
            Config::default(),
        )
    }

    pub fn with_config(
        evaluator: Arc<dyn Evaluator<G> + Sync>,
        eval_name: impl Into<String>,
        presenter: Arc<dyn GamePresenter<G>>,
        human_players: [bool; 2],
        config: Config,
    ) -> Self {
        let seed = fastrand::u64(..);
        let state = presenter.new_game(seed);
        let search = Search::new(state, config);

        Self {
            search,
            evaluator,
            eval_name: eval_name.into(),
            presenter,
            rng: fastrand::Rng::new(),
            configs: [
                PlayerConfig {
                    simulations: 400,
                    human: human_players[0],
                },
                PlayerConfig {
                    simulations: 400,
                    human: human_players[1],
                },
            ],
            history: Vec::new(),
            cursor: 0,
            seed,
            last_explore: None,
            replay: None,
            saved_live_log: None,
            singleplayer_human_player: None,
        }
    }

    /// Create a session with an explicit initial state and MCTS config.
    pub fn with_state(
        state: G,
        evaluator: Arc<dyn Evaluator<G> + Sync>,
        eval_name: impl Into<String>,
        presenter: Arc<dyn GamePresenter<G>>,
        human_players: [bool; 2],
        config: Config,
    ) -> Self {
        let search = Search::new(state, config);

        Self {
            search,
            evaluator,
            eval_name: eval_name.into(),
            presenter,
            rng: fastrand::Rng::new(),
            configs: [
                PlayerConfig {
                    simulations: 400,
                    human: human_players[0],
                },
                PlayerConfig {
                    simulations: 400,
                    human: human_players[1],
                },
            ],
            history: Vec::new(),
            cursor: 0,
            seed: 0,
            last_explore: None,
            replay: None,
            saved_live_log: None,
            singleplayer_human_player: None,
        }
    }

    /// Load a recorded game log for replay.
    ///
    /// Resets the game to `initial_state`, replays all actions into history
    /// (recording labels and chance detection), then rewinds cursor to 0 so
    /// the UI starts at the beginning with full redo available.
    pub fn load_replay(&mut self, initial_state: G, log: &GameLog) {
        self.search.reset(initial_state.clone());
        self.history.clear();
        self.cursor = 0;
        self.replay = None;
        self.saved_live_log = None;

        // Replay all actions into history.
        for &action in &log.actions {
            self.apply_action(action);
        }

        // Rewind: reset to initial state, keep history for redo.
        self.cursor = 0;
        self.search.reset(initial_state);
    }

    /// Load a saved replay and mark the session as replay-mode.
    pub fn load_saved_replay(&mut self, id: impl Into<String>, initial_state: G, log: &GameLog) {
        self.load_replay(initial_state, log);
        self.replay = Some(ReplayMode { id: id.into() });
    }

    /// Parse and load a saved replay log through the presenter codec.
    pub fn load_saved_replay_log(
        &mut self,
        id: impl Into<String>,
        log: &GameLog,
    ) -> Result<(), String> {
        let initial_state = self.presenter.deserialize_log_state(&log.initial_state)?;
        validate_replay_log(initial_state.clone(), log)?;
        self.load_saved_replay(id, initial_state, log);
        Ok(())
    }

    /// Export the currently visible live game prefix as a persistent game log.
    pub fn export_current_log(&self) -> Option<GameLog> {
        if self.replay.is_some() || self.cursor == 0 {
            return None;
        }
        let first = self.history.first()?;
        let initial_state = self.presenter.serialize_log_state(&first.state)?;
        let actions = self.history[..self.cursor]
            .iter()
            .map(|entry| entry.action)
            .collect();
        Some(GameLog {
            initial_state,
            actions,
        })
    }

    /// Export the visible live game prefix unless that exact log was already saved.
    pub fn export_unsaved_current_log(&self) -> Option<GameLog> {
        let log = self.export_current_log()?;
        let already_saved = match &self.saved_live_log {
            Some(saved) => saved.initial_state == log.initial_state && saved.actions == log.actions,
            None => false,
        };
        if already_saved { None } else { Some(log) }
    }

    /// Remember that the given live log has been persisted.
    pub fn mark_current_log_saved(&mut self, log: &GameLog) {
        self.saved_live_log = Some(log.clone());
    }

    /// Load an externally-built timeline (e.g. from colonist.io replay).
    ///
    /// Each entry is `(label, state)`. The first entry is the initial state;
    /// subsequent entries are states after events described by their label.
    /// Redo navigates forward using `next_state` (no `apply_action`).
    pub fn load_timeline(&mut self, timeline: Vec<(String, G)>) {
        if timeline.is_empty() {
            return;
        }
        self.search.reset(timeline[0].1.clone());
        self.history.clear();
        self.cursor = 0;

        for i in 0..timeline.len() - 1 {
            self.history.push(HistoryEntry {
                state: timeline[i].1.clone(),
                action: usize::MAX, // sentinel — not a real game action
                label: timeline[i + 1].0.clone(),
                is_chance: false,
                next_state: Some(timeline[i + 1].1.clone()),
            });
        }
    }

    /// Walk the MCTS tree pointer through a sequence of actions.
    ///
    /// Only advances the tree pointer (preserving accumulated visits) —
    /// does NOT mutate `root_state`. The caller must follow up with
    /// `set_final_state` to set the authoritative game state.
    /// Only walks when viewing the live position (cursor at end).
    pub fn walk_tree(&mut self, actions: &[usize]) -> usize {
        if self.cursor != self.history.len() {
            return 0;
        }
        self.search.walk_tree(actions)
    }

    /// Append new timeline entries from live polling.
    ///
    /// Each entry is `(label, state)` where the first state is the successor
    /// of the current last timeline state. If the cursor was at the end (user
    /// watching live), auto-advances to the new end. Returns true if entries
    /// were added.
    pub fn extend_timeline(&mut self, new_entries: Vec<(String, G)>) -> bool {
        if new_entries.is_empty() {
            return false;
        }

        let was_at_end = self.cursor == self.history.len();

        // The previous "final state" is the next_state of the last history entry,
        // or the search state if history is empty.
        let prev_final = self
            .history
            .last()
            .and_then(|e| e.next_state.clone())
            .unwrap_or_else(|| self.search.state().clone());

        // First new entry: prev_final -> new_entries[0]
        self.history.push(HistoryEntry {
            state: prev_final,
            action: usize::MAX,
            label: new_entries[0].0.clone(),
            is_chance: false,
            next_state: Some(new_entries[0].1.clone()),
        });

        // Subsequent entries chain together
        for i in 1..new_entries.len() {
            self.history.push(HistoryEntry {
                state: new_entries[i - 1].1.clone(),
                action: usize::MAX,
                label: new_entries[i].0.clone(),
                is_chance: false,
                next_state: Some(new_entries[i].1.clone()),
            });
        }

        // Auto-advance cursor if user was watching live.
        // The search state is managed by walk_tree + set_final_state,
        // so we only update the cursor here.
        if was_at_end {
            self.cursor = self.history.len();
        }

        true
    }

    /// Update the final timeline state in-place (e.g. robber, current turn).
    ///
    /// Applies a mutation to the last `next_state` in history. If cursor is
    /// at the end, also updates the search state.
    pub fn update_final_state(&mut self, f: impl Fn(&mut G)) {
        if let Some(last) = self.history.last_mut() {
            if let Some(ref mut ns) = last.next_state {
                f(ns);
            }
        }
        if self.cursor == self.history.len() {
            self.search.update_state(&f);
        }
    }

    /// Replace the final timeline state wholesale.
    ///
    /// Sets the last history entry's `next_state` and, if the cursor is at
    /// the end, updates the search root state (preserving the MCTS tree).
    pub fn set_final_state(&mut self, state: G) {
        if let Some(last) = self.history.last_mut() {
            last.next_state = Some(state.clone());
        }
        if self.cursor == self.history.len() {
            self.search.update_state(|s| *s = state);
        }
    }

    /// Replace the final timeline state and reset the search tree.
    ///
    /// Use when the state changed but the tree can't be walked to match
    /// (e.g. an action wasn't mapped to an engine action, or walk_tree
    /// failed partway). Discards accumulated search work.
    pub fn reset_to_state(&mut self, state: G) {
        if let Some(last) = self.history.last_mut() {
            last.next_state = Some(state.clone());
        }
        if self.cursor == self.history.len() {
            self.search.reset(state);
        }
    }

    /// Current cursor position in the history (0..=history.len()).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Advance cursor to the end of the timeline, setting the search state to
    /// the final replayed position. No-op if history is empty.
    pub fn seek_to_end(&mut self) {
        let _ = self.set_cursor(self.history.len());
    }

    /// Roll back to a previous cursor position, resetting the search state.
    ///
    /// Truncates history beyond `target` and sets the search state to the
    /// entry's `state` (the state *before* that entry's action).
    pub fn rollback_to_cursor(&mut self, target: usize) {
        if target > self.cursor {
            return;
        }
        self.history.truncate(target);
        self.cursor = target;
        if target > 0 {
            // Restore the state that was current at this cursor position.
            // The entry at target-1 has a next_state or we can replay from state.
            let entry = &self.history[target - 1];
            if let Some(ref next) = entry.next_state {
                self.search.reset(next.clone());
            } else {
                let mut state = entry.state.clone();
                state.apply_action(entry.action);
                self.search.reset(state);
            }
        } else if let Some(first) = self.history.first() {
            self.search.reset(first.state.clone());
        }
    }

    fn action_log_with_cursors(&self, perspective: Option<usize>) -> (Vec<String>, Vec<usize>) {
        let mut action_log = Vec::new();
        let mut action_log_cursors = Vec::new();
        for (i, entry) in self.history.iter().enumerate() {
            let label = match perspective {
                Some(player) => self.presenter.action_log_label_for_player(
                    &entry.state,
                    entry.action,
                    entry.is_chance,
                    &entry.label,
                    player,
                ),
                None => entry.label.clone(),
            };
            if label.is_empty() {
                continue;
            }
            action_log.push(label);
            action_log_cursors.push(i + 1);
        }
        (action_log, action_log_cursors)
    }

    fn undo_target_cursor(&self) -> Option<usize> {
        if self.cursor == 0 {
            return None;
        }

        let mut target = self.cursor;
        while target > 0 {
            target -= 1;
            if !self.history[target].is_chance {
                break;
            }
        }

        let entry = self.history.get(target)?;
        if entry.is_chance {
            return None;
        }

        if let Some(human_player) = self.singleplayer_human_player {
            if action_player_idx(&entry.state) != Some(human_player) {
                return None;
            }
            if self
                .presenter
                .is_singleplayer_undo_barrier(&entry.state, entry.action)
            {
                return None;
            }
        }

        Some(target)
    }

    /// Build a GameState server message for the current state (public for live push).
    pub fn state_msg(&self) -> ServerMsg {
        self.state_msg_with_perspective(None)
    }

    /// Build a GameState server message for one multiplayer player.
    pub fn state_msg_for_player(&self, player: usize) -> ServerMsg {
        self.state_msg_with_perspective(Some(player))
    }

    /// Create a lightweight analysis-only session from the current state.
    pub fn fork_analysis_session(&self) -> Self {
        Self::with_state(
            self.search.state().clone(),
            Arc::clone(&self.evaluator),
            self.eval_name.clone(),
            Arc::clone(&self.presenter),
            [true, true],
            Config::default(),
        )
    }

    fn state_msg_with_perspective(&self, perspective: Option<usize>) -> ServerMsg {
        let state = self.search.state();
        let is_terminal = matches!(state.status(), Status::Terminal(_));
        let is_chance = self.is_chance();

        let legal = if is_terminal || is_chance {
            Vec::new()
        } else if let Some(player) = perspective {
            if self.current_player_idx() == player {
                let mut actions = Vec::new();
                self.presenter.human_legal_actions(state, &mut actions);
                actions
                    .iter()
                    .map(|&a| ActionInfo {
                        action: a,
                        label: self.presenter.action_label(state, a),
                    })
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            let actions = self.legal_actions();
            actions
                .iter()
                .map(|&a| ActionInfo {
                    action: a,
                    label: self.presenter.action_label(state, a),
                })
                .collect()
        };

        let result = if let Status::Terminal(reward) = state.status() {
            Some(if reward > 0.0 {
                "P1 wins".into()
            } else if reward < 0.0 {
                "P2 wins".into()
            } else {
                "Draw".into()
            })
        } else {
            None
        };

        let (action_log, action_log_cursors) = self.action_log_with_cursors(perspective);

        ServerMsg::GameState {
            state: match perspective {
                Some(player) => self.presenter.serialize_state_for_player(state, player),
                None => self.presenter.serialize_state(state),
            },
            legal_actions: legal,
            current_player: self.current_player_idx() as u8,
            phase: self.presenter.phase_label(state),
            is_chance,
            is_terminal,
            result,
            action_log,
            history_cursor: self.cursor,
            action_log_cursors,
            can_undo: perspective.is_none() && self.undo_target_cursor().is_some(),
            can_redo: perspective.is_none() && self.cursor < self.history.len(),
            replay: self.replay.as_ref().map(|replay| ReplayState {
                id: replay.id.clone(),
                cursor: self.cursor,
                len: self.history.len(),
            }),
        }
    }

    /// Apply a human-owned action from a multiplayer seat.
    pub fn play_human_action(&mut self, player: usize, action: usize) -> Result<(), String> {
        if self.replay.is_some() {
            return Err("Cannot play actions while viewing a replay".into());
        }
        if self.is_terminal() {
            return Err("Game is over".into());
        }
        if self.current_player_idx() != player {
            return Err("It is not your turn".into());
        }
        let mut legal = Vec::new();
        self.presenter
            .human_legal_actions(self.search.state(), &mut legal);
        if !legal.contains(&action) {
            return Err(format!("Illegal action: {action}"));
        }
        self.apply_action(action);
        self.auto_resolve_chance();
        Ok(())
    }

    /// Process a client message and return response messages.
    pub fn handle(&mut self, msg: ClientMsg) -> Vec<ServerMsg> {
        match msg {
            ClientMsg::Authenticate { .. } => {
                vec![ServerMsg::Error {
                    message: "Already authenticated".into(),
                }]
            }
            ClientMsg::ListReplays
            | ClientMsg::LoadReplay { .. }
            | ClientMsg::LoadSharedReplay { .. }
            | ClientMsg::DeleteReplay { .. }
            | ClientMsg::SetReplayFavorite { .. }
            | ClientMsg::CreateMultiplayerRoom { .. }
            | ClientMsg::ListMultiplayerRooms
            | ClientMsg::JoinMultiplayerRoom { .. }
            | ClientMsg::LeaveMultiplayerRoom
            | ClientMsg::PlayMultiplayerAction { .. }
            | ClientMsg::AddMultiplayerOpponentTime => vec![ServerMsg::Error {
                message: "Replay storage is not available in this session".into(),
            }],
            ClientMsg::NewGame { seed } => {
                self.seed = seed.unwrap_or_else(|| fastrand::u64(..));
                let state = self.presenter.new_game(self.seed);
                self.search.reset(state);
                self.history.clear();
                self.cursor = 0;
                self.replay = None;
                self.saved_live_log = None;
                self.auto_resolve_chance();
                vec![self.state_msg()]
            }
            ClientMsg::StartEditedGame {
                terrains,
                numbers,
                port_layout,
                ports,
            } => {
                let state = match self.presenter.new_game_from_editor(
                    &terrains,
                    &numbers,
                    port_layout.as_deref(),
                    ports.as_deref(),
                ) {
                    Ok(state) => state,
                    Err(message) => return vec![ServerMsg::Error { message }],
                };
                self.seed = fastrand::u64(..);
                self.search.reset(state);
                self.history.clear();
                self.cursor = 0;
                self.replay = None;
                self.saved_live_log = None;
                self.auto_resolve_chance();
                vec![self.state_msg()]
            }
            ClientMsg::PollState | ClientMsg::GetState => {
                vec![self.state_msg()]
            }
            ClientMsg::PlayAction { action } => {
                if self.replay.is_some() {
                    return vec![ServerMsg::Error {
                        message: "Cannot play actions while viewing a replay".into(),
                    }];
                }
                if self.is_terminal() {
                    return vec![ServerMsg::Error {
                        message: "Game is over".into(),
                    }];
                }
                let legal = self.legal_actions();
                if !legal.contains(&action) {
                    return vec![ServerMsg::Error {
                        message: format!("Illegal action: {action}"),
                    }];
                }
                self.apply_action(action);
                self.auto_resolve_chance();
                vec![self.state_msg()]
            }
            ClientMsg::BotMove {
                simulations,
                budget,
            } => {
                if self.replay.is_some() {
                    return vec![ServerMsg::Error {
                        message: "Cannot apply bot moves while viewing a replay".into(),
                    }];
                }
                if self.is_terminal() {
                    return vec![ServerMsg::Error {
                        message: "Game is over".into(),
                    }];
                }
                if self.is_chance() {
                    return vec![ServerMsg::Error {
                        message: "Current state is a chance node".into(),
                    }];
                }
                let budget = self.bot_move_budget(budget, simulations);
                self.apply_search_budget(budget);

                let result = self.run_search();
                self.finish_bot_move(result)
            }
            ClientMsg::RunSims { count, .. } => {
                if self.is_terminal() || self.is_chance() {
                    return vec![ServerMsg::Error {
                        message: "Cannot run sims on chance/terminal state".into(),
                    }];
                }
                self.apply_search_budget(SearchBudget::simulations(count));
                let _result = self.run_search();
                match self.build_snapshot() {
                    Some(snap) => {
                        let labels = self.edge_labels(&snap.edges);
                        vec![ServerMsg::Snapshot {
                            snapshot: snap,
                            action_labels: labels,
                        }]
                    }
                    None => vec![ServerMsg::Error {
                        message: "No snapshot available".into(),
                    }],
                }
            }
            ClientMsg::RunSearch { budget, .. } => {
                if self.is_terminal() || self.is_chance() {
                    return vec![ServerMsg::Error {
                        message: "Cannot run search on chance/terminal state".into(),
                    }];
                }
                self.apply_search_budget(budget);
                let _result = self.run_search();
                match self.build_snapshot() {
                    Some(snap) => {
                        let labels = self.edge_labels(&snap.edges);
                        vec![ServerMsg::Snapshot {
                            snapshot: snap,
                            action_labels: labels,
                        }]
                    }
                    None => vec![ServerMsg::Error {
                        message: "No snapshot available".into(),
                    }],
                }
            }
            ClientMsg::PauseSearch { .. } => self.pause_search(),
            ClientMsg::GetSnapshot => match self.build_snapshot() {
                Some(snap) => {
                    let labels = self.edge_labels(&snap.edges);
                    vec![ServerMsg::Snapshot {
                        snapshot: snap,
                        action_labels: labels,
                    }]
                }
                None => vec![ServerMsg::Error {
                    message: "No snapshot available".into(),
                }],
            },
            ClientMsg::ExploreSubtree {
                action_path, depth, ..
            } => {
                self.last_explore = Some((action_path.clone(), depth));
                let tree = self.search.tree();
                if tree.is_empty() {
                    return vec![ServerMsg::Error {
                        message: "Path not found in tree".into(),
                    }];
                }
                match walk_tree_path(tree, tree.root(), &action_path) {
                    Some(node) => {
                        let mut snap = build_subtree_snapshot(tree, node, depth);
                        self.label_subtree_at(&mut snap, &action_path);
                        vec![ServerMsg::Subtree { tree: snap }]
                    }
                    None => vec![ServerMsg::Error {
                        message: "Path not found in tree".into(),
                    }],
                }
            }
            ClientMsg::TakeOver { player } => {
                if let Some(cfg) = self.configs.get_mut(player as usize) {
                    cfg.human = true;
                    vec![self.state_msg()]
                } else {
                    vec![ServerMsg::Error {
                        message: format!("Invalid player: {player}"),
                    }]
                }
            }
            ClientMsg::ReleaseControl { player } => {
                if let Some(cfg) = self.configs.get_mut(player as usize) {
                    cfg.human = false;
                    vec![self.state_msg()]
                } else {
                    vec![ServerMsg::Error {
                        message: format!("Invalid player: {player}"),
                    }]
                }
            }
            ClientMsg::SetAutoplay { .. } => {
                // Autoplay is handled client-side by sending BotMove in a loop.
                vec![self.state_msg()]
            }
            ClientMsg::SetSingleplayer { human_player } => {
                self.singleplayer_human_player = match human_player {
                    Some(player) if player < 2 => Some(player as usize),
                    Some(player) => {
                        return vec![ServerMsg::Error {
                            message: format!("Invalid player: {player}"),
                        }];
                    }
                    None => None,
                };
                Vec::new()
            }
            ClientMsg::Undo => {
                if let Some(target) = self.undo_target_cursor() {
                    self.cursor = target;
                    self.search.reset(self.history[target].state.clone());
                    vec![self.state_msg()]
                } else {
                    vec![ServerMsg::Error {
                        message: "Nothing to undo".into(),
                    }]
                }
            }
            ClientMsg::Redo => {
                if self.cursor < self.history.len() {
                    // Replay the stored decision + any following chance outcomes.
                    loop {
                        let entry = &self.history[self.cursor];
                        if let Some(ref next) = entry.next_state {
                            self.search.reset(next.clone());
                        } else {
                            self.search.apply_action(entry.action);
                        }
                        self.cursor += 1;
                        let at_chance =
                            self.cursor < self.history.len() && self.history[self.cursor].is_chance;
                        if !at_chance {
                            break;
                        }
                    }
                    vec![self.state_msg()]
                } else {
                    vec![ServerMsg::Error {
                        message: "Nothing to redo".into(),
                    }]
                }
            }
            ClientMsg::SetConfig {
                player,
                simulations,
            } => {
                if let Some(cfg) = self.configs.get_mut(player as usize) {
                    cfg.simulations = simulations;
                    vec![self.state_msg()]
                } else {
                    vec![ServerMsg::Error {
                        message: format!("Invalid player: {player}"),
                    }]
                }
            }
            ClientMsg::SetLogCursor { cursor } => match self.set_cursor(cursor) {
                Ok(()) => vec![self.state_msg()],
                Err(message) => vec![ServerMsg::Error { message }],
            },
            ClientMsg::SetReplayCursor { cursor } => {
                if self.replay.is_none() {
                    return vec![ServerMsg::Error {
                        message: "No replay is loaded".into(),
                    }];
                }
                match self.set_cursor(cursor) {
                    Ok(()) => vec![self.state_msg()],
                    Err(message) => vec![ServerMsg::Error { message }],
                }
            }
            ClientMsg::SetAutoSearch { .. } => {
                // Handled by connection-level loop; respond with current state.
                vec![self.state_msg()]
            }
        }
    }

    fn legal_actions(&self) -> Vec<usize> {
        let mut buf = Vec::new();
        if self.singleplayer_human_player == Some(self.current_player_idx()) {
            self.presenter
                .human_legal_actions(self.search.state(), &mut buf);
        } else {
            self.search.state().legal_actions(&mut buf);
        }
        buf
    }

    fn is_terminal(&self) -> bool {
        matches!(self.search.state().status(), Status::Terminal(_))
    }

    fn is_chance(&self) -> bool {
        matches!(self.search.state().status(), Status::Chance)
    }

    /// Returns true if the current state can be searched (not terminal, not chance).
    pub fn can_search(&self) -> bool {
        !self.is_terminal() && !self.is_chance()
    }

    pub fn current_game_ended(&self) -> bool {
        self.is_terminal()
    }

    pub fn current_result_reward(&self) -> Option<f32> {
        match self.search.state().status() {
            Status::Terminal(reward) => Some(reward),
            _ => None,
        }
    }

    /// Returns true when automatic background search is useful.
    ///
    /// Forced decision states such as Roll or End Turn have only one legal
    /// action, so auto-search should let autoplay advance them instead of
    /// spending the whole simulation budget on a non-choice.
    pub fn should_auto_search(&self) -> bool {
        self.can_search() && self.legal_actions().len() > 1
    }

    pub fn current_player_idx(&self) -> usize {
        match self.search.state().status() {
            Status::Decision(sign) if sign > 0.0 => 0,
            _ => 1,
        }
    }

    /// Returns true if current player is a human.
    pub fn current_is_human(&self) -> bool {
        let idx = self.current_player_idx();
        self.configs[idx].human
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn board_fingerprint(&self) -> Option<u64> {
        self.presenter.board_fingerprint(self.search.state())
    }

    fn apply_action(&mut self, action: usize) {
        let state = self.search.state();
        let mut chances = Vec::new();
        state.chance_outcomes(&mut chances);
        let is_chance = !chances.is_empty();
        let label = if !is_chance {
            let player = match state.status() {
                Status::Decision(sign) if sign > 0.0 => "P1",
                _ => "P2",
            };
            let action_label = self.presenter.action_label(state, action);
            format!("{player}: {action_label}")
        } else {
            self.presenter.chance_label(state, action)
        };
        // If the next history entry matches this action, preserve redo history.
        if self.cursor < self.history.len() && self.history[self.cursor].action == action {
            self.cursor += 1;
            self.search.apply_action(action);
            return;
        }
        // Truncate redo future and push new entry.
        self.history.truncate(self.cursor);
        self.history.push(HistoryEntry {
            state: state.clone(),
            action,
            label,
            is_chance,
            next_state: None,
        });
        self.cursor += 1;
        self.search.apply_action(action);
    }

    /// Auto-resolve chance nodes (dice, steals) until we reach a decision
    /// point or terminal state.
    fn auto_resolve_chance(&mut self) {
        loop {
            if self.is_terminal() {
                break;
            }
            // If the log is still intact and the next entry is a chance action,
            // replay it instead of re-sampling to avoid state divergence.
            if self.cursor < self.history.len() && self.history[self.cursor].is_chance {
                let a = self.history[self.cursor].action;
                self.apply_action(a);
                continue;
            }
            let action = self.search.state().sample_chance(&mut self.rng);
            match action {
                Some(a) => self.apply_action(a),
                None => break,
            }
        }
    }

    /// Run MCTS to completion and return the result.
    fn run_search(&mut self) -> SearchResult {
        loop {
            match self.search.select(&mut self.rng) {
                Select::Eval(leaf_id, state) => {
                    let eval = self.evaluator.evaluate_batch(&[&state], &mut self.rng);
                    self.search
                        .backup(leaf_id, eval.into_iter().next().unwrap());
                }
                Select::Terminal(leaf_id, wdl) => {
                    self.search.backup_terminal(leaf_id, wdl);
                }
                Select::Done => return self.search.result(),
            }
        }
    }

    fn bot_move_budget(
        &self,
        budget: Option<SearchBudget>,
        simulations: Option<u32>,
    ) -> SearchBudget {
        budget.unwrap_or_else(|| {
            let player_idx = self.current_player_idx();
            SearchBudget::simulations(simulations.unwrap_or(self.configs[player_idx].simulations))
        })
    }

    fn apply_search_budget(&mut self, budget: SearchBudget) -> ActiveSearchBudget {
        let budget = sanitize_search_budget(budget);
        let sims_total = match budget {
            SearchBudget::Simulations { value } => {
                self.search.set_num_simulations(value);
                value
            }
            SearchBudget::PvDepth { value, simulations } => {
                let sim_cap = simulations.unwrap_or(PV_DEPTH_SIM_SAFETY_CAP);
                self.search.set_pv_depth_limit(value, sim_cap);
                sim_cap
            }
        };
        self.search.start_search();
        ActiveSearchBudget { budget, sims_total }
    }

    /// Validate state and prepare search for streaming.
    /// Returns the applied budget or `Err(error_msgs)`.
    pub fn begin_search(&mut self, msg: &ClientMsg) -> Result<ActiveSearchBudget, Vec<ServerMsg>> {
        // Cancel stale search state left by a previous interrupted search
        // (e.g. a panicked evaluator or dropped WebSocket connection).
        self.search.cancel_search();
        match msg {
            ClientMsg::BotMove {
                simulations,
                budget,
            } => {
                if self.replay.is_some() {
                    return Err(vec![ServerMsg::Error {
                        message: "Cannot apply bot moves while viewing a replay".into(),
                    }]);
                }
                if self.is_terminal() {
                    return Err(vec![ServerMsg::Error {
                        message: "Game is over".into(),
                    }]);
                }
                if self.is_chance() {
                    return Err(vec![ServerMsg::Error {
                        message: "Current state is a chance node".into(),
                    }]);
                }
                let budget = self.bot_move_budget(*budget, *simulations);
                Ok(self.apply_search_budget(budget))
            }
            ClientMsg::RunSims { count, .. } => {
                if self.is_terminal() || self.is_chance() {
                    return Err(vec![ServerMsg::Error {
                        message: "Cannot run sims on chance/terminal state".into(),
                    }]);
                }
                Ok(self.apply_search_budget(SearchBudget::simulations(*count)))
            }
            ClientMsg::RunSearch { budget, .. } => {
                if self.is_terminal() || self.is_chance() {
                    return Err(vec![ServerMsg::Error {
                        message: "Cannot run search on chance/terminal state".into(),
                    }]);
                }
                Ok(self.apply_search_budget(*budget))
            }
            _ => Err(vec![ServerMsg::Error {
                message: "begin_search called with non-search message".into(),
            }]),
        }
    }

    /// Run one step of MCTS. Returns `Some(result)` when search is complete.
    pub fn search_tick(&mut self) -> Option<SearchResult> {
        match self.search.select(&mut self.rng) {
            Select::Eval(leaf_id, state) => {
                let eval = self.evaluator.evaluate_batch(&[&state], &mut self.rng);
                self.search
                    .backup(leaf_id, eval.into_iter().next().unwrap());
                None
            }
            Select::Terminal(leaf_id, wdl) => {
                self.search.backup_terminal(leaf_id, wdl);
                None
            }
            Select::Done => Some(self.search.result()),
        }
    }

    /// Finish a streaming search: apply action (BotMove) or return snapshot.
    pub fn finish_search(&mut self, msg: &ClientMsg, result: SearchResult) -> Vec<ServerMsg> {
        match msg {
            ClientMsg::BotMove { .. } => self.finish_bot_move(result),
            ClientMsg::RunSims { .. } | ClientMsg::RunSearch { .. } => {
                match self.build_snapshot() {
                    Some(snap) => {
                        let labels = self.edge_labels(&snap.edges);
                        vec![ServerMsg::Snapshot {
                            snapshot: snap,
                            action_labels: labels,
                        }]
                    }
                    None => vec![ServerMsg::Error {
                        message: "No snapshot available".into(),
                    }],
                }
            }
            _ => vec![],
        }
    }

    fn finish_bot_move(&mut self, result: SearchResult) -> Vec<ServerMsg> {
        if self.replay.is_some() {
            return vec![ServerMsg::Error {
                message: "Cannot apply bot moves while viewing a replay".into(),
            }];
        }
        if self.is_terminal() {
            return vec![ServerMsg::Error {
                message: "Game is over".into(),
            }];
        }
        if self.is_chance() {
            return vec![ServerMsg::Error {
                message: "Current state is a chance node".into(),
            }];
        }

        let action = result.selected_action;
        let label = if action < G::NUM_ACTIONS {
            self.presenter.action_label(self.search.state(), action)
        } else {
            format!("Action {action}")
        };
        let legal = self.legal_actions();
        if !legal.contains(&action) {
            let state = self.search.state().clone();
            self.search.reset(state);
            return vec![ServerMsg::Error {
                message: format!("Bot selected illegal action: {label} ({action})"),
            }];
        }

        let (snapshot, action_labels) = match self.build_snapshot() {
            Some(snap) => {
                let labels = self.edge_labels(&snap.edges);
                (Some(snap), labels)
            }
            None => (None, Vec::new()),
        };
        self.apply_action(action);
        self.auto_resolve_chance();
        vec![
            ServerMsg::BotAction {
                action,
                label,
                snapshot,
                action_labels,
            },
            self.state_msg(),
        ]
    }

    /// Generate a Subtree message for the last explored path, if any.
    pub fn explore_subtree_msg(&self) -> Option<ServerMsg> {
        let (path, depth) = self.last_explore.as_ref()?;
        let tree = self.search.tree();
        if tree.is_empty() {
            return None;
        }
        let node = walk_tree_path(tree, tree.root(), path)?;
        let mut snap = build_subtree_snapshot(tree, node, *depth);
        self.label_subtree_at(&mut snap, path);
        Some(ServerMsg::Subtree { tree: snap })
    }

    /// Get current snapshot with action labels.
    pub fn snapshot_with_labels(&self) -> Option<(SearchSnapshot, Vec<String>)> {
        self.build_snapshot().map(|snap| {
            let labels = self.edge_labels(&snap.edges);
            (snap, labels)
        })
    }

    /// Current root WDL for compact analysis displays.
    pub fn root_wdl(&self) -> [f32; 3] {
        match self.search.state().status() {
            Status::Terminal(reward) if reward > 0.0 => [1.0, 0.0, 0.0],
            Status::Terminal(reward) if reward < 0.0 => [0.0, 0.0, 1.0],
            Status::Terminal(_) => [0.0, 1.0, 0.0],
            _ => self
                .build_snapshot()
                .map(|snap| snap.root_wdl)
                .unwrap_or([0.0, 1.0, 0.0]),
        }
    }

    /// Run a compact search and return only the root WDL.
    pub fn run_analysis_bar_search(&mut self, budget: SearchBudget) -> Result<[f32; 3], String> {
        if !self.can_search() {
            return Ok(self.root_wdl());
        }

        let msg = ClientMsg::RunSearch {
            budget,
            target: None,
        };
        if let Err(messages) = self.begin_search(&msg) {
            let message = messages
                .into_iter()
                .find_map(|msg| match msg {
                    ServerMsg::Error { message } => Some(message),
                    _ => None,
                })
                .unwrap_or_else(|| "Could not run analysis".into());
            return Err(message);
        }

        let result = loop {
            if let Some(result) = self.search_tick() {
                break result;
            }
        };
        let _ = self.finish_search(&msg, result);
        Ok(self.root_wdl())
    }

    /// Build a `SearchSnapshot` from the current tree state.
    fn build_snapshot(&self) -> Option<SearchSnapshot> {
        let tree = self.search.tree();
        if tree.is_empty() {
            return None;
        }
        let root = self.search.root_node()?;
        let edges = tree.edges(root);
        let total_sims: u32 = edges.iter().map(|e| e.visits).sum();
        let fresh_visits = self.search.root_fresh_visits();
        let fresh_sims = self.search.fresh_root_visits();

        let edge_snaps: Vec<EdgeSnapshot> = edges
            .iter()
            .enumerate()
            .map(|(idx, e)| {
                let (q, depth) = match e.child {
                    Some(child) => (Some(tree.q(child)), Some(compute_pv_depth(tree, child))),
                    None => (None, None),
                };
                EdgeSnapshot {
                    action: e.action,
                    visits: e.visits,
                    fresh_visits: fresh_visits.get(idx).copied().unwrap_or(e.visits),
                    q,
                    improved_policy: e.prior, // use prior as improved_policy approximation
                    depth,
                }
            })
            .collect();

        let wdl = tree.wdl(root);
        Some(SearchSnapshot {
            total_simulations: total_sims,
            fresh_simulations: fresh_sims,
            pv_depth: compute_pv_depth(tree, root),
            root_wdl: [wdl.w, wdl.d, wdl.l],
            network_value: wdl.q(),
            edges: edge_snaps,
        })
    }

    /// Get action labels for edge snapshots.
    fn edge_labels(&self, edges: &[EdgeSnapshot]) -> Vec<String> {
        let state = self.search.state();
        edges
            .iter()
            .map(|e| self.presenter.action_label(state, e.action))
            .collect()
    }

    /// Cancel any in-progress search (clears pending evaluation contexts).
    pub fn cancel_search(&mut self) {
        self.search.cancel_search();
    }

    /// Cancel an in-progress analysis search and return the partial snapshot.
    pub fn pause_search(&mut self) -> Vec<ServerMsg> {
        self.search.cancel_search();
        match self.build_snapshot() {
            Some(snap) => {
                let labels = self.edge_labels(&snap.edges);
                vec![ServerMsg::Snapshot {
                    snapshot: snap,
                    action_labels: labels,
                }]
            }
            None => vec![ServerMsg::Error {
                message: "No search is running".into(),
            }],
        }
    }

    /// Update the MCTS simulation budget.
    pub fn set_num_simulations(&mut self, n: u32) {
        self.search.set_num_simulations(n);
    }

    /// Total visits on the current root node.
    pub fn root_visits(&self) -> u32 {
        self.search.root_visits()
    }

    fn set_cursor(&mut self, cursor: usize) -> Result<(), String> {
        if cursor > self.history.len() {
            return Err(format!(
                "History cursor {cursor} is past the end of the game ({})",
                self.history.len()
            ));
        }
        self.cursor = cursor;
        if cursor == 0 {
            if let Some(first) = self.history.first() {
                self.search.reset(first.state.clone());
            }
            return Ok(());
        }
        let entry = &self.history[cursor - 1];
        if let Some(ref next) = entry.next_state {
            self.search.reset(next.clone());
        } else {
            let mut state = entry.state.clone();
            state.apply_action(entry.action);
            self.search.reset(state);
        }
        Ok(())
    }

    /// Label all nodes in a subtree by simulating actions from the root state.
    /// Label a subtree whose root is at `path` from the search root.
    /// Advances the search state by the path so the label walker starts
    /// with the correct state for the navigated node.
    fn label_subtree_at(&self, tree: &mut TreeNodeSnapshot, path: &[usize]) {
        let mut state = self.search.state().clone();
        // parent_is_chance tracks whether the most recently applied action
        // was a chance outcome — i.e., whether the navigated node's parent
        // was a chance node. This determines how to label the navigated node.
        let mut parent_is_chance = false;
        for &action in path {
            match state.status() {
                Status::Terminal(_) => break,
                Status::Chance => {
                    state.apply_action(action);
                    parent_is_chance = true;
                }
                Status::Decision(_) => {
                    let mut legal = Vec::new();
                    state.legal_actions(&mut legal);
                    if !legal.contains(&action) {
                        break;
                    }
                    state.apply_action(action);
                    parent_is_chance = false;
                }
            }
        }
        label_subtree_walk(tree, state, &*self.presenter, parent_is_chance);
    }
}

fn sanitize_search_budget(budget: SearchBudget) -> SearchBudget {
    match budget {
        SearchBudget::Simulations { value } => SearchBudget::simulations(value),
        SearchBudget::PvDepth { value, simulations } => SearchBudget::PvDepth {
            value: value.clamp(1, MAX_PV_DEPTH_BUDGET),
            simulations: simulations.map(|value| value.clamp(1, PV_DEPTH_SIM_SAFETY_CAP)),
        },
    }
}

fn validate_replay_log<G: Game>(mut state: G, log: &GameLog) -> Result<(), String> {
    let mut legal = Vec::new();
    let mut chance = Vec::new();
    for (i, &action) in log.actions.iter().enumerate() {
        match state.status() {
            Status::Terminal(_) => {
                return Err(format!(
                    "action {} appears after the game is terminal",
                    i + 1
                ));
            }
            Status::Decision(_) => {
                legal.clear();
                state.legal_actions(&mut legal);
                if !legal.contains(&action) {
                    return Err(format!("illegal replay action {action} at line {}", i + 2));
                }
            }
            Status::Chance => {
                chance.clear();
                state.chance_outcomes(&mut chance);
                if !chance.is_empty() && !chance.iter().any(|&(outcome, _)| outcome == action) {
                    return Err(format!(
                        "illegal replay chance outcome {action} at line {}",
                        i + 2
                    ));
                }
            }
        }
        state.apply_action(action);
    }
    Ok(())
}

fn action_player_idx<G: Game>(state: &G) -> Option<usize> {
    match state.status() {
        Status::Decision(sign) if sign > 0.0 => Some(0),
        Status::Decision(_) => Some(1),
        _ => None,
    }
}

/// Walk the tree along an action path, returning the final node if reachable.
fn walk_tree_path(tree: &Tree, start: NodeId, path: &[usize]) -> Option<NodeId> {
    let mut node = start;
    for &action in path {
        node = tree.child_for_action(node, action)?;
    }
    Some(node)
}

/// Build a recursive `TreeNodeSnapshot` from a tree node, up to `depth` levels.
fn build_subtree_snapshot(tree: &Tree, node: NodeId, depth: usize) -> TreeNodeSnapshot {
    build_subtree_node(tree, node, None, depth)
}

fn build_subtree_node(
    tree: &Tree,
    node: NodeId,
    action: Option<usize>,
    depth: usize,
) -> TreeNodeSnapshot {
    let wdl = tree.wdl(node);
    let kind = tree.kind(node);
    let kind_str = match kind {
        NodeKind::Terminal => "terminal",
        NodeKind::Decision(_) => "decision",
        NodeKind::Chance => "chance",
    };
    let visits: u32 = tree.edges(node).iter().map(|e| e.visits).sum();
    let player = match kind {
        NodeKind::Decision(sign) => Some(if *sign > 0.0 { 0 } else { 1 }),
        _ => None,
    };

    let children = if depth > 0 {
        tree.edges(node)
            .iter()
            .filter_map(|e| {
                e.child
                    .map(|child| build_subtree_node(tree, child, Some(e.action), depth - 1))
            })
            .collect()
    } else {
        Vec::new()
    };

    TreeNodeSnapshot {
        action,
        label: None,
        kind: kind_str.into(),
        visits,
        wdl: [wdl.w, wdl.d, wdl.l],
        player,
        children,
    }
}

/// Compute the principal variation depth from a node.
fn compute_pv_depth(tree: &Tree, node: NodeId) -> u32 {
    let mut depth = 0;
    let mut current = node;
    loop {
        let edges = tree.edges(current);
        let best = edges.iter().max_by_key(|e| e.visits);
        match best.and_then(|e| e.child) {
            Some(child) => {
                depth += 1;
                current = child;
            }
            None => break,
        }
    }
    depth
}

fn label_subtree_walk<G: Game + Clone>(
    node: &mut TreeNodeSnapshot,
    state: G,
    presenter: &dyn GamePresenter<G>,
    parent_is_chance: bool,
) {
    if let Some(action) = node.action {
        let label = if parent_is_chance {
            let chance = presenter.chance_label(&state, action);
            if chance.is_empty() {
                // Stale state — can't resolve chance outcome to phase.
                format!("Outcome {action}")
            } else {
                chance
            }
        } else {
            presenter.action_label(&state, action)
        };
        node.label = Some(label);
    } else if node.label.is_none() {
        node.label = Some(presenter.phase_label(&state));
    }

    let is_chance = node.kind == "chance";

    // Advance state for children. If the action is stale (not legal
    // in the walked state), continue with the un-advanced state —
    // action labels are state-independent so they still decode
    // correctly. Chance labels may be approximate.
    let next = if let Some(action) = node.action {
        let mut s = state;
        let mut legal = Vec::new();
        s.legal_actions(&mut legal);
        if legal.contains(&action) {
            s.apply_action(action);
        }
        s
    } else {
        state
    };
    for child in &mut node.children {
        label_subtree_walk(child, next.clone(), presenter, is_chance);
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use crate::{
        eval::{Evaluation, Evaluator, Wdl},
        game::{Game, Status},
        mcts::SearchResult,
    };

    use super::{
        ClientMsg, GameLog, GamePresenter, GameSession, MAX_PV_DEPTH_BUDGET,
        PV_DEPTH_SIM_SAFETY_CAP, SearchBudget, ServerMsg,
    };

    #[derive(Clone)]
    struct TestGame {
        id: u64,
        moves: u8,
    }

    impl Game for TestGame {
        const NUM_ACTIONS: usize = 2;

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
            if !matches!(self.status(), Status::Terminal(_)) {
                buf.push(0);
                buf.push(1);
            }
        }

        fn apply_action(&mut self, action: usize) {
            assert!(action < Self::NUM_ACTIONS);
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
            serde_json::json!({
                "id": state.id,
                "moves": state.moves,
            })
        }

        fn action_label(&self, _state: &TestGame, action: usize) -> String {
            format!("Action {action}")
        }

        fn is_singleplayer_undo_barrier(&self, _state: &TestGame, action: usize) -> bool {
            action == 1
        }

        fn phase_label(&self, _state: &TestGame) -> String {
            "test".into()
        }

        fn serialize_log_state(&self, state: &TestGame) -> Option<String> {
            Some(state.id.to_string())
        }

        fn deserialize_log_state(&self, text: &str) -> Result<TestGame, String> {
            Ok(TestGame {
                id: text.parse().map_err(|e| format!("bad id: {e}"))?,
                moves: 0,
            })
        }

        fn static_dir(&self) -> &Path {
            Path::new(".")
        }

        fn new_game(&self, seed: u64) -> TestGame {
            TestGame { id: seed, moves: 0 }
        }

        fn new_game_from_editor(
            &self,
            terrains: &[String],
            _numbers: &[Option<u8>],
            port_layout: Option<&str>,
            ports: Option<&[String]>,
        ) -> Result<TestGame, String> {
            if terrains.first().map(|terrain| terrain.as_str()) == Some("bad") {
                Err("bad edited board".into())
            } else if port_layout == Some("alternate")
                && ports.and_then(|p| p.first()).map(|p| p.as_str()) == Some("ore")
            {
                Ok(TestGame { id: 98, moves: 0 })
            } else {
                Ok(TestGame { id: 99, moves: 0 })
            }
        }
    }

    fn test_session() -> GameSession<TestGame> {
        GameSession::with_state(
            TestGame { id: 7, moves: 0 },
            Arc::new(TestEvaluator),
            "test",
            Arc::new(TestPresenter),
            [true, true],
            crate::mcts::Config::default(),
        )
    }

    fn finish_search(session: &mut GameSession<TestGame>) {
        while session.search_tick().is_none() {}
    }

    #[test]
    fn bot_move_result_must_be_legal_before_apply() {
        let mut session = test_session();
        let result = SearchResult {
            policy: vec![0.0; TestGame::NUM_ACTIONS],
            wdl: Wdl::DRAW,
            selected_action: 2,
            network_value: 0.0,
            children_q: Vec::new(),
            prior_top1_action: 2,
            pv_depth: 0,
            max_depth: 0,
        };

        let msgs = session.finish_search(
            &ClientMsg::BotMove {
                simulations: Some(0),
                budget: None,
            },
            result,
        );

        match msgs.as_slice() {
            [ServerMsg::Error { message }] => {
                assert!(message.contains("Bot selected illegal action"));
            }
            other => panic!("expected illegal bot action error, got {other:?}"),
        }
        assert_eq!(session.cursor(), 0);
        assert_eq!(session.search.state().moves, 0);
    }

    #[test]
    fn export_current_log_skips_empty_and_uses_visible_prefix() {
        let mut session = test_session();
        assert!(session.export_current_log().is_none());

        session.handle(ClientMsg::PlayAction { action: 0 });
        session.handle(ClientMsg::PlayAction { action: 0 });
        session.handle(ClientMsg::Undo);

        let log = session
            .export_current_log()
            .expect("expected non-empty log");
        assert_eq!(log.initial_state, "7");
        assert_eq!(log.actions, vec![0]);
    }

    #[test]
    fn export_unsaved_current_log_skips_already_saved_log() {
        let mut session = test_session();

        session.handle(ClientMsg::PlayAction { action: 0 });
        let log = session
            .export_unsaved_current_log()
            .expect("expected first move log");
        assert_eq!(log.actions, vec![0]);

        session.mark_current_log_saved(&log);
        assert!(session.export_unsaved_current_log().is_none());

        session.handle(ClientMsg::PlayAction { action: 0 });
        assert!(session.current_game_ended());
        let terminal_log = session
            .export_unsaved_current_log()
            .expect("terminal log should differ from saved prefix");
        assert_eq!(terminal_log.actions, vec![0, 0]);
    }

    #[test]
    fn singleplayer_can_undo_human_non_barrier_action() {
        let mut session = test_session();
        session.handle(ClientMsg::SetSingleplayer {
            human_player: Some(0),
        });

        match session
            .handle(ClientMsg::PlayAction { action: 0 })
            .as_slice()
        {
            [
                ServerMsg::GameState {
                    can_undo,
                    history_cursor,
                    ..
                },
            ] => {
                assert!(*can_undo);
                assert_eq!(*history_cursor, 1);
            }
            other => panic!("expected GameState after human action, got {other:?}"),
        }

        match session.handle(ClientMsg::Undo).as_slice() {
            [
                ServerMsg::GameState {
                    state,
                    history_cursor,
                    can_undo,
                    ..
                },
            ] => {
                assert_eq!(state["moves"], serde_json::json!(0));
                assert_eq!(*history_cursor, 0);
                assert!(!*can_undo);
            }
            other => panic!("expected undo GameState, got {other:?}"),
        }
    }

    #[test]
    fn singleplayer_cannot_undo_bot_move() {
        let mut session = test_session();
        session.handle(ClientMsg::SetSingleplayer {
            human_player: Some(0),
        });
        session.handle(ClientMsg::PlayAction { action: 0 });

        let result = SearchResult {
            policy: vec![0.0; TestGame::NUM_ACTIONS],
            wdl: Wdl::DRAW,
            selected_action: 0,
            network_value: 0.0,
            children_q: Vec::new(),
            prior_top1_action: 0,
            pv_depth: 0,
            max_depth: 0,
        };
        let msgs = session.finish_search(
            &ClientMsg::BotMove {
                simulations: Some(0),
                budget: None,
            },
            result,
        );

        match msgs.as_slice() {
            [
                ServerMsg::BotAction { .. },
                ServerMsg::GameState {
                    state,
                    history_cursor,
                    can_undo,
                    ..
                },
            ] => {
                assert_eq!(state["moves"], serde_json::json!(2));
                assert_eq!(*history_cursor, 2);
                assert!(!*can_undo);
            }
            other => panic!("expected bot action and GameState, got {other:?}"),
        }

        match session.handle(ClientMsg::Undo).as_slice() {
            [ServerMsg::Error { message }] => assert_eq!(message.as_str(), "Nothing to undo"),
            other => panic!("expected undo error, got {other:?}"),
        }
        assert_eq!(session.cursor(), 2);
        assert_eq!(session.search.state().moves, 2);
    }

    #[test]
    fn singleplayer_cannot_undo_roll_barrier_action() {
        let mut session = test_session();
        session.handle(ClientMsg::SetSingleplayer {
            human_player: Some(0),
        });

        match session
            .handle(ClientMsg::PlayAction { action: 1 })
            .as_slice()
        {
            [
                ServerMsg::GameState {
                    history_cursor,
                    can_undo,
                    ..
                },
            ] => {
                assert_eq!(*history_cursor, 1);
                assert!(!*can_undo);
            }
            other => panic!("expected GameState after barrier action, got {other:?}"),
        }

        match session.handle(ClientMsg::Undo).as_slice() {
            [ServerMsg::Error { message }] => assert_eq!(message.as_str(), "Nothing to undo"),
            other => panic!("expected undo error, got {other:?}"),
        }
        assert_eq!(session.cursor(), 1);
        assert_eq!(session.search.state().moves, 1);
    }

    #[test]
    fn depth_budget_is_clamped_before_reaching_search() {
        let mut session = test_session();
        let active = session
            .begin_search(&ClientMsg::RunSearch {
                budget: SearchBudget::pv_depth(99),
                target: None,
            })
            .expect("depth search should be accepted");

        assert_eq!(active.budget, SearchBudget::pv_depth(MAX_PV_DEPTH_BUDGET));
        assert_eq!(active.sims_total, PV_DEPTH_SIM_SAFETY_CAP);
        assert_eq!(
            session.search.config().target_pv_depth,
            Some(MAX_PV_DEPTH_BUDGET)
        );
        assert_eq!(
            session.search.config().num_simulations,
            PV_DEPTH_SIM_SAFETY_CAP
        );
    }

    #[test]
    fn depth_budget_sim_cap_is_clamped_before_reaching_search() {
        let mut session = test_session();
        let active = session
            .begin_search(&ClientMsg::RunSearch {
                budget: SearchBudget::pv_depth_with_simulations(99, PV_DEPTH_SIM_SAFETY_CAP + 1),
                target: None,
            })
            .expect("depth search should be accepted");

        assert_eq!(
            active.budget,
            SearchBudget::pv_depth_with_simulations(MAX_PV_DEPTH_BUDGET, PV_DEPTH_SIM_SAFETY_CAP)
        );
        assert_eq!(active.sims_total, PV_DEPTH_SIM_SAFETY_CAP);
        assert_eq!(
            session.search.config().target_pv_depth,
            Some(MAX_PV_DEPTH_BUDGET)
        );
        assert_eq!(
            session.search.config().num_simulations,
            PV_DEPTH_SIM_SAFETY_CAP
        );
    }

    #[test]
    fn search_budget_protocol_accepts_new_and_legacy_messages() {
        let run_search: ClientMsg =
            serde_json::from_str(r#"{"type":"RunSearch","budget":{"mode":"pv_depth","value":8}}"#)
                .expect("new RunSearch message");
        match run_search {
            ClientMsg::RunSearch { budget, target } => {
                assert_eq!(budget, SearchBudget::pv_depth(8));
                assert_eq!(target, None);
            }
            other => panic!("expected RunSearch, got {other:?}"),
        }

        let capped_depth: ClientMsg = serde_json::from_str(
            r#"{"type":"RunSearch","budget":{"mode":"pv_depth","value":8,"simulations":1200}}"#,
        )
        .expect("capped depth RunSearch message");
        match capped_depth {
            ClientMsg::RunSearch { budget, target } => {
                assert_eq!(budget, SearchBudget::pv_depth_with_simulations(8, 1200));
                assert_eq!(target, None);
            }
            other => panic!("expected RunSearch, got {other:?}"),
        }

        let legacy_run_sims: ClientMsg = serde_json::from_str(r#"{"type":"RunSims","count":7}"#)
            .expect("legacy RunSims message");
        match legacy_run_sims {
            ClientMsg::RunSims { count, target } => {
                assert_eq!(count, 7);
                assert_eq!(target, None);
            }
            other => panic!("expected RunSims, got {other:?}"),
        }

        let legacy_bot_move: ClientMsg =
            serde_json::from_str(r#"{"type":"BotMove","simulations":12}"#)
                .expect("legacy BotMove message");
        match legacy_bot_move {
            ClientMsg::BotMove {
                simulations,
                budget,
            } => {
                assert_eq!(simulations, Some(12));
                assert_eq!(budget, None);
            }
            other => panic!("expected BotMove, got {other:?}"),
        }

        let pause_search: ClientMsg =
            serde_json::from_str(r#"{"type":"PauseSearch"}"#).expect("pause search message");
        match pause_search {
            ClientMsg::PauseSearch { target } => assert_eq!(target, None),
            other => panic!("expected PauseSearch, got {other:?}"),
        }

        let pause_replay: ClientMsg =
            serde_json::from_str(r#"{"type":"PauseSearch","target":"replay"}"#)
                .expect("targeted pause search message");
        match pause_replay {
            ClientMsg::PauseSearch { target } => {
                assert_eq!(target, Some(super::super::protocol::ViewTarget::Replay));
            }
            other => panic!("expected PauseSearch, got {other:?}"),
        }

        let set_log_cursor: ClientMsg =
            serde_json::from_str(r#"{"type":"SetLogCursor","cursor":1}"#)
                .expect("raw log cursor message");
        match set_log_cursor {
            ClientMsg::SetLogCursor { cursor } => assert_eq!(cursor, 1),
            other => panic!("expected SetLogCursor, got {other:?}"),
        }
    }

    #[test]
    fn set_log_cursor_browses_full_history_without_truncating() {
        let mut session = test_session();
        session.handle(ClientMsg::PlayAction { action: 0 });
        session.handle(ClientMsg::PlayAction { action: 0 });

        match session
            .handle(ClientMsg::SetLogCursor { cursor: 1 })
            .as_slice()
        {
            [
                ServerMsg::GameState {
                    state,
                    action_log,
                    history_cursor,
                    action_log_cursors,
                    can_redo,
                    ..
                },
            ] => {
                assert_eq!(state["moves"], serde_json::json!(1));
                assert_eq!(*history_cursor, 1);
                assert_eq!(action_log.len(), 2);
                assert_eq!(action_log_cursors, &vec![1, 2]);
                assert!(*can_redo);
            }
            other => panic!("expected browsed GameState, got {other:?}"),
        }
    }

    #[test]
    fn playing_different_action_from_history_replaces_future_branch() {
        let mut session = test_session();
        session.handle(ClientMsg::PlayAction { action: 0 });
        session.handle(ClientMsg::PlayAction { action: 0 });
        session.handle(ClientMsg::SetLogCursor { cursor: 1 });

        match session
            .handle(ClientMsg::PlayAction { action: 1 })
            .as_slice()
        {
            [
                ServerMsg::GameState {
                    state,
                    action_log,
                    history_cursor,
                    action_log_cursors,
                    can_redo,
                    ..
                },
            ] => {
                assert_eq!(state["moves"], serde_json::json!(2));
                assert_eq!(*history_cursor, 2);
                assert_eq!(action_log.len(), 2);
                assert!(action_log[1].contains("Action 1"));
                assert_eq!(action_log_cursors, &vec![1, 2]);
                assert!(!*can_redo);
            }
            other => panic!("expected branched GameState, got {other:?}"),
        }
    }

    #[test]
    fn pause_search_returns_partial_snapshot_without_clearing_tree() {
        let mut session = test_session();
        session
            .begin_search(&ClientMsg::RunSearch {
                budget: SearchBudget::simulations(10),
                target: None,
            })
            .expect("search starts");

        for _ in 0..20 {
            if session.root_visits() > 0 {
                break;
            }
            let _ = session.search_tick();
        }
        let visits_before_pause = session.root_visits();
        assert!(
            visits_before_pause > 0,
            "test search should accumulate visits before pause"
        );

        match session.pause_search().as_slice() {
            [ServerMsg::Snapshot { snapshot, .. }] => {
                assert_eq!(snapshot.total_simulations, visits_before_pause);
                assert_eq!(snapshot.fresh_simulations, visits_before_pause);
            }
            other => panic!("expected partial snapshot, got {other:?}"),
        }

        session
            .begin_search(&ClientMsg::RunSearch {
                budget: SearchBudget::simulations(1),
                target: None,
            })
            .expect("search resumes");
        assert_eq!(
            session.root_visits(),
            visits_before_pause,
            "pause should preserve accumulated search visits"
        );
        let (snapshot, _) = session
            .snapshot_with_labels()
            .expect("snapshot should remain available");
        assert_eq!(snapshot.total_simulations, visits_before_pause);
        assert_eq!(
            snapshot.fresh_simulations, 0,
            "a new explicit search starts a fresh baseline"
        );
    }

    #[test]
    fn same_root_search_resets_fresh_visit_baseline() {
        let mut session = test_session();
        session
            .begin_search(&ClientMsg::RunSearch {
                budget: SearchBudget::simulations(5),
                target: None,
            })
            .expect("first search starts");
        finish_search(&mut session);

        let (first, _) = session
            .snapshot_with_labels()
            .expect("first snapshot should exist");
        assert_eq!(first.total_simulations, 5);
        assert_eq!(first.fresh_simulations, 5);
        assert_eq!(
            first.edges.iter().map(|edge| edge.visits).sum::<u32>(),
            first.total_simulations
        );
        assert_eq!(
            first
                .edges
                .iter()
                .map(|edge| edge.fresh_visits)
                .sum::<u32>(),
            first.fresh_simulations
        );

        session
            .begin_search(&ClientMsg::RunSearch {
                budget: SearchBudget::simulations(3),
                target: None,
            })
            .expect("second search starts");
        let (baseline, _) = session
            .snapshot_with_labels()
            .expect("baseline snapshot should exist");
        assert_eq!(baseline.total_simulations, 5);
        assert_eq!(baseline.fresh_simulations, 0);
        assert!(baseline.edges.iter().all(|edge| edge.fresh_visits == 0));

        finish_search(&mut session);
        let (second, _) = session
            .snapshot_with_labels()
            .expect("second snapshot should exist");
        assert_eq!(second.total_simulations, 8);
        assert_eq!(second.fresh_simulations, 3);
        assert_eq!(
            second
                .edges
                .iter()
                .map(|edge| edge.fresh_visits)
                .sum::<u32>(),
            second.fresh_simulations
        );
    }

    #[test]
    fn loaded_replay_reports_metadata_and_rejects_play_actions() {
        let mut session = test_session();
        let log = GameLog {
            initial_state: "9".into(),
            actions: vec![0, 0],
        };
        session
            .load_saved_replay_log("saved.log", &log)
            .expect("valid replay");

        match session.state_msg() {
            ServerMsg::GameState { replay, .. } => {
                let replay = replay.expect("replay metadata");
                assert_eq!(replay.id, "saved.log");
                assert_eq!(replay.cursor, 0);
                assert_eq!(replay.len, 2);
            }
            _ => panic!("expected GameState"),
        }

        assert!(
            session
                .begin_search(&ClientMsg::RunSims {
                    count: 1,
                    target: None,
                })
                .is_ok()
        );

        match session
            .handle(ClientMsg::PlayAction { action: 0 })
            .as_slice()
        {
            [ServerMsg::Error { message }] => assert!(message.contains("replay")),
            other => panic!("expected replay error, got {other:?}"),
        }

        session.handle(ClientMsg::SetReplayCursor { cursor: 2 });
        match session.state_msg() {
            ServerMsg::GameState { replay, state, .. } => {
                assert_eq!(replay.expect("replay metadata").cursor, 2);
                assert_eq!(state["moves"], serde_json::json!(2));
            }
            _ => panic!("expected GameState"),
        }
    }

    #[test]
    fn seek_to_end_reaches_loaded_replay_result() {
        let mut session = test_session();
        let log = GameLog {
            initial_state: "9".into(),
            actions: vec![0, 0],
        };
        session
            .load_saved_replay_log("saved.log", &log)
            .expect("valid replay");

        assert_eq!(session.current_result_reward(), None);
        session.seek_to_end();

        assert_eq!(session.cursor(), 2);
        assert_eq!(session.current_result_reward(), Some(1.0));
    }

    #[test]
    fn start_edited_game_replaces_state_and_invalid_keeps_current_state() {
        let mut session = test_session();
        session.handle(ClientMsg::PlayAction { action: 0 });

        let msgs = session.handle(ClientMsg::StartEditedGame {
            terrains: vec!["forest".into()],
            numbers: vec![Some(5)],
            port_layout: Some("alternate".into()),
            ports: Some(vec!["ore".into()]),
        });
        match msgs.as_slice() {
            [
                ServerMsg::GameState {
                    replay,
                    state,
                    action_log,
                    ..
                },
            ] => {
                assert!(replay.is_none());
                assert_eq!(state["id"], serde_json::json!(98));
                assert_eq!(state["moves"], serde_json::json!(0));
                assert!(action_log.is_empty());
            }
            other => panic!("expected edited GameState, got {other:?}"),
        }

        let msgs = session.handle(ClientMsg::StartEditedGame {
            terrains: vec!["bad".into()],
            numbers: vec![Some(5)],
            port_layout: None,
            ports: None,
        });
        match msgs.as_slice() {
            [ServerMsg::Error { message }] => assert!(message.contains("bad edited board")),
            other => panic!("expected edited board error, got {other:?}"),
        }

        match session.state_msg() {
            ServerMsg::GameState { state, .. } => {
                assert_eq!(state["id"], serde_json::json!(98));
                assert_eq!(state["moves"], serde_json::json!(0));
            }
            _ => panic!("expected GameState"),
        }
    }

    #[test]
    fn corrupt_replay_does_not_replace_current_session() {
        let mut session = test_session();
        let log = GameLog {
            initial_state: "9".into(),
            actions: vec![99],
        };
        assert!(session.load_saved_replay_log("bad.log", &log).is_err());

        match session.state_msg() {
            ServerMsg::GameState { replay, state, .. } => {
                assert!(replay.is_none());
                assert_eq!(state["id"], serde_json::json!(7));
                assert_eq!(state["moves"], serde_json::json!(0));
            }
            _ => panic!("expected GameState"),
        }
    }
}

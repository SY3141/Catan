/// Iterator that replays a [`GameLog`], yielding `(action, state_after)` pairs.
pub struct Replay<'a, G> {
    state: G,
    actions: std::slice::Iter<'a, usize>,
}

impl<G: crate::game::Game> Iterator for Replay<'_, G> {
    type Item = (usize, G);

    fn next(&mut self) -> Option<(usize, G)> {
        let &action = self.actions.next()?;
        self.state.apply_action(action);
        Some((action, self.state.clone()))
    }
}

/// A complete record of a single game, sufficient for deterministic replay.
///
/// The `initial_state` string recreates the starting position (e.g. board
/// code for Catan, FEN for chess) and `actions` contains every action
/// applied — both chance outcomes and player decisions — in order.
#[derive(Clone, Debug)]
pub struct GameLog {
    pub initial_state: String,
    pub actions: Vec<usize>,
}

impl GameLog {
    fn to_text(&self) -> String {
        use std::fmt::Write;
        let mut buf = String::new();
        writeln!(buf, "{}", self.initial_state).unwrap();
        for &action in &self.actions {
            writeln!(buf, "{action}").unwrap();
        }
        buf
    }

    /// Write a game log as plain text: initial state on the first line,
    /// then one action per line.
    pub fn write(&self, path: &std::path::Path) {
        self.write_result(path).expect("failed to write game log");
    }

    /// Write a game log as plain text, returning an error instead of panicking.
    pub fn write_result(&self, path: &std::path::Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_text())
    }

    /// Iterate over replay states, yielding `(action, state_after)` for each action.
    pub fn replay<G: crate::game::Game>(&self, initial_state: G) -> Replay<'_, G> {
        Replay {
            state: initial_state,
            actions: self.actions.iter(),
        }
    }

    /// Read a game log from plain text (initial state on first line,
    /// then one action per line).
    pub fn read(path: &std::path::Path) -> Self {
        Self::read_result(path).expect("failed to read game log")
    }

    /// Read a game log from plain text, returning an error instead of panicking.
    pub fn read_result(path: &std::path::Path) -> Result<Self, String> {
        let data = std::fs::read_to_string(path).map_err(|e| format!("failed to read log: {e}"))?;
        let mut lines = data.lines();
        let initial_state = lines
            .next()
            .ok_or_else(|| "empty log file".to_string())?
            .to_string();
        let actions: Result<Vec<usize>, String> = lines
            .enumerate()
            .filter(|(_, l)| !l.is_empty())
            .map(|(i, l)| {
                l.parse()
                    .map_err(|e| format!("invalid action on line {}: {e}", i + 2))
            })
            .collect();
        Ok(Self {
            initial_state,
            actions: actions?,
        })
    }
}

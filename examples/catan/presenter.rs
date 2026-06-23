use std::path::{Path, PathBuf};
use std::sync::Arc;

use hexfish::player::Player;
use hexfish::server::GamePresenter;

use crate::game;
use crate::game::action::{ActionId, DISCARD_END, DISCARD_START, ROLL};
use crate::game::board::Terrain;
use crate::game::dev_card::{DevCardDeck, DevCardKind};
use crate::game::dice::Dice;
use crate::game::resource::{ALL_RESOURCES, Resource};
use crate::game::state::{GameState, Phase};
use crate::game::topology::{PORT_COUNT, PortLayout, Topology};
use crate::visualize;

const EDITOR_TILE_COUNT: usize = 19;
const EDITOR_LOG_V1_PREFIX: &str = "editor-v1:";
const EDITOR_LOG_V2_PREFIX: &str = "editor-v2:";

/// Compute expected hidden dev card distribution for each player.
///
/// Returns `[[f32; 5]; 2]` — un-normalized expected counts per dev card type.
/// For a player with no hidden cards, the row is all zeros (exact counts are
/// already in `PlayerFrame.dev_cards`).
///
/// Bank estimate is `None` when no hidden cards exist (the unknown pool IS
/// the bank — no uncertainty), so the frontend falls back to exact counts.
fn expected_hidden_dev_cards(state: &GameState) -> ([[f32; 5]; 2], Option<[f32; 5]>) {
    let pool = state.unknown_dev_pool();
    let pool_total: f32 = pool.iter().sum::<u8>() as f32;

    let mut players = [[0.0f32; 5]; 2];
    let total_hidden: u8 =
        state.players[Player::One].hidden_dev_cards + state.players[Player::Two].hidden_dev_cards;

    if total_hidden == 0 {
        return (players, None);
    }

    let bank_cards = (state.dev_deck.total as f32) - (total_hidden as f32);

    for (i, &pid) in [Player::One, Player::Two].iter().enumerate() {
        let hidden = state.players[pid].hidden_dev_cards as f32;
        if hidden > 0.0 && pool_total > 0.0 {
            for t in 0..5 {
                players[i][t] = pool[t] as f32 * hidden / pool_total;
            }
        }
    }

    let mut bank = [0.0f32; 5];
    if bank_cards > 0.0 && pool_total > 0.0 {
        for t in 0..5 {
            bank[t] = pool[t] as f32 * bank_cards / pool_total;
        }
    }

    (players, Some(bank))
}

pub struct CatanPresenter {
    static_dir: PathBuf,
    dice: Dice,
    /// Optional player names: [P1 name, P2 name].
    player_names: Option<[String; 2]>,
}

impl CatanPresenter {
    pub fn new(static_dir: PathBuf, dice: Dice) -> Self {
        Self {
            static_dir,
            dice,
            player_names: None,
        }
    }

    pub fn with_player_names(mut self, names: [String; 2]) -> Self {
        self.player_names = Some(names);
        self
    }

    fn build_edited_game(
        &self,
        terrains: &[String],
        numbers: &[Option<u8>],
        port_layout: Option<&str>,
        ports: Option<&[String]>,
    ) -> Result<GameState, String> {
        let (terrains, numbers) = parse_editor_layout(terrains, numbers)?;
        let (port_layout, port_resources) = parse_editor_ports(port_layout, ports)?;
        Ok(GameState::new(
            Arc::new(Topology::from_layout_with_port_layout(
                terrains,
                numbers,
                port_resources,
                port_layout,
            )),
            DevCardDeck::new(),
            self.dice,
        ))
    }
}

fn parse_editor_layout(
    terrains: &[String],
    numbers: &[Option<u8>],
) -> Result<
    (
        [Terrain; EDITOR_TILE_COUNT],
        [Option<u8>; EDITOR_TILE_COUNT],
    ),
    String,
> {
    if terrains.len() != EDITOR_TILE_COUNT {
        return Err(format!(
            "edited board must include {EDITOR_TILE_COUNT} terrains, got {}",
            terrains.len()
        ));
    }
    if numbers.len() != EDITOR_TILE_COUNT {
        return Err(format!(
            "edited board must include {EDITOR_TILE_COUNT} numbers, got {}",
            numbers.len()
        ));
    }

    let mut parsed_terrains = [Terrain::Desert; EDITOR_TILE_COUNT];
    let mut parsed_numbers = [None; EDITOR_TILE_COUNT];

    for (i, terrain) in terrains.iter().enumerate() {
        parsed_terrains[i] =
            parse_terrain_name(terrain).map_err(|message| format!("tile {}: {message}", i + 1))?;
    }

    for i in 0..EDITOR_TILE_COUNT {
        match (parsed_terrains[i], numbers[i]) {
            (Terrain::Desert, Some(_)) => {
                return Err(format!("tile {} is desert and cannot have a number", i + 1));
            }
            (Terrain::Desert, None) => {}
            (_, Some(number)) if is_editor_number(number) => {
                parsed_numbers[i] = Some(number);
            }
            (_, Some(number)) => {
                return Err(format!(
                    "tile {} has invalid number {number}; use 2-12 excluding 7",
                    i + 1
                ));
            }
            (_, None) => {
                return Err(format!("tile {} needs a number", i + 1));
            }
        }
    }

    Ok((parsed_terrains, parsed_numbers))
}

fn parse_terrain_name(name: &str) -> Result<Terrain, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "forest" => Ok(Terrain::Forest),
        "hills" => Ok(Terrain::Hills),
        "pasture" => Ok(Terrain::Pasture),
        "fields" => Ok(Terrain::Fields),
        "mountains" => Ok(Terrain::Mountains),
        "desert" => Ok(Terrain::Desert),
        other => Err(format!("unknown terrain '{other}'")),
    }
}

fn parse_editor_port_layout(layout: Option<&str>) -> Result<PortLayout, String> {
    match layout
        .unwrap_or("primary")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "primary" => Ok(PortLayout::Primary),
        "alternate" => Ok(PortLayout::Alternate),
        other => Err(format!("unknown port layout '{other}'")),
    }
}

fn parse_port_name(name: &str) -> Result<Option<Resource>, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "generic" => Ok(None),
        "lumber" => Ok(Some(Resource::Lumber)),
        "brick" => Ok(Some(Resource::Brick)),
        "wool" => Ok(Some(Resource::Wool)),
        "grain" => Ok(Some(Resource::Grain)),
        "ore" => Ok(Some(Resource::Ore)),
        other => Err(format!("unknown port kind '{other}'")),
    }
}

fn parse_editor_ports(
    port_layout: Option<&str>,
    ports: Option<&[String]>,
) -> Result<(PortLayout, [Option<Resource>; PORT_COUNT]), String> {
    let port_layout = parse_editor_port_layout(port_layout)?;
    let Some(ports) = ports else {
        return Ok((port_layout, Topology::default_port_resources()));
    };
    if ports.len() != PORT_COUNT {
        return Err(format!(
            "edited board must include {PORT_COUNT} ports, got {}",
            ports.len()
        ));
    }

    let mut parsed = [None; PORT_COUNT];
    for (i, port) in ports.iter().enumerate() {
        parsed[i] =
            parse_port_name(port).map_err(|message| format!("port {}: {message}", i + 1))?;
    }
    Ok((port_layout, parsed))
}

fn is_editor_number(number: u8) -> bool {
    (2..=12).contains(&number) && number != 7
}

fn terrain_name(terrain: Terrain) -> &'static str {
    match terrain {
        Terrain::Forest => "forest",
        Terrain::Hills => "hills",
        Terrain::Pasture => "pasture",
        Terrain::Fields => "fields",
        Terrain::Mountains => "mountains",
        Terrain::Desert => "desert",
    }
}

fn port_name(resource: Option<Resource>) -> &'static str {
    match resource {
        Some(Resource::Lumber) => "lumber",
        Some(Resource::Brick) => "brick",
        Some(Resource::Wool) => "wool",
        Some(Resource::Grain) => "grain",
        Some(Resource::Ore) => "ore",
        None => "generic",
    }
}

fn tile_number(topology: &Topology, tile_index: usize) -> Option<u8> {
    for roll in 2..=12u8 {
        if topology.dice_to_tiles[roll as usize]
            .iter()
            .any(|tile| tile.0 as usize == tile_index)
        {
            return Some(roll);
        }
    }
    None
}

fn topology_layout_matches(a: &Topology, b: &Topology) -> bool {
    if a.tiles.len() != b.tiles.len() || a.nodes.len() != b.nodes.len() {
        return false;
    }
    for i in 0..a.tiles.len() {
        if a.tiles[i].terrain != b.tiles[i].terrain || tile_number(a, i) != tile_number(b, i) {
            return false;
        }
    }
    for i in 0..a.nodes.len() {
        if a.nodes[i].port != b.nodes[i].port {
            return false;
        }
    }
    true
}

fn can_use_compact_log_state(state: &GameState) -> bool {
    let decoded = Topology::from_board_code(state.topology.board_code());
    topology_layout_matches(&state.topology, &decoded)
}

fn encode_editor_log_state(state: &GameState) -> String {
    let terrains = state
        .topology
        .tiles
        .iter()
        .map(|tile| terrain_name(tile.terrain))
        .collect::<Vec<_>>()
        .join(",");
    let numbers = (0..state.topology.tiles.len())
        .map(|i| {
            tile_number(&state.topology, i)
                .map(|number| number.to_string())
                .unwrap_or_else(|| "0".to_string())
        })
        .collect::<Vec<_>>()
        .join(",");
    let ports = state
        .topology
        .port_resources()
        .into_iter()
        .map(port_name)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{}{}:{}:{}:{}",
        EDITOR_LOG_V2_PREFIX,
        terrains,
        numbers,
        state.topology.port_layout().as_str(),
        ports
    )
}

struct EditorLogState {
    terrains: Vec<String>,
    numbers: Vec<Option<u8>>,
    port_layout: Option<String>,
    ports: Option<Vec<String>>,
}

fn decode_editor_v1_log_state(text: &str) -> Result<EditorLogState, String> {
    let body = text
        .trim()
        .strip_prefix(EDITOR_LOG_V1_PREFIX)
        .ok_or_else(|| "edited board log state is missing editor-v1 prefix".to_string())?;
    let (terrain_text, number_text) = body
        .split_once(':')
        .ok_or_else(|| "edited board log state is missing number layout".to_string())?;
    let (terrains, numbers) = decode_editor_tiles(terrain_text, number_text)?;
    Ok(EditorLogState {
        terrains,
        numbers,
        port_layout: None,
        ports: None,
    })
}

fn decode_editor_v2_log_state(text: &str) -> Result<EditorLogState, String> {
    let body = text
        .trim()
        .strip_prefix(EDITOR_LOG_V2_PREFIX)
        .ok_or_else(|| "edited board log state is missing editor-v2 prefix".to_string())?;
    let (terrain_text, rest) = body
        .split_once(':')
        .ok_or_else(|| "edited board log state is missing number layout".to_string())?;
    let (number_text, rest) = rest
        .split_once(':')
        .ok_or_else(|| "edited board log state is missing port layout".to_string())?;
    let (port_layout, port_text) = rest
        .split_once(':')
        .ok_or_else(|| "edited board log state is missing ports".to_string())?;
    let (terrains, numbers) = decode_editor_tiles(terrain_text, number_text)?;
    let ports = port_text
        .split(',')
        .map(|port| port.trim().to_string())
        .collect::<Vec<_>>();
    Ok(EditorLogState {
        terrains,
        numbers,
        port_layout: Some(port_layout.trim().to_string()),
        ports: Some(ports),
    })
}

fn decode_editor_tiles(
    terrain_text: &str,
    number_text: &str,
) -> Result<(Vec<String>, Vec<Option<u8>>), String> {
    let terrains = terrain_text
        .split(',')
        .map(|terrain| terrain.trim().to_string())
        .collect::<Vec<_>>();
    let numbers = number_text
        .split(',')
        .map(|number| {
            let number = number.trim();
            if number.is_empty() || number == "0" {
                return Ok(None);
            }
            number
                .parse::<u8>()
                .map(Some)
                .map_err(|e| format!("invalid edited board number '{number}': {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((terrains, numbers))
}

impl GamePresenter<GameState> for CatanPresenter {
    fn serialize_state(&self, state: &GameState) -> serde_json::Value {
        self.serialize_state_with_perspective(state, None)
    }

    fn serialize_state_for_player(&self, state: &GameState, player: usize) -> serde_json::Value {
        let perspective = match player {
            0 => Some(Player::One),
            1 => Some(Player::Two),
            _ => None,
        };
        self.serialize_state_with_perspective(state, perspective)
    }

    fn action_label(&self, state: &GameState, action: usize) -> String {
        visualize::format_action_desc(ActionId(action as u8), state)
    }

    fn human_legal_actions(&self, state: &GameState, actions: &mut Vec<usize>) {
        actions.clear();
        let mut catan_actions = Vec::new();
        game::action::human_legal_actions(state, &mut catan_actions);
        actions.extend(catan_actions.iter().map(|a| a.0 as usize));
    }

    fn is_singleplayer_undo_barrier(&self, _state: &GameState, action: usize) -> bool {
        action == ROLL as usize
    }

    fn action_description(&self, state: &GameState, action: usize) -> String {
        visualize::format_action_desc(ActionId(action as u8), state)
    }

    fn chance_label(&self, state: &GameState, outcome: usize) -> String {
        match state.phase {
            Phase::Roll => {
                let roll = (outcome + 2) as u8;
                format!("Rolled {roll}")
            }
            Phase::StealResolve => {
                if let Some(&r) = ALL_RESOURCES.get(outcome) {
                    format!("Stole {r}")
                } else {
                    String::new()
                }
            }
            Phase::DevCardDraw => {
                if let Some(&kind) = DevCardKind::ALL.get(outcome) {
                    format!("Drew {kind:?}")
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        }
    }

    fn action_log_label_for_player(
        &self,
        state: &GameState,
        action: usize,
        _is_chance: bool,
        label: &str,
        player: usize,
    ) -> String {
        let perspective = match player {
            0 => Player::One,
            1 => Player::Two,
            _ => return label.to_string(),
        };

        if matches!(state.phase, Phase::DevCardDraw) && state.current_player != perspective {
            return "Drew dev".into();
        }

        if (DISCARD_START..DISCARD_END).contains(&(action as u8)) {
            if let Phase::Discard { player, .. } = state.phase {
                if player != perspective {
                    let player_num = if player == Player::One { 1 } else { 2 };
                    return format!("P{player_num}: Discard");
                }
            }
        }

        label.to_string()
    }

    fn phase_label(&self, state: &GameState) -> String {
        visualize::format_phase(&state.phase)
    }

    fn board_fingerprint(&self, state: &GameState) -> Option<u64> {
        Some(state.topology.board_code())
    }

    fn serialize_log_state(&self, state: &GameState) -> Option<String> {
        Some(if can_use_compact_log_state(state) {
            state.to_string()
        } else {
            encode_editor_log_state(state)
        })
    }

    fn deserialize_log_state(&self, text: &str) -> Result<GameState, String> {
        if text.trim().starts_with(EDITOR_LOG_V2_PREFIX) {
            let log_state = decode_editor_v2_log_state(text)?;
            return self.build_edited_game(
                &log_state.terrains,
                &log_state.numbers,
                log_state.port_layout.as_deref(),
                log_state.ports.as_deref(),
            );
        }
        if text.trim().starts_with(EDITOR_LOG_V1_PREFIX) {
            let log_state = decode_editor_v1_log_state(text)?;
            return self.build_edited_game(
                &log_state.terrains,
                &log_state.numbers,
                log_state.port_layout.as_deref(),
                log_state.ports.as_deref(),
            );
        }
        let mut state: GameState = text.parse()?;
        state.dice = self.dice;
        Ok(state)
    }

    fn normalize_replay_actions(&self, initial_state: &GameState, actions: &[usize]) -> Vec<usize> {
        game::action::canonicalize_replay_actions(initial_state, actions)
    }

    fn static_dir(&self) -> &Path {
        &self.static_dir
    }

    fn new_game(&self, seed: u64) -> GameState {
        game::new_game(seed, self.dice, 15, 9)
    }

    fn new_game_from_editor(
        &self,
        terrains: &[String],
        numbers: &[Option<u8>],
        port_layout: Option<&str>,
        ports: Option<&[String]>,
    ) -> Result<GameState, String> {
        self.build_edited_game(terrains, numbers, port_layout, ports)
    }
}

impl CatanPresenter {
    fn serialize_state_with_perspective(
        &self,
        state: &GameState,
        perspective: Option<Player>,
    ) -> serde_json::Value {
        let board = visualize::build_board(state);
        let frame = visualize::capture_frame_with_perspective(
            state,
            "",
            state.current_player as u8,
            None,
            perspective,
        );

        let (expected_dev, expected_bank_dev) = expected_hidden_dev_cards(state);

        // Balanced dice info: normalized probabilities for the next roll.
        // If the current player already rolled (main phase), the next roller
        // is the opponent; otherwise it's the current player.
        let dice_info = match &state.dice {
            Dice::Balanced(b) => {
                let next_roller = if state.pre_roll || state.setup_count < 4 {
                    state.current_player
                } else {
                    state.current_player.opponent()
                };
                let ws = b.weights(next_roller);
                let total: f64 = ws.iter().map(|(_, w)| *w as f64).sum();
                let probs: Vec<f64> = ws
                    .iter()
                    .map(|(_, w)| if total > 0.0 { *w as f64 / total } else { 0.0 })
                    .collect();
                Some(serde_json::json!({
                    "probs": probs,
                    "cards_left": b.cards_left(),
                    "total_cards": 36,
                }))
            }
            Dice::Random => None,
        };

        let mut v = serde_json::json!({
            "board": board,
            "frame": frame,
            "turn": state.turn_number,
            "current_player": state.current_player as u8,
            "p1_vp": state.total_vps(Player::One),
            "p2_vp": state.total_vps(Player::Two),
            "expected_dev": expected_dev,
        });
        if let Some(bank_dev) = expected_bank_dev {
            v["expected_bank_dev"] = serde_json::json!(bank_dev);
        }
        if let Some(dice) = dice_info {
            v["dice"] = dice;
        }
        if let Some(ref names) = self.player_names {
            v["player_names"] = serde_json::json!(names);
        }
        if let Some(player) = perspective {
            v["private_view"] = serde_json::json!(true);
            v["local_player"] = serde_json::json!(player as u8);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presenter() -> CatanPresenter {
        CatanPresenter::new(PathBuf::new(), Dice::default())
    }

    fn valid_editor_layout() -> (Vec<String>, Vec<Option<u8>>) {
        let terrains = [
            "forest",
            "hills",
            "pasture",
            "fields",
            "mountains",
            "desert",
            "forest",
            "hills",
            "pasture",
            "fields",
            "mountains",
            "forest",
            "hills",
            "pasture",
            "fields",
            "mountains",
            "forest",
            "pasture",
            "fields",
        ]
        .iter()
        .map(|terrain| terrain.to_string())
        .collect::<Vec<_>>();
        let numbers = vec![
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(6),
            None,
            Some(8),
            Some(9),
            Some(10),
            Some(11),
            Some(12),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(6),
            Some(8),
            Some(9),
            Some(10),
        ];
        (terrains, numbers)
    }

    fn valid_editor_ports() -> Vec<String> {
        [
            "generic", "lumber", "brick", "wool", "grain", "ore", "generic", "generic", "generic",
        ]
        .iter()
        .map(|port| port.to_string())
        .collect()
    }

    #[test]
    fn edited_game_validation_rejects_bad_layouts() {
        let presenter = presenter();
        let (terrains, mut numbers) = valid_editor_layout();
        numbers[0] = Some(7);
        assert!(
            presenter
                .new_game_from_editor(&terrains, &numbers, None, None)
                .is_err()
        );

        let (mut terrains, mut numbers) = valid_editor_layout();
        terrains[5] = "desert".into();
        numbers[5] = Some(8);
        assert!(
            presenter
                .new_game_from_editor(&terrains, &numbers, None, None)
                .is_err()
        );

        let (mut terrains, numbers) = valid_editor_layout();
        terrains[0] = "swamp".into();
        assert!(
            presenter
                .new_game_from_editor(&terrains, &numbers, None, None)
                .is_err()
        );

        let (terrains, numbers) = valid_editor_layout();
        assert!(
            presenter
                .new_game_from_editor(&terrains, &numbers, Some("sideways"), None)
                .is_err()
        );

        let mut ports = valid_editor_ports();
        ports.pop();
        assert!(
            presenter
                .new_game_from_editor(&terrains, &numbers, Some("primary"), Some(&ports))
                .is_err()
        );

        let mut ports = valid_editor_ports();
        ports[0] = "gold".into();
        assert!(
            presenter
                .new_game_from_editor(&terrains, &numbers, Some("primary"), Some(&ports))
                .is_err()
        );
    }

    #[test]
    fn edited_game_accepts_custom_port_types_and_layout() {
        let presenter = presenter();
        let (terrains, numbers) = valid_editor_layout();
        let ports = [
            "ore", "ore", "generic", "lumber", "brick", "wool", "grain", "generic", "generic",
        ]
        .iter()
        .map(|port| port.to_string())
        .collect::<Vec<_>>();

        let state = presenter
            .new_game_from_editor(&terrains, &numbers, Some("alternate"), Some(&ports))
            .expect("valid edited ports");

        assert_eq!(state.topology.port_layout(), PortLayout::Alternate);
        assert_eq!(state.topology.port_resources()[0], Some(Resource::Ore));
        assert_eq!(state.topology.port_resources()[1], Some(Resource::Ore));
        assert_eq!(state.topology.port_resources()[2], None);
    }

    #[test]
    fn serialized_board_includes_stable_port_metadata() {
        let presenter = presenter();
        let (terrains, numbers) = valid_editor_layout();
        let ports = valid_editor_ports();
        let state = presenter
            .new_game_from_editor(&terrains, &numbers, Some("alternate"), Some(&ports))
            .expect("valid edited ports");

        let serialized = presenter.serialize_state(&state);
        assert_eq!(serialized["board"]["port_layout"], "alternate");
        assert_eq!(serialized["board"]["ports"][0]["index"], 0);
        assert_eq!(serialized["board"]["ports"][0]["kind"], "generic");
    }

    #[test]
    fn edited_v2_log_state_round_trips_custom_numbers_and_ports() {
        let presenter = presenter();
        let (terrains, numbers) = valid_editor_layout();
        let ports = [
            "ore", "ore", "generic", "lumber", "brick", "wool", "grain", "generic", "generic",
        ]
        .iter()
        .map(|port| port.to_string())
        .collect::<Vec<_>>();
        let state = presenter
            .new_game_from_editor(&terrains, &numbers, Some("alternate"), Some(&ports))
            .expect("valid edited layout");

        let encoded = presenter
            .serialize_log_state(&state)
            .expect("edited log state");
        assert!(encoded.starts_with(EDITOR_LOG_V2_PREFIX));

        let decoded = presenter
            .deserialize_log_state(&encoded)
            .expect("edited log state round trip");
        for i in 0..EDITOR_TILE_COUNT {
            assert_eq!(
                state.topology.tiles[i].terrain,
                decoded.topology.tiles[i].terrain
            );
            assert_eq!(
                tile_number(&state.topology, i),
                tile_number(&decoded.topology, i)
            );
        }
        assert_eq!(decoded.topology.port_layout(), PortLayout::Alternate);
        assert_eq!(
            decoded.topology.port_resources(),
            state.topology.port_resources()
        );
    }

    #[test]
    fn edited_v1_log_state_still_decodes() {
        let presenter = presenter();
        let (terrains, numbers) = valid_editor_layout();
        let terrain_text = terrains.join(",");
        let number_text = numbers
            .iter()
            .map(|number| {
                number
                    .map(|number| number.to_string())
                    .unwrap_or_else(|| "0".to_string())
            })
            .collect::<Vec<_>>()
            .join(",");
        let encoded = format!("{EDITOR_LOG_V1_PREFIX}{terrain_text}:{number_text}");

        let decoded = presenter
            .deserialize_log_state(&encoded)
            .expect("editor-v1 log state");

        assert_eq!(decoded.topology.port_layout(), PortLayout::Primary);
        assert_eq!(
            decoded.topology.port_resources(),
            Topology::default_port_resources()
        );
    }

    #[test]
    fn multiplayer_log_redacts_opponent_private_card_labels() {
        let presenter = presenter();
        let mut state = presenter.new_game(42);

        state.current_player = Player::One;
        state.phase = Phase::DevCardDraw;
        assert_eq!(
            presenter.action_log_label_for_player(&state, 0, true, "Drew Knight", 0),
            "Drew Knight"
        );
        assert_eq!(
            presenter.action_log_label_for_player(&state, 0, true, "Drew Knight", 1),
            "Drew dev"
        );

        state.phase = Phase::Discard {
            player: Player::One,
            remaining: 1,
            roller: Player::Two,
            min_resource: 0,
        };
        assert_eq!(
            presenter.action_log_label_for_player(
                &state,
                DISCARD_START as usize,
                false,
                "P1: Drop ore",
                1,
            ),
            "P1: Discard"
        );
    }

    #[test]
    fn presenter_human_legal_actions_use_relaxed_catan_helper() {
        let presenter = presenter();
        let state = presenter.new_game(42);

        let mut presenter_actions = Vec::new();
        presenter.human_legal_actions(&state, &mut presenter_actions);

        let mut expected = Vec::new();
        game::action::human_legal_actions(&state, &mut expected);
        let expected = expected.iter().map(|a| a.0 as usize).collect::<Vec<_>>();

        assert_eq!(presenter_actions, expected);
        assert_eq!(
            presenter_actions.len(),
            54,
            "singleplayer setup should expose every distance-legal settlement"
        );
    }
}

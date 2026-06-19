// Entry point: wires components together.

const session = new Session({
  getAuthToken: () => window.hexfishGetClerkToken ? window.hexfishGetClerkToken() : null,
});
const board = new Board(document.getElementById('board-svg'));
window.hexfishBoard = board;
const mctsPanel = new MCTSPanel();
const controls = new Controls(session);

const RESOURCE_NAMES = ['lumber', 'brick', 'wool', 'grain', 'ore'];
const TERRAIN_RESOURCE_INDEX = {
  forest: 0,
  hills: 1,
  pasture: 2,
  fields: 3,
  mountains: 4,
};
const DEV_CARD_NAMES = ['Knight', 'VP', 'Road Building', 'Year of Plenty', 'Monopoly'];
const DEV_SHORT = ['Kn', 'VP', 'RB', 'YP', 'Mo'];
const PLAYER_COLORS = ['#4a9eff', '#ff6b6b'];
const EDITOR_TERRAINS = ['forest', 'hills', 'pasture', 'fields', 'mountains', 'desert'];
const EDITOR_NUMBERS = [2, 3, 4, 5, 6, 8, 9, 10, 11, 12];
const EDITOR_PORT_KINDS = ['generic', 'lumber', 'brick', 'wool', 'grain', 'ore'];
const EDITOR_PORT_LABELS = {
  generic: 'Generic',
  lumber: 'Lumber',
  brick: 'Brick',
  wool: 'Wool',
  grain: 'Grain',
  ore: 'Ore',
};
const EDITOR_PORT_COLORS = {
  generic: '#ffffff',
  lumber: '#2d5a27',
  brick: '#b85c38',
  wool: '#7ec850',
  grain: '#e8b430',
  ore: '#7a7a7a',
};
const PLAY_DIFFICULTIES = [
  { level: 1, simulations: 50, depth: 2 },
  { level: 2, simulations: 100, depth: 3 },
  { level: 3, simulations: 200, depth: 5 },
  { level: 4, simulations: 350, depth: 7 },
  { level: 5, simulations: 550, depth: 9 },
  { level: 6, simulations: 800, depth: 12 },
  { level: 7, simulations: 1200, depth: 15 },
  { level: 8, simulations: 1700, depth: 18 },
  { level: 9, simulations: 2300, depth: 21 },
  { level: 10, simulations: 3000, depth: 25 },
];
const EDITOR_TERRAIN_BAG = [
  'forest', 'forest', 'forest', 'forest',
  'hills', 'hills', 'hills',
  'pasture', 'pasture', 'pasture', 'pasture',
  'fields', 'fields', 'fields', 'fields',
  'mountains', 'mountains', 'mountains',
  'desert',
];
const EDITOR_NUMBER_BAG = [2, 3, 3, 4, 4, 5, 5, 6, 6, 8, 8, 9, 9, 10, 10, 11, 11, 12];
const EDITOR_PORT_BAG = ['lumber', 'brick', 'wool', 'grain', 'ore', 'generic', 'generic', 'generic', 'generic'];

function playerColor(playerIndex) {
  return playerIndex === 0 || playerIndex === 1 ? PLAYER_COLORS[playerIndex] : '';
}

function createBlankEditorTiles() {
  return Array.from({ length: 19 }, () => ({ terrain: null, number: null }));
}

function isEditorPortKind(kind) {
  return EDITOR_PORT_KINDS.includes(kind);
}

function shuffled(values) {
  const out = [...values];
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

let currentState = null;
let analysisState = null;
let replayState = null;
let currentBoard = null;
let editorBaseBoard = null;
let editorTiles = createBlankEditorTiles();
let selectedEditorTile = null;
let editorPortLayout = 'primary';
let editorPorts = [];
let selectedEditorPort = null;
let pendingEditorStart = false;
let pendingNewGameSearch = false;
let lastActionLogLength = { analysis: null, replay: null };
let previousFrameHands = { analysis: null, replay: null };
let activeView = 'play-setup';
let selectedPlayHumanPlayer = 0;
let selectedPlayDifficulty = 5;
let playMode = {
  active: false,
  humanPlayer: 0,
  botThinking: false,
  forcedMoveKey: null,
  pendingHumanMove: false,
};
let serverSingleplayerHumanPlayer = undefined;

function setServerSingleplayerHumanPlayer(player) {
  const humanPlayer = player == null ? null : player;
  if (serverSingleplayerHumanPlayer === humanPlayer) return;
  serverSingleplayerHumanPlayer = humanPlayer;
  session.send({ type: 'SetSingleplayer', human_player: humanPlayer });
}

controls.onNewGame = () => {
  if (activeView === 'play') {
    pendingNewGameSearch = false;
    playMode.active = false;
    playMode.botThinking = false;
    playMode.forcedMoveKey = null;
    playMode.pendingHumanMove = false;
    setServerSingleplayerHumanPlayer(null);
    mctsPanel.clear();
    board.clearSearchHighlights();
    showPlaySetupView();
    return false;
  }
  pendingNewGameSearch = true;
  playMode.active = false;
  playMode.botThinking = false;
  playMode.forcedMoveKey = null;
  playMode.pendingHumanMove = false;
  mctsPanel.clear();
  board.clearSearchHighlights();
  return true;
};

function updateBoardChromePlacement() {
  const shell = document.getElementById('board-shell');
  const svg = document.getElementById('board-svg');
  if (!shell || !svg) return;

  const shellRect = shell.getBoundingClientRect();
  const svgRect = svg.getBoundingClientRect();
  const viewBox = svg.viewBox?.baseVal;
  if (!shellRect.width || !shellRect.height || !svgRect.width || !svgRect.height || !viewBox?.width || !viewBox?.height) {
    return;
  }

  const svgAspect = svgRect.width / svgRect.height;
  const viewAspect = viewBox.width / viewBox.height;
  let contentWidth = svgRect.width;
  let contentHeight = svgRect.height;
  let insetX = 0;
  let insetY = 0;

  if (svgAspect > viewAspect) {
    contentWidth = svgRect.height * viewAspect;
    insetX = (svgRect.width - contentWidth) / 2;
  } else {
    contentHeight = svgRect.width / viewAspect;
    insetY = (svgRect.height - contentHeight) / 2;
  }

  const left = Math.max(0, svgRect.left - shellRect.left + insetX);
  const top = Math.max(0, svgRect.top - shellRect.top + insetY);
  const right = Math.max(0, shellRect.right - svgRect.right + insetX);
  const bottom = Math.max(0, shellRect.bottom - svgRect.bottom + insetY);

  shell.style.setProperty('--board-content-left', `${left}px`);
  shell.style.setProperty('--board-content-top', `${top}px`);
  shell.style.setProperty('--board-content-right', `${right}px`);
  shell.style.setProperty('--board-content-bottom', `${bottom}px`);
  shell.style.setProperty('--board-content-center-x', `${left + contentWidth / 2}px`);

}

function scheduleBoardChromePlacement() {
  requestAnimationFrame(updateBoardChromePlacement);
}

function logHistoryCursor(msg) {
  if (Number.isFinite(msg?.history_cursor)) return msg.history_cursor;
  if (Number.isFinite(msg?.replay?.cursor)) return msg.replay.cursor;
  return Array.isArray(msg?.action_log) ? msg.action_log.length : 0;
}

function actionLogCursors(msg) {
  if (Array.isArray(msg?.action_log_cursors)) return msg.action_log_cursors;
  const entries = Array.isArray(msg?.action_log) ? msg.action_log : [];
  return entries.map((_, i) => i + 1);
}

function activeActionLogLength(msg) {
  const cursors = actionLogCursors(msg);
  const cursor = logHistoryCursor(msg);
  let active = 0;
  while (active < cursors.length && cursors[active] <= cursor) {
    active++;
  }
  return active;
}

function activeActionLogIndex(msg) {
  return activeActionLogLength(msg) - 1;
}

function playViewActive() {
  return activeView === 'play' && playMode.active;
}

function isPlayBotTurn(msg = currentState) {
  return !!(
    playViewActive() &&
    msg &&
    !msg.replay &&
    !msg.is_terminal &&
    !msg.is_chance &&
    msg.current_player !== playMode.humanPlayer
  );
}

function playBotName(level = selectedPlayDifficulty) {
  return `HexFish${playDifficultyConfig(level).level}`;
}

function updatePlayBotLabels() {
  const heading = document.getElementById('play-setup-title');
  if (heading) heading.textContent = `You vs ${playBotName()}`;
}

function playNameForPlayer(idx) {
  if (!playViewActive()) return null;
  return idx === playMode.humanPlayer ? 'You' : playBotName();
}

function playerDisplayName(idx) {
  return playNameForPlayer(idx) || `P${idx + 1}`;
}

function formatPlayerRefs(text) {
  if (!playViewActive()) return text;
  return String(text).replace(/\bP([12])\b/g, (_, num) => playerDisplayName(Number(num) - 1));
}

function formatResultBanner(text) {
  const winner = parseResultWinner(text);
  if (playViewActive() && winner === playMode.humanPlayer) {
    return `${playerDisplayName(winner)} win`;
  }
  return formatPlayerRefs(text);
}

function playDifficultyConfig(level = selectedPlayDifficulty) {
  return PLAY_DIFFICULTIES.find(cfg => cfg.level === level) || PLAY_DIFFICULTIES[4];
}

function playDifficultyDetails(cfg = playDifficultyConfig()) {
  return `Level ${cfg.level}: target depth ${cfg.depth}, up to ${cfg.simulations.toLocaleString()} sims`;
}

function playDifficultyBudget() {
  const cfg = playDifficultyConfig();
  return {
    mode: 'pv_depth',
    value: cfg.depth,
    simulations: cfg.simulations,
  };
}

function playCanPonder(msg = currentState) {
  return !!(
    playViewActive() &&
    msg &&
    !msg.replay &&
    !msg.is_terminal &&
    !msg.is_chance &&
    msg.current_player === playMode.humanPlayer &&
    Array.isArray(msg.legal_actions) &&
    msg.legal_actions.length > 0
  );
}

function playForcedMoveKey(msg, action) {
  return `${logHistoryCursor(msg)}:${msg.current_player}:${msg.phase}:${action}`;
}

function sendPlayAction(action) {
  const msg = { type: 'PlayAction', action };
  if (playViewActive()) {
    if (playMode.pendingHumanMove) return;
    playMode.pendingHumanMove = true;
    if (controls.searchRunning && controls.interruptSearchForCommand(msg)) return;
  }
  session.send(msg);
}

function setPlayDifficulty(level) {
  const nextLevel = Number.isFinite(level) ? Math.max(1, Math.min(10, Math.round(level))) : 5;
  selectedPlayDifficulty = nextLevel;
  const cfg = playDifficultyConfig();
  const detail = playDifficultyDetails(cfg);
  const select = document.getElementById('play-difficulty-select');
  const label = document.getElementById('play-difficulty-label');
  if (select) {
    select.value = String(cfg.level);
    select.title = detail;
    select.setAttribute('aria-label', `HexFish difficulty. ${detail}`);
  }
  if (label) label.title = detail;
  updatePlayBotLabels();
}

function initPlayDifficultySelect() {
  const select = document.getElementById('play-difficulty-select');
  if (!select) return;
  select.innerHTML = '';
  for (const cfg of PLAY_DIFFICULTIES) {
    const option = document.createElement('option');
    option.value = String(cfg.level);
    option.textContent = String(cfg.level);
    option.title = playDifficultyDetails(cfg);
    select.appendChild(option);
  }
  select.addEventListener('change', () => setPlayDifficulty(Number(select.value)));
  setPlayDifficulty(selectedPlayDifficulty);
}

function setPlaySide(player) {
  selectedPlayHumanPlayer = player === 1 ? 1 : 0;
  for (const btn of document.querySelectorAll('.play-side-btn')) {
    const active = Number(btn.dataset.player) === selectedPlayHumanPlayer;
    btn.classList.toggle('bg-accent', active);
    btn.classList.toggle('text-white', active);
    btn.classList.toggle('bg-bg-3', !active);
    btn.classList.toggle('text-gray-300', !active);
    btn.classList.toggle('hover:bg-bg', !active);
    btn.setAttribute('aria-pressed', active ? 'true' : 'false');
  }
}

// ── Message handlers ─────────────────────────────────────────────────

session.on('GameState', (msg) => {
  if (!msg.replay && msg.state?.board) {
    editorBaseBoard = msg.state.board;
    if (activeView !== 'editor') resetEditorPortsFromBaseBoard();
  }
  if (msg.replay) {
    replayState = msg;
    if (activeView === 'replay-board') renderGameState(msg);
  } else {
    analysisState = msg;
    if (pendingEditorStart) {
      pendingEditorStart = false;
      setEditorStatus('');
      showAnalysisView();
      return;
    }
    if (activeView === 'editor') {
      renderEditorBoard();
      return;
    }
    if (activeView === 'play') {
      renderGameState(msg);
      runPlayAutomation(msg);
      return;
    }
    if (activeView === 'analysis') {
      renderGameState(msg);
      runPendingNewGameSearch(msg);
    }
  }
});

function runPendingNewGameSearch(msg) {
  if (!pendingNewGameSearch || msg.replay) return;
  if (msg.is_terminal || msg.is_chance || !Array.isArray(msg.legal_actions) || msg.legal_actions.length === 0) {
    pendingNewGameSearch = false;
    return;
  }
  pendingNewGameSearch = false;
  controls.runSearch();
}

function renderGameState(msg) {
  currentState = msg;
  if (playViewActive()) playMode.pendingHumanMove = false;
  const state = msg.state;
  const viewKey = msg.replay ? 'replay' : 'analysis';

  // Initialize board on first state (or after new game).
  if (state.board) {
    const boardChanged = !currentBoard ||
      JSON.stringify(state.board) !== JSON.stringify(currentBoard);
    if (boardChanged) {
      currentBoard = state.board;
      board.initBoard(currentBoard);
    }
  }

  // Update frame
  if (state.frame && currentBoard) {
    board.updateFrame(state.frame, currentBoard);
  }

  // Phase / turn
  document.getElementById('phase-label').textContent = msg.phase;
  document.getElementById('turn-label').textContent = state.turn != null ? `Turn ${state.turn}` : '';

  // Player panels
  updatePlayerPanel(0, state);
  updatePlayerPanel(1, state);
  updateBoardResourceLegend(msg, state);
  updateBank(state);
  updateDice(state);

  // Highlight active player
  document.getElementById('player-0').classList.toggle('active', msg.current_player === 0);
  document.getElementById('player-1').classList.toggle('active', msg.current_player === 1);

  // Legal actions
  const actionList = document.getElementById('action-list');
  actionList.innerHTML = '';
  board.clearOverlays();
  const playBotTurn = isPlayBotTurn(msg);
  updateActionPanelStatus(msg, playBotTurn);

  if (!msg.replay && !msg.is_terminal && !msg.is_chance && !playBotTurn) {
    // Board overlays for spatial actions
    if (currentBoard) {
      board.showLegalActions(msg.legal_actions, currentBoard, msg.current_player);
    }

    // Button list for all actions
    for (const a of msg.legal_actions) {
      const btn = document.createElement('button');
      btn.className = 'w-full py-1 px-2 text-left text-[11px] leading-snug bg-bg-3 border border-gray-700 text-gray-200 rounded cursor-pointer hover:bg-accent transition-colors';
      btn.textContent = a.label;
      btn.addEventListener('click', () => {
        if (isPlayBotTurn()) return;
        sendPlayAction(a.action);
      });
      btn.addEventListener('mouseenter', () => showLegalActionPreview(a.action));
      btn.addEventListener('mouseleave', () => board.clearActionPreview());
      btn.addEventListener('focus', () => showLegalActionPreview(a.action));
      btn.addEventListener('blur', () => board.clearActionPreview());
      actionList.appendChild(btn);
    }
  }

  // Game log
  const logView = document.getElementById('log-view');
  const wasAtBottom = logView.scrollHeight - logView.scrollTop - logView.clientHeight < 8;
  const previousScrollTop = logView.scrollTop;
  const cursors = actionLogCursors(msg);
  const activeLogIndex = activeActionLogIndex(msg);
  logView.innerHTML = '';
  if (msg.action_log) {
    for (let i = 0; i < msg.action_log.length; i++) {
      const line = document.createElement('div');
      const active = i === activeLogIndex;
      line.className = active
        ? 'py-0.5 px-1 rounded bg-bg-3 text-gray-100'
        : 'py-0.5 px-1 rounded hover:bg-bg-3';
      const rawText = msg.action_log[i];
      const text = formatPlayerRefs(rawText);
      if (rawText.startsWith('P1:')) {
        line.style.color = PLAYER_COLORS[0];
      } else if (rawText.startsWith('P2:')) {
        line.style.color = PLAYER_COLORS[1];
      } else {
        line.style.color = '#a0a0a0';
        line.style.fontStyle = 'italic';
      }
      const parts = text.split('\n');
      line.textContent = `${i + 1}. ${parts[0]}`;
      line.style.cursor = 'pointer';
      line.addEventListener('click', () => {
        controls.stopAutoplay();
        controls._disableAutoSearch();
        const cursor = cursors[i] ?? i + 1;
        if (msg.replay) {
          session.send({ type: 'SetReplayCursor', cursor });
        } else {
          session.send({ type: 'SetLogCursor', cursor });
        }
      });
      logView.appendChild(line);
      for (let p = 1; p < parts.length; p++) {
        const sub = document.createElement('div');
        sub.style.color = '#888';
        sub.style.fontSize = '0.85em';
        sub.style.paddingLeft = '1.5em';
        sub.textContent = parts[p].trim();
        logView.appendChild(sub);
      }
    }
    if (wasAtBottom) {
      logView.scrollTop = logView.scrollHeight;
    } else {
      logView.scrollTop = previousScrollTop;
    }
  }

  updateRollBadge(msg, state, viewKey);

  // Undo/Redo button states
  document.getElementById('btn-undo').disabled = playBotTurn || !msg.can_undo;
  document.getElementById('btn-redo').disabled = playBotTurn || !msg.can_redo;

  // Result banner
  const banner = document.getElementById('result-banner');
  if (msg.is_terminal && msg.result) {
    const winner = parseResultWinner(msg.result);
    banner.textContent = formatResultBanner(msg.result);
    banner.style.background = playerColor(winner) || '';
    banner.classList.remove('hidden');
    controls.onGameOver();
  } else {
    banner.classList.add('hidden');
    banner.style.background = '';
  }

  controls.onStateUpdate(msg);
  updateViewChrome(msg);
  scheduleBoardChromePlacement();
}

function updateActionPanelStatus(msg, playBotTurn) {
  const title = document.getElementById('action-panel-title');
  const status = document.getElementById('play-status');
  if (!title || !status) return;

  if (!playViewActive()) {
    title.textContent = 'Legal Moves';
    status.textContent = '';
    status.title = '';
    status.classList.add('hidden');
    return;
  }

  if (playBotTurn) {
    title.textContent = 'Play';
    status.textContent = `${playBotName()} thinking`;
    status.title = playDifficultyDetails();
    status.classList.remove('hidden');
    return;
  }

  title.textContent = msg?.is_terminal ? 'Play' : 'Legal Moves';
  status.textContent = '';
  status.title = '';
  status.classList.add('hidden');
}

function showLegalActionPreview(action) {
  if (!currentBoard || !currentState) return;
  if (isPlayBotTurn()) return;
  board.showActionPreview(action, currentBoard, currentState.current_player);
}

function updateRollBadge(msg, state, viewKey) {
  const entries = msg.action_log || [];
  const currentLength = activeActionLogLength(msg);
  const currentHands = frameHands(state);
  const previousLength = lastActionLogLength[viewKey];

  if (previousLength == null) {
    lastActionLogLength[viewKey] = currentLength;
    previousFrameHands[viewKey] = currentHands;
    return;
  }

  if (currentLength < previousLength) {
    hideRollBadge();
    clearResourceProductionAnimations();
    lastActionLogLength[viewKey] = currentLength;
    previousFrameHands[viewKey] = currentHands;
    return;
  }

  if (currentLength === previousLength) {
    previousFrameHands[viewKey] = currentHands;
    return;
  }

  const newEntries = entries.slice(previousLength, currentLength);
  const handGained = hasPositiveHandGain(previousFrameHands[viewKey], currentHands);
  for (let i = newEntries.length - 1; i >= 0; i--) {
    const total = parseRollTotal(newEntries[i]);
    if (total == null) continue;

    const entryIndex = previousLength + i;
    const roller = parseRoller(newEntries[i]) ??
      findRecentRoller(entries, entryIndex - 1) ??
      msg.current_player;

    if (total === 7) {
      hideRollBadge();
    } else if (rollLogHasExplicitGain(newEntries[i]) || handGained) {
      showRollBadge(total, roller);
      showResourceProductionAnimations(total, previousFrameHands[viewKey], currentHands, state);
    } else {
      hideRollBadge();
    }
    break;
  }

  lastActionLogLength[viewKey] = currentLength;
  previousFrameHands[viewKey] = currentHands;
}

function frameHands(state) {
  const players = state.frame && state.frame.players;
  if (!players) return null;
  return players.map(p => p.hand ? [...p.hand] : [0, 0, 0, 0, 0]);
}

function hasPositiveHandGain(before, after) {
  if (!before || !after) return false;
  for (let p = 0; p < after.length; p++) {
    for (let r = 0; r < after[p].length; r++) {
      if (after[p][r] > (before[p]?.[r] ?? 0)) return true;
    }
  }
  return false;
}

function parseRollTotal(entry) {
  const firstLine = String(entry).split('\n')[0];
  const match = firstLine.match(/\b(?:Rolled|rolls)\s+(\d{1,2})\b/i);
  if (!match) return null;
  const total = Number(match[1]);
  return total >= 2 && total <= 12 ? total : null;
}

function parseRoller(entry) {
  const firstLine = String(entry).split('\n')[0];
  if (!/\broll(?:ed|s)?\b/i.test(firstLine)) return null;

  const match = firstLine.match(/\bP([12])\b/i);
  if (!match) return null;
  return Number(match[1]) - 1;
}

function findRecentRoller(entries, startIndex) {
  for (let i = startIndex; i >= 0; i--) {
    const roller = parseRoller(entries[i]);
    if (roller != null) return roller;
    if (parseRollTotal(entries[i]) != null) break;
  }
  return null;
}

function parseResultWinner(result) {
  const match = String(result).match(/\bP([12])\s+wins\b/i);
  if (!match) return null;
  return Number(match[1]) - 1;
}

function rollLogHasExplicitGain(entry) {
  return String(entry)
    .split('\n')
    .slice(1)
    .some(line => /^\s*P[12]:\s*\S/.test(line));
}

function showRollBadge(total, roller) {
  const badge = document.getElementById('roll-badge');
  if (!badge) return;
  const playerIndex = roller === 0 || roller === 1 ? roller : null;
  badge.textContent = total;
  badge.style.background = playerColor(playerIndex);
  badge.setAttribute('aria-label', playerIndex == null
    ? `Roll ${total}`
    : `${playerDisplayName(playerIndex)} rolled ${total}`);
  badge.classList.remove('hidden');
}

function hideRollBadge() {
  const badge = document.getElementById('roll-badge');
  if (!badge) return;
  badge.classList.add('hidden');
}

function showResourceProductionAnimations(roll, beforeHands, afterHands, state) {
  const layer = document.getElementById('resource-animation-layer');
  if (!layer || !currentBoard || !state?.frame || !beforeHands || !afterHands) return;

  const items = resourceProductionAnimationItems(roll, beforeHands, afterHands, state);
  if (items.length === 0) return;

  for (let i = 0; i < items.length; i++) {
    const item = items[i];
    const tile = currentBoard.tiles[item.tileIndex];
    const from = tile ? resourceAnimationSource(tile) : null;
    const to = resourceAnimationTarget(item.resourceIndex, item.playerIndex);
    if (!from || !to) continue;

    const chip = document.createElement('span');
    chip.className = `resource-card resource-production-chip ${RESOURCE_NAMES[item.resourceIndex]}`;
    chip.textContent = `+${item.amount}`;
    chip.style.left = `${from.x}px`;
    chip.style.top = `${from.y}px`;
    chip.style.setProperty('--resource-dx', `${to.x - from.x}px`);
    chip.style.setProperty('--resource-dy', `${to.y - from.y}px`);
    chip.style.animationDelay = `${Math.min(i * 70, 420)}ms`;
    layer.appendChild(chip);

    chip.addEventListener('animationend', () => chip.remove(), { once: true });
    window.setTimeout(() => chip.remove(), 1600 + Math.min(i * 70, 420));
  }
}

function clearResourceProductionAnimations() {
  const layer = document.getElementById('resource-animation-layer');
  if (layer) layer.innerHTML = '';
}

function resourceProductionAnimationItems(roll, beforeHands, afterHands, state) {
  const frame = state.frame;
  const items = [];
  for (let playerIndex = 0; playerIndex < afterHands.length; playerIndex++) {
    for (let resourceIndex = 0; resourceIndex < RESOURCE_NAMES.length; resourceIndex++) {
      let remaining = (afterHands[playerIndex]?.[resourceIndex] ?? 0) -
        (beforeHands[playerIndex]?.[resourceIndex] ?? 0);
      if (remaining <= 0) continue;

      const sources = productionSourcesFor(playerIndex, resourceIndex, roll, frame);
      for (const source of sources) {
        if (remaining <= 0) break;
        const amount = Math.min(source.amount, remaining);
        items.push({
          tileIndex: source.tileIndex,
          playerIndex,
          resourceIndex,
          amount,
        });
        remaining -= amount;
      }
    }
  }
  return items;
}

function productionSourcesFor(playerIndex, resourceIndex, roll, frame) {
  const buildings = frame.buildings?.[playerIndex];
  if (!currentBoard?.tiles || !buildings) return [];

  const sources = [];
  const robberTile = Number(frame.robber);
  for (let tileIndex = 0; tileIndex < currentBoard.tiles.length; tileIndex++) {
    const tile = currentBoard.tiles[tileIndex];
    if (!tile || Number(tile.number) !== roll || tileIndex === robberTile) continue;
    if (TERRAIN_RESOURCE_INDEX[tile.terrain] !== resourceIndex) continue;

    const amount = tileProductionAmount(tile, buildings);
    if (amount > 0) {
      sources.push({ tileIndex, amount });
    }
  }
  return sources;
}

function tileProductionAmount(tile, buildings) {
  const nodeSet = new Set((tile.nodes || []).map(Number));
  let amount = 0;
  for (const node of buildings.settlements || []) {
    if (nodeSet.has(Number(node))) amount += 1;
  }
  for (const node of buildings.cities || []) {
    if (nodeSet.has(Number(node))) amount += 2;
  }
  return amount;
}

function resourceAnimationSource(tile) {
  const shell = document.getElementById('board-shell');
  const point = board.boardPointToShellPoint(tile.cx, tile.cy);
  if (!shell || !point) return null;
  const shellRect = shell.getBoundingClientRect();
  return {
    x: shellRect.left + point.x,
    y: shellRect.top + point.y,
  };
}

function resourceAnimationTarget(resourceIndex, playerIndex) {
  const target = resourceAnimationTargetElement(resourceIndex, playerIndex);
  if (!target) return null;
  return visibleElementCenter(target, 20);
}

function resourceAnimationTargetElement(resourceIndex, playerIndex) {
  if (playViewActive() && playerIndex !== playMode.humanPlayer) {
    return document.getElementById(`player-${playerIndex}`);
  }
  return document.querySelector(`#board-resource-legend [data-resource-index="${resourceIndex}"]`);
}

function visibleElementCenter(element, margin = 0) {
  const rect = element.getBoundingClientRect();
  if (!rect.width || !rect.height) return null;

  let clip = {
    left: margin,
    top: margin,
    right: Math.max(margin, window.innerWidth - margin),
    bottom: Math.max(margin, window.innerHeight - margin),
  };

  for (let parent = element.parentElement; parent; parent = parent.parentElement) {
    const style = window.getComputedStyle(parent);
    const overflow = `${style.overflow} ${style.overflowX} ${style.overflowY}`;
    if (!/(auto|scroll|hidden|clip)/.test(overflow)) continue;

    const parentRect = parent.getBoundingClientRect();
    clip = intersectRects(clip, parentRect) || clip;
  }

  const visible = intersectRects(rect, clip);
  if (visible) {
    return {
      x: (visible.left + visible.right) / 2,
      y: (visible.top + visible.bottom) / 2,
    };
  }

  return {
    x: clampNumber(rect.left + rect.width / 2, clip.left, clip.right),
    y: clampNumber(rect.top + rect.height / 2, clip.top, clip.bottom),
  };
}

function intersectRects(a, b) {
  const left = Math.max(a.left, b.left);
  const top = Math.max(a.top, b.top);
  const right = Math.min(a.right, b.right);
  const bottom = Math.min(a.bottom, b.bottom);
  if (right <= left || bottom <= top) return null;
  return { left, top, right, bottom };
}

function clampNumber(value, min, max) {
  if (max < min) return min;
  return Math.min(max, Math.max(min, value));
}

function updateSearchHighlights(snapshot, labels) {
  if (playViewActive()) {
    board.clearSearchHighlights();
    return;
  }
  if (!currentBoard || !snapshot.edges) return;
  const edges = snapshot.edges.map((e, i) => ({
    ...e,
    label: labels[i] || `Action ${e.action}`,
  }));
  // Sort by fresh visits descending (matches MCTS panel order).
  const fresh = (edge) => Number.isFinite(edge?.fresh_visits) ? edge.fresh_visits : (edge?.visits ?? 0);
  edges.sort((a, b) => {
    const visitDiff = fresh(b) - fresh(a);
    if (visitDiff !== 0) return visitDiff;
    return (b.improved_policy ?? 0) - (a.improved_policy ?? 0);
  });
  board.showSearchHighlights(edges, currentBoard);
}

session.on('Snapshot', (msg) => {
  mctsPanel.updateSnapshot(msg.snapshot, msg.action_labels, currentState?.current_player ?? 0);
  updateSearchHighlights(msg.snapshot, msg.action_labels);
  controls.onSimsDone(msg.snapshot);
});

session.on('ReplayList', (msg) => {
  renderReplayList(msg.entries || []);
});

session.on('Subtree', (msg) => {
  mctsPanel.showSubtree(msg.tree);
});

session.on('SearchProgress', (msg) => {
  controls.onSearchProgress();
  if (controls.isPausePending()) return;
  mctsPanel.updateSnapshot(msg.snapshot, msg.action_labels, currentState?.current_player ?? 0);
  mctsPanel.showProgress(msg.snapshot, msg.budget, msg.sims_total);
  updateSearchHighlights(msg.snapshot, msg.action_labels);
});

session.on('BotAction', (msg) => {
  playMode.botThinking = false;
  playMode.forcedMoveKey = null;
  playMode.pendingHumanMove = false;
  if (msg.snapshot) {
    mctsPanel.updateSnapshot(msg.snapshot, msg.action_labels || [], currentState?.current_player ?? 0);
    updateSearchHighlights(msg.snapshot, msg.action_labels || []);
  }
  controls.onBotDone();
});

session.on('Error', (msg) => {
  if (pendingEditorStart) {
    pendingEditorStart = false;
    setEditorStatus(msg.message);
  }
  controls.onSearchError();
  playMode.botThinking = false;
  playMode.forcedMoveKey = null;
  playMode.pendingHumanMove = false;
  updateActionPanelStatus(currentState, isPlayBotTurn(currentState));
  setReplayStatus(msg.message);
  console.error('Server error:', msg.message);
});

session.on('Disconnected', () => {
  controls.onSearchError();
  playMode.botThinking = false;
  playMode.forcedMoveKey = null;
  playMode.pendingHumanMove = false;
});

// ── View tabs / replay list ────────────────────────────────────────────────

function setTabState(tab) {
  const playTab = document.getElementById('tab-play');
  const boardTab = document.getElementById('tab-board');
  const replayTab = document.getElementById('tab-replay');
  const editorTab = document.getElementById('tab-editor');
  const isPlay = tab === 'play';
  const isAnalysis = tab === 'analysis';
  const isReplay = tab === 'replay';
  const isEditor = tab === 'editor';
  const activeClasses = 'view-tab active px-2 py-1 rounded text-gray-100 bg-bg-3';
  const idleClasses = 'view-tab px-2 py-1 rounded text-gray-400 hover:text-gray-100 hover:bg-bg-3';

  playTab.className = isPlay ? activeClasses : idleClasses;
  boardTab.className = isAnalysis ? activeClasses : idleClasses;
  replayTab.className = isReplay ? activeClasses : idleClasses;
  editorTab.className = isEditor ? activeClasses : idleClasses;
  playTab.setAttribute('aria-selected', String(isPlay));
  boardTab.setAttribute('aria-selected', String(isAnalysis));
  replayTab.setAttribute('aria-selected', String(isReplay));
  editorTab.setAttribute('aria-selected', String(isEditor));
}

function showPlayView() {
  if (!playMode.active) {
    showPlaySetupView();
    return;
  }
  setServerSingleplayerHumanPlayer(playMode.humanPlayer);
  activeView = 'play';
  setTabState('play');
  setEditorChrome(false);
  setGameHeaderLabelsVisible(true);
  document.getElementById('main-layout').classList.remove('hidden');
  document.getElementById('play-setup-view').classList.add('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.remove('hidden');
  controls.stopAutoplay();
  controls._disableAutoSearch();
  controls.setOptionsAvailable(false);
  if (analysisState) {
    renderGameState(analysisState);
    runPlayAutomation(analysisState);
  } else {
    updateViewChrome(null);
  }
  scheduleBoardChromePlacement();
}

function showPlaySetupView() {
  activeView = 'play-setup';
  setServerSingleplayerHumanPlayer(null);
  setTabState('play');
  controls.stopAutoplay();
  controls._disableAutoSearch();
  controls.setOptionsAvailable(false);
  setPlaySide(selectedPlayHumanPlayer);
  setGameHeaderLabelsVisible(false);
  document.getElementById('main-layout').classList.add('hidden');
  document.getElementById('play-setup-view').classList.remove('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.add('hidden');
}

function showAnalysisView() {
  activeView = 'analysis';
  setServerSingleplayerHumanPlayer(null);
  setTabState('analysis');
  setEditorChrome(false);
  setGameHeaderLabelsVisible(true);
  document.getElementById('main-layout').classList.remove('hidden');
  document.getElementById('play-setup-view').classList.add('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.remove('hidden');
  controls.setOptionsAvailable(true);
  if (analysisState) renderGameState(analysisState);
  else updateViewChrome(null);
  scheduleBoardChromePlacement();
}

function showReplayView() {
  activeView = 'replay-list';
  setServerSingleplayerHumanPlayer(null);
  setTabState('replay');
  setEditorChrome(false);
  setGameHeaderLabelsVisible(false);
  document.getElementById('main-layout').classList.add('hidden');
  document.getElementById('play-setup-view').classList.add('hidden');
  document.getElementById('replay-view').classList.remove('hidden');
  document.getElementById('controls').classList.add('hidden');
  controls.setOptionsAvailable(false);
  requestReplayList();
}

function showReplayBoardView() {
  activeView = 'replay-board';
  setServerSingleplayerHumanPlayer(null);
  setTabState('replay');
  setEditorChrome(false);
  setGameHeaderLabelsVisible(true);
  document.getElementById('main-layout').classList.remove('hidden');
  document.getElementById('play-setup-view').classList.add('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.remove('hidden');
  controls.setOptionsAvailable(false);
  if (replayState) renderGameState(replayState);
  else updateViewChrome(null);
  scheduleBoardChromePlacement();
}

function showEditorView() {
  activeView = 'editor';
  setServerSingleplayerHumanPlayer(null);
  setTabState('editor');
  controls.stopAutoplay();
  controls._disableAutoSearch();
  document.getElementById('main-layout').classList.remove('hidden');
  document.getElementById('play-setup-view').classList.add('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.remove('hidden');
  if (!editorBaseBoard && analysisState?.state?.board) {
    editorBaseBoard = analysisState.state.board;
  }
  setEditorChrome(true);
  controls.setOptionsAvailable(false);
  renderEditorBoard();
  scheduleBoardChromePlacement();
}

function setGameHeaderLabelsVisible(visible) {
  document.getElementById('phase-label').classList.toggle('hidden', !visible);
  document.getElementById('turn-label').classList.toggle('hidden', !visible);
}

function updateNewGameButtonLabel() {
  const btn = document.getElementById('btn-new-game');
  if (!btn) return;
  btn.textContent = activeView === 'play' ? 'Lobby' : 'New Game';
}

function setEditorChrome(enabled) {
  updateNewGameButtonLabel();
  if (enabled) hideRollBadge();
  if (enabled) clearResourceProductionAnimations();
  document.getElementById('left-ad-panel')?.classList.toggle('hidden', enabled);
  document.getElementById('players-panel').classList.toggle('hidden', enabled);
  document.getElementById('analysis-panel').classList.toggle('hidden', enabled);
  document.getElementById('editor-panel').classList.toggle('hidden', !enabled);
  document.getElementById('action-panel')?.classList.toggle('hidden', enabled);
  document.getElementById('analysis-bar-panel')?.classList.toggle('hidden', enabled);
  document.getElementById('board-history-controls')?.classList.toggle('hidden', enabled);
  document.getElementById('board-resource-legend')?.classList.toggle('hidden', enabled);
  document.getElementById('resource-animation-layer')?.classList.toggle('hidden', enabled);
  document.getElementById('phase-label').classList.toggle('hidden', enabled);
  document.getElementById('turn-label').classList.toggle('hidden', enabled);
  board.onTileClick = enabled ? handleEditorTileClick : null;
  board.onPortClick = enabled ? handleEditorPortClick : null;

  const controlsEl = document.getElementById('controls');
  for (const child of controlsEl.children) {
    if (child.id === 'btn-start-edited-game') {
      child.classList.toggle('hidden', !enabled);
    } else {
      child.classList.toggle('hidden', enabled);
    }
  }
}

function setMctsMoveDetailsVisible(visible) {
  document.getElementById('policy-header')?.classList.toggle('hidden', !visible);
  document.getElementById('policy-bars')?.classList.toggle('hidden', !visible);
  document.getElementById('tree-explorer')?.classList.toggle('hidden', !visible);
}

function updateViewChrome(msg) {
  updateNewGameButtonLabel();
  const inPlay = playViewActive();
  const replay = !!msg?.replay || activeView === 'replay-board';
  setMctsMoveDetailsVisible(!inPlay);
  document.getElementById('analysis-bar-panel')?.classList.toggle('hidden', activeView === 'editor');
  document.getElementById('board-resource-legend')?.classList.toggle('hidden', activeView === 'editor');
  document.getElementById('resource-animation-layer')?.classList.toggle('hidden', activeView === 'editor');

  document.getElementById('search-action-control')?.classList.toggle('hidden', inPlay);
  document.getElementById('budget-control')?.classList.toggle('hidden', inPlay);

  const botMove = document.getElementById('btn-bot-move');
  if (botMove) {
    if (inPlay) {
      botMove.classList.add('hidden');
      botMove.disabled = true;
    } else if (activeView === 'analysis' && !replay) {
      botMove.classList.remove('hidden');
      botMove.disabled = false;
    }
  }
  for (const btn of document.querySelectorAll('.takeover-btn')) {
    const hide = inPlay || replay || activeView === 'editor';
    btn.classList.toggle('hidden', hide);
    btn.disabled = hide;
  }

  controls.setOptionsAvailable(activeView === 'analysis' && !replay);
  if (inPlay) board.clearSearchHighlights();
}

function startPlayGame() {
  playMode.active = true;
  playMode.humanPlayer = selectedPlayHumanPlayer;
  playMode.botThinking = false;
  playMode.forcedMoveKey = null;
  playMode.pendingHumanMove = false;
  pendingNewGameSearch = false;
  controls.stopAutoplay();
  controls._disableAutoSearch();
  document.getElementById('apply-toggle').checked = false;
  document.getElementById('autoplay-toggle').checked = false;
  document.getElementById('autosearch-toggle').checked = false;
  mctsPanel.clear();
  board.clearSearchHighlights();
  activeView = 'play';
  setTabState('play');
  setEditorChrome(false);
  setGameHeaderLabelsVisible(true);
  document.getElementById('main-layout').classList.remove('hidden');
  document.getElementById('play-setup-view').classList.add('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.remove('hidden');
  updateViewChrome(analysisState);
  setServerSingleplayerHumanPlayer(playMode.humanPlayer);
  session.send({ type: 'NewGame', seed: null });
}

function runPlayAutomation(msg) {
  if (!isPlayBotTurn(msg)) {
    if (playViewActive() && (msg?.is_terminal || msg?.current_player === playMode.humanPlayer)) {
      playMode.botThinking = false;
      playMode.forcedMoveKey = null;
    }
    updateActionPanelStatus(msg, false);
    runPlayPonderSearch(msg);
    return;
  }
  updateActionPanelStatus(msg, true);
  if (Array.isArray(msg.legal_actions) && msg.legal_actions.length === 1) {
    const action = msg.legal_actions[0].action;
    const key = playForcedMoveKey(msg, action);
    if (playMode.forcedMoveKey === key) return;
    playMode.forcedMoveKey = key;
    playMode.botThinking = false;
    session.send({ type: 'PlayAction', action });
    return;
  }
  playMode.forcedMoveKey = null;
  if (playMode.botThinking) return;
  playMode.botThinking = true;
  session.send({
    type: 'BotMove',
    budget: playDifficultyBudget(),
  });
}

function runPlayPonderSearch(msg) {
  if (!playCanPonder(msg) || controls.searchRunning) return;
  controls.runSearchWithBudget(playDifficultyBudget(), 'analysis');
}

function requestReplayList() {
  setReplayStatus('Loading...');
  session.send({ type: 'ListReplays' });
}

function setReplayStatus(text) {
  const status = document.getElementById('replay-status');
  if (!status) return;
  status.textContent = text || '';
  status.classList.toggle('hidden', !text);
}

function renderReplayList(entries) {
  const favoriteList = document.getElementById('favorite-replay-list');
  const list = document.getElementById('replay-list');
  const favorites = entries.filter(entry => entry.favorite);
  const replays = entries.filter(entry => !entry.favorite);
  favoriteList.innerHTML = '';
  setReplayStatus('');

  renderReplaySection(favoriteList, favorites, 'No favourited replays');
  renderReplaySection(list, replays, entries.length ? 'No other replays' : 'No replays');
}

function renderReplaySection(list, entries, emptyText) {
  list.innerHTML = '';

  if (!entries.length) {
    const empty = document.createElement('div');
    empty.className = 'px-3 py-4 text-xs text-gray-500';
    empty.textContent = emptyText;
    list.appendChild(empty);
    return;
  }

  for (const entry of entries) {
    const row = document.createElement('div');
    row.className = 'w-full flex items-center gap-2 px-2 py-2 border-b border-gray-700 last:border-b-0 hover:bg-bg-3 transition-colors';

    const star = document.createElement('button');
    star.type = 'button';
    star.className = 'w-7 h-7 flex items-center justify-center text-base text-yellow-300 rounded hover:bg-bg cursor-pointer';
    star.textContent = entry.favorite ? '\u2605' : '\u2606';
    star.title = entry.favorite ? 'Remove favourite' : 'Favourite replay';
    star.setAttribute('aria-label', star.title);
    star.addEventListener('click', (event) => {
      event.stopPropagation();
      session.send({
        type: 'SetReplayFavorite',
        id: entry.id,
        favorite: !entry.favorite,
      });
    });

    const load = document.createElement('button');
    load.type = 'button';
    load.className = 'flex min-w-0 flex-1 items-center justify-between gap-3 text-left rounded px-1 py-1 hover:text-gray-100 cursor-pointer';
    const saved = document.createElement('span');
    saved.className = 'min-w-0 truncate text-xs text-gray-100';
    saved.textContent = formatReplayTime(entry.saved_at_ms);

    const actions = document.createElement('span');
    actions.className = 'text-[11px] text-gray-400 whitespace-nowrap';
    actions.textContent = `${entry.action_count} actions`;

    load.appendChild(saved);
    load.appendChild(actions);
    load.addEventListener('click', () => {
      controls.stopAutoplay();
      controls._disableAutoSearch();
      session.send({ type: 'LoadReplay', id: entry.id });
      showReplayBoardView();
    });

    const trash = document.createElement('button');
    trash.type = 'button';
    trash.className = 'w-7 h-7 flex items-center justify-center text-sm text-gray-400 rounded hover:bg-accent hover:text-white cursor-pointer';
    trash.textContent = '\u{1F5D1}';
    trash.title = 'Delete replay';
    trash.setAttribute('aria-label', 'Delete replay');
    trash.addEventListener('click', (event) => {
      event.stopPropagation();
      if (!window.confirm('Delete this replay?')) return;
      session.send({ type: 'DeleteReplay', id: entry.id });
    });

    row.appendChild(star);
    row.appendChild(load);
    row.appendChild(trash);
    list.appendChild(row);
  }
}

function formatReplayTime(ms) {
  const date = new Date(Number(ms));
  if (Number.isNaN(date.getTime())) return 'Saved replay';
  return date.toLocaleString();
}

// Board editor

function initEditorControls() {
  const terrainList = document.getElementById('editor-terrain-buttons');
  for (const terrain of EDITOR_TERRAINS) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'editor-terrain-btn';
    btn.dataset.terrain = terrain;

    const swatch = document.createElement('span');
    swatch.className = 'editor-swatch';
    swatch.style.background = TERRAIN_COLORS[terrain];
    btn.appendChild(swatch);

    const label = document.createElement('span');
    label.textContent = terrain.charAt(0).toUpperCase() + terrain.slice(1);
    btn.appendChild(label);

    btn.addEventListener('click', () => applyEditorTerrain(terrain));
    terrainList.appendChild(btn);
  }

  const numberList = document.getElementById('editor-number-buttons');
  for (const number of EDITOR_NUMBERS) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'editor-number-btn';
    btn.dataset.number = String(number);

    const label = document.createElement('span');
    label.className = 'editor-number-label';
    label.textContent = number;
    btn.appendChild(label);

    const pips = document.createElement('span');
    pips.className = 'editor-number-pips';
    for (let i = 0; i < catanPips(number); i++) {
      pips.appendChild(document.createElement('span'));
    }
    btn.appendChild(pips);

    btn.addEventListener('click', () => applyEditorNumber(number));
    numberList.appendChild(btn);
  }

  const portList = document.getElementById('editor-port-buttons');
  for (const kind of EDITOR_PORT_KINDS) {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'editor-port-btn';
    btn.dataset.portKind = kind;

    const swatch = document.createElement('span');
    swatch.className = 'editor-swatch';
    swatch.style.background = EDITOR_PORT_COLORS[kind];
    btn.appendChild(swatch);

    const label = document.createElement('span');
    label.className = 'editor-port-label';
    label.textContent = EDITOR_PORT_LABELS[kind];
    btn.appendChild(label);

    btn.addEventListener('click', () => applyEditorPort(kind));
    portList.appendChild(btn);
  }

  updateEditorPanel();
}

function renderEditorBoard() {
  if (!editorBaseBoard) {
    currentBoard = null;
    board.svg.innerHTML = '';
    setEditorStatus('Loading board');
    updateEditorPanel();
    scheduleBoardChromePlacement();
    return;
  }

  ensureEditorPorts();
  const draftPorts = (editorBaseBoard.ports || []).map((port, fallbackIndex) => {
    const index = Number.isInteger(port.index) ? port.index : fallbackIndex;
    return {
      ...port,
      index,
      kind: editorPorts[index] || port.kind || 'generic',
      selected: index === selectedEditorPort,
    };
  });

  const draftBoard = {
    ...editorBaseBoard,
    port_layout: editorPortLayout,
    ports: draftPorts,
    tiles: editorBaseBoard.tiles.map((tile, i) => ({
      ...tile,
      terrain: editorTiles[i]?.terrain || null,
      number: editorTiles[i]?.number || null,
      selected: i === selectedEditorTile,
    })),
  };

  currentBoard = null;
  board.renderEditorBoard(draftBoard);
  updateEditorPanel();
  scheduleBoardChromePlacement();
}

function handleEditorTileClick(tileIndex) {
  selectedEditorTile = tileIndex;
  selectedEditorPort = null;
  setEditorStatus('');
  renderEditorBoard();
}

function handleEditorPortClick(portIndex) {
  selectedEditorPort = portIndex;
  selectedEditorTile = null;
  setEditorStatus('');
  renderEditorBoard();
}

function applyEditorTerrain(terrain) {
  if (selectedEditorTile == null) {
    setEditorStatus('No tile selected');
    return;
  }
  const tile = editorTiles[selectedEditorTile];
  tile.terrain = terrain;
  if (terrain === 'desert') {
    tile.number = null;
  }
  setEditorStatus('');
  renderEditorBoard();
}

function applyEditorNumber(number) {
  if (selectedEditorTile == null) {
    setEditorStatus('No tile selected');
    return;
  }
  const tile = editorTiles[selectedEditorTile];
  if (!tile.terrain) {
    setEditorStatus('Terrain required');
    return;
  }
  if (tile.terrain === 'desert') {
    tile.number = null;
    setEditorStatus('Desert has no number');
    renderEditorBoard();
    return;
  }
  tile.number = number;
  setEditorStatus('');
  renderEditorBoard();
}

function applyEditorPort(kind) {
  if (selectedEditorPort == null) {
    setEditorStatus('No port selected');
    return;
  }
  if (!isEditorPortKind(kind)) {
    setEditorStatus('Invalid port');
    return;
  }
  ensureEditorPorts();
  editorPorts[selectedEditorPort] = kind;
  setEditorStatus('');
  renderEditorBoard();
}

function resetEditorPortsFromBaseBoard() {
  if (!editorBaseBoard) {
    editorPortLayout = 'primary';
    editorPorts = [];
    selectedEditorPort = null;
    return;
  }
  editorPortLayout = editorBaseBoard.port_layout || 'primary';
  const sourcePorts = [...(editorBaseBoard.ports || [])]
    .sort((a, b) => (a.index ?? 0) - (b.index ?? 0));
  editorPorts = sourcePorts.map(port => isEditorPortKind(port.kind) ? port.kind : 'generic');
  while (editorPorts.length < EDITOR_PORT_BAG.length) {
    editorPorts.push(EDITOR_PORT_BAG[editorPorts.length]);
  }
  editorPorts = editorPorts.slice(0, EDITOR_PORT_BAG.length);
  if (selectedEditorPort != null && selectedEditorPort >= editorPorts.length) {
    selectedEditorPort = null;
  }
}

function ensureEditorPorts() {
  if (editorBaseBoard?.port_layout) {
    editorPortLayout = editorBaseBoard.port_layout;
  }
  if (editorPorts.length !== EDITOR_PORT_BAG.length) {
    resetEditorPortsFromBaseBoard();
  }
}

function randomizeEditorBoard() {
  const terrains = shuffled(EDITOR_TERRAIN_BAG);
  const numbers = shuffled(EDITOR_NUMBER_BAG);
  let numberIndex = 0;

  editorTiles = terrains.map((terrain) => ({
    terrain,
    number: terrain === 'desert' ? null : numbers[numberIndex++],
  }));
  editorPorts = shuffled(EDITOR_PORT_BAG);
  setEditorStatus('');
  renderEditorBoard();
}

function updateEditorPanel() {
  const selection = document.getElementById('editor-selection');
  const tile = selectedEditorTile == null ? null : editorTiles[selectedEditorTile];
  const portKind = selectedEditorPort == null ? null : editorPorts[selectedEditorPort];
  if (portKind) {
    selection.textContent = `Port ${selectedEditorPort + 1} - ${EDITOR_PORT_LABELS[portKind]}`;
  } else if (!tile) {
    selection.textContent = '';
  } else {
    const parts = [`Tile ${selectedEditorTile + 1}`];
    if (tile.terrain) parts.push(tile.terrain);
    if (tile.number) parts.push(String(tile.number));
    selection.textContent = parts.join(' - ');
  }

  for (const btn of document.querySelectorAll('.editor-terrain-btn')) {
    const active = tile?.terrain === btn.dataset.terrain;
    btn.disabled = selectedEditorTile == null;
    btn.classList.toggle('active', active);
  }

  const numbersEnabled = !!tile?.terrain && tile.terrain !== 'desert';
  for (const btn of document.querySelectorAll('.editor-number-btn')) {
    const number = Number(btn.dataset.number);
    btn.disabled = !numbersEnabled;
    btn.classList.toggle('active', tile?.number === number);
  }

  for (const btn of document.querySelectorAll('.editor-port-btn')) {
    const kind = btn.dataset.portKind;
    btn.disabled = selectedEditorPort == null;
    btn.classList.toggle('active', portKind === kind);
  }
}

function setEditorStatus(text) {
  const status = document.getElementById('editor-status');
  if (!status) return;
  status.textContent = text || '';
  document.getElementById('btn-start-edited-game').disabled = pendingEditorStart;
}

function validateEditorDraft() {
  for (let i = 0; i < editorTiles.length; i++) {
    const tile = editorTiles[i];
    if (!tile.terrain) return `Tile ${i + 1} needs terrain`;
    if (tile.terrain === 'desert' && tile.number != null) {
      return `Tile ${i + 1} is desert`;
    }
    if (tile.terrain !== 'desert' && !EDITOR_NUMBERS.includes(tile.number)) {
      return `Tile ${i + 1} needs a number`;
    }
  }
  ensureEditorPorts();
  if (editorPorts.length !== EDITOR_PORT_BAG.length) return 'Ports are still loading';
  for (let i = 0; i < editorPorts.length; i++) {
    if (!isEditorPortKind(editorPorts[i])) return `Port ${i + 1} needs a type`;
  }
  return '';
}

function startEditedGame() {
  const error = validateEditorDraft();
  if (error) {
    setEditorStatus(error);
    return;
  }
  pendingEditorStart = true;
  playMode.active = false;
  playMode.botThinking = false;
  playMode.forcedMoveKey = null;
  playMode.pendingHumanMove = false;
  setEditorStatus('Starting');
  session.send({
    type: 'StartEditedGame',
    terrains: editorTiles.map(tile => tile.terrain),
    numbers: editorTiles.map(tile => tile.terrain === 'desert' ? null : tile.number),
    port_layout: editorPortLayout || editorBaseBoard?.port_layout || 'primary',
    ports: [...editorPorts],
  });
}

// ── Board action clicks ──────────────────────────────────────────────

document.getElementById('tab-play').addEventListener('click', showPlayView);
document.getElementById('tab-board').addEventListener('click', showAnalysisView);
document.getElementById('tab-replay').addEventListener('click', showReplayView);
document.getElementById('tab-editor').addEventListener('click', showEditorView);
document.getElementById('btn-refresh-replays').addEventListener('click', requestReplayList);
document.getElementById('btn-start-play-game').addEventListener('click', startPlayGame);
document.getElementById('btn-start-edited-game').addEventListener('click', startEditedGame);
document.getElementById('btn-random-editor-board').addEventListener('click', randomizeEditorBoard);
for (const btn of document.querySelectorAll('.play-side-btn')) {
  btn.addEventListener('click', () => setPlaySide(Number(btn.dataset.player)));
}
initPlayDifficultySelect();
setPlaySide(selectedPlayHumanPlayer);
initEditorControls();
showPlaySetupView();

const boardShellEl = document.getElementById('board-shell');
if (boardShellEl && window.ResizeObserver) {
  new ResizeObserver(updateBoardChromePlacement).observe(boardShellEl);
}
window.addEventListener('resize', scheduleBoardChromePlacement);
scheduleBoardChromePlacement();

board.onActionClick = (action) => {
  if (activeView === 'editor') return;
  if (currentState?.replay) return;
  if (isPlayBotTurn()) return;
  sendPlayAction(action);
};

document.getElementById('btn-rotate-left').addEventListener('click', () => {
  board.rotateCounterclockwise();
});

document.getElementById('btn-mirror-board').addEventListener('click', () => {
  board.toggleMirror();
});

document.getElementById('btn-rotate-right').addEventListener('click', () => {
  board.rotateClockwise();
});

// ── MCTS explore ─────────────────────────────────────────────────────

mctsPanel.onExplore = (actionPath) => {
  session.send({
    type: 'ExploreSubtree',
    action_path: actionPath,
    depth: 20,
    target: currentState?.replay ? 'replay' : 'analysis',
  });
};

mctsPanel.onPreview = (action) => {
  showLegalActionPreview(action);
};

mctsPanel.onPreviewClear = () => {
  board.clearActionPreview();
};

// ── Player panel helpers ─────────────────────────────────────────────

function updatePlayerPanel(idx, state) {
  const frame = state.frame;
  if (!frame) return;

  const pf = frame.players[idx];
  if (!pf) return;

  const displayName = playNameForPlayer(idx) || state.player_names?.[idx] || `P${idx + 1}`;
  document.getElementById(`p${idx}-name`).textContent = displayName;

  // Total resource cards
  const resourceCount = pf.hand ? pf.hand.reduce((sum, count) => sum + count, 0) : 0;
  document.getElementById(`p${idx}-card-count`).textContent =
    `${resourceCount} ${resourceCount === 1 ? 'Card' : 'Cards'}`;

  // VP
  document.getElementById(`p${idx}-vp`).textContent = pf.vp;

  // Hand — always show all 5 resources as colored rectangles
  const handEl = document.getElementById(`p${idx}-hand`);
  handEl.innerHTML = '';
  const hideResourceBreakdown = playViewActive() && idx !== playMode.humanPlayer;
  if (!hideResourceBreakdown) {
    for (let r = 0; r < 5; r++) {
      const card = document.createElement('span');
      card.className = `resource-card ${RESOURCE_NAMES[r]}`;
      card.textContent = pf.hand[r];
      handEl.appendChild(card);
    }
  }

  // Dev cards
  const devEl = document.getElementById(`p${idx}-dev`);
  devEl.innerHTML = '';
  const hideDevBreakdown = playViewActive() && idx !== playMode.humanPlayer;
  if (hideDevBreakdown) {
    const visibleDevCards = pf.dev_cards ? pf.dev_cards.reduce((sum, count) => sum + count, 0) : 0;
    const hiddenDevCards = pf.hidden_dev_cards || 0;
    const totalDevCards = visibleDevCards + hiddenDevCards;
    if (totalDevCards > 0) {
      const chip = document.createElement('span');
      chip.className = 'dev-chip hidden-dev';
      chip.textContent = `${totalDevCards} unknown`;
      devEl.appendChild(chip);
    }
  } else {
    for (let d = 0; d < 5; d++) {
      if (pf.dev_cards[d] > 0) {
        const chip = document.createElement('span');
        const bought = pf.dev_cards_bought_this_turn ? pf.dev_cards_bought_this_turn[d] : 0;
        const playable = pf.dev_cards[d] - bought;
        if (playable > 0) {
          chip.className = 'dev-chip';
          chip.textContent = `${playable} ${DEV_CARD_NAMES[d]}`;
          devEl.appendChild(chip);
        }
        if (bought > 0) {
          const bchip = document.createElement('span');
          bchip.className = 'dev-chip bought-this-turn';
          bchip.textContent = `${bought} ${DEV_CARD_NAMES[d]}`;
          bchip.title = 'Bought this turn — cannot play yet';
          devEl.appendChild(bchip);
        }
      }
    }
  }
  if (!hideDevBreakdown && pf.hidden_dev_cards > 0) {
    const est = state.expected_dev && state.expected_dev[idx];
    const hasEstimate = est && est.some(v => v > 0);
    if (hasEstimate) {
      // Show hypergeometric expected distribution
      for (let d = 0; d < 5; d++) {
        if (est[d] >= 0.05) {
          const chip = document.createElement('span');
          chip.className = 'dev-chip dev-estimate';
          chip.textContent = `~${est[d].toFixed(1)} ${DEV_SHORT[d]}`;
          devEl.appendChild(chip);
        }
      }
    } else {
      const chip = document.createElement('span');
      chip.className = 'dev-chip hidden-dev';
      chip.textContent = `${pf.hidden_dev_cards} unknown`;
      devEl.appendChild(chip);
    }
  }

  // Stats: knights played + awards
  const statsEl = document.getElementById(`p${idx}-stats`);
  const parts = [];
  parts.push(`Knights: ${pf.knights}`);
  if (frame.longest_road && frame.longest_road[0] === idx) {
    parts.push(`Longest Road: ${frame.longest_road[1]}`);
  }
  if (frame.largest_army && frame.largest_army[0] === idx) {
    parts.push(`Largest Army: ${frame.largest_army[1]}`);
  }
  statsEl.textContent = parts.join(' · ');
}

function updateBoardResourceLegend(msg, state) {
  const legend = document.getElementById('board-resource-legend');
  if (!legend || !state?.frame?.players) return;

  const playerIndex = playViewActive()
    ? playMode.humanPlayer
    : (msg.current_player === 0 || msg.current_player === 1 ? msg.current_player : 0);
  const hand = state.frame.players[playerIndex]?.hand || [0, 0, 0, 0, 0];

  for (const card of legend.querySelectorAll('[data-resource-index]')) {
    const resourceIndex = Number(card.dataset.resourceIndex);
    if (!Number.isInteger(resourceIndex)) continue;
    const count = hand[resourceIndex] ?? 0;
    const name = RESOURCE_NAMES[resourceIndex] || '';
    card.textContent = `${count} ${capitalizeResourceName(name)}`;
  }
}

function capitalizeResourceName(name) {
  return name ? name[0].toUpperCase() + name.slice(1) : '';
}

function updateBank(state) {
  const bankEl = document.getElementById('bank-dev');
  if (!bankEl) return;
  bankEl.innerHTML = '';

  // Use hypergeometric estimate when available (colonist mode — bank is unknown).
  const est = state.expected_bank_dev;
  const hasEstimate = est && est.some(v => v > 0);

  if (est) {
    // Colonist mode: bank contents are uncertain, show estimates.
    for (let d = 0; d < 5; d++) {
      if (est[d] >= 0.05) {
        const chip = document.createElement('span');
        chip.className = 'dev-chip dev-estimate';
        chip.textContent = `~${est[d].toFixed(1)} ${DEV_SHORT[d]}`;
        bankEl.appendChild(chip);
      }
    }
  } else {
    // Self-play: no hidden cards, show exact pool counts.
    const pool = state.frame && state.frame.dev_pool;
    if (!pool) return;
    for (let d = 0; d < 5; d++) {
      if (pool[d] > 0) {
        const chip = document.createElement('span');
        chip.className = 'dev-chip';
        chip.textContent = `${pool[d]} ${DEV_SHORT[d]}`;
        bankEl.appendChild(chip);
      }
    }
    if (pool.every(v => v === 0)) {
      bankEl.textContent = 'Empty';
    }
  }
}

// ── Dice probability chart ───────────────────────────────────────────

// Fair 2d6 probabilities for reference line.
const FAIR_PROBS = [1,2,3,4,5,6,5,4,3,2,1].map(v => v / 36);

function updateDice(state) {
  const panel = document.getElementById('dice-panel');
  const dice = state.dice;
  if (!dice || playViewActive()) { panel.style.display = 'none'; return; }
  panel.style.display = '';

  document.getElementById('dice-cards').textContent = `(${dice.cards_left}/${dice.total_cards})`;

  const chart = document.getElementById('dice-chart');
  chart.innerHTML = '';

  // Fixed scale: fair 7 probability (~16.7%) maps to ~75% of chart height,
  // leaving headroom for sums that exceed fair odds.
  const scale = FAIR_PROBS[5] / 0.75; // denominator so fair-7 bar is 75% tall
  const chartH = 64;

  for (let i = 0; i < 11; i++) {
    const sum = i + 2;
    const prob = dice.probs[i];
    const fair = FAIR_PROBS[i];

    const col = document.createElement('div');
    col.className = 'dice-col';
    col.style.flex = '1';
    col.style.display = 'flex';
    col.style.flexDirection = 'column';
    col.style.alignItems = 'center';
    col.style.justifyContent = 'flex-end';
    col.style.height = chartH + 'px';
    col.style.position = 'relative';

    // Fair probability reference tick
    const ref = document.createElement('div');
    ref.style.position = 'absolute';
    const labelH = 14;
    ref.style.bottom = (labelH + Math.round(fair / scale * (chartH - labelH))) + 'px';
    ref.style.width = '100%';
    ref.style.height = '1px';
    ref.style.background = '#555';
    col.appendChild(ref);

    // Actual probability bar
    const bar = document.createElement('div');
    const h = Math.max(1, Math.min(chartH - labelH, Math.round(prob / scale * (chartH - labelH))));
    bar.style.width = '80%';
    bar.style.height = h + 'px';
    bar.style.borderRadius = '1px';
    const ratio = fair > 0 ? prob / fair : 1;
    if (ratio > 1.15) {
      bar.style.background = '#e94560'; // above fair — red/hot
    } else if (ratio < 0.85) {
      bar.style.background = '#2a6';    // below fair — green/cold
    } else {
      bar.style.background = '#4a9eff'; // near fair — blue
    }
    col.appendChild(bar);

    // Tooltip on the whole column
    col.title = `Roll ${sum}: ${(prob * 100).toFixed(1)}% (fair: ${(fair * 100).toFixed(1)}%)`;

    // Label
    const lbl = document.createElement('div');
    lbl.style.fontSize = '9px';
    lbl.style.color = sum === 7 ? '#e94560' : '#888';
    lbl.style.marginTop = '2px';
    lbl.textContent = sum;
    col.appendChild(lbl);

    chart.appendChild(col);
  }
}

// ── Keyboard shortcuts ────────────────────────────────────────────────

document.addEventListener('keydown', (e) => {
  if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA') return;
  if (activeView === 'replay-list' || activeView === 'editor' || activeView === 'play-setup') return;
  if (isPlayBotTurn()) return;
  if (currentState?.replay && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
    e.preventDefault();
    const replay = currentState.replay;
    const delta = e.key === 'ArrowLeft' ? -1 : 1;
    const cursor = Math.max(0, Math.min(replay.len, replay.cursor + delta));
    session.send({ type: 'SetReplayCursor', cursor });
    return;
  }
  if (e.key === 'ArrowLeft' && currentState?.can_undo) {
    e.preventDefault();
    controls.stopAutoplay();
    controls._disableAutoSearch();
    session.send({ type: 'Undo' });
  } else if (e.key === 'ArrowRight' && currentState?.can_redo) {
    e.preventDefault();
    controls.stopAutoplay();
    controls._disableAutoSearch();
    session.send({ type: 'Redo' });
  }
});

// ── Start ────────────────────────────────────────────────────────────

let appStarted = false;

window.hexfishStartApp = () => {
  if (appStarted) return;
  appStarted = true;
  session.connect();
};

window.hexfishStopApp = () => {
  if (!appStarted) return;
  appStarted = false;
  session.disconnect();
};

document.addEventListener('hexfish-auth-signed-in', window.hexfishStartApp);
document.addEventListener('hexfish-auth-signed-out', window.hexfishStopApp);

if (window.hexfishAuthSignedIn) {
  window.hexfishStartApp();
}

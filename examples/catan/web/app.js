// Entry point: wires components together.

const session = new Session({
  getAuthToken: () => window.hexfishGetClerkToken ? window.hexfishGetClerkToken() : null,
});
const board = new Board(document.getElementById('board-svg'));
window.hexfishBoard = board;
const mctsPanel = new MCTSPanel();
const controls = new Controls(session);

const RESOURCE_NAMES = ['lumber', 'brick', 'wool', 'grain', 'ore'];
const DEV_CARD_NAMES = ['Knight', 'VP', 'Road Building', 'Year of Plenty', 'Monopoly'];
const DEV_SHORT = ['Kn', 'VP', 'RB', 'YP', 'Mo'];
const PLAYER_COLORS = ['#4a9eff', '#ff6b6b'];

function playerColor(playerIndex) {
  return playerIndex === 0 || playerIndex === 1 ? PLAYER_COLORS[playerIndex] : '';
}

let currentState = null;
let analysisState = null;
let replayState = null;
let currentBoard = null;
let lastActionLogLength = { analysis: null, replay: null };
let previousFrameHands = { analysis: null, replay: null };
let activeView = 'analysis';

// ── Message handlers ─────────────────────────────────────────────────

session.on('GameState', (msg) => {
  if (msg.replay) {
    replayState = msg;
    if (activeView === 'replay-board') renderGameState(msg);
  } else {
    analysisState = msg;
    if (activeView === 'analysis') renderGameState(msg);
  }
});

function renderGameState(msg) {
  currentState = msg;
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
  updateBank(state);
  updateDice(state);

  // Highlight active player
  document.getElementById('player-0').classList.toggle('active', msg.current_player === 0);
  document.getElementById('player-1').classList.toggle('active', msg.current_player === 1);

  // Legal actions
  const actionList = document.getElementById('action-list');
  actionList.innerHTML = '';
  board.clearOverlays();

  if (!msg.replay && !msg.is_terminal && !msg.is_chance) {
    // Board overlays for spatial actions
    if (currentBoard) {
      board.showLegalActions(msg.legal_actions, currentBoard, msg.current_player);
    }

    // Button list for all actions
    for (const a of msg.legal_actions) {
      const btn = document.createElement('button');
      btn.className = 'py-1 px-2 text-[11px] bg-bg-3 border border-gray-700 text-gray-200 rounded cursor-pointer whitespace-nowrap hover:bg-accent transition-colors';
      btn.textContent = a.label;
      btn.addEventListener('click', () => {
        session.send({ type: 'PlayAction', action: a.action });
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
  logView.innerHTML = '';
  if (msg.action_log) {
    for (let i = 0; i < msg.action_log.length; i++) {
      const line = document.createElement('div');
      line.className = 'py-0.5';
      const text = msg.action_log[i];
      if (text.startsWith('P1:')) {
        line.style.color = PLAYER_COLORS[0];
      } else if (text.startsWith('P2:')) {
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
        if (msg.replay) {
          session.send({ type: 'SetReplayCursor', cursor: i + 1 });
        } else {
          session.send({ type: 'SetLogCursor', index: i });
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
    }
  }

  updateRollBadge(msg, state, viewKey);

  // Undo/Redo button states
  document.getElementById('btn-undo').disabled = !msg.can_undo;
  document.getElementById('btn-redo').disabled = !msg.can_redo;

  // Result banner
  const banner = document.getElementById('result-banner');
  if (msg.is_terminal && msg.result) {
    const winner = parseResultWinner(msg.result);
    banner.textContent = msg.result;
    banner.style.background = playerColor(winner) || '';
    banner.classList.remove('hidden');
    controls.onGameOver();
  } else {
    banner.classList.add('hidden');
    banner.style.background = '';
  }

  controls.onStateUpdate(msg);
}

function showLegalActionPreview(action) {
  if (!currentBoard || !currentState) return;
  board.showActionPreview(action, currentBoard, currentState.current_player);
}

function updateRollBadge(msg, state, viewKey) {
  const entries = msg.action_log || [];
  const currentLength = entries.length;
  const currentHands = frameHands(state);
  const previousLength = lastActionLogLength[viewKey];

  if (previousLength == null) {
    lastActionLogLength[viewKey] = currentLength;
    previousFrameHands[viewKey] = currentHands;
    return;
  }

  if (currentLength < previousLength) {
    hideRollBadge();
    lastActionLogLength[viewKey] = currentLength;
    previousFrameHands[viewKey] = currentHands;
    return;
  }

  if (currentLength === previousLength) {
    previousFrameHands[viewKey] = currentHands;
    return;
  }

  const newEntries = entries.slice(previousLength);
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
    : `P${playerIndex + 1} rolled ${total}`);
  badge.classList.remove('hidden');
}

function hideRollBadge() {
  const badge = document.getElementById('roll-badge');
  if (!badge) return;
  badge.classList.add('hidden');
}

function updateSearchHighlights(snapshot, labels) {
  if (!currentBoard || !snapshot.edges) return;
  const edges = snapshot.edges.map((e, i) => ({
    ...e,
    label: labels[i] || `Action ${e.action}`,
  }));
  // Sort by visits descending (matches MCTS panel order).
  edges.sort((a, b) => b.visits - a.visits);
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
  mctsPanel.updateSnapshot(msg.snapshot, msg.action_labels, currentState?.current_player ?? 0);
  mctsPanel.showProgress(msg.snapshot.total_simulations, msg.sims_total);
  updateSearchHighlights(msg.snapshot, msg.action_labels);
});

session.on('BotAction', (msg) => {
  if (msg.snapshot) {
    mctsPanel.updateSnapshot(msg.snapshot, msg.action_labels || [], currentState?.current_player ?? 0);
    updateSearchHighlights(msg.snapshot, msg.action_labels || []);
  }
  controls.onBotDone();
});

session.on('Error', (msg) => {
  controls.onSearchError();
  setReplayStatus(msg.message);
  console.error('Server error:', msg.message);
});

// ── View tabs / replay list ────────────────────────────────────────────────

function setTabState(tab) {
  const boardTab = document.getElementById('tab-board');
  const replayTab = document.getElementById('tab-replay');
  const isAnalysis = tab === 'analysis';

  boardTab.className = isAnalysis
    ? 'view-tab active px-2 py-1 rounded text-gray-100 bg-bg-3'
    : 'view-tab px-2 py-1 rounded text-gray-400 hover:text-gray-100 hover:bg-bg-3';
  replayTab.className = !isAnalysis
    ? 'view-tab active px-2 py-1 rounded text-gray-100 bg-bg-3'
    : 'view-tab px-2 py-1 rounded text-gray-400 hover:text-gray-100 hover:bg-bg-3';
  boardTab.setAttribute('aria-selected', String(isAnalysis));
  replayTab.setAttribute('aria-selected', String(!isAnalysis));
}

function showAnalysisView() {
  activeView = 'analysis';
  setTabState('analysis');
  document.getElementById('main-layout').classList.remove('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.remove('hidden');
  if (analysisState) renderGameState(analysisState);
}

function showReplayView() {
  activeView = 'replay-list';
  setTabState('replay');
  document.getElementById('main-layout').classList.add('hidden');
  document.getElementById('replay-view').classList.remove('hidden');
  document.getElementById('controls').classList.add('hidden');
  requestReplayList();
}

function showReplayBoardView() {
  activeView = 'replay-board';
  setTabState('replay');
  document.getElementById('main-layout').classList.remove('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.remove('hidden');
  if (replayState) renderGameState(replayState);
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

// ── Board action clicks ──────────────────────────────────────────────

document.getElementById('tab-board').addEventListener('click', showAnalysisView);
document.getElementById('tab-replay').addEventListener('click', showReplayView);
document.getElementById('btn-refresh-replays').addEventListener('click', requestReplayList);

board.onActionClick = (action) => {
  if (currentState?.replay) return;
  session.send({ type: 'PlayAction', action });
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

// ── Player panel helpers ─────────────────────────────────────────────

function updatePlayerPanel(idx, state) {
  const frame = state.frame;
  if (!frame) return;

  const pf = frame.players[idx];
  if (!pf) return;

  // Player name
  if (state.player_names && state.player_names[idx]) {
    document.getElementById(`p${idx}-name`).textContent = state.player_names[idx];
  }

  // VP
  document.getElementById(`p${idx}-vp`).textContent = pf.vp;

  // Hand — always show all 5 resources as colored rectangles
  const handEl = document.getElementById(`p${idx}-hand`);
  handEl.innerHTML = '';
  for (let r = 0; r < 5; r++) {
    const card = document.createElement('span');
    card.className = `resource-card ${RESOURCE_NAMES[r]}`;
    card.textContent = pf.hand[r];
    handEl.appendChild(card);
  }

  // Dev cards
  const devEl = document.getElementById(`p${idx}-dev`);
  devEl.innerHTML = '';
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
  if (pf.hidden_dev_cards > 0) {
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
  if (!dice) { panel.style.display = 'none'; return; }
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
  if (activeView === 'replay-list') return;
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

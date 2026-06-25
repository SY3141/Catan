// Entry point: wires components together.

const session = new Session();
session.setAuthTokenProvider(async () => {
  const clerk = window.Clerk;
  if (!window.hexfishAuthSignedIn || typeof clerk?.session?.getToken !== 'function') return '';
  try {
    return await clerk.session.getToken() || '';
  } catch (_error) {
    return '';
  }
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
const DEV_VP_INDEX = 1;
const PIECE_LIMITS = {
  settlements: 5,
  cities: 4,
  roads: 15,
};
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
const LAST_MULTIPLAYER_ROOM_KEY = 'hexfish-last-multiplayer-room-code';
const PENDING_SHARED_REPLAY_KEY = 'hexfish-pending-shared-replay-slug';
const MOVE_SOUND_ENABLED_KEY = 'hexfish-sound-enabled';
const MOVE_SOUND_ASSETS = {
  road: 'sounds/build.mp3',
  settlement: 'sounds/build.mp3',
  city: 'sounds/build.mp3',
  roll: 'sounds/dice.mp3',
  card: 'sounds/card.mp3',
  discard: 'sounds/discard.mp3',
};
const MOVE_SOUND_KINDS = Object.keys(MOVE_SOUND_ASSETS);
const CATAN_ROLL_ACTION = 180;
const CATAN_END_TURN_ACTION = 181;
const CATAN_SETTLEMENT_START = 0;
const CATAN_SETTLEMENT_END = 54;
const CATAN_ROAD_START = 54;
const CATAN_ROAD_END = 126;
const CATAN_CITY_START = 126;
const CATAN_CITY_END = 180;
const CATAN_ROBBER_START = 205;
const CATAN_ROBBER_END = 224;
const CATAN_MARITIME_START = 229;
const CATAN_MARITIME_END = 249;
const CATAN_NODE_COUNT = CATAN_SETTLEMENT_END - CATAN_SETTLEMENT_START;
const CATAN_EDGE_COUNT = CATAN_ROAD_END - CATAN_ROAD_START;
const CATAN_TILE_COUNT = CATAN_ROBBER_END - CATAN_ROBBER_START;
const DEFAULT_DISCARD_THRESHOLD = 9;
const BASE_DOCUMENT_TITLE = document.title || 'HexFish';
const MULTIPLAYER_TURN_DOCUMENT_TITLE = 'Your turn - HexFish';
const MULTIPLAYER_TURN_TITLE_FLASH_MS = 900;
const MULTIPLAYER_ROOM_SYNC_MS = 5000;
const MULTIPLAYER_DISCONNECT_MODAL_GRACE_MS = 5000;
const MULTIPLAYER_LOBBY_COUNTDOWN_MS = 1000;
const MULTIPLAYER_ROOM_CODE_ALPHABET = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';
const DEFAULT_MULTIPLAYER_TIME_MINUTES = 15;
const DEFAULT_MULTIPLAYER_INCREMENT_SECONDS = 0;
const SINGLEPLAYER_RECOVERY_REARM_MS = 250;
const SINGLEPLAYER_RECOVERY_RETRY_MS = 1000;
const SINGLEPLAYER_RECOVERY_MAX_ATTEMPTS = 3;
const PLAY_BOT_WATCHDOG_INTERVAL_MS = 5000;
const APP_WEBSOCKET_AUTH_CHANGED_CLOSE_CODE = 4002;
const APP_WEBSOCKET_APP_STOPPED_CLOSE_CODE = 4003;

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
let replayBoardStatusText = '';
let editorBaseBoard = null;
let editorTiles = createBlankEditorTiles();
let selectedEditorTile = null;
let editorPortLayout = 'primary';
let editorPorts = [];
let selectedEditorPort = null;
let pendingEditorStart = false;
let pendingNewGameSearch = false;
let lastActionLogLength = { analysis: null, replay: null };
const expandedDiscardLogGroups = new Set();
let previousFrameHands = { analysis: null, replay: null };
let activeView = 'play-setup';
let selectedSingleplayerHumanPlayer = 0;
let selectedMultiplayerHumanPlayer = 0;
let profileUsername = '';
let pendingProfileSave = false;
let profileModalMode = 'profile';
let profileUsernameSyncTimer = null;
let moveSoundEnabled = readStoredMoveSoundEnabled();
let moveSoundUnlocked = false;
let moveSoundAudios = new Map();
let lastLiveMoveSoundSignature = null;
let lastLiveMoveSoundLength = 0;
let suppressNextLiveMoveSound = true;
let autoRollEndEnabled = false;
let openTradeGiveResource = null;
let lastCanUseLegalActions = false;
let selectedPlayMode = 'bot';
let selectedPlayDifficulty = 5;
const initialUrlParams = new URLSearchParams(window.location.search);
const initialSharedReplaySlug = normalizeReplaySlug(initialUrlParams.get('replay'));
const initialUrlRoomCode = normalizeRoomCode(initialUrlParams.get('room'));
let pendingSharedReplaySlug = initialSharedReplaySlug || readPendingSharedReplaySlug();
let loadingSharedReplaySlug = '';
let activeReplayShareSlug = '';
let pendingAutoJoinRoomCode = pendingSharedReplaySlug ? '' : initialUrlRoomCode;
let pendingReconnectRoomCode = '';
let pendingPlayTabReconnectRoomCode = '';
let multiplayerDisconnectModalTimer = null;
let multiplayerDisconnectModalPending = null;
if (pendingAutoJoinRoomCode) selectedPlayMode = 'multiplayer';
let autoJoinRoomAttempted = false;
let multiplayerLobbyRooms = [];
let multiplayerLobbyRenderFrame = null;
let multiplayerRoomUiFrame = null;
let lastMultiplayerRoomCode = normalizeRoomCode(initialUrlRoomCode || readLastMultiplayerRoomCode());
let multiplayerInviteCopiedCode = '';
let shownMultiplayerReplayShareSlug = '';
let savedGameOverReplayLink = '';
let copiedGameOverReplayLink = '';
let createRoomSettings = {
  code: '',
  isPublic: true,
  timeMinutes: DEFAULT_MULTIPLAYER_TIME_MINUTES,
  incrementSeconds: DEFAULT_MULTIPLAYER_INCREMENT_SECONDS,
};
let multiplayerClockSyncedAtMs = Date.now();
let multiplayerClockTimer = null;
let multiplayerLobbyCountdownTimer = null;
let playMode = {
  active: false,
  mode: 'bot',
  humanPlayer: 0,
  viewerRole: 'player',
  botThinking: false,
  botThinkingKey: null,
  botThinkingStartedAt: 0,
  lastBotProgressAt: 0,
  botThinkingReason: '',
  pendingBotMoveAfterSearch: false,
  singleplayerResigned: false,
  forcedMoveKey: null,
  autoRollEndKey: null,
  pendingHumanMove: false,
  singleplayerNeedsRecovery: false,
  singleplayerRecoveryGeneration: 0,
  multiplayerRoom: null,
  multiplayerStatus: '',
  rejoiningRoom: false,
};
let multiplayerChatRoomCode = '';
let multiplayerChatMessages = [];
let serverSingleplayerHumanPlayer = undefined;
let multiplayerTurnTitleTimer = null;
let multiplayerTurnTitleActive = false;
window.hexfishProfileUsername = '';
clearStoredProfileUsername();

function guestMultiplayerMode() {
  return window.hexfishGuestMultiplayer === true;
}

function guestPlayMode() {
  return window.hexfishGuestPlay === true;
}

function guestSharedReplayMode() {
  return window.hexfishGuestSharedReplay === true;
}

function guestMode() {
  return guestPlayMode() || guestMultiplayerMode() || guestSharedReplayMode();
}

function guestReplayAnalysisLocked() {
  return guestSharedReplayMode() && (activeView === 'replay-board' || !!currentState?.replay);
}

function syncGuestUiState() {
  document.getElementById('singleplayer-setup-section')?.classList.remove('hidden');
  const startSingleplayer = document.getElementById('btn-start-play-game');
  if (startSingleplayer) {
    startSingleplayer.disabled = false;
    startSingleplayer.setAttribute('aria-disabled', 'false');
    startSingleplayer.title = guestSingleplayerDifficultyLocked() ? 'Sign in to use difficulty 6 or higher.' : '';
  }
}

function guestSingleplayerDifficultyLocked() {
  return guestMode() && selectedPlayDifficulty >= 6;
}

function prepareGuestMultiplayerEntry() {
  if (!guestMultiplayerMode()) return;
  const guestRoomCode = normalizeRoomCode(window.hexfishGuestRoomCode || initialUrlRoomCode);
  pendingSharedReplaySlug = '';
  loadingSharedReplaySlug = '';
  if (guestRoomCode && !autoJoinRoomAttempted) {
    pendingAutoJoinRoomCode = guestRoomCode;
  }
  setPlayModeChoice('multiplayer');
}

function showGuestSignInRequiredModal(feature) {
  const modal = document.getElementById('guest-signin-required-modal');
  const copy = document.getElementById('guest-signin-required-copy');
  if (!modal) return;
  if (copy) {
    copy.textContent = `${feature} requires a signed-in account. Sign in to use it and keep access to your account features.`;
  }
  modal.classList.remove('hidden');
  modal.classList.add('flex');
  document.getElementById('btn-guest-signin-required-signin')?.focus();
}

function hideGuestSignInRequiredModal() {
  const modal = document.getElementById('guest-signin-required-modal');
  if (!modal) return;
  modal.classList.add('hidden');
  modal.classList.remove('flex');
}

function signInFromGuestRequiredModal() {
  hideGuestSignInRequiredModal();
  if (typeof window.hexfishShowSignInFromGuest === 'function') {
    window.hexfishShowSignInFromGuest();
  }
}

function showGuestFeatureBlocked(feature) {
  if (!guestMode()) return false;
  if (feature === 'Singleplayer') return showGuestSingleplayerDifficultyBlocked();
  if (guestMultiplayerMode()) prepareGuestMultiplayerEntry();
  showGuestSignInRequiredModal(feature);
  return true;
}

function showGuestSingleplayerDifficultyBlocked() {
  if (!guestSingleplayerDifficultyLocked()) return false;
  showGuestSignInRequiredModal('Difficulty 6 or higher');
  return true;
}

function enterGuestPlayMode() {
  if (!guestMultiplayerMode()) return;
  prepareGuestMultiplayerEntry();
  if (playMode.active && playMode.mode === 'multiplayer') {
    showPlayView();
  } else {
    showPlaySetupView();
  }
}

function enterGuestSharedReplayMode() {
  if (!guestSharedReplayMode()) return false;
  syncGuestUiState();
  return maybeLoadSharedReplayFromUrl();
}

function signInToAnalyzeGuestReplay() {
  if (typeof window.hexfishShowSignInFromGuest === 'function') {
    window.hexfishShowSignInFromGuest();
    return;
  }
  showGuestSignInRequiredModal('Analysis');
}

function handleTopbarBrandClick() {
  if (window.hexfishAuthSignedIn || guestMode()) {
    showPlayView();
    return;
  }
  if (typeof window.hexfishShowLandingPage === 'function') {
    window.hexfishShowLandingPage();
  }
}

function localProfileName() {
  return profileUsername || '';
}

function clerkProfileUsername() {
  return validProfileUsername(window.Clerk?.user?.username || '');
}

function setProfileUsername(username) {
  profileUsername = validProfileUsername(username);
  window.hexfishProfileUsername = profileUsername;
  window.hexfishUsername = profileUsername;
  updateProfileUi();
}

function syncProfileUsernameFromClerk() {
  setProfileUsername(clerkProfileUsername());
  return profileUsername;
}

function usernameRequiredForSignedInUser() {
  return window.hexfishAuthSignedIn === true
    && !guestMode()
    && !profileUsername;
}

function showUsernameRequired(feature = 'This feature') {
  if (!usernameRequiredForSignedInUser()) return false;
  showProfileModal({ prompt: true });
  setProfileStatus(`${feature} requires a username.`, true);
  return true;
}

function setProfileStatus(message, isError = false) {
  const status = document.getElementById('profile-username-status');
  if (!status) return;
  status.textContent = message || '';
  status.classList.toggle('text-accent', !!isError);
  status.classList.toggle('text-gray-400', !isError);
}

function clerkProfileErrorMessage(error) {
  const clerkError = Array.isArray(error?.errors) && error.errors.length
    ? error.errors[0]
    : null;
  const message = String(clerkError?.longMessage || clerkError?.message || error?.message || error || 'Could not save username.')
    .replace(/\s+/g, ' ')
    .trim()
    .slice(0, 180);
  if (/username.*not a valid parameter|not a valid parameter.*username/i.test(message)) {
    return 'Username is not enabled in this Clerk instance. Enable Username in Clerk, then reload.';
  }
  return message;
}

function isClerkReverificationError(error) {
  const messages = [
    error?.message,
    error?.longMessage,
    ...(Array.isArray(error?.errors)
      ? error.errors.flatMap((entry) => [entry?.code, entry?.message, entry?.longMessage])
      : []),
  ].filter(Boolean).join(' ');
  return /additional verification|reverification|re-?verification/i.test(messages);
}

function stopProfileUsernameSyncPolling() {
  if (!profileUsernameSyncTimer) return;
  window.clearInterval(profileUsernameSyncTimer);
  profileUsernameSyncTimer = null;
}

async function refreshClerkProfileUsername() {
  const user = window.Clerk?.user;
  if (!user) return '';
  if (typeof user.reload === 'function') {
    try {
      await user.reload();
    } catch (_error) {
      return profileUsername;
    }
  }
  return syncProfileUsernameFromClerk();
}

function startProfileUsernameSyncPolling() {
  stopProfileUsernameSyncPolling();
  const deadline = Date.now() + 120_000;
  profileUsernameSyncTimer = window.setInterval(async () => {
    const username = await refreshClerkProfileUsername();
    if (username) {
      stopProfileUsernameSyncPolling();
      setProfileStatus('Saved.');
      window.setTimeout(hideProfileModal, 250);
      startOrReconnectForAuthChange({ reason: 'profile-username-updated' });
      return;
    }
    if (Date.now() > deadline) {
      stopProfileUsernameSyncPolling();
      setProfileStatus('Complete verification in Clerk, then come back here to continue.', true);
    }
  }, 1500);
}

function showClerkProfileVerificationAction() {
  const open = document.getElementById('btn-open-clerk-profile');
  if (open) {
    open.classList.remove('hidden');
    open.disabled = false;
    open.focus();
  }
  setProfileStatus('Clerk requires additional verification. Open Clerk, verify, and save your username there.', true);
}

function openClerkProfileForUsernameSetup() {
  const clerk = window.Clerk;
  if (typeof clerk?.openUserProfile !== 'function') {
    setProfileStatus('Clerk requires verification. Sign out and back in, then save your username within 10 minutes.', true);
    return;
  }
  try {
    clerk.openUserProfile();
    setProfileStatus('Complete verification in Clerk and save your username there.');
    startProfileUsernameSyncPolling();
  } catch (_error) {
    setProfileStatus('Could not open Clerk profile. Sign out and back in, then save your username within 10 minutes.', true);
  }
}

function updateProfileUi() {
  const btn = document.getElementById('btn-profile');
  if (btn) {
    const label = profileUsername || (guestMode() ? 'Profile' : 'Username');
    btn.textContent = label;
    btn.title = guestMode()
      ? 'Sign in to view your profile.'
      : profileUsername ? `Signed in as ${profileUsername}` : 'Choose a username';
  }
  const input = document.getElementById('profile-username-input');
  if (input && document.getElementById('profile-modal')?.classList.contains('hidden')) {
    input.value = profileUsername;
  }
  if (currentState?.state?.frame) {
    updatePlayerPanel(0, currentState.state);
    updatePlayerPanel(1, currentState.state);
  }
}

function playTabActiveForProfilePrompt() {
  return activeView === 'play' || activeView === 'play-setup';
}

function needsProfileUsernamePrompt() {
  return window.hexfishAuthSignedIn === true
    && !guestMode()
    && !profileUsername
    && playTabActiveForProfilePrompt();
}

function maybePromptForProfileUsername() {
  if (!needsProfileUsernamePrompt()) return;
  window.setTimeout(() => {
    if (!needsProfileUsernamePrompt()) return;
    showProfileModal({ prompt: true });
  }, 0);
}

function setProfileModalMode(mode) {
  profileModalMode = mode === 'username-prompt' ? 'username-prompt' : 'profile';
  const prompt = profileModalMode === 'username-prompt';
  const title = document.getElementById('profile-modal-title');
  const copy = document.getElementById('profile-modal-copy');
  const close = document.getElementById('btn-close-profile-modal');
  const label = document.querySelector('label[for="profile-username-input"]');
  const input = document.getElementById('profile-username-input');
  const cancel = document.getElementById('btn-cancel-profile');
  const open = document.getElementById('btn-open-clerk-profile');
  const save = document.getElementById('btn-save-profile');
  if (title) title.textContent = prompt ? 'Choose Username' : 'Profile';
  if (copy) {
    copy.textContent = prompt
      ? 'Set a username before playing so other players can recognize you.'
      : '';
    copy.classList.toggle('hidden', !prompt);
  }
  if (close) close.classList.toggle('hidden', prompt);
  if (label) label.classList.remove('hidden');
  if (input) {
    input.readOnly = !prompt;
    input.classList.toggle('opacity-70', !prompt);
  }
  if (cancel) {
    cancel.textContent = prompt ? 'Required' : 'Close';
    cancel.disabled = prompt;
    cancel.classList.toggle('hidden', prompt);
  }
  if (open) {
    open.classList.add('hidden');
    open.disabled = false;
  }
  if (save) {
    save.textContent = prompt ? 'Save Username' : 'Save';
    save.classList.toggle('hidden', !prompt);
  }
}

function showProfileModal(options = {}) {
  if (guestMode()) {
    showGuestSignInRequiredModal('Viewing your profile');
    return;
  }
  const modal = document.getElementById('profile-modal');
  const input = document.getElementById('profile-username-input');
  if (!modal || !input) return;
  syncProfileUsernameFromClerk();
  const prompt = options.prompt || !profileUsername;
  setProfileModalMode(prompt ? 'username-prompt' : 'profile');
  input.value = prompt ? (profileUsername || '') : profileUsername;
  setProfileStatus('');
  modal.classList.remove('hidden');
  modal.classList.add('flex');
  if (prompt) {
    input.focus();
    input.select();
  } else {
    document.getElementById('btn-cancel-profile')?.focus();
  }
}

function hideProfileModal() {
  if (profileModalMode === 'username-prompt' && usernameRequiredForSignedInUser()) {
    return;
  }
  const modal = document.getElementById('profile-modal');
  if (!modal) return;
  modal.classList.add('hidden');
  modal.classList.remove('flex');
  pendingProfileSave = false;
  stopProfileUsernameSyncPolling();
  setProfileModalMode('profile');
  const save = document.getElementById('btn-save-profile');
  if (save) save.disabled = false;
}

async function saveProfileUsername() {
  if (profileModalMode !== 'username-prompt') return;
  if (guestMode()) {
    showGuestSignInRequiredModal('Choosing a username');
    return;
  }
  const input = document.getElementById('profile-username-input');
  const save = document.getElementById('btn-save-profile');
  const username = validProfileUsername(input?.value);
  if (!username) {
    setProfileStatus('Use 3-24 letters, numbers, dashes, or underscores.', true);
    return;
  }
  pendingProfileSave = true;
  if (save) save.disabled = true;
  setProfileStatus('Saving...');
  try {
    const user = window.Clerk?.user;
    if (!user || typeof user.update !== 'function') {
      throw new Error('Clerk user profile is not ready.');
    }
    await user.update({ username });
    if (typeof user.reload === 'function') {
      await user.reload();
    }
    pendingProfileSave = false;
    if (save) save.disabled = false;
    syncProfileUsernameFromClerk();
    setProfileStatus('Saved.');
    window.setTimeout(hideProfileModal, 250);
    startOrReconnectForAuthChange({ reason: 'profile-username-updated' });
  } catch (error) {
    pendingProfileSave = false;
    if (save) save.disabled = false;
    if (isClerkReverificationError(error)) {
      showClerkProfileVerificationAction();
      return;
    }
    setProfileStatus(clerkProfileErrorMessage(error), true);
  }
}

controls.isAnalysisBlocked = (target) => (
  guestSharedReplayMode() && (target === 'replay' || !!currentState?.replay)
);
controls.onAnalysisBlocked = () => showGuestSignInRequiredModal('Analysis');

function setServerSingleplayerHumanPlayer(player, options = {}) {
  const humanPlayer = player == null ? null : player;
  if (!options.force && serverSingleplayerHumanPlayer === humanPlayer) return false;
  serverSingleplayerHumanPlayer = humanPlayer;
  session.send({ type: 'SetSingleplayer', human_player: humanPlayer });
  return true;
}

controls.onNewGame = () => {
  hideGameOverModal();
  clearSavedGameOverReplayLink();
  if (activeView === 'play') {
    if (playMultiplayerActive()) {
      leaveMultiplayerRoom(false, true);
    }
    pendingNewGameSearch = false;
    playMode.active = false;
    playMode.mode = selectedPlayMode;
    clearPlayBotThinking();
    playMode.pendingBotMoveAfterSearch = false;
    playMode.singleplayerResigned = false;
    playMode.forcedMoveKey = null;
    playMode.autoRollEndKey = null;
    playMode.pendingHumanMove = false;
    playMode.multiplayerRoom = null;
    playMode.multiplayerStatus = '';
    playMode.rejoiningRoom = false;
    setServerSingleplayerHumanPlayer(null);
    mctsPanel.clear();
    board.clearSearchHighlights();
    showPlaySetupView();
    return false;
  }
  pendingNewGameSearch = true;
  playMode.active = false;
  playMode.mode = selectedPlayMode;
  clearPlayBotThinking();
  playMode.pendingBotMoveAfterSearch = false;
  playMode.singleplayerResigned = false;
  playMode.forcedMoveKey = null;
  playMode.autoRollEndKey = null;
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

function actionLogEntriesThroughCursor(msg) {
  const entries = Array.isArray(msg?.action_log) ? msg.action_log : [];
  return entries.slice(0, activeActionLogLength(msg));
}

function parseActionLogPlayer(entry) {
  const firstLine = String(entry ?? '').split('\n')[0];
  const match = firstLine.match(/^\s*P([12])\s*:/i);
  return match ? Number(match[1]) - 1 : null;
}

function parseDiscardLogEntry(entry, soundKind = '') {
  if (soundKind && soundKind !== 'discard') return null;
  const firstLine = String(entry ?? '').split('\n')[0];
  const player = parseActionLogPlayer(firstLine);
  if (player == null) return null;
  const match = firstLine.match(/:\s*Drop\s+([a-z]+)\b/i);
  if (!match) return null;
  const resource = match[1].toLowerCase();
  const resourceIndex = RESOURCE_NAMES.indexOf(resource);
  if (resourceIndex < 0) return null;
  return { player, resource, resourceIndex };
}

function currentPhaseIsDiscard(msg) {
  return /\bdiscards\b/i.test(String(msg?.phase || ''));
}

function discardLogGroupKey(group) {
  return `${group.player}:${group.firstCursor}:${group.lastCursor}`;
}

function discardSummaryText(player, counts) {
  const parts = [];
  for (let i = 0; i < RESOURCE_NAMES.length; i++) {
    if (counts[i] > 0) parts.push(`${counts[i]} ${capitalizeResourceName(RESOURCE_NAMES[i])}`);
  }
  return `P${player + 1}: Discarded ${parts.join(', ')}`;
}

function buildActionLogDisplayEntries(msg, cursors, activeLogIndex) {
  const rawEntries = Array.isArray(msg?.action_log) ? msg.action_log : [];
  const soundKinds = Array.isArray(msg?.action_log_sound_kinds) ? msg.action_log_sound_kinds : [];
  const displayEntries = [];
  const discardPhaseActive = currentPhaseIsDiscard(msg);
  let i = 0;

  while (i < rawEntries.length) {
    const discard = parseDiscardLogEntry(rawEntries[i], soundKinds[i] || '');
    if (!discard) {
      displayEntries.push({
        type: 'entry',
        rawText: rawEntries[i],
        index: i,
        cursor: cursors[i] ?? i + 1,
        active: i === activeLogIndex,
      });
      i++;
      continue;
    }

    const start = i;
    const player = discard.player;
    const counts = [0, 0, 0, 0, 0];
    const children = [];
    while (i < rawEntries.length) {
      const next = parseDiscardLogEntry(rawEntries[i], soundKinds[i] || '');
      if (!next || next.player !== player) break;
      counts[next.resourceIndex]++;
      children.push({
        type: 'entry',
        rawText: rawEntries[i],
        index: i,
        cursor: cursors[i] ?? i + 1,
        active: i === activeLogIndex,
        child: true,
      });
      i++;
    }

    const end = i - 1;
    const reachesLogEnd = end === rawEntries.length - 1;
    const completed = !(discardPhaseActive && reachesLogEnd);
    if (children.length >= 2 && completed) {
      const group = {
        type: 'discard-group',
        player,
        startIndex: start,
        endIndex: end,
        firstCursor: children[0].cursor,
        lastCursor: children[children.length - 1].cursor,
        counts,
        children,
        active: activeLogIndex >= start && activeLogIndex <= end,
      };
      group.key = discardLogGroupKey(group);
      group.rawText = discardSummaryText(player, counts);
      displayEntries.push(group);
    } else {
      displayEntries.push(...children);
    }
  }

  return displayEntries;
}

function applyActionLogLineColor(line, rawText) {
  if (rawText.startsWith('P1:')) {
    line.style.color = PLAYER_COLORS[0];
  } else if (rawText.startsWith('P2:')) {
    line.style.color = PLAYER_COLORS[1];
  } else {
    line.style.color = '#a0a0a0';
    line.style.fontStyle = 'italic';
  }
}

function navigateActionLogCursor(msg, cursor) {
  if (playMultiplayerActive()) return;
  controls.stopAutoplay();
  controls._disableAutoSearch();
  if (msg.replay) {
    session.send({ type: 'SetReplayCursor', cursor });
  } else {
    session.send({ type: 'SetLogCursor', cursor });
  }
}

function renderActionLogEntry(logView, msg, entry) {
  const line = document.createElement('div');
  line.className = entry.active
    ? 'py-0.5 px-1 rounded bg-bg-3 text-gray-100'
    : 'py-0.5 px-1 rounded hover:bg-bg-3';
  if (entry.child) line.classList.add('log-discard-child');
  applyActionLogLineColor(line, entry.rawText);

  const parts = formatPlayerRefs(entry.rawText).split('\n');
  line.textContent = `${entry.index + 1}. ${parts[0]}`;
  if (playMultiplayerActive()) {
    line.style.cursor = 'default';
  } else {
    line.style.cursor = 'pointer';
    line.addEventListener('click', () => navigateActionLogCursor(msg, entry.cursor));
  }
  logView.appendChild(line);

  for (let p = 1; p < parts.length; p++) {
    const sub = document.createElement('div');
    sub.style.color = '#888';
    sub.style.fontSize = '0.85em';
    sub.style.paddingLeft = entry.child ? '2.5em' : '1.5em';
    sub.textContent = parts[p].trim();
    logView.appendChild(sub);
  }
}

function renderDiscardLogGroup(logView, msg, group) {
  const expanded = expandedDiscardLogGroups.has(group.key);
  const line = document.createElement('div');
  line.className = group.active
    ? 'log-discard-summary py-0.5 px-1 rounded bg-bg-3 text-gray-100'
    : 'log-discard-summary py-0.5 px-1 rounded hover:bg-bg-3';
  applyActionLogLineColor(line, group.rawText);
  line.style.cursor = playMultiplayerActive() ? 'default' : 'pointer';

  const toggle = document.createElement('button');
  toggle.type = 'button';
  toggle.className = 'log-discard-toggle';
  toggle.textContent = expanded ? 'v' : '>';
  toggle.setAttribute('aria-label', expanded ? 'Collapse discarded resources' : 'Expand discarded resources');
  toggle.setAttribute('aria-expanded', expanded ? 'true' : 'false');
  toggle.addEventListener('click', (event) => {
    event.stopPropagation();
    if (expandedDiscardLogGroups.has(group.key)) {
      expandedDiscardLogGroups.delete(group.key);
    } else {
      expandedDiscardLogGroups.add(group.key);
    }
    renderGameState(currentState);
  });

  const text = document.createElement('span');
  const indexText = group.startIndex === group.endIndex
    ? `${group.startIndex + 1}`
    : `${group.startIndex + 1}-${group.endIndex + 1}`;
  text.textContent = `${indexText}. ${formatPlayerRefs(group.rawText)}`;

  line.append(toggle, text);
  if (!playMultiplayerActive()) {
    line.addEventListener('click', () => navigateActionLogCursor(msg, group.lastCursor));
  }
  logView.appendChild(line);

  if (expanded) {
    for (const child of group.children) {
      renderActionLogEntry(logView, msg, child);
    }
  }
}

function spatialAction(kind, id, count, player) {
  if (!Number.isInteger(id) || id < 0 || id >= count) return null;
  return { player, kind, id };
}

function parseSpatialActionLogEntry(entry) {
  const firstLine = String(entry ?? '').split('\n')[0];
  const player = parseActionLogPlayer(firstLine);
  if (player == null) return null;
  let match = firstLine.match(/:\s*Settle\s+N(\d+)\b/i);
  if (match) {
    return spatialAction('settlement', Number(match[1]), CATAN_NODE_COUNT, player);
  }
  match = firstLine.match(/:\s*Road\s+E(\d+)\b/i);
  if (match) {
    return spatialAction('road', Number(match[1]), CATAN_EDGE_COUNT, player);
  }
  match = firstLine.match(/:\s*City\s+N(\d+)\b/i);
  if (match) {
    return spatialAction('city', Number(match[1]), CATAN_NODE_COUNT, player);
  }
  match = firstLine.match(/:\s*Robber\s+T(\d+)\b/i);
  if (match) {
    return spatialAction('robber', Number(match[1]), CATAN_TILE_COUNT, player);
  }
  return null;
}

function boardHighlightKey(item) {
  return `${item.kind}:${item.id}`;
}

function deriveLastBoardMoveHighlights(msg) {
  const entries = actionLogEntriesThroughCursor(msg);
  const lastSpatial = parseSpatialActionLogEntry(entries[entries.length - 1]);
  if (!lastSpatial) return null;
  const actor = lastSpatial.player;

  const pieces = [];
  const seen = new Set();
  for (let i = entries.length - 1; i >= 0; i--) {
    const spatial = parseSpatialActionLogEntry(entries[i]);
    if (!spatial || spatial.player !== actor) break;
    const key = boardHighlightKey(spatial);
    if (seen.has(key)) continue;
    seen.add(key);
    pieces.unshift({ kind: spatial.kind, id: spatial.id });
  }

  return pieces.length ? { player: actor, pieces } : null;
}

function readStoredMoveSoundEnabled() {
  try {
    const value = window.localStorage?.getItem(MOVE_SOUND_ENABLED_KEY);
    return value == null ? true : value !== 'false';
  } catch (_error) {
    return true;
  }
}

function writeStoredMoveSoundEnabled(enabled) {
  try {
    window.localStorage?.setItem(MOVE_SOUND_ENABLED_KEY, enabled ? 'true' : 'false');
  } catch (_error) {
    // Storage may be unavailable in private or embedded contexts.
  }
}

function normalizeMoveSoundKind(kind) {
  return Object.prototype.hasOwnProperty.call(MOVE_SOUND_ASSETS, kind) ? kind : null;
}

function moveSoundAudio(kind) {
  const normalized = normalizeMoveSoundKind(kind);
  if (!normalized) return null;
  let audio = moveSoundAudios.get(normalized);
  if (!audio) {
    audio = new Audio(MOVE_SOUND_ASSETS[normalized]);
    audio.preload = 'auto';
    audio.volume = 0.38;
    moveSoundAudios.set(normalized, audio);
  }
  return audio;
}

function preloadMoveSounds() {
  for (const kind of MOVE_SOUND_KINDS) {
    moveSoundAudio(kind)?.load();
  }
}

function unlockMoveSounds() {
  if (moveSoundUnlocked) return;
  moveSoundUnlocked = true;
  if (moveSoundEnabled) preloadMoveSounds();
}

function updateMoveSoundToggle() {
  const button = document.getElementById('btn-board-sound');
  if (button) {
    button.title = moveSoundEnabled ? 'Mute' : 'Unmute';
    button.setAttribute('aria-label', button.title);
    button.setAttribute('aria-pressed', moveSoundEnabled ? 'true' : 'false');
    button.classList.toggle('muted', !moveSoundEnabled);
  }
}

function setMoveSoundEnabled(enabled) {
  moveSoundEnabled = !!enabled;
  writeStoredMoveSoundEnabled(moveSoundEnabled);
  updateMoveSoundToggle();
  if (moveSoundEnabled && moveSoundUnlocked) preloadMoveSounds();
}

function playMoveSound(kind) {
  if (!moveSoundEnabled || !moveSoundUnlocked) return;
  const audio = moveSoundAudio(kind);
  if (!audio) return;
  try {
    audio.currentTime = 0;
  } catch (_error) {
    // Some browsers reject seeking before metadata is ready; playback can still proceed.
  }
  const play = audio.play();
  if (play && typeof play.catch === 'function') play.catch(() => {});
}

function liveMoveSoundSignature(msg) {
  const actionLog = Array.isArray(msg?.action_log) ? msg.action_log : [];
  const cursors = actionLogCursors(msg);
  const soundKinds = Array.isArray(msg?.action_log_sound_kinds) ? msg.action_log_sound_kinds : [];
  return actionLog
    .map((label, index) => `${cursors[index] ?? index + 1}:${soundKinds[index] || ''}:${label}`)
    .join('|');
}

function maybePlayMoveSoundForGameState(msg) {
  if (msg?.replay) return;
  const actionLog = Array.isArray(msg?.action_log) ? msg.action_log : [];
  const soundKinds = Array.isArray(msg?.action_log_sound_kinds) ? msg.action_log_sound_kinds : [];
  const signature = liveMoveSoundSignature(msg);
  const previousSignature = lastLiveMoveSoundSignature;
  const previousLength = lastLiveMoveSoundLength;

  lastLiveMoveSoundSignature = signature;
  lastLiveMoveSoundLength = actionLog.length;

  if (suppressNextLiveMoveSound || previousSignature == null) {
    suppressNextLiveMoveSound = false;
    return;
  }
  if (!actionLog.length || signature === previousSignature || actionLog.length < previousLength) return;

  const newEntries = actionLog.slice(previousLength);
  const newSoundKinds = soundKinds.slice(previousLength);
  const rollSevenStartedDiscard = currentPhaseIsDiscard(msg) && newEntries.some((entry, index) => (
    newSoundKinds[index] === 'roll' && parseRollTotal(entry) === 7
  ));
  if (rollSevenStartedDiscard) {
    playMoveSound('discard');
    return;
  }

  const soundKind = soundKinds[actionLog.length - 1];
  if (soundKind === 'discard') return;
  playMoveSound(soundKind);
}

function playViewActive() {
  return activeView === 'play' && playMode.active;
}

function playMultiplayerActive() {
  return playViewActive() && playMode.mode === 'multiplayer';
}

function multiplayerSpectatorActive() {
  return playMultiplayerActive() && playMode.viewerRole === 'spectator';
}

function multiplayerPlayerActive() {
  return playMultiplayerActive() && playMode.viewerRole !== 'spectator';
}

function multiplayerGameOpen() {
  return !!(playMode.active && playMode.mode === 'multiplayer' && playMode.multiplayerRoom?.code);
}

function playBotActive() {
  return playViewActive() && playMode.mode !== 'multiplayer';
}

function singleplayerGameActive() {
  return playViewActive() && playMode.mode !== 'multiplayer';
}

function singleplayerRecoveryContext(extra = {}) {
  return {
    generation: playMode.singleplayerRecoveryGeneration,
    connected: session.connected,
    activeView,
    mode: playMode.mode,
    current_player: currentState?.current_player,
    phase: currentState?.phase,
    search_running: controls.searchRunning,
    bot_thinking: playMode.botThinking,
    ...extra,
  };
}

function logSingleplayerRecovery(reason, extra = {}) {
  console.debug('HexFish singleplayer recovery', singleplayerRecoveryContext({ reason, ...extra }));
}

function markSingleplayerNeedsRecovery(reason, options = {}) {
  if (!singleplayerGameActive()) return false;
  const { newGeneration = false, ...extra } = options;
  if (newGeneration || !playMode.singleplayerNeedsRecovery) {
    playMode.singleplayerRecoveryGeneration += 1;
  }
  playMode.singleplayerNeedsRecovery = true;
  logSingleplayerRecovery(reason, extra);
  return true;
}

function clearSingleplayerRecovery(reason) {
  if (!playMode.singleplayerNeedsRecovery) return;
  playMode.singleplayerNeedsRecovery = false;
  logSingleplayerRecovery(reason);
}

function resetSingleplayerRecoveryState() {
  playMode.singleplayerNeedsRecovery = false;
  playMode.singleplayerRecoveryGeneration += 1;
}

function scheduleSingleplayerRecoveryTick(generation, attempt = 0) {
  const delay = attempt === 0 ? SINGLEPLAYER_RECOVERY_REARM_MS : SINGLEPLAYER_RECOVERY_RETRY_MS;
  window.setTimeout(() => {
    if (!singleplayerGameActive() || activeView !== 'play') return;
    if (playMode.singleplayerRecoveryGeneration !== generation) return;
    if (!session.connected) return;
    if (playMode.singleplayerNeedsRecovery) {
      if (attempt < SINGLEPLAYER_RECOVERY_MAX_ATTEMPTS) {
        logSingleplayerRecovery('retry-get-state', { attempt: attempt + 1 });
        session.send({ type: 'GetState' });
        scheduleSingleplayerRecoveryTick(generation, attempt + 1);
      }
      return;
    }
    if (!isPlayBotTurn(currentState)) {
      updateActionPanelStatus(currentState, false);
      return;
    }
    if (playMode.botThinking || controls.searchRunning) return;
    logSingleplayerRecovery('resume-automation');
    runPlayAutomation(currentState);
  }, delay);
}

function recoverSingleplayerConnection(reason) {
  if (!singleplayerGameActive()) return false;
  if (!session.connected) return false;
  if (!playMode.singleplayerNeedsRecovery) {
    playMode.singleplayerRecoveryGeneration += 1;
    playMode.singleplayerNeedsRecovery = true;
  }
  const generation = playMode.singleplayerRecoveryGeneration;
  logSingleplayerRecovery(reason);
  setServerSingleplayerHumanPlayer(playMode.humanPlayer, { force: true });
  session.send({ type: 'GetState' });
  scheduleSingleplayerRecoveryTick(generation);
  return true;
}

function checkPlayBotWatchdog() {
  if (!playBotActive() || !playMode.botThinking || controls.searchRunning) return;
  const elapsedMs = Date.now() - (playMode.botThinkingStartedAt || Date.now());
  logSingleplayerRecovery('bot-thinking-without-search', {
    elapsed_ms: elapsedMs,
    last_progress_age_ms: playMode.lastBotProgressAt ? Date.now() - playMode.lastBotProgressAt : null,
    bot_thinking_reason: playMode.botThinkingReason,
  });
  clearPlayBotThinking();
  controls.onSearchError();
  playMode.pendingBotMoveAfterSearch = false;
  playMode.forcedMoveKey = null;
  updateActionPanelStatus(currentState, false);
  if (session.connected) {
    markSingleplayerNeedsRecovery('watchdog', { newGeneration: true });
    recoverSingleplayerConnection('watchdog-get-state');
  } else {
    markSingleplayerNeedsRecovery('watchdog-disconnected', { newGeneration: true });
  }
}

function markSingleplayerSocketRecovery(reason, details = {}) {
  if (!singleplayerGameActive()) return false;
  controls.onSearchError();
  clearPlayBotThinking();
  playMode.pendingBotMoveAfterSearch = false;
  playMode.forcedMoveKey = null;
  playMode.autoRollEndKey = null;
  playMode.pendingHumanMove = false;
  markSingleplayerNeedsRecovery(reason, {
    newGeneration: true,
    ...details,
  });
  updateActionPanelStatus(currentState, false);
  return true;
}

window.hexfishIsMultiplayerActive = playMultiplayerActive;

function browserTabNeedsAttention() {
  const focused = typeof document.hasFocus === 'function' ? document.hasFocus() : true;
  return document.hidden || !focused;
}

function isLocalMultiplayerTurn(msg = currentState) {
  return !!(
    playMultiplayerActive() &&
    msg &&
    !msg.replay &&
    !msg.is_terminal &&
    !msg.is_chance &&
    !multiplayerSpectatorActive() &&
    multiplayerRoomFull() &&
    msg.current_player === playMode.humanPlayer &&
    !playMode.pendingHumanMove &&
    Array.isArray(msg.legal_actions) &&
    msg.legal_actions.length > 0
  );
}

function stopMultiplayerTurnTitleFlash() {
  if (multiplayerTurnTitleTimer) {
    window.clearInterval(multiplayerTurnTitleTimer);
    multiplayerTurnTitleTimer = null;
  }
  multiplayerTurnTitleActive = false;
  if (document.title !== BASE_DOCUMENT_TITLE) document.title = BASE_DOCUMENT_TITLE;
}

function startMultiplayerTurnTitleFlash() {
  if (multiplayerTurnTitleTimer) return;
  multiplayerTurnTitleActive = true;
  document.title = MULTIPLAYER_TURN_DOCUMENT_TITLE;
  multiplayerTurnTitleTimer = window.setInterval(() => {
    multiplayerTurnTitleActive = !multiplayerTurnTitleActive;
    document.title = multiplayerTurnTitleActive ? MULTIPLAYER_TURN_DOCUMENT_TITLE : BASE_DOCUMENT_TITLE;
  }, MULTIPLAYER_TURN_TITLE_FLASH_MS);
}

function updateMultiplayerTurnAttention() {
  if (isLocalMultiplayerTurn() && browserTabNeedsAttention()) {
    startMultiplayerTurnTitleFlash();
  } else {
    stopMultiplayerTurnTitleFlash();
  }
}

function isPlayBotTurn(msg = currentState) {
  return !!(
    playBotActive() &&
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
  if (heading) heading.textContent = 'Challenge HexFish';
}

function playNameForPlayer(idx) {
  if (!playViewActive()) return null;
  const localName = localProfileName();
  if (playMultiplayerActive()) {
    const roomName = String(playMode.multiplayerRoom?.players?.[idx]?.name || '').trim();
    if (roomName) return roomName;
    if (multiplayerSpectatorActive()) return `P${idx + 1}`;
    return idx === playMode.humanPlayer ? (localName || 'You') : 'Opponent';
  }
  return idx === playMode.humanPlayer ? (localName || 'You') : playBotName();
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
  if (playViewActive() && !multiplayerSpectatorActive() && winner === playMode.humanPlayer) {
    const name = playerDisplayName(winner);
    return name === 'You' ? 'You win' : `${name} wins`;
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
    playBotActive() &&
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

function playBotSearchKey(msg = currentState) {
  if (!msg) return '';
  const actions = Array.isArray(msg.legal_actions)
    ? msg.legal_actions.map(a => Number(a?.action)).join(',')
    : '';
  const nodeKind = msg.is_chance ? 'chance' : 'decision';
  return `${logHistoryCursor(msg)}:${msg.current_player}:${msg.phase}:${nodeKind}:${actions}`;
}

function clearPlayBotThinking() {
  playMode.botThinking = false;
  playMode.botThinkingKey = null;
  playMode.botThinkingStartedAt = 0;
  playMode.lastBotProgressAt = 0;
  playMode.botThinkingReason = '';
}

function markPlayBotThinking(msg = currentState, reason = 'bot-move') {
  const now = Date.now();
  playMode.botThinking = true;
  playMode.botThinkingKey = playBotSearchKey(msg);
  playMode.botThinkingStartedAt = now;
  playMode.lastBotProgressAt = now;
  playMode.botThinkingReason = reason;
}

function reconcilePlayBotThinking(msg = currentState) {
  if (!playMode.botThinking) return;
  const stale = !playBotActive() ||
    !msg ||
    msg.replay ||
    msg.is_terminal ||
    msg.is_chance ||
    msg.current_player === playMode.humanPlayer ||
    (playMode.botThinkingKey && playMode.botThinkingKey !== playBotSearchKey(msg)) ||
    (!playMode.botThinkingKey && !controls.searchRunning);
  if (!stale) return;
  clearPlayBotThinking();
  if (controls.searchRunning) controls.onSearchError();
}

function isAutoRollEndAction(action) {
  return action === CATAN_ROLL_ACTION || action === CATAN_END_TURN_ACTION;
}

function autoRollEndForcedAction(msg) {
  if (!autoRollEndEnabled || !playViewActive() || playMode.pendingHumanMove) return null;
  if (!msg || msg.replay || msg.is_terminal || msg.is_chance) return null;
  if (msg.current_player !== playMode.humanPlayer) return null;
  if (!Array.isArray(msg.legal_actions) || msg.legal_actions.length !== 1) return null;
  if (playMultiplayerActive() && (
    multiplayerTimeoutWinner() != null ||
    !multiplayerRoomFull()
  )) return null;

  const action = Number(msg.legal_actions[0]?.action);
  return isAutoRollEndAction(action) ? action : null;
}

function maybeAutoRollEnd(msg) {
  const action = autoRollEndForcedAction(msg);
  if (action == null) {
    playMode.autoRollEndKey = null;
    return false;
  }
  const key = playForcedMoveKey(msg, action);
  if (playMode.autoRollEndKey === key) return true;
  playMode.autoRollEndKey = key;
  sendPlayAction(action);
  return true;
}

function updateAutoRollEndToggle() {
  const toggle = document.getElementById('auto-roll-end-toggle');
  if (toggle) toggle.checked = autoRollEndEnabled;
}

function setAutoRollEndEnabled(enabled) {
  autoRollEndEnabled = !!enabled;
  updateAutoRollEndToggle();
  if (autoRollEndEnabled) maybeAutoRollEnd(currentState);
}

function sendPlayAction(action) {
  if (multiplayerSpectatorActive()) return;
  if (playMultiplayerActive() && multiplayerTimeoutWinner() != null) return;
  const msg = playMultiplayerActive()
    ? { type: 'PlayMultiplayerAction', action }
    : { type: 'PlayAction', action };
  if (playViewActive()) {
    if (playMode.pendingHumanMove) return;
    playMode.pendingHumanMove = true;
    updateMultiplayerTurnAttention();
    if (playBotActive() && controls.searchRunning && controls.interruptSearchForCommand(msg)) return;
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
  syncGuestUiState();
  if (guestSingleplayerDifficultyLocked()) showGuestSingleplayerDifficultyBlocked();
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

function normalizePlaySide(player, _scope = 'singleplayer') {
  if (player === 'random') return 'random';
  return Number(player) === 1 ? 1 : 0;
}

function playSideScope(btn) {
  return btn?.dataset?.playScope === 'multiplayer' ? 'multiplayer' : 'singleplayer';
}

function selectedPlaySideForScope(scope) {
  return scope === 'multiplayer' ? selectedMultiplayerHumanPlayer : selectedSingleplayerHumanPlayer;
}

function playSideButtonValue(btn) {
  return btn?.dataset?.player === 'random' ? 'random' : Number(btn?.dataset?.player);
}

function updatePlaySideButtons() {
  for (const btn of document.querySelectorAll('.play-side-btn')) {
    const active = playSideButtonValue(btn) === selectedPlaySideForScope(playSideScope(btn));
    btn.classList.toggle('bg-accent', active);
    btn.classList.toggle('text-white', active);
    btn.classList.toggle('bg-bg-3', !active);
    btn.classList.toggle('text-gray-300', !active);
    btn.classList.toggle('hover:bg-bg', !active);
    btn.setAttribute('aria-pressed', active ? 'true' : 'false');
  }
}

function setPlaySide(player, scope = 'singleplayer') {
  const side = normalizePlaySide(player, scope);
  if (scope === 'multiplayer') {
    selectedMultiplayerHumanPlayer = side;
  } else {
    selectedSingleplayerHumanPlayer = side;
  }
  updatePlaySideButtons();
}

function resolvedSingleplayerHumanPlayer() {
  return selectedSingleplayerHumanPlayer === 'random'
    ? Math.floor(Math.random() * 2)
    : selectedSingleplayerHumanPlayer;
}

// ── Message handlers ─────────────────────────────────────────────────

function normalizeRoomCode(code) {
  return String(code || '').trim().toUpperCase().replace(/[^A-Z0-9]/g, '').slice(0, 12);
}

function normalizeReplaySlug(slug) {
  return String(slug || '').trim().replace(/[^A-Za-z0-9_-]/g, '').slice(0, 128);
}

function normalizeProfileUsername(username) {
  return String(username || '')
    .trim()
    .replace(/[^A-Za-z0-9_-]/g, '')
    .slice(0, 24);
}

function validProfileUsername(username) {
  const normalized = normalizeProfileUsername(username);
  return normalized.length >= 3 ? normalized : '';
}

function clearStoredProfileUsername() {
  try {
    window.localStorage?.removeItem('hexfish-profile-username');
  } catch (_error) {
    // Storage may be unavailable in private or embedded contexts.
  }
}

function currentSharedReplaySlugFromUrl() {
  return normalizeReplaySlug(new URLSearchParams(window.location.search).get('replay'));
}

function currentReplayShareSlug() {
  return normalizeReplaySlug(activeReplayShareSlug || currentSharedReplaySlugFromUrl());
}

function updateReplayShareButton() {
  const button = document.getElementById('btn-replay-share');
  if (!button) return;
  const slug = currentReplayShareSlug();
  const visible = (activeView === 'replay-board' || !!currentState?.replay) && !!slug;
  button.classList.toggle('hidden', !visible);
  button.disabled = !visible;
}

function readPendingSharedReplaySlug() {
  try {
    return normalizeReplaySlug(window.sessionStorage?.getItem(PENDING_SHARED_REPLAY_KEY));
  } catch (_error) {
    return '';
  }
}

function writePendingSharedReplaySlug(slug) {
  const normalized = normalizeReplaySlug(slug);
  try {
    if (normalized) {
      window.sessionStorage?.setItem(PENDING_SHARED_REPLAY_KEY, normalized);
    } else {
      window.sessionStorage?.removeItem(PENDING_SHARED_REPLAY_KEY);
    }
  } catch (_error) {
    // Session storage may be unavailable in private or embedded contexts.
  }
}

function readLastMultiplayerRoomCode() {
  try {
    return window.localStorage?.getItem(LAST_MULTIPLAYER_ROOM_KEY) || '';
  } catch (_error) {
    return '';
  }
}

function writeLastMultiplayerRoomCode(code) {
  const normalized = normalizeRoomCode(code);
  lastMultiplayerRoomCode = normalized;
  try {
    if (normalized) {
      window.localStorage?.setItem(LAST_MULTIPLAYER_ROOM_KEY, normalized);
    } else {
      window.localStorage?.removeItem(LAST_MULTIPLAYER_ROOM_KEY);
    }
  } catch (_error) {
    // Storage may be unavailable in private or embedded contexts.
  }
  updateMultiplayerReconnectUi();
}

function reconnectRoomCode() {
  return normalizeRoomCode(
    pendingAutoJoinRoomCode ||
    lastMultiplayerRoomCode ||
    readLastMultiplayerRoomCode()
  );
}

function setPlayModeChoice(mode) {
  selectedPlayMode = mode === 'multiplayer' ? 'multiplayer' : 'bot';
  if (!playMode.active) playMode.mode = selectedPlayMode;
  for (const btn of document.querySelectorAll('.play-mode-btn')) {
    const active = btn.dataset.playMode === selectedPlayMode;
    btn.classList.toggle('bg-accent', active);
    btn.classList.toggle('text-white', active);
    btn.classList.toggle('bg-bg-3', !active);
    btn.classList.toggle('text-gray-300', !active);
    btn.classList.toggle('hover:bg-bg', !active);
    btn.setAttribute('aria-pressed', active ? 'true' : 'false');
  }
  updatePlayBotLabels();
  updatePlaySideButtons();
  syncGuestUiState();
  updateMultiplayerRoomUi();
}

function multiplayerRoomFull(room = playMode.multiplayerRoom) {
  return !!(room?.players?.[0]?.occupied && room?.players?.[1]?.occupied);
}

function multiplayerOpponentConnected(room = playMode.multiplayerRoom) {
  if (multiplayerSpectatorActive()) return multiplayerRoomFull(room);
  if (!room || (playMode.humanPlayer !== 0 && playMode.humanPlayer !== 1)) return false;
  return !!room.players?.[playMode.humanPlayer === 0 ? 1 : 0]?.connected;
}

function roomInviteUrl(code) {
  const url = new URL(window.location.href);
  url.searchParams.delete('replay');
  url.searchParams.set('room', normalizeRoomCode(code));
  return url.toString();
}

function multiplayerRoomHasClock(room = playMode.multiplayerRoom) {
  return room?.time_minutes != null;
}

function formatMultiplayerTimeControl(room = playMode.multiplayerRoom) {
  if (!room) return '';
  if (!multiplayerRoomHasClock(room)) return 'Unlimited';
  const time = Number(room?.time_minutes);
  const increment = Number(room?.increment_seconds);
  if (!Number.isFinite(time) || time <= 0) return '';
  const safeTime = Math.round(time);
  const safeIncrement = Number.isFinite(increment) && increment > 0 ? Math.round(increment) : 0;
  return safeIncrement > 0 ? `${safeTime}+${safeIncrement}` : `${safeTime}+0`;
}

function formatClockMillis(milliseconds) {
  if (milliseconds == null) return '--:--';
  const value = Number(milliseconds);
  if (!Number.isFinite(value) || value < 0) return '--:--';
  const total = Math.ceil(value / 1000);
  const minutes = Math.floor(total / 60);
  const secs = total % 60;
  return `${minutes}:${String(secs).padStart(2, '0')}`;
}

function multiplayerWinner() {
  const rawWinner = playMode.multiplayerRoom?.winner;
  if (rawWinner == null) return null;
  const winner = Number(rawWinner);
  return winner === 0 || winner === 1 ? winner : null;
}

function multiplayerResultText() {
  const winner = multiplayerWinner();
  if (winner == null) return '';
  const winnerName = playerDisplayName(winner);
  const reason = String(playMode.multiplayerRoom?.finish_reason || '').toLowerCase();
  const verb = winnerName === 'You' ? 'win' : 'wins';
  if (reason === 'timeout') return `${winnerName} ${verb} on time`;
  if (reason === 'resignation') return `${winnerName} ${verb} by resignation`;
  return `${winnerName} ${verb}`;
}

function multiplayerTimeoutWinner() {
  return multiplayerWinner();
}

function multiplayerTimeoutResultText() {
  return multiplayerResultText();
}

function updateVictoryBadge(idx, playerFrame) {
  const vpEl = document.getElementById(`p${idx}-vp`);
  if (!vpEl || !playerFrame) return;
  const publicVp = publicVictoryPoints(playerFrame);
  const totalVp = Number(playerFrame.vp) || 0;
  vpEl.textContent = formatVictoryPoints(idx, publicVp, totalVp);
  vpEl.title = totalVp > publicVp
    ? `${publicVp} public VP, ${totalVp} total VP`
    : `${totalVp} VP`;
}

function updateMultiplayerTimeoutWinnerBadge() {
  for (let player = 0; player < 2; player++) {
    const card = document.getElementById(`player-${player}`);
    const vpEl = document.getElementById(`p${player}-vp`);
    if (!card || !vpEl) continue;
    card.classList.remove('timeout-winner');
    vpEl.classList.remove('win-badge');
    if (currentState?.state?.frame?.players?.[player]) {
      updateVictoryBadge(player, currentState.state.frame.players[player]);
    }
  }
}

function updateMultiplayerClockUi() {
  const room = playMode.multiplayerRoom;
  const show = playMultiplayerActive() && multiplayerRoomHasClock(room);
  const increment = Number(room?.increment_seconds);
  const incrementText = Number.isFinite(increment) ? `+${Math.max(0, Math.round(increment))}s inc` : '+0s inc';
  const elapsedMs = Date.now() - multiplayerClockSyncedAtMs;
  const winner = multiplayerTimeoutWinner();

  for (let player = 0; player < 2; player++) {
    const card = document.getElementById(`multiplayer-clock-${player}`);
    const time = document.getElementById(`multiplayer-clock-time-${player}`);
    const inc = document.getElementById(`multiplayer-clock-increment-${player}`);
    const add = document.getElementById(`btn-add-opponent-time-${player}`);
    if (!card || !time || !inc || !add) continue;

    const playerInfo = room?.players?.[player];
    const baseClock = Number(playerInfo?.time_millis);
    const playerClock = Number.isFinite(baseClock)
      ? Math.max(0, baseClock - (playerInfo?.clock_active && winner == null ? elapsedMs : 0))
      : null;
    const isOpponent = show && !multiplayerSpectatorActive() && winner == null && player !== playMode.humanPlayer;
    card.classList.toggle('hidden', !show);
    time.textContent = formatClockMillis(playerClock);
    time.classList.toggle('low-time', playerClock != null && playerClock < 60_000);
    time.classList.toggle('critical-time', playerClock != null && playerClock < 10_000);
    inc.textContent = incrementText;
    add.classList.toggle('hidden', !isOpponent);
    add.disabled = !isOpponent || playerClock == null;
    add.title = isOpponent ? 'Add 15 seconds to opponent clock' : '';
  }
}

function updateMultiplayerClockTimer() {
  const shouldRun = playMultiplayerActive() &&
    multiplayerRoomHasClock() &&
    multiplayerTimeoutWinner() == null &&
    (playMode.multiplayerRoom?.players || []).some(player => player?.clock_active);
  if (shouldRun && !multiplayerClockTimer) {
    multiplayerClockTimer = window.setInterval(updateMultiplayerClockUi, 250);
  } else if (!shouldRun && multiplayerClockTimer) {
    window.clearInterval(multiplayerClockTimer);
    multiplayerClockTimer = null;
  }
}

function updateMultiplayerResultBanner() {
  hideBottomResultBanner();
  const winner = multiplayerTimeoutWinner();
  if (playMultiplayerActive() && winner != null) {
    updateMultiplayerTimeoutWinnerBadge();
    controls.onGameOver();
  } else if (!currentState?.result) {
    updateMultiplayerTimeoutWinnerBadge();
  }
  updateGameOverModal(currentState);
  updateActionPanelStatus(currentState, false);
}

function setRoomCodeInUrl(code) {
  const normalized = normalizeRoomCode(code);
  const url = new URL(window.location.href);
  if (normalized) {
    url.searchParams.set('room', normalized);
  } else {
    url.searchParams.delete('room');
  }
  window.history.replaceState(window.history.state, '', url.toString());
  pendingAutoJoinRoomCode = normalized;
}

function setMultiplayerSetupStatus(text) {
  const status = document.getElementById('multiplayer-room-status');
  if (!status) return;
  status.textContent = text || '';
  status.classList.toggle('hidden', !text);
}

function clampInteger(value, min, max, fallback) {
  const number = Number(value);
  if (!Number.isFinite(number)) return fallback;
  return Math.max(min, Math.min(max, Math.round(number)));
}

function createRoomHasClock() {
  return createRoomSettings.timeMinutes != null;
}

function parseCreateRoomTimeMinutes(value) {
  if (String(value).toLowerCase() === 'unlimited') return null;
  return clampInteger(value, 1, 180, DEFAULT_MULTIPLAYER_TIME_MINUTES);
}

function randomMultiplayerRoomCode() {
  let code = '';
  for (let i = 0; i < 6; i++) {
    code += MULTIPLAYER_ROOM_CODE_ALPHABET[Math.floor(Math.random() * MULTIPLAYER_ROOM_CODE_ALPHABET.length)];
  }
  return code;
}

function ensureCreateRoomCode() {
  if (!createRoomSettings.code) {
    createRoomSettings.code = randomMultiplayerRoomCode();
  }
  return createRoomSettings.code;
}

function updateCreateRoomPresetButtons() {
  for (const btn of document.querySelectorAll('[data-room-visibility]')) {
    const active = (btn.dataset.roomVisibility === 'public') === createRoomSettings.isPublic;
    btn.classList.toggle('active', active);
    btn.setAttribute('aria-pressed', active ? 'true' : 'false');
  }
  for (const btn of document.querySelectorAll('[data-time-minutes]')) {
    const preset = parseCreateRoomTimeMinutes(btn.dataset.timeMinutes);
    const active = preset === createRoomSettings.timeMinutes;
    btn.classList.toggle('active', active);
    btn.setAttribute('aria-pressed', active ? 'true' : 'false');
  }
  for (const btn of document.querySelectorAll('[data-increment-seconds]')) {
    const active = createRoomHasClock() && Number(btn.dataset.incrementSeconds) === createRoomSettings.incrementSeconds;
    btn.classList.toggle('active', active);
    btn.disabled = !createRoomHasClock();
    btn.setAttribute('aria-pressed', active ? 'true' : 'false');
    btn.setAttribute('aria-disabled', createRoomHasClock() ? 'false' : 'true');
  }
}

function updateCreateRoomModalUi() {
  const codeEl = document.getElementById('create-room-code-preview');
  const timeInput = document.getElementById('create-room-time-input');
  const timeSlider = document.getElementById('create-room-time-slider');
  const incrementInput = document.getElementById('create-room-increment-input');
  const incrementSlider = document.getElementById('create-room-increment-slider');
  const hasClock = createRoomHasClock();

  if (codeEl) codeEl.textContent = ensureCreateRoomCode();
  if (timeInput) {
    timeInput.value = hasClock ? String(createRoomSettings.timeMinutes) : '';
    timeInput.placeholder = hasClock ? '' : 'Unlimited';
  }
  if (timeSlider) {
    timeSlider.value = String(Math.min(Number(timeSlider.max), createRoomSettings.timeMinutes || Number(timeSlider.max)));
    timeSlider.disabled = !hasClock;
    timeSlider.setAttribute('aria-disabled', hasClock ? 'false' : 'true');
  }
  if (incrementInput) incrementInput.value = String(createRoomSettings.incrementSeconds);
  if (incrementInput) {
    incrementInput.disabled = !hasClock;
    incrementInput.setAttribute('aria-disabled', hasClock ? 'false' : 'true');
  }
  if (incrementSlider) {
    incrementSlider.value = String(Math.min(Number(incrementSlider.max), createRoomSettings.incrementSeconds));
    incrementSlider.disabled = !hasClock;
    incrementSlider.setAttribute('aria-disabled', hasClock ? 'false' : 'true');
  }
  updateCreateRoomPresetButtons();
  updatePlaySideButtons();
}

function setCreateRoomTimeMinutes(value) {
  createRoomSettings.timeMinutes = parseCreateRoomTimeMinutes(value);
  updateCreateRoomModalUi();
}

function setCreateRoomIncrementSeconds(value) {
  createRoomSettings.incrementSeconds = clampInteger(value, 0, 120, DEFAULT_MULTIPLAYER_INCREMENT_SECONDS);
  updateCreateRoomModalUi();
}

function setCreateRoomVisibility(value) {
  createRoomSettings.isPublic = value !== 'private';
  updateCreateRoomModalUi();
}

function openCreateMultiplayerRoomModal() {
  setPlayModeChoice('multiplayer');
  createRoomSettings.code = randomMultiplayerRoomCode();
  updateCreateRoomModalUi();
  const modal = document.getElementById('create-multiplayer-room-modal');
  if (!modal) return;
  modal.classList.remove('hidden');
  modal.classList.add('flex');
  document.getElementById('btn-confirm-create-multiplayer-room')?.focus();
}

function hideCreateMultiplayerRoomModal() {
  const modal = document.getElementById('create-multiplayer-room-modal');
  if (!modal) return;
  modal.classList.add('hidden');
  modal.classList.remove('flex');
}

function multiplayerLobbySeatCount(value) {
  const count = Number(value);
  if (!Number.isFinite(count)) return 0;
  return Math.max(0, Math.min(2, Math.round(count)));
}

function multiplayerLobbyRoomFull(room) {
  return multiplayerLobbySeatCount(room?.occupied) >= 2 || room?.status === 'active';
}

function multiplayerLobbySpectatorCount(room) {
  const count = Number(room?.spectator_count);
  if (!Number.isFinite(count)) return 0;
  return Math.max(0, Math.round(count));
}

function formatShortDuration(milliseconds) {
  const safe = Math.max(0, Math.ceil(Number(milliseconds) / 1000));
  const minutes = Math.floor(safe / 60);
  const seconds = safe % 60;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}

function multiplayerLobbyCloseCountdownText(room) {
  const deadline = Number(room?.empty_room_closes_at_ms);
  if (!Number.isFinite(deadline) || deadline <= 0) return '';
  return `Closes in ${formatShortDuration(deadline - Date.now())}`;
}

function multiplayerLobbyStatusText(room) {
  const occupied = multiplayerLobbySeatCount(room?.occupied);
  const connected = multiplayerLobbySeatCount(room?.connected);
  const spectators = multiplayerLobbySpectatorCount(room);
  const timeControl = formatMultiplayerTimeControl(room);
  const closeText = multiplayerLobbyCloseCountdownText(room);
  const parts = [];
  if (timeControl) parts.push(timeControl);
  if (spectators > 0) parts.push(`${spectators} watching`);
  if (closeText) parts.push(closeText);
  const suffix = parts.length ? `, ${parts.join(', ')}` : '';
  if (occupied >= 2) {
    return connected >= 2 ? `Full${suffix}` : `Full, ${connected} online${suffix}`;
  }
  return `${occupied}/2 seats, ${connected} online${suffix}`;
}

function updateMultiplayerLobbyCountdownTimer() {
  const hasCountdown = multiplayerLobbyRooms.some(room => multiplayerLobbyCloseCountdownText(room));
  if (hasCountdown && !multiplayerLobbyCountdownTimer) {
    multiplayerLobbyCountdownTimer = window.setInterval(() => {
      renderMultiplayerLobby();
    }, MULTIPLAYER_LOBBY_COUNTDOWN_MS);
  } else if (!hasCountdown && multiplayerLobbyCountdownTimer) {
    window.clearInterval(multiplayerLobbyCountdownTimer);
    multiplayerLobbyCountdownTimer = null;
  }
}

function requestMultiplayerLobby() {
  if (showUsernameRequired('Multiplayer')) return;
  session.send({ type: 'ListMultiplayerRooms' });
}

function renderMultiplayerLobby(rooms = multiplayerLobbyRooms) {
  const list = document.getElementById('multiplayer-lobby-list');
  if (!list) return;
  list.innerHTML = '';

  const entries = (Array.isArray(rooms) ? rooms : [])
    .map(room => ({ room, code: normalizeRoomCode(room?.code) }))
    .filter(entry => entry.code);
  if (!entries.length) {
    const empty = document.createElement('div');
    empty.className = 'px-2.5 py-2 text-xs text-gray-500';
    empty.textContent = 'No rooms';
    list.appendChild(empty);
    updateMultiplayerLobbyCountdownTimer();
    return;
  }

  const currentCode = normalizeRoomCode(playMode.multiplayerRoom?.code);
  for (const { room, code } of entries) {
    const isCurrent = code && code === currentCode;
    const isFull = multiplayerLobbyRoomFull(room);
    const row = document.createElement('button');
    row.type = 'button';
    row.className = 'flex w-full items-center justify-between gap-2 border-b border-gray-700 px-2.5 py-2 text-left text-xs text-gray-200 hover:bg-bg-3 disabled:cursor-default disabled:opacity-50';
    row.disabled = !!isCurrent;
    row.title = isCurrent ? `Current room ${code}` : isFull ? `Spectate room ${code}` : `Join room ${code}`;
    if (isCurrent) row.classList.add('bg-bg-3');
    row.addEventListener('click', () => {
      const input = document.getElementById('multiplayer-room-code-input');
      if (input) input.value = code;
      if (!isCurrent) joinMultiplayerRoom(code);
    });

    const roomInfo = document.createElement('span');
    roomInfo.className = 'flex min-w-0 flex-col gap-0.5';
    const codeText = document.createElement('span');
    codeText.className = 'font-mono text-gray-100';
    codeText.textContent = code;
    const metaText = document.createElement('span');
    metaText.className = 'text-[11px] text-gray-500';
    metaText.textContent = multiplayerLobbyStatusText(room);
    roomInfo.append(codeText, metaText);

    const actionText = document.createElement('span');
    actionText.className = 'shrink-0 text-[11px] text-gray-400';
    actionText.textContent = isCurrent ? 'Current' : isFull ? 'Spectate' : 'Join';

    row.append(roomInfo, actionText);
    list.appendChild(row);
  }
  updateMultiplayerLobbyCountdownTimer();
}

function requestMultiplayerUiFrame(callback) {
  if (typeof window.requestAnimationFrame === 'function') {
    return window.requestAnimationFrame(callback);
  }
  return window.setTimeout(callback, 16);
}

function scheduleMultiplayerLobbyRender() {
  if (multiplayerLobbyRenderFrame != null) return;
  multiplayerLobbyRenderFrame = requestMultiplayerUiFrame(() => {
    multiplayerLobbyRenderFrame = null;
    renderMultiplayerLobby();
  });
}

function scheduleMultiplayerRoomUiUpdate() {
  if (multiplayerRoomUiFrame != null) return;
  multiplayerRoomUiFrame = requestMultiplayerUiFrame(() => {
    multiplayerRoomUiFrame = null;
    updateMultiplayerRoomUi();
  });
}

function resetMultiplayerChat(roomCode = '') {
  multiplayerChatRoomCode = normalizeRoomCode(roomCode);
  multiplayerChatMessages = [];
  renderMultiplayerChat();
}

function normalizeMultiplayerChatMessages(messages) {
  if (!Array.isArray(messages)) return [];
  return messages.map((message) => {
    const player = Number(message?.player);
    const id = Number(message?.id);
    const sentAt = Number(message?.sent_at_ms);
    return {
      id: Number.isFinite(id) ? id : 0,
      sent_at_ms: Number.isFinite(sentAt) ? sentAt : 0,
      player: player === 0 || player === 1 ? player : null,
      name: String(message?.name || '').trim(),
      text: String(message?.text || ''),
    };
  }).filter(message => message.player != null && message.text);
}

function updateMultiplayerChatUi() {
  renderMultiplayerChat();
}

function renderMultiplayerChat() {
  const panel = document.getElementById('multiplayer-chat-panel');
  const log = document.getElementById('multiplayer-chat-log');
  const form = document.getElementById('multiplayer-chat-form');
  const input = document.getElementById('multiplayer-chat-input');
  const button = document.getElementById('btn-send-multiplayer-chat');
  const roomEl = document.getElementById('multiplayer-chat-room');
  if (!panel || !log || !form || !input || !button) return;

  const roomCode = normalizeRoomCode(playMode.multiplayerRoom?.code || multiplayerChatRoomCode);
  const show = multiplayerPlayerActive() && !!roomCode;
  panel.classList.toggle('hidden', !show);
  input.disabled = !show || !session.connected;
  button.disabled = !show || !session.connected;
  if (roomEl) roomEl.textContent = show ? roomCode : '';
  if (!show) return;

  const wasAtBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 8;
  log.innerHTML = '';
  for (const message of multiplayerChatMessages) {
    const own = message.player === playMode.humanPlayer;
    const row = document.createElement('div');
    row.className = `multiplayer-chat-message ${own ? 'own' : 'other'} p${message.player + 1}`;

    const meta = document.createElement('div');
    meta.className = 'multiplayer-chat-meta';
    meta.textContent = message.name || playerDisplayName(message.player);

    const text = document.createElement('div');
    text.className = 'multiplayer-chat-text';
    text.textContent = message.text;

    row.append(meta, text);
    log.appendChild(row);
  }
  if (wasAtBottom) log.scrollTop = log.scrollHeight;
}

function handleMultiplayerChatMessage(msg) {
  if (!multiplayerPlayerActive()) return;
  const roomCode = normalizeRoomCode(playMode.multiplayerRoom?.code);
  if (!roomCode) return;
  multiplayerChatRoomCode = roomCode;
  multiplayerChatMessages = normalizeMultiplayerChatMessages(msg.messages);
  renderMultiplayerChat();
}

function sendMultiplayerChat(event) {
  event?.preventDefault();
  if (!multiplayerPlayerActive() || !session.connected) return;
  const input = document.getElementById('multiplayer-chat-input');
  if (!input) return;
  const text = String(input.value || '');
  if (!text.trim()) return;
  input.value = '';
  session.send({ type: 'SendMultiplayerChat', text });
  renderMultiplayerChat();
}

function updateMultiplayerReconnectUi() {
  const row = document.getElementById('multiplayer-reconnect-row');
  const codeEl = document.getElementById('multiplayer-reconnect-code');
  const btn = document.getElementById('btn-reconnect-multiplayer-room');
  if (!row || !codeEl || !btn) return;
  const code = reconnectRoomCode();
  const currentCode = normalizeRoomCode(playMode.multiplayerRoom?.code);
  const show = !!code && code !== currentCode;
  codeEl.textContent = code || '-';
  btn.disabled = !show;
  btn.title = code ? `Reconnect to room ${code}` : '';
  row.classList.toggle('hidden', !show);
}

function updateMultiplayerRoomUi() {
  const room = playMode.multiplayerRoom;
  const code = normalizeRoomCode(room?.code);

  renderMultiplayerLobby();
  updateMultiplayerReconnectUi();
  updateMultiplayerInviteModal();
  updateMultiplayerClockUi();
  updateMultiplayerClockTimer();
  updateMultiplayerResultBanner();
  updateBoardSpectatorButton();
  updateMultiplayerChatUi();

  if (!room) {
    if (selectedPlayMode === 'multiplayer') {
      setMultiplayerSetupStatus(pendingAutoJoinRoomCode && !autoJoinRoomAttempted ? `Ready to join ${pendingAutoJoinRoomCode}` : '');
    }
    return;
  }

  const spectating = multiplayerSpectatorActive();
  const side = playMode.humanPlayer === 1 ? 'P2' : 'P1';
  const timeControl = formatMultiplayerTimeControl(room);
  const timeText = multiplayerRoomHasClock(room)
    ? timeControl ? ` Clock ${timeControl}.` : ''
    : ' No clock.';
  if (spectating) {
    setMultiplayerSetupStatus(`Room ${code}.${timeText} Spectating.`);
  } else if (room.status === 'waiting' || !multiplayerRoomFull(room)) {
    setMultiplayerSetupStatus(`Room ${code} is waiting. You are ${side}.${timeText}`);
  } else if (!multiplayerOpponentConnected(room)) {
    setMultiplayerSetupStatus(`Room ${code}.${timeText} Opponent disconnected.`);
  } else {
    setMultiplayerSetupStatus(`Room ${code}.${timeText} You are ${side}.`);
  }
}

function requestActiveMultiplayerRoomSync() {
  if (!session.connected || !playMode.active || playMode.mode !== 'multiplayer') return;
  if (!playMode.multiplayerRoom?.code || pendingReconnectRoomCode || playMode.rejoiningRoom) return;
  session.send({ type: 'GetMultiplayerRoom' });
}

function spectatorDisplayNames(room = playMode.multiplayerRoom) {
  const spectators = Array.isArray(room?.spectators) ? room.spectators : [];
  return spectators.map((spectator, index) => {
    const name = String(spectator?.name || '').trim();
    if (name) return spectator?.you ? `${name} (you)` : name;
    return spectator?.you ? 'You' : `Spectator ${index + 1}`;
  });
}

function updateBoardSpectatorButton() {
  const btn = document.getElementById('btn-board-spectators');
  const countEl = document.getElementById('board-spectator-count');
  if (!btn || !countEl) return;
  const show = playMultiplayerActive() && !!playMode.multiplayerRoom?.code;
  const names = spectatorDisplayNames();
  const count = names.length;
  btn.classList.toggle('hidden', !show);
  btn.classList.toggle('active', count > 0);
  countEl.textContent = String(count);
  const label = count > 0 ? `Watching: ${names.join(', ')}` : 'No spectators watching';
  btn.title = label;
  btn.dataset.spectators = label;
  btn.setAttribute('aria-label', label);
}

function updateMultiplayerInviteModal() {
  const modal = document.getElementById('multiplayer-invite-modal');
  const linkInput = document.getElementById('multiplayer-invite-link');
  const codeEl = document.getElementById('multiplayer-invite-room-code');
  const copyBtn = document.getElementById('btn-copy-multiplayer-invite');
  if (!modal || !linkInput || !codeEl || !copyBtn) return;

  const code = normalizeRoomCode(playMode.multiplayerRoom?.code);
  const show = playMultiplayerActive() && !multiplayerSpectatorActive() && !!code && !multiplayerRoomFull();
  if (!show) multiplayerInviteCopiedCode = '';
  modal.classList.toggle('hidden', !show);
  modal.classList.toggle('flex', show);
  codeEl.textContent = code || '-';
  linkInput.value = show ? roomInviteUrl(code) : '';
  copyBtn.textContent = show && multiplayerInviteCopiedCode === code ? 'Copied' : 'Copy';
}

function updateMultiplayerResignButton() {
  const btn = document.getElementById('btn-resign-multiplayer');
  if (!btn) return;
  const show = multiplayerPlayerActive() && multiplayerWinner() == null && !currentState?.is_terminal;
  const player = playMode.humanPlayer === 1 ? 1 : 0;
  const actions = document.getElementById(`player-${player}-actions`);
  if (actions && btn.parentElement !== actions) {
    actions.appendChild(btn);
  }
  btn.classList.toggle('hidden', !show);
  btn.disabled = !show || !multiplayerRoomFull();
}

function updateSingleplayerResignButton() {
  const btn = document.getElementById('btn-resign-singleplayer');
  if (!btn) return;
  const show = playBotActive() &&
    activeView === 'play' &&
    !currentState?.replay &&
    !currentState?.is_terminal;
  const player = playMode.humanPlayer === 1 ? 1 : 0;
  const actions = document.getElementById(`player-${player}-actions`);
  if (actions && btn.parentElement !== actions) {
    actions.appendChild(btn);
  }
  btn.classList.toggle('hidden', !show);
  btn.disabled = !show || playMode.botThinking;
}

function resetMultiplayerState(statusText = '') {
  clearSavedGameOverReplayLink();
  hideMultiplayerDisconnectModal();
  playMode.active = false;
  playMode.mode = 'multiplayer';
  playMode.viewerRole = 'player';
  clearPlayBotThinking();
  playMode.pendingBotMoveAfterSearch = false;
  playMode.singleplayerResigned = false;
  playMode.forcedMoveKey = null;
  playMode.autoRollEndKey = null;
  playMode.pendingHumanMove = false;
  playMode.multiplayerRoom = null;
  playMode.multiplayerStatus = statusText;
  playMode.rejoiningRoom = false;
  pendingReconnectRoomCode = '';
  shownMultiplayerReplayShareSlug = '';
  resetMultiplayerChat('');
  updateMultiplayerRoomUi();
  updateMultiplayerResignButton();
  if (statusText) setMultiplayerSetupStatus(statusText);
  updateMultiplayerTurnAttention();
}

function returnToMultiplayerLobbyAfterDisconnect() {
  const code = normalizeRoomCode(pendingReconnectRoomCode || playMode.multiplayerRoom?.code);
  if (code) writeLastMultiplayerRoomCode(code);
  pendingReconnectRoomCode = '';
  playMode.rejoiningRoom = false;
  hideMultiplayerDisconnectModal();
  resetMultiplayerState('');
  showPlaySetupView();
  if (session.connected) requestMultiplayerLobby();
}

function openCreateMultiplayerRoom() {
  if (showUsernameRequired('Multiplayer')) return;
  openCreateMultiplayerRoomModal();
}

function createMultiplayerRoom() {
  if (showUsernameRequired('Multiplayer')) return;
  const code = ensureCreateRoomCode();
  setPlayModeChoice('multiplayer');
  hideCreateMultiplayerRoomModal();
  resetMultiplayerState(`Creating room ${code}...`);
  const preferredPlayer = selectedMultiplayerHumanPlayer === 0 || selectedMultiplayerHumanPlayer === 1
    ? selectedMultiplayerHumanPlayer
    : null;
  session.send({
    type: 'CreateMultiplayerRoom',
    preferred_player: preferredPlayer,
    code,
    is_public: createRoomSettings.isPublic,
    time_minutes: createRoomSettings.timeMinutes,
    increment_seconds: createRoomHasClock() ? createRoomSettings.incrementSeconds : null,
  });
}

function joinMultiplayerRoom(code = null, statusText = null) {
  if (showUsernameRequired('Multiplayer')) return;
  const input = document.getElementById('multiplayer-room-code-input');
  const roomCode = normalizeRoomCode(code ?? input?.value);
  setPlayModeChoice('multiplayer');
  if (!roomCode) {
    setMultiplayerSetupStatus('Enter an invite code.');
    return;
  }
  if (input) input.value = roomCode;
  resetMultiplayerState(statusText || `Joining ${roomCode}...`);
  session.send({ type: 'JoinMultiplayerRoom', code: roomCode });
}

function reconnectMultiplayerRoom() {
  if (showUsernameRequired('Multiplayer')) return;
  const code = reconnectRoomCode();
  setPlayModeChoice('multiplayer');
  if (!code) {
    setMultiplayerSetupStatus('No room to reconnect.');
    updateMultiplayerReconnectUi();
    return;
  }
  const input = document.getElementById('multiplayer-room-code-input');
  if (input) input.value = code;
  joinMultiplayerRoom(code);
}

function rememberMultiplayerRoomForPlayTabReconnect() {
  const code = normalizeRoomCode(playMode.multiplayerRoom?.code);
  if (!code) return;
  pendingPlayTabReconnectRoomCode = code;
  writeLastMultiplayerRoomCode(code);
}

function reconnectMultiplayerRoomFromPlayTab() {
  const code = normalizeRoomCode(pendingPlayTabReconnectRoomCode);
  if (!code) return false;
  pendingPlayTabReconnectRoomCode = '';
  setPlayModeChoice('multiplayer');
  const input = document.getElementById('multiplayer-room-code-input');
  if (input) input.value = code;
  showPlaySetupView();
  joinMultiplayerRoom(code, `Reconnecting ${code}...`);
  return true;
}

function leaveMultiplayerRoom(showSetup = true, preserveReconnect = false) {
  const code = normalizeRoomCode(playMode.multiplayerRoom?.code);
  if (preserveReconnect && code) {
    writeLastMultiplayerRoomCode(code);
  } else if (!preserveReconnect) {
    writeLastMultiplayerRoomCode('');
  }
  if (playMode.mode === 'multiplayer' && code) {
    session.send({ type: 'LeaveMultiplayerRoom' });
  }
  setRoomCodeInUrl('');
  resetMultiplayerState('');
  if (showSetup) showPlaySetupView();
}

function addOpponentClockTime() {
  if (!playMultiplayerActive() || !playMode.multiplayerRoom) return;
  if (multiplayerSpectatorActive()) return;
  if (!multiplayerRoomHasClock()) return;
  session.send({ type: 'AddMultiplayerOpponentTime' });
}

function resignMultiplayerGame() {
  if (!playMultiplayerActive() || multiplayerWinner() != null) return;
  if (multiplayerSpectatorActive()) return;
  if (!multiplayerRoomFull()) return;
  if (!window.confirm('Resign this multiplayer game?')) return;
  session.send({ type: 'ResignMultiplayerGame' });
}

function resignSingleplayerGame() {
  if (!playBotActive() || activeView !== 'play' || currentState?.is_terminal || currentState?.replay) return;
  if (playMode.botThinking) return;
  if (!window.confirm('Resign this singleplayer game?')) return;
  controls.stopAutoplay();
  controls._disableAutoSearch();
  controls.pauseBeforeCommand();
  playMode.pendingBotMoveAfterSearch = false;
  playMode.singleplayerResigned = true;
  playMode.pendingHumanMove = false;
  session.send({ type: 'ResignGame' });
}

async function copyMultiplayerInviteLink() {
  const code = normalizeRoomCode(playMode.multiplayerRoom?.code);
  const input = document.getElementById('multiplayer-invite-link');
  const copyBtn = document.getElementById('btn-copy-multiplayer-invite');
  const link = code ? roomInviteUrl(code) : input?.value;
  if (!link) return;
  try {
    await navigator.clipboard.writeText(link);
    multiplayerInviteCopiedCode = code;
    if (copyBtn) copyBtn.textContent = 'Copied';
    setMultiplayerSetupStatus(`Copied invite link for ${code}.`);
  } catch (_error) {
    input?.focus();
    input?.select();
    setMultiplayerSetupStatus('Invite link selected.');
  }
}

function maybeAutoJoinRoomFromUrl() {
  if (!pendingAutoJoinRoomCode || autoJoinRoomAttempted) return;
  autoJoinRoomAttempted = true;
  setPlayModeChoice('multiplayer');
  const input = document.getElementById('multiplayer-room-code-input');
  if (input) input.value = pendingAutoJoinRoomCode;
  joinMultiplayerRoom(pendingAutoJoinRoomCode);
}

function sharedReplayUrl(slug) {
  const url = new URL(window.location.origin);
  url.searchParams.set('replay', normalizeReplaySlug(slug));
  return url.toString();
}

function hideBottomResultBanner() {
  const banner = document.getElementById('result-banner');
  if (!banner) return;
  banner.classList.add('hidden');
  banner.style.background = '';
  banner.textContent = '';
}

function hideGameOverModal() {
  const modal = document.getElementById('game-over-modal');
  if (!modal) return;
  modal.classList.add('hidden');
  modal.classList.remove('flex');
}

function clearSavedGameOverReplayLink() {
  savedGameOverReplayLink = '';
  copiedGameOverReplayLink = '';
  updateGameOverReplayLink();
}

function currentGameOverReplayLink() {
  const multiplayerSlug = normalizeReplaySlug(playMode.multiplayerRoom?.replay_share_slug);
  if (playMultiplayerActive() && multiplayerSlug) return sharedReplayUrl(multiplayerSlug);
  return savedGameOverReplayLink;
}

function gameOverWinnerText(msg = currentState) {
  if (playMultiplayerActive() && multiplayerWinner() != null) return multiplayerResultText();
  if (msg?.is_terminal && msg.result) return formatResultBanner(msg.result);
  return 'Game over';
}

function shouldShowGameOverModal(msg = currentState) {
  const boardTab = activeView === 'play' || activeView === 'analysis';
  if (!boardTab || msg?.replay) return false;
  if (playMultiplayerActive() && multiplayerWinner() != null) return true;
  return !!(msg?.is_terminal && msg.result);
}

function updateGameOverReplayLink() {
  const input = document.getElementById('game-over-replay-link');
  const copyBtn = document.getElementById('btn-copy-game-over-replay');
  if (!input || !copyBtn) return;
  const link = currentGameOverReplayLink();
  input.value = link || '';
  input.placeholder = link ? '' : 'Saving replay link...';
  copyBtn.disabled = !link;
  copyBtn.textContent = link && copiedGameOverReplayLink === link ? 'Copied' : 'Copy';
}

function updateGameOverModal(msg = currentState) {
  const modal = document.getElementById('game-over-modal');
  const winner = document.getElementById('game-over-winner');
  const primary = document.getElementById('btn-game-over-primary');
  const actionRow = document.getElementById('game-over-action-row');
  if (!modal || !winner || !primary) return;

  if (!shouldShowGameOverModal(msg)) {
    hideGameOverModal();
    return;
  }

  winner.textContent = gameOverWinnerText(msg);
  const action = activeView === 'play' ? 'lobby' : 'new-game';
  primary.dataset.action = action;
  if (actionRow) actionRow.dataset.action = action;
  primary.textContent = action === 'lobby' ? 'Lobby' : 'New Game';
  updateGameOverReplayLink();
  modal.classList.remove('hidden');
  modal.classList.add('flex');
}

async function copyGameOverReplayLink() {
  const input = document.getElementById('game-over-replay-link');
  const link = input?.value || currentGameOverReplayLink();
  if (!link) return;
  try {
    await navigator.clipboard.writeText(link);
    copiedGameOverReplayLink = link;
    updateGameOverReplayLink();
  } catch (_error) {
    input?.focus();
    input?.select();
  }
}

function returnToPlayLobbyFromGameOver() {
  if (!playMultiplayerActive() && currentState?.is_terminal) {
    session.send({ type: 'SaveReplay' });
  }
  hideGameOverModal();
  clearSavedGameOverReplayLink();
  controls.stopAutoplay();
  controls.pauseBeforeCommand();
  controls.onNewGame?.();
}

function startNewGameFromGameOver() {
  hideGameOverModal();
  clearSavedGameOverReplayLink();
  controls.stopAutoplay();
  controls.pauseBeforeCommand();
  const shouldStartNewGame = controls.onNewGame?.() !== false;
  if (shouldStartNewGame) {
    session.send({ type: 'NewGame', seed: null });
  }
}

function handleGameOverPrimaryAction() {
  const action = document.getElementById('btn-game-over-primary')?.dataset.action;
  if (action === 'lobby') {
    returnToPlayLobbyFromGameOver();
  } else {
    startNewGameFromGameOver();
  }
}

function sessionReadyForSharedReplay() {
  return !!(
    session.connected &&
    session.authenticated &&
    session.ws &&
    session.ws.readyState === WebSocket.OPEN
  );
}

function maybeLoadSharedReplayFromUrl() {
  if (guestMultiplayerMode()) return false;
  const slug = normalizeReplaySlug(
    pendingSharedReplaySlug ||
    window.hexfishGuestReplaySlug ||
    currentSharedReplaySlugFromUrl() ||
    readPendingSharedReplaySlug()
  );
  if (!slug) return false;
  pendingSharedReplaySlug = slug;
  activeReplayShareSlug = slug;
  writePendingSharedReplaySlug(slug);
  controls.stopAutoplay();
  controls._disableAutoSearch();
  controls.pauseBeforeCommand();
  if (!loadingSharedReplaySlug) replayState = null;
  controls.showReplayLoading(slug, 0);
  setReplayStatus('Loading shared replay...');
  showReplayBoardView();
  setReplayBoardStatus('Loading shared replay...');
  if (!sessionReadyForSharedReplay()) return true;
  if (loadingSharedReplaySlug === slug) return true;
  loadingSharedReplaySlug = slug;
  session.send({ type: 'LoadSharedReplay', slug });
  return true;
}

async function copyReplayShareLink(entry) {
  const slug = normalizeReplaySlug(entry?.share_slug);
  if (!slug) return;
  const link = sharedReplayUrl(slug);
  try {
    await navigator.clipboard.writeText(link);
    showReplayShareModal(link, true);
  } catch (_error) {
    showReplayShareModal(link, false);
  }
}

async function copyActiveReplayShareLink() {
  const slug = currentReplayShareSlug();
  if (!slug) return;
  await copyReplayShareLink({ share_slug: slug });
}

function showReplayShareModal(link, copied) {
  const modal = document.getElementById('replay-share-modal');
  const title = document.getElementById('replay-share-modal-title');
  const input = document.getElementById('replay-share-link');
  const copyBtn = document.getElementById('btn-copy-replay-share-link');
  if (!modal || !title || !input || !copyBtn) return;

  title.textContent = copied ? 'Replay link copied' : 'Copy replay link';
  copyBtn.textContent = copied ? 'Copy Again' : 'Copy';
  input.value = link || '';
  modal.classList.remove('hidden');
  modal.classList.add('flex');
  if (!copied) {
    input.focus();
    input.select();
  } else {
    copyBtn.focus();
  }
}

function maybeShowMultiplayerReplayShareModal(room) {
  const slug = normalizeReplaySlug(room?.replay_share_slug);
  if (!slug || shownMultiplayerReplayShareSlug === slug) return;
  shownMultiplayerReplayShareSlug = slug;
  updateGameOverModal(currentState);
}

function hideReplayShareModal() {
  const modal = document.getElementById('replay-share-modal');
  if (!modal) return;
  modal.classList.add('hidden');
  modal.classList.remove('flex');
}

function hideMultiplayerAnalysisGuard() {
  const modal = document.getElementById('multiplayer-analysis-guard-modal');
  if (!modal) return;
  modal.classList.add('hidden');
  modal.classList.remove('flex');
}

function showMultiplayerDisconnectModal(code, statusText = 'Reconnecting...') {
  clearScheduledMultiplayerDisconnectModal();
  const modal = document.getElementById('multiplayer-disconnect-modal');
  const copy = document.getElementById('multiplayer-disconnect-copy');
  const status = document.getElementById('multiplayer-disconnect-status');
  if (!modal) return;
  const roomCode = normalizeRoomCode(code);
  if (copy) {
    copy.textContent = roomCode
      ? `Your connection to room ${roomCode} was interrupted.`
      : 'Your multiplayer connection was interrupted.';
  }
  if (status) status.textContent = statusText || 'Reconnecting...';
  modal.classList.remove('hidden');
  modal.classList.add('flex');
  document.getElementById('btn-multiplayer-disconnect-lobby')?.focus();
}

function clearScheduledMultiplayerDisconnectModal() {
  if (multiplayerDisconnectModalTimer) {
    window.clearTimeout(multiplayerDisconnectModalTimer);
    multiplayerDisconnectModalTimer = null;
  }
  multiplayerDisconnectModalPending = null;
}

function isMultiplayerDisconnectModalVisible() {
  const modal = document.getElementById('multiplayer-disconnect-modal');
  return !!modal && !modal.classList.contains('hidden');
}

function scheduleMultiplayerDisconnectModal(code, statusText = 'Reconnecting...') {
  clearScheduledMultiplayerDisconnectModal();
  multiplayerDisconnectModalPending = {
    code: normalizeRoomCode(code),
    statusText: statusText || 'Reconnecting...',
  };
  multiplayerDisconnectModalTimer = window.setTimeout(() => {
    const pending = multiplayerDisconnectModalPending;
    multiplayerDisconnectModalTimer = null;
    multiplayerDisconnectModalPending = null;
    if (!pending) return;
    showMultiplayerDisconnectModal(pending.code, pending.statusText);
  }, MULTIPLAYER_DISCONNECT_MODAL_GRACE_MS);
}

function hideMultiplayerDisconnectModal() {
  clearScheduledMultiplayerDisconnectModal();
  const modal = document.getElementById('multiplayer-disconnect-modal');
  if (!modal) return;
  modal.classList.add('hidden');
  modal.classList.remove('flex');
}

function showMultiplayerAnalysisGuard() {
  hideGameOverModal();
  activeView = 'analysis-guard';
  setTabState('analysis');
  setEditorChrome(false);
  setGameHeaderLabelsVisible(false);
  document.getElementById('main-layout').classList.add('hidden');
  document.getElementById('play-setup-view').classList.add('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.add('hidden');
  controls.stopAutoplay();
  controls._disableAutoSearch();
  controls.setOptionsAvailable(false);
  updateMultiplayerInviteModal();
  updateMultiplayerTurnAttention();
  const modal = document.getElementById('multiplayer-analysis-guard-modal');
  if (!modal) return;
  modal.classList.remove('hidden');
  modal.classList.add('flex');
  document.getElementById('btn-return-to-multiplayer-game')?.focus();
}

async function copyReplayShareModalLink() {
  const input = document.getElementById('replay-share-link');
  const link = input?.value || '';
  if (!link) return;
  try {
    await navigator.clipboard.writeText(link);
    showReplayShareModal(link, true);
  } catch (_error) {
    input?.focus();
    input?.select();
  }
}

function handleMultiplayerRoomMessage(msg) {
  setPlayModeChoice('multiplayer');

  if (msg.status === 'left') {
    setRoomCodeInUrl('');
    resetMultiplayerState('');
    resetMultiplayerChat('');
    shownMultiplayerReplayShareSlug = '';
    if (activeView === 'play') showPlaySetupView();
    updateMultiplayerTurnAttention();
    return;
  }

  const viewerRole = msg.viewer_role === 'spectator' ? 'spectator' : 'player';
  const localPlayer = Number(msg.local_player);
  if (viewerRole === 'player' && localPlayer !== 0 && localPlayer !== 1) {
    resetMultiplayerState('Room joined, but no seat was assigned.');
    updateMultiplayerTurnAttention();
    return;
  }

  playMode.active = true;
  playMode.mode = 'multiplayer';
  playMode.viewerRole = viewerRole;
  playMode.humanPlayer = viewerRole === 'player' ? localPlayer : 0;
  clearPlayBotThinking();
  playMode.pendingBotMoveAfterSearch = false;
  playMode.singleplayerResigned = false;
  playMode.forcedMoveKey = null;
  playMode.autoRollEndKey = null;
  playMode.pendingHumanMove = false;
  playMode.rejoiningRoom = false;
  pendingReconnectRoomCode = '';
  pendingPlayTabReconnectRoomCode = '';
  hideMultiplayerDisconnectModal();
  const previousRoomCode = normalizeRoomCode(playMode.multiplayerRoom?.code);
  const nextRoomCode = normalizeRoomCode(msg.code);
  if (previousRoomCode && previousRoomCode !== nextRoomCode) {
    shownMultiplayerReplayShareSlug = '';
  }
  playMode.multiplayerRoom = {
    code: nextRoomCode,
    status: msg.status,
    viewer_role: viewerRole,
    players: Array.isArray(msg.players) ? msg.players : [],
    spectators: Array.isArray(msg.spectators) ? msg.spectators : [],
    time_minutes: msg.time_minutes,
    increment_seconds: msg.increment_seconds,
    winner: msg.winner,
    finish_reason: msg.finish_reason,
    replay_share_slug: msg.replay_share_slug,
  };
  if (previousRoomCode !== nextRoomCode) {
    resetMultiplayerChat(nextRoomCode);
  } else {
    updateMultiplayerChatUi();
  }
  multiplayerClockSyncedAtMs = Date.now();
  writeLastMultiplayerRoomCode(playMode.multiplayerRoom.code);
  setRoomCodeInUrl(playMode.multiplayerRoom.code);
  if (viewerRole === 'player') setPlaySide(localPlayer, 'multiplayer');
  scheduleMultiplayerRoomUiUpdate();
  updateMultiplayerResignButton();

  if (activeView === 'play-setup' || activeView === 'play') {
    showPlayView();
  }
  updateActionPanelStatus(currentState, false);
  updateMultiplayerResultBanner();
  if (multiplayerWinner() != null) {
    const actionList = document.getElementById('action-list');
    if (actionList) actionList.innerHTML = '';
    board.clearOverlays();
  }
  maybeShowMultiplayerReplayShareModal(playMode.multiplayerRoom);
  updateMultiplayerTurnAttention();
}

session.on('GameState', (msg) => {
  if (!msg.replay && msg.state?.board) {
    editorBaseBoard = msg.state.board;
    if (activeView !== 'editor') resetEditorPortsFromBaseBoard();
  }
  if (!msg.replay && playMultiplayerActive() && !msg.state?.private_view) {
    analysisState = msg;
    return;
  }
  if (msg.replay) {
    const fromSharedReplayLink = !!(
      pendingSharedReplaySlug ||
      loadingSharedReplaySlug ||
      currentSharedReplaySlugFromUrl() ||
      readPendingSharedReplaySlug()
    );
    replayState = msg;
    activeReplayShareSlug = currentReplayShareSlug();
    setReplayBoardStatus('');
    pendingSharedReplaySlug = '';
    loadingSharedReplaySlug = '';
    writePendingSharedReplaySlug('');
    if (activeView === 'replay-board') {
      renderGameState(msg);
    } else if (fromSharedReplayLink) {
      showReplayBoardView();
    }
    updateReplayShareButton();
  } else {
    analysisState = msg;
    maybePlayMoveSoundForGameState(msg);
    if (pendingSharedReplaySlug && !loadingSharedReplaySlug) {
      maybeLoadSharedReplayFromUrl();
      return;
    }
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
      const holdSingleplayerAutomation = playMode.mode !== 'multiplayer' && playMode.singleplayerNeedsRecovery;
      renderGameState(msg, {
        serverFresh: true,
        holdPlayAutomation: holdSingleplayerAutomation,
      });
      if (holdSingleplayerAutomation) return;
      runPlayAutomation(msg);
      return;
    }
    if (activeView === 'analysis') {
      renderGameState(msg, { serverFresh: true });
      runPendingNewGameSearch(msg);
    }
  }
});

session.on('MultiplayerRoom', handleMultiplayerRoomMessage);
session.on('MultiplayerChat', handleMultiplayerChatMessage);

function runPendingNewGameSearch(msg) {
  if (!pendingNewGameSearch || msg.replay) return;
  if (msg.is_terminal || msg.is_chance || !Array.isArray(msg.legal_actions) || msg.legal_actions.length === 0) {
    pendingNewGameSearch = false;
    return;
  }
  pendingNewGameSearch = false;
  controls.runSearch();
}

function renderGameState(msg, options = {}) {
  currentState = msg;
  if (playViewActive()) {
    playMode.pendingHumanMove = false;
    if (options.serverFresh && playMode.mode !== 'multiplayer' && !msg.replay) {
      clearSingleplayerRecovery('fresh-game-state');
    }
    reconcilePlayBotThinking(msg);
  }
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
    board.updateFrame(state.frame, currentBoard, deriveLastBoardMoveHighlights(msg));
  }

  // Phase / turn
  document.getElementById('phase-label').textContent = msg.phase;
  document.getElementById('turn-label').textContent = state.turn != null ? `Turn ${state.turn}` : '';

  // Player panels
  updatePlayerPanel(0, state);
  updatePlayerPanel(1, state);
  updateMultiplayerTimeoutWinnerBadge();
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
  const showPlayBotThinking = playBotTurn && !(
    options.holdPlayAutomation ||
    (playMode.mode !== 'multiplayer' && playMode.singleplayerNeedsRecovery)
  );
  const playMultiplayerBlocked = playMultiplayerActive() && (
    multiplayerSpectatorActive() ||
    multiplayerTimeoutWinner() != null ||
    !multiplayerRoomFull() ||
    msg.current_player !== playMode.humanPlayer
  );
  updateActionPanelStatus(msg, showPlayBotThinking);
  const canUseLegalActions = !msg.replay && !msg.is_terminal && !msg.is_chance && !playBotTurn && !playMultiplayerBlocked;
  updateBoardResourceLegend(msg, state, canUseLegalActions);
  updateBoardPieceCounts(msg, state, canUseLegalActions);

  if (canUseLegalActions) {
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
    const displayEntries = buildActionLogDisplayEntries(msg, cursors, activeLogIndex);
    for (const entry of displayEntries) {
      if (entry.type === 'discard-group') {
        renderDiscardLogGroup(logView, msg, entry);
      } else {
        renderActionLogEntry(logView, msg, entry);
      }
    }
    if (wasAtBottom) {
      logView.scrollTop = logView.scrollHeight;
    } else {
      logView.scrollTop = previousScrollTop;
    }
  }

  updateRollBadge(msg, state, viewKey);
  syncRollBadgeActionState(msg);
  syncEndTurnBadgeActionState(msg);

  // Undo/Redo button states
  document.getElementById('btn-undo').disabled = playBotTurn || !msg.can_undo;
  document.getElementById('btn-redo').disabled = playBotTurn || !msg.can_redo;

  // Game over modal
  hideBottomResultBanner();
  const timeoutWinner = multiplayerTimeoutWinner();
  if (timeoutWinner != null) {
    controls.onGameOver();
  } else if (msg.is_terminal && msg.result) {
    controls.onGameOver();
  }
  updateGameOverModal(msg);

  controls.onStateUpdate(msg);
  updateViewChrome(msg);
  updateMultiplayerTurnAttention();
  scheduleBoardChromePlacement();
}

function updateActionPanelStatus(msg, playBotTurn) {
  const title = document.getElementById('action-panel-title');
  const status = document.getElementById('play-status');
  const autoRollEndControl = document.getElementById('auto-roll-end-control');
  if (!title || !status) return;
  autoRollEndControl?.classList.toggle('hidden', !playViewActive());

  if (activeView === 'replay-board') {
    autoRollEndControl?.classList.add('hidden');
    title.textContent = 'Replay';
    status.textContent = replayBoardStatusText;
    status.title = '';
    status.classList.toggle('hidden', !replayBoardStatusText);
    return;
  }

  if (!playViewActive()) {
    autoRollEndControl?.classList.add('hidden');
    title.textContent = 'Legal Moves';
    status.textContent = '';
    status.title = '';
    status.classList.add('hidden');
    return;
  }

  if (playMultiplayerActive()) {
    title.textContent = 'Play';
    status.title = '';
    const timeoutText = multiplayerTimeoutResultText();
    if (timeoutText) {
      status.textContent = timeoutText;
      status.classList.remove('hidden');
      return;
    }
    if (msg?.is_terminal) {
      status.textContent = '';
      status.classList.add('hidden');
      return;
    }
    if (playMode.rejoiningRoom) {
      status.textContent = 'Reconnecting room';
    } else if (multiplayerSpectatorActive()) {
      status.textContent = 'Spectating';
    } else if (!multiplayerRoomFull()) {
      status.textContent = 'Waiting for opponent';
    } else if (!multiplayerOpponentConnected() && msg?.current_player === playMode.humanPlayer) {
      status.textContent = 'Your turn - opponent disconnected';
    } else if (!multiplayerOpponentConnected()) {
      status.textContent = 'Opponent disconnected';
    } else if (msg?.current_player === playMode.humanPlayer) {
      status.textContent = 'Your turn';
    } else {
      status.textContent = 'Waiting for opponent';
    }
    status.classList.toggle('hidden', !status.textContent);
    return;
  }

  if (playBotTurn) {
    title.textContent = 'Play';
    status.textContent = `${playBotName()} thinking`;
    status.title = playDifficultyDetails();
    status.classList.remove('hidden');
    return;
  }

  title.textContent = 'Play';
  status.textContent = '';
  status.title = '';
  status.classList.add('hidden');
}

function setReplayBoardStatus(text) {
  replayBoardStatusText = text || '';
  if (activeView === 'replay-board') {
    updateActionPanelStatus(currentState, false);
  }
}

function showLegalActionPreview(action) {
  if (!currentBoard || !currentState) return;
  if (isPlayBotTurn()) return;
  if (playMultiplayerActive() && (
    multiplayerSpectatorActive() ||
    multiplayerTimeoutWinner() != null ||
    !multiplayerRoomFull() ||
    currentState.current_player !== playMode.humanPlayer
  )) return;
  board.showActionPreview(action, currentBoard, currentState.current_player);
}

function hasLegalAction(msg, action) {
  return Array.isArray(msg?.legal_actions) && msg.legal_actions.some(a => Number(a?.action) === action);
}

function legalActionCount(msg) {
  return Array.isArray(msg?.legal_actions) ? msg.legal_actions.length : 0;
}

function decodeCatanMaritimeAction(action) {
  const value = Number(action);
  if (!Number.isInteger(value) || value < CATAN_MARITIME_START || value >= CATAN_MARITIME_END) {
    return null;
  }
  const index = value - CATAN_MARITIME_START;
  const give = Math.floor(index / 4);
  const adjustedReceive = index % 4;
  const receive = adjustedReceive < give ? adjustedReceive : adjustedReceive + 1;
  if (give < 0 || give >= RESOURCE_NAMES.length || receive < 0 || receive >= RESOURCE_NAMES.length) {
    return null;
  }
  return { give, receive, action: value };
}

function legalMaritimeTradesByGive(msg, canUseLegalActions) {
  const trades = new Map();
  if (!canUseLegalActions || !Array.isArray(msg?.legal_actions)) return trades;
  for (const entry of msg.legal_actions) {
    const trade = decodeCatanMaritimeAction(entry?.action);
    if (!trade) continue;
    const options = trades.get(trade.give) || [];
    options.push({
      action: trade.action,
      receive: trade.receive,
      label: entry?.label || '',
    });
    trades.set(trade.give, options);
  }
  for (const options of trades.values()) {
    options.sort((a, b) => a.receive - b.receive);
  }
  return trades;
}

function endTurnBadgeCanEnd(msg = currentState) {
  if (!msg || msg.replay || msg.is_terminal || msg.is_chance) return false;
  if (!hasLegalAction(msg, CATAN_END_TURN_ACTION)) return false;
  if (isPlayBotTurn(msg)) return false;
  if (playMultiplayerActive()) {
    if (multiplayerSpectatorActive()) return false;
    if (multiplayerTimeoutWinner() != null) return false;
    if (!multiplayerRoomFull()) return false;
    if (msg.current_player !== playMode.humanPlayer) return false;
  }
  return true;
}

function syncEndTurnBadgeActionState(msg = currentState) {
  const badge = document.getElementById('end-turn-badge');
  if (!badge) return;
  const canEnd = endTurnBadgeCanEnd(msg);
  badge.classList.toggle('hidden', !canEnd);
  badge.classList.toggle('end-turn-forced', canEnd && legalActionCount(msg) === 1);
  badge.disabled = !canEnd;
  badge.title = canEnd ? 'End turn' : '';
  badge.setAttribute('aria-label', canEnd ? 'End turn' : 'End turn unavailable');
}

function handleEndTurnBadgeClick() {
  if (!endTurnBadgeCanEnd(currentState)) return;
  sendPlayAction(CATAN_END_TURN_ACTION);
}

function rollBadgeCanRoll(msg = currentState) {
  if (!msg || msg.replay || msg.is_terminal || msg.is_chance) return false;
  if (!hasLegalAction(msg, CATAN_ROLL_ACTION)) return false;
  if (isPlayBotTurn(msg)) return false;
  if (playMultiplayerActive()) {
    if (multiplayerSpectatorActive()) return false;
    if (multiplayerTimeoutWinner() != null) return false;
    if (!multiplayerRoomFull()) return false;
    if (msg.current_player !== playMode.humanPlayer) return false;
  }
  return true;
}

function syncRollBadgeActionState(msg = currentState) {
  const badge = document.getElementById('roll-badge');
  if (!badge) return;
  const canRoll = rollBadgeCanRoll(msg);
  if (canRoll) {
    if (badge.classList.contains('hidden') || !badge.textContent.trim()) {
      badge.textContent = 'Roll';
      badge.dataset.rollPlaceholder = 'true';
      badge.style.background = playerColor(msg.current_player);
      badge.classList.remove('hidden');
    }
    badge.disabled = false;
    badge.classList.add('roll-badge-action');
    badge.title = 'Roll dice';
    badge.setAttribute('aria-label', 'Roll dice');
    return;
  }

  badge.disabled = true;
  badge.classList.remove('roll-badge-action');
  badge.title = '';
  if (badge.dataset.rollPlaceholder === 'true') {
    delete badge.dataset.rollPlaceholder;
    badge.textContent = '';
    badge.classList.add('hidden');
  }
}

function handleRollBadgeClick() {
  if (!rollBadgeCanRoll(currentState)) return;
  sendPlayAction(CATAN_ROLL_ACTION);
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

    const alwaysShowRollBadge = playMultiplayerActive();
    if (total === 7) {
      if (alwaysShowRollBadge) {
        showRollBadge(total, roller);
      } else {
        hideRollBadge();
      }
    } else if (alwaysShowRollBadge) {
      showRollBadge(total, roller);
      if (rollLogHasExplicitGain(newEntries[i]) || handGained) {
        showResourceProductionAnimations(total, previousFrameHands[viewKey], currentHands, state);
      }
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
  delete badge.dataset.rollPlaceholder;
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
  if (badge.dataset.rollPlaceholder === 'true') {
    delete badge.dataset.rollPlaceholder;
    badge.textContent = '';
  }
  badge.disabled = true;
  badge.classList.remove('roll-badge-action');
  badge.classList.add('hidden');
}

function hideEndTurnBadge() {
  const badge = document.getElementById('end-turn-badge');
  if (!badge) return;
  badge.disabled = true;
  badge.classList.remove('end-turn-forced');
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
  resumePlayBotAutomationAfterSearchSettles();
});

session.on('ReplayList', (msg) => {
  renderReplayList(msg.entries || []);
});

session.on('ReplaySaved', (msg) => {
  const slug = normalizeReplaySlug(msg.entry?.share_slug);
  savedGameOverReplayLink = slug ? sharedReplayUrl(slug) : '';
  copiedGameOverReplayLink = '';
  updateGameOverModal(currentState);
});

session.on('Profile', (msg) => {
  if (window.hexfishAuthSignedIn) {
    syncProfileUsernameFromClerk();
  } else {
    setProfileUsername(msg.username || '');
  }
  maybePromptForProfileUsername();
});

session.on('Subtree', (msg) => {
  mctsPanel.showSubtree(msg.tree);
});

session.on('SearchProgress', (msg) => {
  controls.onSearchProgress();
  if (playMode.botThinking) playMode.lastBotProgressAt = Date.now();
  if (controls.isPausePending()) return;
  mctsPanel.updateSnapshot(msg.snapshot, msg.action_labels, currentState?.current_player ?? 0);
  mctsPanel.showProgress(msg.snapshot, msg.budget, msg.sims_total, msg.cpu_load);
  updateSearchHighlights(msg.snapshot, msg.action_labels);
});

session.on('MultiplayerAnalysis', (msg) => {
  if (!playMultiplayerActive()) return;
  mctsPanel.updateAnalysisBar(msg.root_wdl);
});

session.on('MultiplayerLobby', (msg) => {
  multiplayerLobbyRooms = Array.isArray(msg.rooms) ? msg.rooms : [];
  scheduleMultiplayerLobbyRender();
});

session.on('BotAction', (msg) => {
  clearPlayBotThinking();
  playMode.pendingBotMoveAfterSearch = false;
  playMode.forcedMoveKey = null;
  playMode.autoRollEndKey = null;
  playMode.pendingHumanMove = false;
  if (msg.snapshot) {
    mctsPanel.updateSnapshot(msg.snapshot, msg.action_labels || [], currentState?.current_player ?? 0);
    updateSearchHighlights(msg.snapshot, msg.action_labels || []);
  }
  controls.onBotDone();
});

session.on('Error', (msg) => {
  if (loadingSharedReplaySlug) {
    loadingSharedReplaySlug = '';
    if (/shared replay|replay share/i.test(String(msg.message || ''))) {
      pendingSharedReplaySlug = '';
      activeReplayShareSlug = '';
      writePendingSharedReplaySlug('');
      updateReplayShareButton();
    }
  }
  if (pendingEditorStart) {
    pendingEditorStart = false;
    setEditorStatus(msg.message);
  }
  if (pendingReconnectRoomCode) {
    const code = pendingReconnectRoomCode;
    pendingReconnectRoomCode = '';
    playMode.rejoiningRoom = false;
    if (/room not found/i.test(String(msg.message || ''))) {
      pendingPlayTabReconnectRoomCode = '';
      writeLastMultiplayerRoomCode('');
      resetMultiplayerState('Room not found. Create a new room or ask for a fresh invite.');
      showPlaySetupView();
      return;
    }
    showMultiplayerDisconnectModal(
      code,
      msg.message || 'Could not reconnect. Return to the lobby and try again.'
    );
  }
  if (
    playMode.mode === 'multiplayer' &&
    !playMode.active &&
    /room not found/i.test(String(msg.message || ''))
  ) {
    pendingReconnectRoomCode = '';
    pendingPlayTabReconnectRoomCode = '';
    writeLastMultiplayerRoomCode('');
    resetMultiplayerState('Room not found. Create a new room or ask for a fresh invite.');
    showPlaySetupView();
    return;
  }
  if (
    playMode.mode === 'multiplayer' &&
    playMode.multiplayerRoom?.code &&
    /join a multiplayer room before refreshing it/i.test(String(msg.message || ''))
  ) {
    const code = normalizeRoomCode(playMode.multiplayerRoom.code || lastMultiplayerRoomCode || readLastMultiplayerRoomCode());
    if (code && session.connected) {
      pendingReconnectRoomCode = code;
      playMode.rejoiningRoom = true;
      writeLastMultiplayerRoomCode(code);
      setMultiplayerSetupStatus(`Rejoining ${code}...`);
      scheduleMultiplayerDisconnectModal(code, 'Room connection lost. Rejoining...');
      session.send({ type: 'JoinMultiplayerRoom', code });
      return;
    }
  }
  if (pendingProfileSave) {
    pendingProfileSave = false;
    const save = document.getElementById('btn-save-profile');
    if (save) save.disabled = false;
    setProfileStatus(msg.message, true);
  }
  const retryPlayBotMove = playMode.pendingBotMoveAfterSearch && playSearchErrorCanRetry(msg.message);
  controls.onSearchError();
  clearPlayBotThinking();
  playMode.pendingBotMoveAfterSearch = false;
  playMode.forcedMoveKey = null;
  playMode.autoRollEndKey = null;
  playMode.pendingHumanMove = false;
  updateActionPanelStatus(currentState, isPlayBotTurn(currentState));
  updateMultiplayerTurnAttention();
  if (activeView === 'replay-board') setReplayBoardStatus(msg.message);
  if (selectedPlayMode === 'multiplayer' || playMode.mode === 'multiplayer') {
    setMultiplayerSetupStatus(msg.message);
  }
  setReplayStatus(msg.message);
  console.error('Server error:', msg.message);
  if (retryPlayBotMove) {
    playMode.pendingBotMoveAfterSearch = true;
    resumePlayBotAutomationAfterSearchSettles();
  }
});

session.on('Disconnected', (msg) => {
  suppressNextLiveMoveSound = true;
  loadingSharedReplaySlug = '';
  const singleplayerWasActive = markSingleplayerSocketRecovery('disconnected', {
    close_code: msg?.code,
    close_reason: msg?.reason || '',
    was_clean: msg?.was_clean,
    last_message_age_ms: msg?.last_message_age_ms,
    will_reconnect: msg?.will_reconnect,
  });
  if (!singleplayerWasActive) {
    controls.onSearchError();
    clearPlayBotThinking();
    playMode.pendingBotMoveAfterSearch = false;
    playMode.forcedMoveKey = null;
    playMode.autoRollEndKey = null;
    playMode.pendingHumanMove = false;
  }
  updateMultiplayerTurnAttention();
  if (playMode.mode === 'multiplayer' && playMode.multiplayerRoom?.code && appStarted) {
    const code = normalizeRoomCode(playMode.multiplayerRoom.code);
    pendingReconnectRoomCode = code;
    writeLastMultiplayerRoomCode(code);
    playMode.rejoiningRoom = true;
    setMultiplayerSetupStatus(`Reconnecting ${code}...`);
    if (msg?.will_reconnect) {
      scheduleMultiplayerDisconnectModal(code, 'Reconnecting...');
    } else {
      clearScheduledMultiplayerDisconnectModal();
    }
    updateActionPanelStatus(currentState, false);
  }
});

session.on('Connected', () => {
  session.send({ type: 'GetProfile' });
  if (pendingReconnectRoomCode) {
    clearScheduledMultiplayerDisconnectModal();
    if (isMultiplayerDisconnectModalVisible()) {
      showMultiplayerDisconnectModal(pendingReconnectRoomCode, 'Connection restored. Rejoining room...');
    }
    session.send({ type: 'JoinMultiplayerRoom', code: pendingReconnectRoomCode });
    return;
  }
  requestActiveMultiplayerRoomSync();
  if (selectedPlayMode === 'multiplayer' && activeView === 'play-setup') {
    requestMultiplayerLobby();
  }
  if (!guestMultiplayerMode() && (pendingSharedReplaySlug || currentSharedReplaySlugFromUrl() || readPendingSharedReplaySlug())) {
    maybeLoadSharedReplayFromUrl();
  }
  recoverSingleplayerConnection('connected');
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
  hideMultiplayerAnalysisGuard();
  if (!playMode.active && reconnectMultiplayerRoomFromPlayTab()) return;
  if (!playMode.active) {
    showPlaySetupView();
    return;
  }
  controls.stopAutoplay();
  controls._disableAutoSearch();
  if (!playMode.botThinking) controls.pauseBeforeCommand();
  if (playMode.active && playMode.mode !== 'multiplayer') {
    setServerSingleplayerHumanPlayer(playMode.humanPlayer);
  }
  activeView = 'play';
  setTabState('play');
  setEditorChrome(false);
  setGameHeaderLabelsVisible(true);
  document.getElementById('main-layout').classList.remove('hidden');
  document.getElementById('play-setup-view').classList.add('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.remove('hidden');
  controls.setOptionsAvailable(false);
  if (analysisState && (!playMultiplayerActive() || analysisState.state?.private_view)) {
    renderGameState(analysisState);
    runPlayAutomation(analysisState);
  } else {
    updateViewChrome(null);
  }
  updateMultiplayerInviteModal();
  updateMultiplayerClockUi();
  updateMultiplayerTurnAttention();
  scheduleBoardChromePlacement();
  maybePromptForProfileUsername();
}

function showPlaySetupView() {
  hideMultiplayerAnalysisGuard();
  hideGameOverModal();
  activeView = 'play-setup';
  setServerSingleplayerHumanPlayer(null);
  setTabState('play');
  controls.stopAutoplay();
  controls._disableAutoSearch();
  controls.setOptionsAvailable(false);
  setPlayModeChoice(selectedPlayMode);
  updatePlaySideButtons();
  setGameHeaderLabelsVisible(false);
  document.getElementById('main-layout').classList.add('hidden');
  document.getElementById('play-setup-view').classList.remove('hidden');
  document.getElementById('replay-view').classList.add('hidden');
  document.getElementById('controls').classList.add('hidden');
  updateMultiplayerInviteModal();
  updateMultiplayerClockUi();
  updateMultiplayerTurnAttention();
  maybePromptForProfileUsername();
}

function showAnalysisView() {
  if (showGuestFeatureBlocked('Analysis')) return;
  if (showUsernameRequired('Analysis')) return;
  if (multiplayerGameOpen()) {
    showMultiplayerAnalysisGuard();
    return;
  }
  hideMultiplayerAnalysisGuard();
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
  updateMultiplayerInviteModal();
  updateMultiplayerClockUi();
  updateMultiplayerTurnAttention();
  scheduleBoardChromePlacement();
}

function showReplayView() {
  hideGameOverModal();
  if (guestSharedReplayMode()) {
    if (replayState || currentState?.replay) {
      showReplayBoardView();
      return;
    }
    const loadingSharedReplay = maybeLoadSharedReplayFromUrl();
    if (loadingSharedReplay) {
      showReplayBoardView();
      return;
    }
    showGuestFeatureBlocked('Replays');
    return;
  }
  if (showGuestFeatureBlocked('Replays')) return;
  if (showUsernameRequired('Replays')) return;
  hideMultiplayerAnalysisGuard();
  if (multiplayerGameOpen()) {
    rememberMultiplayerRoomForPlayTabReconnect();
    leaveMultiplayerRoom(false, true);
  }
  activeView = 'replay-list';
  updateReplayShareButton();
  setServerSingleplayerHumanPlayer(null);
  setTabState('replay');
  controls.pauseBeforeCommand();
  setEditorChrome(false);
  setGameHeaderLabelsVisible(false);
  document.getElementById('main-layout').classList.add('hidden');
  document.getElementById('play-setup-view').classList.add('hidden');
  document.getElementById('replay-view').classList.remove('hidden');
  document.getElementById('controls').classList.add('hidden');
  controls.setOptionsAvailable(false);
  requestReplayList();
  updateMultiplayerInviteModal();
  updateMultiplayerClockUi();
  updateMultiplayerTurnAttention();
}

function showReplayBoardView() {
  hideMultiplayerAnalysisGuard();
  hideGameOverModal();
  activeView = 'replay-board';
  updateReplayShareButton();
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
  updateMultiplayerInviteModal();
  updateMultiplayerClockUi();
  updateMultiplayerTurnAttention();
  scheduleBoardChromePlacement();
}

function showEditorView() {
  if (showGuestFeatureBlocked('Editor')) return;
  if (showUsernameRequired('Editor')) return;
  hideMultiplayerAnalysisGuard();
  hideGameOverModal();
  if (multiplayerGameOpen()) {
    rememberMultiplayerRoomForPlayTabReconnect();
    leaveMultiplayerRoom(false, true);
  }
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
  updateMultiplayerInviteModal();
  updateMultiplayerClockUi();
  updateMultiplayerTurnAttention();
  scheduleBoardChromePlacement();
}

function setGameHeaderLabelsVisible(visible) {
  document.getElementById('action-panel-context')?.classList.toggle('hidden', !visible);
  document.getElementById('phase-label').classList.toggle('hidden', !visible);
  document.getElementById('turn-label').classList.toggle('hidden', !visible);
}

function updateNewGameButtonLabel() {
  const btn = document.getElementById('btn-new-game');
  if (!btn) return;
  btn.textContent = activeView === 'play' ? 'Lobby' : 'New Game';
}

function resetBoardPieceCounts() {
  const panel = document.getElementById('board-piece-counts');
  document.getElementById('piece-count-settlements').textContent = PIECE_LIMITS.settlements;
  document.getElementById('piece-count-cities').textContent = PIECE_LIMITS.cities;
  document.getElementById('piece-count-roads').textContent = PIECE_LIMITS.roads;
  if (!panel) return;
  panel.classList.remove('p1', 'p2');
  panel.setAttribute(
    'aria-label',
    `${PIECE_LIMITS.settlements} settlements, ${PIECE_LIMITS.cities} cities, ${PIECE_LIMITS.roads} roads remaining`
  );
  for (const item of panel.querySelectorAll('[data-piece-kind]')) {
    item.classList.remove('can-build');
  }
}

function setEditorChrome(enabled) {
  updateNewGameButtonLabel();
  if (enabled) resetBoardPieceCounts();
  if (enabled) hideRollBadge();
  if (enabled) hideEndTurnBadge();
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
  setGameHeaderLabelsVisible(!enabled);
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
  const guestReplayLocked = guestSharedReplayMode() && replay;
  document.getElementById('btn-replay-return')?.classList.toggle(
    'hidden',
    activeView !== 'replay-board' || guestSharedReplayMode()
  );
  document.getElementById('btn-new-game')?.classList.toggle('hidden', replay);
  setMctsMoveDetailsVisible(!inPlay && !guestReplayLocked);
  document.getElementById('guest-replay-analysis-control')?.classList.toggle('hidden', !guestReplayLocked);
  document.getElementById('guest-replay-analysis-panel')?.classList.toggle('hidden', !guestReplayLocked);
  document.getElementById('search-info')?.classList.toggle('hidden', guestReplayLocked);
  document.getElementById('analysis-bar-panel')?.classList.toggle('hidden', activeView === 'editor' || guestReplayLocked);
  document.getElementById('board-resource-legend')?.classList.toggle(
    'hidden',
    activeView === 'editor' || multiplayerSpectatorActive()
  );
  document.getElementById('board-piece-counts')?.classList.toggle('hidden', activeView === 'editor');
  document.getElementById('board-history-controls')?.classList.toggle('hidden', activeView === 'editor');
  document.getElementById('resource-animation-layer')?.classList.toggle('hidden', activeView === 'editor');

  document.getElementById('search-action-control')?.classList.toggle('hidden', inPlay || guestReplayLocked);
  document.getElementById('budget-control')?.classList.toggle('hidden', inPlay || guestReplayLocked);
  updateMultiplayerResignButton();
  updateSingleplayerResignButton();

  const botMove = document.getElementById('btn-bot-move');
  if (botMove) {
    if (inPlay || guestReplayLocked) {
      botMove.classList.add('hidden');
      botMove.disabled = true;
    } else if (activeView === 'analysis' && !replay) {
      botMove.classList.remove('hidden');
      botMove.disabled = false;
    }
  }
  for (const btn of document.querySelectorAll('.takeover-btn')) {
    btn.classList.add('hidden');
    btn.disabled = true;
  }

  controls.setOptionsAvailable(activeView === 'analysis' && !replay && !guestReplayLocked);
  updateReplayShareButton();
  if (inPlay || guestReplayLocked) board.clearSearchHighlights();
}

function startPlayGame() {
  if (showGuestSingleplayerDifficultyBlocked()) return;
  if (showUsernameRequired('Singleplayer')) return;
  hideGameOverModal();
  clearSavedGameOverReplayLink();
  setPlayModeChoice('bot');
  playMode.active = true;
  playMode.mode = 'bot';
  playMode.humanPlayer = resolvedSingleplayerHumanPlayer();
  clearPlayBotThinking();
  playMode.pendingBotMoveAfterSearch = false;
  playMode.singleplayerResigned = false;
  playMode.forcedMoveKey = null;
  playMode.autoRollEndKey = null;
  playMode.pendingHumanMove = false;
  playMode.multiplayerRoom = null;
  playMode.multiplayerStatus = '';
  playMode.rejoiningRoom = false;
  resetMultiplayerChat('');
  resetSingleplayerRecoveryState();
  pendingNewGameSearch = false;
  controls.stopAutoplay();
  controls._disableAutoSearch();
  controls.pauseBeforeCommand();
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
  reconcilePlayBotThinking(msg);
  if (playBotActive() && playMode.singleplayerNeedsRecovery) {
    updateActionPanelStatus(msg, false);
    recoverSingleplayerConnection('automation-waiting-for-recovery');
    return;
  }
  if (playBotActive() && !session.connected) {
    markSingleplayerNeedsRecovery('automation-disconnected');
    updateActionPanelStatus(msg, false);
    return;
  }
  if (maybeAutoRollEnd(msg)) return;
  if (playMultiplayerActive()) {
    clearPlayBotThinking();
    playMode.pendingBotMoveAfterSearch = false;
    playMode.forcedMoveKey = null;
    updateActionPanelStatus(msg, false);
    return;
  }
  if (!isPlayBotTurn(msg)) {
    if (playViewActive() && (msg?.is_terminal || msg?.current_player === playMode.humanPlayer)) {
      clearPlayBotThinking();
      playMode.pendingBotMoveAfterSearch = false;
      playMode.forcedMoveKey = null;
    }
    updateActionPanelStatus(msg, false);
    runPlayPonderSearch(msg);
    return;
  }
  updateActionPanelStatus(msg, true);
  if (Array.isArray(msg.legal_actions) && msg.legal_actions.length === 1) {
    if (controls.searchRunning) {
      playMode.pendingBotMoveAfterSearch = true;
      controls.pauseSearch();
      return;
    }
    const action = msg.legal_actions[0].action;
    const key = playForcedMoveKey(msg, action);
    if (playMode.forcedMoveKey === key) return;
    playMode.pendingBotMoveAfterSearch = false;
    playMode.forcedMoveKey = key;
    clearPlayBotThinking();
    session.send({ type: 'PlayAction', action });
    return;
  }
  playMode.forcedMoveKey = null;
  if (playMode.botThinking) return;
  if (controls.searchRunning) {
    playMode.pendingBotMoveAfterSearch = true;
    controls.pauseSearch();
    return;
  }
  playMode.pendingBotMoveAfterSearch = false;
  markPlayBotThinking(msg, 'bot-move');
  session.send({
    type: 'BotMove',
    budget: playDifficultyBudget(),
  });
  controls.onSearchStarted();
}

function playSearchErrorCanRetry(message) {
  if (!playBotActive() || activeView !== 'play' || !isPlayBotTurn(currentState)) return false;
  const text = String(message || '');
  return /no search is running/i.test(text) || /search is running; pause/i.test(text);
}

function resumePlayBotAutomationAfterSearchSettles() {
  if (!playBotActive() || activeView !== 'play') return;
  if (!playMode.pendingBotMoveAfterSearch) return;
  if (!isPlayBotTurn(currentState) || playMode.botThinking) return;
  window.setTimeout(() => {
    if (!playBotActive() || activeView !== 'play') return;
    if (!playMode.pendingBotMoveAfterSearch) return;
    if (!isPlayBotTurn(currentState) || playMode.botThinking) return;
    playMode.pendingBotMoveAfterSearch = false;
    runPlayAutomation(currentState);
  }, 0);
}

function runPlayPonderSearch(msg) {
  if (!playCanPonder(msg) || controls.searchRunning) return;
  controls.runSearchWithBudget(playDifficultyBudget(), 'analysis');
}

function requestReplayList() {
  if (showGuestFeatureBlocked('Replays')) return;
  if (showUsernameRequired('Replays')) return;
  setReplayStatus('Loading...');
  session.send({ type: 'ListReplays' });
}

function setReplayStatus(text) {
  const status = document.getElementById('replay-status');
  if (!status) return;
  status.textContent = text || '';
  status.classList.toggle('hidden', !text);
}

function replayOutcome(entry) {
  const result = String(entry?.result || 'incomplete').toLowerCase();
  if (result.startsWith('won')) return 'won';
  if (result.startsWith('lost')) return 'lost';
  if (result === 'draw') return 'draw';
  return 'incomplete';
}

function replayOutcomeLabel(entry) {
  const result = String(entry?.result || 'incomplete').toLowerCase();
  if (result === 'won_by_resignation') return 'Won by Resignation';
  if (result === 'lost_by_resignation') return 'Lost by Resignation';
  if (result === 'won_on_time') return 'Won on Time';
  if (result === 'lost_on_time') return 'Lost on Time';
  if (result === 'won') return 'Won';
  if (result === 'lost') return 'Lost';
  if (result === 'draw') return 'Draw';
  return 'Incomplete';
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

    const outcome = document.createElement('span');
    outcome.className = `replay-outcome ${replayOutcome(entry)}`;
    outcome.textContent = replayOutcomeLabel(entry);

    const meta = document.createElement('span');
    meta.className = 'flex shrink-0 items-center gap-1.5';
    meta.append(outcome, actions);

    load.appendChild(saved);
    load.appendChild(meta);
    load.addEventListener('click', () => {
      controls.stopAutoplay();
      controls._disableAutoSearch();
      controls.pauseBeforeCommand();
      replayState = null;
      activeReplayShareSlug = normalizeReplaySlug(entry.share_slug);
      updateReplayShareButton();
      controls.showReplayLoading(entry.id, entry.action_count);
      session.send({ type: 'LoadReplay', id: entry.id });
      showReplayBoardView();
      setReplayBoardStatus('Loading replay...');
    });

    const share = document.createElement('button');
    share.type = 'button';
    share.className = 'h-7 px-2 flex items-center justify-center text-[11px] text-gray-300 rounded hover:bg-bg cursor-pointer disabled:cursor-default disabled:opacity-40';
    share.textContent = 'Share';
    share.title = 'Copy replay link';
    share.setAttribute('aria-label', 'Copy replay link');
    share.disabled = !normalizeReplaySlug(entry.share_slug);
    share.addEventListener('click', (event) => {
      event.stopPropagation();
      copyReplayShareLink(entry);
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
    row.appendChild(share);
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
  if (showGuestFeatureBlocked('Editor')) return;
  if (showUsernameRequired('Editor')) return;
  const error = validateEditorDraft();
  if (error) {
    setEditorStatus(error);
    return;
  }
  pendingEditorStart = true;
  playMode.active = false;
  clearPlayBotThinking();
  playMode.pendingBotMoveAfterSearch = false;
  playMode.singleplayerResigned = false;
  playMode.forcedMoveKey = null;
  playMode.autoRollEndKey = null;
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
document.getElementById('btn-topbar-brand')?.addEventListener('click', handleTopbarBrandClick);
document.getElementById('btn-refresh-replays').addEventListener('click', requestReplayList);
document.getElementById('btn-replay-return')?.addEventListener('click', showReplayView);
document.getElementById('btn-start-play-game').addEventListener('click', startPlayGame);
document.getElementById('btn-profile')?.addEventListener('click', showProfileModal);
document.getElementById('btn-create-multiplayer-room')?.addEventListener('click', openCreateMultiplayerRoom);
document.getElementById('btn-join-multiplayer-room')?.addEventListener('click', () => joinMultiplayerRoom());
document.getElementById('btn-reconnect-multiplayer-room')?.addEventListener('click', reconnectMultiplayerRoom);
document.getElementById('btn-copy-multiplayer-invite')?.addEventListener('click', copyMultiplayerInviteLink);
document.getElementById('btn-refresh-multiplayer-lobby')?.addEventListener('click', requestMultiplayerLobby);
document.getElementById('btn-resign-multiplayer')?.addEventListener('click', resignMultiplayerGame);
document.getElementById('btn-resign-singleplayer')?.addEventListener('click', resignSingleplayerGame);
document.getElementById('multiplayer-chat-form')?.addEventListener('submit', sendMultiplayerChat);
document.getElementById('roll-badge')?.addEventListener('click', handleRollBadgeClick);
document.getElementById('end-turn-badge')?.addEventListener('click', handleEndTurnBadgeClick);
document.getElementById('btn-add-opponent-time-0')?.addEventListener('click', addOpponentClockTime);
document.getElementById('btn-add-opponent-time-1')?.addEventListener('click', addOpponentClockTime);
document.getElementById('btn-close-replay-share-modal')?.addEventListener('click', hideReplayShareModal);
document.getElementById('btn-copy-replay-share-link')?.addEventListener('click', copyReplayShareModalLink);
document.getElementById('btn-replay-share')?.addEventListener('click', copyActiveReplayShareLink);
document.getElementById('btn-copy-game-over-replay')?.addEventListener('click', copyGameOverReplayLink);
document.getElementById('btn-game-over-primary')?.addEventListener('click', handleGameOverPrimaryAction);
document.getElementById('btn-close-profile-modal')?.addEventListener('click', hideProfileModal);
document.getElementById('btn-cancel-profile')?.addEventListener('click', hideProfileModal);
document.getElementById('btn-open-clerk-profile')?.addEventListener('click', openClerkProfileForUsernameSetup);
document.getElementById('btn-save-profile')?.addEventListener('click', saveProfileUsername);
document.getElementById('btn-close-create-multiplayer-room-modal')?.addEventListener('click', hideCreateMultiplayerRoomModal);
document.getElementById('btn-cancel-create-multiplayer-room')?.addEventListener('click', hideCreateMultiplayerRoomModal);
document.getElementById('btn-confirm-create-multiplayer-room')?.addEventListener('click', createMultiplayerRoom);
document.getElementById('btn-return-to-multiplayer-game')?.addEventListener('click', showPlayView);
document.getElementById('btn-multiplayer-disconnect-lobby')?.addEventListener('click', returnToMultiplayerLobbyAfterDisconnect);
document.getElementById('btn-close-guest-signin-required')?.addEventListener('click', hideGuestSignInRequiredModal);
document.getElementById('btn-cancel-guest-signin-required')?.addEventListener('click', hideGuestSignInRequiredModal);
document.getElementById('btn-guest-signin-required-signin')?.addEventListener('click', signInFromGuestRequiredModal);
for (const btn of document.querySelectorAll('.guest-replay-analysis-signin')) {
  btn.addEventListener('click', signInToAnalyzeGuestReplay);
}
document.getElementById('btn-regenerate-create-room-code')?.addEventListener('click', () => {
  createRoomSettings.code = randomMultiplayerRoomCode();
  updateCreateRoomModalUi();
});
document.getElementById('create-room-time-slider')?.addEventListener('input', (event) => setCreateRoomTimeMinutes(event.target.value));
document.getElementById('create-room-time-input')?.addEventListener('input', (event) => setCreateRoomTimeMinutes(event.target.value));
document.getElementById('create-room-increment-slider')?.addEventListener('input', (event) => setCreateRoomIncrementSeconds(event.target.value));
document.getElementById('create-room-increment-input')?.addEventListener('input', (event) => setCreateRoomIncrementSeconds(event.target.value));
for (const btn of document.querySelectorAll('[data-time-minutes]')) {
  btn.addEventListener('click', () => setCreateRoomTimeMinutes(btn.dataset.timeMinutes));
}
for (const btn of document.querySelectorAll('[data-increment-seconds]')) {
  btn.addEventListener('click', () => setCreateRoomIncrementSeconds(btn.dataset.incrementSeconds));
}
for (const btn of document.querySelectorAll('[data-room-visibility]')) {
  btn.addEventListener('click', () => setCreateRoomVisibility(btn.dataset.roomVisibility));
}
document.getElementById('replay-share-modal')?.addEventListener('click', (event) => {
  if (event.target === event.currentTarget) hideReplayShareModal();
});
document.getElementById('profile-modal')?.addEventListener('click', (event) => {
  if (event.target === event.currentTarget) hideProfileModal();
});
document.getElementById('create-multiplayer-room-modal')?.addEventListener('click', (event) => {
  if (event.target === event.currentTarget) hideCreateMultiplayerRoomModal();
});
document.getElementById('guest-signin-required-modal')?.addEventListener('click', (event) => {
  if (event.target === event.currentTarget) hideGuestSignInRequiredModal();
});
window.addEventListener('focus', updateMultiplayerTurnAttention);
window.addEventListener('blur', updateMultiplayerTurnAttention);
document.addEventListener('visibilitychange', updateMultiplayerTurnAttention);
document.getElementById('multiplayer-room-code-input')?.addEventListener('input', (event) => {
  const input = event.target;
  const normalized = normalizeRoomCode(input.value);
  if (input.value !== normalized) input.value = normalized;
});
document.getElementById('multiplayer-room-code-input')?.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') joinMultiplayerRoom();
});
document.getElementById('profile-username-input')?.addEventListener('input', (event) => {
  const input = event.target;
  const normalized = normalizeProfileUsername(input.value);
  if (input.value !== normalized) input.value = normalized;
});
document.getElementById('profile-username-input')?.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') saveProfileUsername();
});
document.getElementById('btn-board-sound')?.addEventListener('click', () => {
  setMoveSoundEnabled(!moveSoundEnabled);
});
document.getElementById('auto-roll-end-toggle')?.addEventListener('change', (event) => {
  setAutoRollEndEnabled(event.target.checked);
});
document.getElementById('btn-start-edited-game').addEventListener('click', startEditedGame);
document.getElementById('btn-random-editor-board').addEventListener('click', randomizeEditorBoard);
for (const btn of document.querySelectorAll('.play-mode-btn')) {
  btn.addEventListener('click', () => setPlayModeChoice(btn.dataset.playMode));
}
for (const btn of document.querySelectorAll('.play-side-btn')) {
  btn.addEventListener('click', () => setPlaySide(btn.dataset.player, playSideScope(btn)));
}
initPlayDifficultySelect();
setPlayModeChoice(selectedPlayMode);
updatePlaySideButtons();
updateProfileUi();
updateMoveSoundToggle();
updateAutoRollEndToggle();
initEditorControls();
initCatanRulesPopover();
showPlaySetupView();

for (const eventName of ['pointerdown', 'keydown', 'touchstart']) {
  document.addEventListener(eventName, unlockMoveSounds, { capture: true, once: true });
}

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
  if (playMultiplayerActive() && (
    multiplayerTimeoutWinner() != null ||
    !multiplayerRoomFull() ||
    currentState?.current_player !== playMode.humanPlayer
  )) return;
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
  if (guestReplayAnalysisLocked()) {
    showGuestSignInRequiredModal('Analysis');
    return;
  }
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
  const resourceCount = Number.isFinite(Number(pf.hand_total))
    ? Number(pf.hand_total)
    : (pf.hand ? pf.hand.reduce((sum, count) => sum + count, 0) : 0);
  const cardCountEl = document.getElementById(`p${idx}-card-count`);
  const discardThreshold = Number.isFinite(Number(state.discard_threshold))
    ? Number(state.discard_threshold)
    : DEFAULT_DISCARD_THRESHOLD;
  const overDiscardLimit = resourceCount > discardThreshold;
  cardCountEl.textContent = `${resourceCount} ${resourceCount === 1 ? 'Card' : 'Cards'}`;
  cardCountEl.classList.toggle('discard-limit-warning', overDiscardLimit);
  cardCountEl.title = overDiscardLimit
    ? `Over discard limit (${discardThreshold})`
    : 'Total resource cards';

  // VP
  updateVictoryBadge(idx, pf);

  // Hand — always show all 5 resources as colored rectangles
  const handEl = document.getElementById(`p${idx}-hand`);
  handEl.innerHTML = '';
  const hideResourceBreakdown = playViewActive() && (
    multiplayerSpectatorActive() || idx !== playMode.humanPlayer
  );
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
  const hideDevBreakdown = playViewActive() && (
    multiplayerSpectatorActive() || idx !== playMode.humanPlayer
  );
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

function publicVictoryPoints(playerFrame) {
  const total = Number(playerFrame?.vp) || 0;
  const vpCards = Number(playerFrame?.dev_cards?.[DEV_VP_INDEX]) || 0;
  return Math.max(0, total - vpCards);
}

function formatVictoryPoints(playerIndex, publicVp, totalVp) {
  const revealFinalScore = !!currentState?.is_terminal || multiplayerWinner() != null;
  if (multiplayerSpectatorActive() && !revealFinalScore) {
    return String(publicVp);
  }
  if (playViewActive() && playerIndex !== playMode.humanPlayer && !revealFinalScore) {
    return String(publicVp);
  }
  return totalVp > publicVp ? `${publicVp}(${totalVp})` : String(totalVp);
}

function closeResourceTradeMenu() {
  if (openTradeGiveResource == null) return;
  openTradeGiveResource = null;
  updateBoardResourceLegend(currentState, currentState?.state, lastCanUseLegalActions);
}

function playerIndexForResourceLegend(msg) {
  return playViewActive()
    ? playMode.humanPlayer
    : (msg?.current_player === 0 || msg?.current_player === 1 ? msg.current_player : 0);
}

function buildResourceTradeMenu(giveResource, options, ratios) {
  const menu = document.createElement('div');
  menu.className = 'resource-trade-menu';
  menu.setAttribute('role', 'menu');
  menu.addEventListener('click', (event) => event.stopPropagation());

  const ratio = ratios?.[giveResource];
  for (const option of options) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'resource-trade-option';
    button.setAttribute('role', 'menuitem');
    const receiveName = capitalizeResourceName(RESOURCE_NAMES[option.receive]);
    button.textContent = Number.isFinite(Number(ratio))
      ? `${ratio}:1 ${receiveName}`
      : receiveName;
    button.title = option.label || `Trade for ${receiveName}`;
    button.addEventListener('click', (event) => {
      event.stopPropagation();
      openTradeGiveResource = null;
      updateBoardResourceLegend(currentState, currentState?.state, lastCanUseLegalActions);
      sendPlayAction(option.action);
    });
    menu.appendChild(button);
  }
  return menu;
}

function resourceTradeMenuSignature(giveResource, options, ratios) {
  const ratio = Number.isFinite(Number(ratios?.[giveResource])) ? Number(ratios[giveResource]) : '';
  return `${ratio}|${options.map(option => `${option.action}:${option.receive}:${option.label}`).join('|')}`;
}

function updateResourceCardLabel(card, label) {
  if (card.firstChild?.nodeType === Node.TEXT_NODE) {
    card.firstChild.nodeValue = label;
  } else {
    card.insertBefore(document.createTextNode(label), card.firstChild || null);
  }
}

function resourceTradeMenuElement(card) {
  return Array.from(card.children).find(child => child.classList?.contains('resource-trade-menu')) || null;
}

function removeResourceTradeMenu(card) {
  resourceTradeMenuElement(card)?.remove();
}

function syncResourceTradeMenu(card, resourceIndex, options, tradeRatios) {
  const signature = resourceTradeMenuSignature(resourceIndex, options, tradeRatios);
  const existing = resourceTradeMenuElement(card);
  if (existing?.dataset.signature === signature) return;
  existing?.remove();
  const menu = buildResourceTradeMenu(resourceIndex, options, tradeRatios);
  menu.dataset.signature = signature;
  card.appendChild(menu);
}

function updateBoardResourceLegend(msg, state, canUseLegalActions = false) {
  lastCanUseLegalActions = !!canUseLegalActions;
  const legend = document.getElementById('board-resource-legend');
  if (!legend || !state?.frame?.players) return;
  if (multiplayerSpectatorActive()) {
    legend.classList.add('hidden');
    legend.classList.remove('has-trades');
    openTradeGiveResource = null;
    return;
  }
  legend.classList.remove('hidden');

  const playerIndex = playerIndexForResourceLegend(msg);
  const hand = state.frame.players[playerIndex]?.hand || [0, 0, 0, 0, 0];
  const tradeRatios = state.frame.players[playerIndex]?.trade_ratios || [];
  const trades = legalMaritimeTradesByGive(msg, canUseLegalActions);
  if (openTradeGiveResource != null && !trades.has(openTradeGiveResource)) {
    openTradeGiveResource = null;
  }
  legend.classList.toggle('has-trades', trades.size > 0);

  for (const card of legend.querySelectorAll('[data-resource-index]')) {
    const resourceIndex = Number(card.dataset.resourceIndex);
    if (!Number.isInteger(resourceIndex)) continue;
    const count = hand[resourceIndex] ?? 0;
    const name = RESOURCE_NAMES[resourceIndex] || '';
    const displayName = capitalizeResourceName(name);
    const options = trades.get(resourceIndex) || [];
    const canTrade = options.length > 0;
    updateResourceCardLabel(card, `${count} ${displayName}`);
    card.classList.toggle('can-trade', canTrade);
    card.classList.toggle('trade-menu-open', canTrade && openTradeGiveResource === resourceIndex);
    card.setAttribute('aria-expanded', canTrade && openTradeGiveResource === resourceIndex ? 'true' : 'false');
    if (canTrade) {
      card.setAttribute('role', 'button');
      card.tabIndex = 0;
      card.title = `Trade ${displayName}`;
      card.setAttribute('aria-label', `Trade ${displayName}`);
      card.onclick = (event) => {
        event.stopPropagation();
        openTradeGiveResource = openTradeGiveResource === resourceIndex ? null : resourceIndex;
        updateBoardResourceLegend(currentState, currentState?.state, canUseLegalActions);
      };
      card.onkeydown = (event) => {
        if (event.key !== 'Enter' && event.key !== ' ') return;
        event.preventDefault();
        card.click();
      };
      if (openTradeGiveResource === resourceIndex) {
        syncResourceTradeMenu(card, resourceIndex, options, tradeRatios);
      } else {
        removeResourceTradeMenu(card);
      }
    } else {
      removeResourceTradeMenu(card);
      card.removeAttribute('role');
      card.removeAttribute('aria-label');
      card.removeAttribute('title');
      card.removeAttribute('tabindex');
      card.onclick = null;
      card.onkeydown = null;
    }
  }
}

function legalBuildAvailability(msg, canUseLegalActions) {
  const available = { settlement: false, city: false, road: false };
  if (!canUseLegalActions || !Array.isArray(msg?.legal_actions)) return available;

  for (const entry of msg.legal_actions) {
    const action = Number(entry?.action);
    if (!Number.isInteger(action)) continue;
    if (action >= 0 && action < 54) {
      available.settlement = true;
    } else if (action >= 54 && action < 126) {
      available.road = true;
    } else if (action >= 126 && action < 180) {
      available.city = true;
    }
  }
  return available;
}

function updateBoardPieceCounts(msg, state, canUseLegalActions = false) {
  const panel = document.getElementById('board-piece-counts');
  if (!panel || !state?.frame?.buildings) return;

  const playerIndex = multiplayerSpectatorActive()
    ? (msg.current_player === 0 || msg.current_player === 1 ? msg.current_player : 0)
    : playViewActive()
    ? playMode.humanPlayer
    : (msg.current_player === 0 || msg.current_player === 1 ? msg.current_player : 0);
  const buildings = state.frame.buildings[playerIndex];
  if (!buildings) {
    panel.classList.add('hidden');
    return;
  }

  const settlements = Math.max(0, PIECE_LIMITS.settlements - (buildings.settlements?.length || 0));
  const cities = Math.max(0, PIECE_LIMITS.cities - (buildings.cities?.length || 0));
  const roads = Math.max(0, PIECE_LIMITS.roads - (buildings.roads?.length || 0));

  document.getElementById('piece-count-settlements').textContent = settlements;
  document.getElementById('piece-count-cities').textContent = cities;
  document.getElementById('piece-count-roads').textContent = roads;

  const available = legalBuildAvailability(msg, canUseLegalActions);
  for (const item of panel.querySelectorAll('[data-piece-kind]')) {
    const kind = item.dataset.pieceKind;
    item.classList.toggle('can-build', !!available[kind]);
  }

  panel.classList.remove('hidden', 'p1', 'p2');
  panel.classList.add(playerIndex === 0 ? 'p1' : 'p2');
  panel.setAttribute(
    'aria-label',
    `${playerDisplayName(playerIndex)} pieces remaining: ${settlements} settlements, ${cities} cities, ${roads} roads`
  );
}

function initCatanRulesPopover() {
  const button = document.getElementById('btn-catan-rules');
  const popover = document.getElementById('catan-rules-popover');
  const closeButton = document.getElementById('btn-close-catan-rules');
  if (!button || !popover) return;

  const show = () => {
    popover.classList.remove('hidden');
    button.classList.add('active');
    button.setAttribute('aria-expanded', 'true');
  };
  const hide = () => {
    popover.classList.add('hidden');
    button.classList.remove('active');
    button.setAttribute('aria-expanded', 'false');
  };

  button.setAttribute('aria-expanded', 'false');
  button.addEventListener('click', (event) => {
    event.stopPropagation();
    if (popover.classList.contains('hidden')) show();
    else hide();
  });
  closeButton?.addEventListener('click', hide);
  popover.addEventListener('click', (event) => event.stopPropagation());
  document.addEventListener('click', hide);
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') hide();
  });
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

  if (state.private_view && !est) {
    bankEl.textContent = 'Unknown';
  } else if (est) {
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
  if (e.key === 'Escape') {
    closeResourceTradeMenu();
    hideReplayShareModal();
    hideProfileModal();
    hideGuestSignInRequiredModal();
  }
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

document.addEventListener('click', closeResourceTradeMenu);

// ── Start ────────────────────────────────────────────────────────────

let appStarted = false;
let lastSocketAuthKey = '';

function currentAuthUserId() {
  const clerk = window.Clerk;
  return String(clerk?.user?.id || clerk?.session?.user?.id || '')
    .replace(/[\u0000-\u001f\u007f]/g, '')
    .trim()
    .slice(0, 128);
}

function currentSocketAuthKey() {
  if (window.hexfishAuthSignedIn === true) {
    const username = typeof window.hexfishAuthUsername === 'function'
      ? window.hexfishAuthUsername()
      : window.hexfishUsername;
    return `signed-in:${currentAuthUserId() || 'unknown'}:${normalizeProfileUsername(username)}`;
  }
  if (guestSharedReplayMode()) {
    return `guest:replay:${normalizeReplaySlug(window.hexfishGuestReplaySlug || currentSharedReplaySlugFromUrl())}`;
  }
  if (guestMultiplayerMode()) {
    return `guest:room:${normalizeRoomCode(window.hexfishGuestRoomCode || initialUrlRoomCode)}`;
  }
  if (guestPlayMode()) {
    return 'guest:play';
  }
  return 'signed-out';
}

function authKeyFromEvent(event) {
  return String(event?.detail?.auth_key || window.hexfishAuthKey || currentSocketAuthKey());
}

function logSkippedDuplicateAuth(reason, authKey) {
  console.debug('HexFish auth event skipped duplicate', {
    reason,
    auth_key: authKey,
    app_started: appStarted,
    connected: session.connected,
  });
}

window.hexfishStartApp = () => {
  if (appStarted) return;
  if (!lastSocketAuthKey) lastSocketAuthKey = currentSocketAuthKey();
  appStarted = true;
  if (guestMultiplayerMode()) enterGuestPlayMode();
  else syncGuestUiState();
  session.connect();
  const loadingSharedReplay = guestMultiplayerMode()
    ? false
    : guestSharedReplayMode()
      ? enterGuestSharedReplayMode()
      : maybeLoadSharedReplayFromUrl();
  if (!loadingSharedReplay) {
    requestMultiplayerLobby();
    maybeAutoJoinRoomFromUrl();
  }
};

window.hexfishStopApp = (options = {}) => {
  if (!appStarted) return;
  appStarted = false;
  session.disconnect(options);
};

function startOrReconnectForAuthChange(options = {}) {
  const reason = options.reason || 'auth changed';
  const nextAuthKey = String(options.authKey || currentSocketAuthKey());
  if (lastSocketAuthKey === nextAuthKey) {
    logSkippedDuplicateAuth(reason, nextAuthKey);
    if (!appStarted) {
      window.hexfishStartApp();
    } else if (!session.connected) {
      session.connect();
    }
    return false;
  }
  const previousAuthKey = lastSocketAuthKey;
  lastSocketAuthKey = nextAuthKey;
  console.debug('HexFish auth changed; reconnecting WebSocket if needed', {
    reason,
    previous_auth_key: previousAuthKey,
    auth_key: nextAuthKey,
    app_started: appStarted,
    connected: session.connected,
  });
  if (!appStarted) {
    window.hexfishStartApp();
    return true;
  }
  markSingleplayerSocketRecovery('auth-reconnect');
  session.disconnect({
    code: APP_WEBSOCKET_AUTH_CHANGED_CLOSE_CODE,
    reason: 'auth changed',
  });
  session.connect();
  if (activeView === 'play' || activeView === 'play-setup') requestMultiplayerLobby();
  if (activeView === 'replay-list') requestReplayList();
  return true;
}

document.addEventListener('hexfish-auth-signed-in', (event) => {
  syncProfileUsernameFromClerk();
  syncGuestUiState();
  updateProfileUi();
  maybePromptForProfileUsername();
  startOrReconnectForAuthChange({
    authKey: authKeyFromEvent(event),
    reason: 'signed-in',
  });
});
document.addEventListener('hexfish-auth-guest', (event) => {
  setProfileUsername('');
  if (guestMultiplayerMode()) enterGuestPlayMode();
  else if (guestPlayMode()) showPlayView();
  else syncGuestUiState();
  updateProfileUi();
  startOrReconnectForAuthChange({
    authKey: authKeyFromEvent(event),
    reason: 'guest',
  });
});
document.addEventListener('hexfish-auth-signed-out', (event) => {
  const nextAuthKey = authKeyFromEvent(event);
  if (lastSocketAuthKey === nextAuthKey && !appStarted) {
    logSkippedDuplicateAuth('signed-out', nextAuthKey);
    return;
  }
  lastSocketAuthKey = nextAuthKey;
  stopProfileUsernameSyncPolling();
  setProfileUsername('');
  syncGuestUiState();
  updateProfileUi();
  window.hexfishStopApp({
    code: APP_WEBSOCKET_APP_STOPPED_CLOSE_CODE,
    reason: 'app stopped',
  });
});

window.setInterval(requestActiveMultiplayerRoomSync, MULTIPLAYER_ROOM_SYNC_MS);
window.setInterval(checkPlayBotWatchdog, PLAY_BOT_WATCHDOG_INTERVAL_MS);

if (window.hexfishAuthSignedIn || guestMode()) {
  window.hexfishStartApp();
}

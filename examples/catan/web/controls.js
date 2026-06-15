// Control bar: buttons, auto-search toggle, autoplay.
//
// The server drives polling and search batches via a budget model.
// RunSims adds to the budget; SetAutoSearch sets auto-refill on state
// change. The client just sends commands and renders server updates.

class Controls {
  constructor(session) {
    this.session = session;
    this.autoplay = false;
    this.pendingAutoplay = false;
    this.lastState = null;
    this.replayMode = false;
    this.autoSearch = document.getElementById('autosearch-toggle').checked;
    this.searchRunning = false;
    this.pauseRequested = false;
    this.optionsAvailable = true;
    this.optionsOpen = false;
    this.budgetMode = 'simulations';
    this.budgetValues = {
      simulations: parseInt(document.getElementById('sims-input').value) || 50,
      pv_depth: 8,
    };
    this.onNewGame = null;
    this._setBudgetMode(this.budgetMode);
    this._bind();
    this._updateSearchButtons();
    this._updateOptionsVisibility();
    // Tell the server our initial auto-search state.
    if (this.autoSearch) this._syncAutoSearch();
  }

  _budgetInput() {
    return document.getElementById('sims-input');
  }

  _saveBudgetValue() {
    const input = this._budgetInput();
    let value = parseInt(input.value);
    if (!Number.isFinite(value)) value = this.budgetValues[this.budgetMode];
    const min = parseInt(input.min);
    const max = parseInt(input.max);
    value = Math.max(min, Math.min(max, value));
    this.budgetValues[this.budgetMode] = value;
    input.value = value;
    return value;
  }

  _currentBudget() {
    return {
      mode: this.budgetMode,
      value: this._saveBudgetValue(),
    };
  }

  _setBudgetMode(mode) {
    this.budgetMode = mode;
    const input = this._budgetInput();
    const label = document.getElementById('budget-value-label');
    if (mode === 'pv_depth') {
      label.firstChild.textContent = 'Depth:';
      input.min = '1';
      input.max = '30';
      input.step = '1';
      input.value = this.budgetValues.pv_depth;
    } else {
      label.firstChild.textContent = 'Sims:';
      input.min = '0';
      input.max = '10000';
      input.step = '50';
      input.value = this.budgetValues.simulations;
    }
    for (const btn of document.querySelectorAll('.budget-mode-btn')) {
      const active = btn.dataset.budgetMode === mode;
      btn.classList.toggle('bg-accent', active);
      btn.classList.toggle('text-white', active);
      btn.classList.toggle('bg-bg-3', !active);
      btn.classList.toggle('text-gray-300', !active);
      btn.setAttribute('aria-pressed', active ? 'true' : 'false');
    }
  }

  _syncAutoSearch() {
    const budget = this._currentBudget();
    const target = budget.mode === 'simulations' ? budget.value : 0;
    this.session.send({ type: 'SetAutoSearch', enabled: this.autoSearch, target, budget });
  }

  _disableAutoSearch() {
    if (!this.autoSearch) return;
    this.autoSearch = false;
    document.getElementById('autosearch-toggle').checked = false;
    this.session.send({
      type: 'SetAutoSearch',
      enabled: false,
      target: 0,
      budget: { mode: 'simulations', value: 0 },
    });
  }

  _setSearchRunning(running) {
    this.searchRunning = !!running;
    this.session.setSearchInterruptMode?.(this.searchRunning);
    this._updateSearchButtons();
  }

  _finishSearch() {
    this._setSearchRunning(false);
    this.pauseRequested = false;
  }

  _updateSearchButtons() {
    const searchBtn = document.getElementById('btn-run-sims');
    const pauseBtn = document.getElementById('btn-pause-search');
    if (!searchBtn || !pauseBtn) return;
    const searchActive = this.searchRunning && !this.pauseRequested;
    const pauseActive = !searchActive;
    searchBtn.disabled = false;
    pauseBtn.disabled = false;
    this._setSegmentActive(searchBtn, searchActive);
    this._setSegmentActive(pauseBtn, pauseActive);
  }

  _setSegmentActive(btn, active) {
    btn.classList.toggle('bg-accent', active);
    btn.classList.toggle('text-white', active);
    btn.classList.toggle('bg-bg-3', !active);
    btn.classList.toggle('text-gray-300', !active);
    btn.classList.toggle('hover:bg-bg', !active);
    btn.setAttribute('aria-pressed', active ? 'true' : 'false');
  }

  setOptionsAvailable(available) {
    this.optionsAvailable = !!available;
    this._updateOptionsVisibility();
  }

  _setOptionsOpen(open) {
    this.optionsOpen = !!open;
    const flyout = document.getElementById('options-flyout');
    const btn = document.getElementById('btn-options-menu');
    if (flyout) flyout.classList.toggle('hidden', !this.optionsOpen);
    if (btn) btn.setAttribute('aria-expanded', this.optionsOpen ? 'true' : 'false');
  }

  _updateOptionsVisibility() {
    const control = document.getElementById('options-menu-control');
    if (!control) return;
    const hidden = !this.optionsAvailable || this.replayMode;
    control.classList.toggle('hidden', hidden);
    if (hidden) this._setOptionsOpen(false);
  }

  /// Called on every GameState update.
  onStateUpdate(msg) {
    this.lastState = msg;
    this.replayMode = !!msg.replay;
    this._updateReplayControls(msg);
    if (this.replayMode) {
      this.stopAutoplay();
      this._disableAutoSearch();
      this._finishSearch();
      this.pendingAutoplay = false;
      return;
    }
    if (!this.autoplay || msg.is_terminal) {
      if (msg.is_terminal) this._finishSearch();
      this.pendingAutoplay = false;
      return;
    }
    if (this._playForcedMove(msg)) {
      return;
    }
    if (this.pendingAutoplay) {
      this.pendingAutoplay = false;
      this._runSims();
      return;
    }
  }

  /// Called when a Snapshot arrives (manual search completed).
  onSimsDone(snapshot) {
    const paused = this.pauseRequested;
    this._finishSearch();
    if (this.replayMode) return;
    if (paused) return;
    if (document.getElementById('apply-toggle').checked || this.autoplay) {
      if (snapshot && snapshot.edges && snapshot.edges.length > 0) {
        const best = snapshot.edges.reduce((a, b) => b.visits > a.visits ? b : a);
        this.pendingAutoplay = this.autoplay;
        this.session.send({ type: 'PlayAction', action: best.action });
      }
    }
  }

  /// Called when BotMove completes.
  onBotDone() {
    this._finishSearch();
  }

  /// Called on server Error.
  onSearchError() {
    this._finishSearch();
  }

  onGameOver() {
    this.stopAutoplay();
    this._finishSearch();
  }

  runSearch() {
    this._runSims();
  }

  _runSims() {
    if (this.searchRunning) return;
    this.runSearchWithBudget(this._currentBudget(), this._searchTarget());
  }

  runSearchWithBudget(budget, target = this._searchTarget()) {
    if (this.searchRunning) return false;
    this.session.send({
      type: 'RunSearch',
      budget,
      target,
    });
    this.onSearchStarted();
    return true;
  }

  pauseBeforeCommand() {
    if (this.searchRunning && !this.pauseRequested) {
      this.pauseSearch();
    }
  }

  pauseSearch() {
    if (!this.searchRunning || this.pauseRequested) return;
    this.pauseRequested = true;
    this._updateSearchButtons();
    this.session.send({
      type: 'PauseSearch',
      target: this._searchTarget(),
    });
  }

  onSearchStarted() {
    this.pauseRequested = false;
    this._setSearchRunning(true);
  }

  onSearchProgress() {
    if (this.searchRunning) this._updateSearchButtons();
  }

  isPausePending() {
    return this.pauseRequested;
  }

  _searchTarget() {
    return this.replayMode ? 'replay' : 'analysis';
  }

  _forcedAction(msg) {
    if (!msg || msg.replay || msg.is_terminal || msg.is_chance) return null;
    if (!Array.isArray(msg.legal_actions) || msg.legal_actions.length !== 1) return null;
    return msg.legal_actions[0];
  }

  _playForcedMove(msg) {
    const forced = this._forcedAction(msg);
    if (!forced) return false;
    this.pendingAutoplay = true;
    this.session.send({ type: 'PlayAction', action: forced.action });
    return true;
  }

  _bind() {
    document.getElementById('btn-new-game').addEventListener('click', () => {
      this.stopAutoplay();
      this.pauseBeforeCommand();
      this.onNewGame?.();
      this.session.send({ type: 'NewGame', seed: null });
    });

    document.getElementById('btn-undo').addEventListener('click', () => {
      this.stopAutoplay();
      this._disableAutoSearch();
      this.pauseBeforeCommand();
      this.session.send({ type: 'Undo' });
    });

    document.getElementById('btn-redo').addEventListener('click', () => {
      this.stopAutoplay();
      this._disableAutoSearch();
      this.pauseBeforeCommand();
      this.session.send({ type: 'Redo' });
    });

    document.getElementById('btn-bot-move').addEventListener('click', () => {
      if (this.replayMode) return;
      const budget = this._currentBudget();
      const msg = { type: 'BotMove', budget };
      if (budget.mode === 'simulations') msg.simulations = budget.value;
      this.session.send(msg);
    });

    document.getElementById('btn-run-sims').addEventListener('click', () => {
      this._runSims();
    });

    document.getElementById('btn-pause-search').addEventListener('click', () => {
      this.pauseSearch();
    });

    const optionsBtn = document.getElementById('btn-options-menu');
    const optionsFlyout = document.getElementById('options-flyout');
    optionsBtn?.addEventListener('click', (event) => {
      event.stopPropagation();
      this._setOptionsOpen(!this.optionsOpen);
    });
    optionsFlyout?.addEventListener('click', (event) => {
      event.stopPropagation();
    });
    document.addEventListener('click', () => {
      if (this.optionsOpen) this._setOptionsOpen(false);
    });
    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && this.optionsOpen) {
        this._setOptionsOpen(false);
        optionsBtn?.focus();
      }
    });

    document.getElementById('autoplay-toggle').addEventListener('change', (e) => {
      if (this.replayMode) {
        e.target.checked = false;
        return;
      }
      if (e.target.checked) {
        this.startAutoplay();
      } else {
        this.stopAutoplay();
      }
    });

    document.getElementById('autosearch-toggle').addEventListener('change', (e) => {
      if (this.replayMode) {
        e.target.checked = false;
        this.autoSearch = false;
        return;
      }
      this.autoSearch = e.target.checked;
      this._syncAutoSearch();
    });

    for (const btn of document.querySelectorAll('.budget-mode-btn')) {
      btn.addEventListener('click', () => {
        this._saveBudgetValue();
        this._setBudgetMode(btn.dataset.budgetMode);
        if (this.autoSearch) this._syncAutoSearch();
      });
    }

    // Re-sync target when the budget input changes.
    document.getElementById('sims-input').addEventListener('change', () => {
      this._saveBudgetValue();
      if (this.autoSearch) this._syncAutoSearch();
    });

    for (const btn of document.querySelectorAll('.takeover-btn')) {
      btn.addEventListener('click', () => {
        const player = parseInt(btn.dataset.player);
        const isHuman = btn.textContent.trim() === 'Take Over';
        if (isHuman) {
          this.session.send({ type: 'TakeOver', player });
          btn.textContent = 'Release';
        } else {
          this.session.send({ type: 'ReleaseControl', player });
          btn.textContent = 'Take Over';
        }
      });
    }

    document.getElementById('btn-replay-first').addEventListener('click', () => {
      this._setReplayCursor(0);
    });
    document.getElementById('btn-replay-prev').addEventListener('click', () => {
      const replay = this.lastState?.replay;
      if (!replay) return;
      this._setReplayCursor(Math.max(0, replay.cursor - 1));
    });
    document.getElementById('btn-replay-next').addEventListener('click', () => {
      const replay = this.lastState?.replay;
      if (!replay) return;
      this._setReplayCursor(Math.min(replay.len, replay.cursor + 1));
    });
    document.getElementById('btn-replay-last').addEventListener('click', () => {
      const replay = this.lastState?.replay;
      if (!replay) return;
      this._setReplayCursor(replay.len);
    });
    document.getElementById('replay-slider').addEventListener('input', (e) => {
      const replay = this.lastState?.replay;
      if (!replay) return;
      const cursor = Math.max(0, Math.min(replay.len, parseInt(e.target.value) || 0));
      document.getElementById('replay-counter').textContent = `${cursor} / ${replay.len}`;
      this._setReplayCursor(cursor);
    });
  }

  _setReplayCursor(cursor) {
    if (!this.lastState?.replay) return;
    this.session.send({ type: 'SetReplayCursor', cursor });
  }

  _updateReplayControls(msg) {
    const replay = msg.replay;
    const replayControls = document.getElementById('replay-controls');
    replayControls.classList.toggle('hidden', !replay);
    document.getElementById('board-history-controls')?.classList.toggle('hidden', !!replay);

    document.getElementById('btn-new-game').classList.toggle('hidden', !!replay);

    const botMove = document.getElementById('btn-bot-move');
    botMove.disabled = !!replay;
    botMove.classList.toggle('hidden', !!replay);

    const applyControl = document.getElementById('apply-control');
    const autoplayControl = document.getElementById('autoplay-control');
    const autosearchControl = document.getElementById('autosearch-control');
    applyControl.classList.toggle('hidden', !!replay);
    autoplayControl.classList.toggle('hidden', !!replay);
    autosearchControl.classList.toggle('hidden', !!replay);
    document.getElementById('apply-toggle').disabled = !!replay;
    document.getElementById('autoplay-toggle').disabled = !!replay;
    document.getElementById('autosearch-toggle').disabled = !!replay;
    for (const btn of document.querySelectorAll('.takeover-btn')) {
      btn.disabled = !!replay;
      btn.classList.toggle('hidden', !!replay);
    }
    if (replay) {
      document.getElementById('apply-toggle').checked = false;
      document.getElementById('autoplay-toggle').checked = false;
      document.getElementById('autosearch-toggle').checked = false;
    }
    this._updateOptionsVisibility();

    if (!replay) return;
    document.getElementById('replay-counter').textContent = `${replay.cursor} / ${replay.len}`;
    const slider = document.getElementById('replay-slider');
    slider.min = '0';
    slider.max = String(replay.len);
    slider.value = String(replay.cursor);
    slider.disabled = replay.len <= 0;
    document.getElementById('btn-replay-first').disabled = replay.cursor <= 0;
    document.getElementById('btn-replay-prev').disabled = replay.cursor <= 0;
    document.getElementById('btn-replay-next').disabled = replay.cursor >= replay.len;
    document.getElementById('btn-replay-last').disabled = replay.cursor >= replay.len;
  }

  startAutoplay() {
    if (this.replayMode) return;
    this.autoplay = true;
    this.pendingAutoplay = false;
    if (this._playForcedMove(this.lastState)) return;
    this._runSims();
  }

  stopAutoplay() {
    this.autoplay = false;
    this.pendingAutoplay = false;
    document.getElementById('autoplay-toggle').checked = false;
  }
}

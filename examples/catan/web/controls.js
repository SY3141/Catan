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
    this._bind();
    // Tell the server our initial auto-search state.
    if (this.autoSearch) this._syncAutoSearch();
  }

  _syncAutoSearch() {
    const target = parseInt(document.getElementById('sims-input').value);
    this.session.send({ type: 'SetAutoSearch', enabled: this.autoSearch, target });
  }

  _disableAutoSearch() {
    if (!this.autoSearch) return;
    this.autoSearch = false;
    document.getElementById('autosearch-toggle').checked = false;
    this.session.send({ type: 'SetAutoSearch', enabled: false, target: 0 });
  }

  /// Called on every GameState update.
  onStateUpdate(msg) {
    this.lastState = msg;
    this.replayMode = !!msg.replay;
    this._updateReplayControls(msg);
    if (this.replayMode) {
      this.stopAutoplay();
      this._disableAutoSearch();
      this.pendingAutoplay = false;
      return;
    }
    if (!this.autoplay || msg.is_terminal) {
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

  /// Called when a Snapshot arrives (manual RunSims completed).
  onSimsDone(snapshot) {
    if (this.replayMode) return;
    if (document.getElementById('apply-toggle').checked || this.autoplay) {
      if (snapshot && snapshot.edges && snapshot.edges.length > 0) {
        const best = snapshot.edges.reduce((a, b) => b.visits > a.visits ? b : a);
        this.pendingAutoplay = this.autoplay;
        this.session.send({ type: 'PlayAction', action: best.action });
      }
    }
  }

  /// Called when BotMove completes.
  onBotDone() {}

  /// Called on server Error.
  onSearchError() {}

  onGameOver() {
    this.stopAutoplay();
  }

  _runSims() {
    const count = parseInt(document.getElementById('sims-input').value);
    this.session.send({ type: 'RunSims', count, target: this._searchTarget() });
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
      this.session.send({ type: 'NewGame', seed: null });
    });

    document.getElementById('btn-undo').addEventListener('click', () => {
      this.stopAutoplay();
      this._disableAutoSearch();
      this.session.send({ type: 'Undo' });
    });

    document.getElementById('btn-redo').addEventListener('click', () => {
      this.stopAutoplay();
      this._disableAutoSearch();
      this.session.send({ type: 'Redo' });
    });

    document.getElementById('btn-bot-move').addEventListener('click', () => {
      if (this.replayMode) return;
      const sims = parseInt(document.getElementById('sims-input').value);
      this.session.send({ type: 'BotMove', simulations: sims });
    });

    document.getElementById('btn-run-sims').addEventListener('click', () => {
      this._runSims();
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

    // Re-sync target when the sims input changes.
    document.getElementById('sims-input').addEventListener('change', () => {
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
  }

  _setReplayCursor(cursor) {
    if (!this.lastState?.replay) return;
    this.session.send({ type: 'SetReplayCursor', cursor });
  }

  _updateReplayControls(msg) {
    const replay = msg.replay;
    const replayControls = document.getElementById('replay-controls');
    replayControls.classList.toggle('hidden', !replay);

    document.getElementById('btn-new-game').classList.toggle('hidden', !!replay);
    document.getElementById('btn-undo').classList.toggle('hidden', !!replay);
    document.getElementById('btn-redo').classList.toggle('hidden', !!replay);

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

    if (!replay) return;
    document.getElementById('replay-counter').textContent = `${replay.cursor} / ${replay.len}`;
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

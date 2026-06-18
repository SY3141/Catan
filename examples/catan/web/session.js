// WebSocket client for the analysis board.
//
// Dispatches incoming messages to registered handlers.

class Session {
  constructor(options = {}) {
    this.ws = null;
    this.handlers = {};
    this.connected = false;
    this.authenticated = false;
    this.queue = [];
    this.shouldReconnect = false;
    this.deferDuringSearch = false;
    this.getAuthToken = options.getAuthToken || null;
    this.anonymousSessionId = options.anonymousSessionId || this._loadAnonymousSessionId();
  }

  setAuthTokenProvider(provider) {
    this.getAuthToken = provider;
  }

  connect() {
    if (this.ws && (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING)) {
      return;
    }
    this.shouldReconnect = true;
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${proto}//${location.host}/ws`;
    const ws = new WebSocket(url);
    this.ws = ws;

    ws.onopen = async () => {
      if (this.ws !== ws) {
        ws.close();
        return;
      }
      let token = '';
      try {
        token = await this._getAuthToken() || '';
      } catch (error) {
        console.warn('Could not read Clerk session token; trying anonymous WebSocket session.', error);
      }
      ws.send(JSON.stringify({
        type: 'Authenticate',
        token,
        anonymous_session: this.anonymousSessionId,
      }));
    };

    ws.onmessage = (event) => {
      if (this.ws !== ws) return;
      const msg = JSON.parse(event.data);
      const handler = this.handlers[msg.type];

      if (!this.authenticated) {
        if (msg.type === 'Error') {
          if (handler) handler(msg);
          ws.close();
          return;
        }
        this.authenticated = true;
        this.connected = true;
        if (handler) handler(msg);
        for (const queued of this.queue) {
          ws.send(queued);
        }
        this.queue = [];
        return;
      }

      if (handler) handler(msg);
    };

    ws.onclose = () => {
      const shouldReconnect = this.shouldReconnect && this.ws === ws;
      if (this.ws === ws) {
        this.connected = false;
        this.authenticated = false;
        this.ws = null;
      }
      const handler = this.handlers.Disconnected;
      if (handler) handler({ type: 'Disconnected' });
      if (shouldReconnect) {
        setTimeout(() => this.connect(), 2000);
      }
    };

    ws.onerror = () => {
      ws.close();
    };
  }

  send(msg) {
    const json = JSON.stringify(msg);
    if (this.deferDuringSearch && msg.type !== 'PauseSearch') {
      this.queue.push(json);
      return;
    }
    this._sendOrQueue(json);
  }

  sendInterrupt(msg) {
    this._sendOrQueue(JSON.stringify(msg));
  }

  setSearchInterruptMode(enabled) {
    this.deferDuringSearch = !!enabled;
    if (!this.deferDuringSearch) this._flushQueue();
  }

  _sendOrQueue(json) {
    if (this.connected && this.authenticated && this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(json);
    } else {
      this.queue.push(json);
    }
  }

  _flushQueue() {
    if (!this.connected || !this.authenticated || !this.ws || this.ws.readyState !== WebSocket.OPEN) {
      return;
    }
    const queued = this.queue;
    this.queue = [];
    for (const json of queued) {
      this.ws.send(json);
    }
  }

  disconnect() {
    this.shouldReconnect = false;
    this.connected = false;
    this.authenticated = false;
    this.deferDuringSearch = false;
    this.queue = [];
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  on(type, handler) {
    this.handlers[type] = handler;
  }

  async _getAuthToken() {
    if (typeof this.getAuthToken !== 'function') return null;
    return this.getAuthToken();
  }

  _loadAnonymousSessionId() {
    const key = 'hexfish-anonymous-session-id';
    try {
      let id = window.localStorage && window.localStorage.getItem(key);
      if (!id) {
        id = window.crypto && typeof window.crypto.randomUUID === 'function'
          ? window.crypto.randomUUID()
          : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
        window.localStorage && window.localStorage.setItem(key, id);
      }
      return id;
    } catch (_error) {
      return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
    }
  }
}

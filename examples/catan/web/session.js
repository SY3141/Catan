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
    this.getAuthToken = options.getAuthToken || null;
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
      ws.send(JSON.stringify({ type: 'Authenticate', token }));
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
    if (this.connected && this.authenticated && this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(json);
    } else {
      this.queue.push(json);
    }
  }

  disconnect() {
    this.shouldReconnect = false;
    this.connected = false;
    this.authenticated = false;
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
}

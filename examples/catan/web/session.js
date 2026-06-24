// WebSocket client for the analysis board.
//
// Dispatches incoming messages to registered handlers.

const ANONYMOUS_SESSION_KEY_V1 = 'hexfish-anonymous-session-id';
const ANONYMOUS_SESSION_KEY_V2 = 'hexfish-anonymous-session-id-v2';
const WEBSOCKET_HEARTBEAT_INTERVAL_MS = 10000;
const WEBSOCKET_HEARTBEAT_TIMEOUT_MS = 90000;
const WEBSOCKET_HEARTBEAT_TIMEOUT_CLOSE_CODE = 4000;
const WEBSOCKET_HEARTBEAT_SEND_FAILED_CLOSE_CODE = 4001;
const WEBSOCKET_AUTH_CHANGED_CLOSE_CODE = 4002;
const WEBSOCKET_APP_STOPPED_CLOSE_CODE = 4003;
const WEBSOCKET_BAD_MESSAGE_CLOSE_CODE = 4004;
const WEBSOCKET_MAX_QUEUE_MESSAGES = 100;
const WEBSOCKET_RECONNECT_BASE_MS = 1000;
const WEBSOCKET_RECONNECT_MAX_MS = 30000;
const WEBSOCKET_RECONNECT_JITTER = 0.35;
const WEBSOCKET_LOW_PRIORITY_QUEUE_TYPES = new Set([
  'GetMultiplayerRoom',
  'ListMultiplayerRooms',
  'GetProfile',
]);

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
    this.heartbeatTimer = null;
    this.reconnectTimer = null;
    this.reconnectAttempts = 0;
    this.lastMessageAt = 0;
  }

  setAuthTokenProvider(provider) {
    this.getAuthToken = provider;
  }

  connect() {
    if (this.ws && (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING)) {
      return;
    }
    this._clearReconnectTimer();
    this.shouldReconnect = true;
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${proto}//${location.host}/ws`;
    const ws = new WebSocket(url);
    this.ws = ws;
    this.lastMessageAt = Date.now();

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
        clerk_user_id: this._getAuthUserId(),
        username: this._getAuthUsername(),
      }));
    };

    ws.onmessage = (event) => {
      if (this.ws !== ws) return;
      this.lastMessageAt = Date.now();
      let msg;
      try {
        msg = JSON.parse(event.data);
      } catch (error) {
        console.warn('HexFish WebSocket received malformed JSON', error);
        ws.close(WEBSOCKET_BAD_MESSAGE_CLOSE_CODE, 'bad message');
        return;
      }
      if (msg.type === 'Pong') return;
      const handler = this.handlers[msg.type];

      if (!this.authenticated) {
        if (msg.type === 'Error') {
          if (handler) handler(msg);
          ws.close();
          return;
        }
        this.authenticated = true;
        this.connected = true;
        this.reconnectAttempts = 0;
        this._startHeartbeat(ws);
        if (handler) handler(msg);
        this._flushQueue();
        const connectedHandler = this.handlers.Connected;
        if (connectedHandler) connectedHandler({ type: 'Connected' });
        return;
      }

      if (handler) handler(msg);
    };

    ws.onclose = (event) => {
      if (this.ws !== ws) return;
      const shouldReconnect = this.shouldReconnect;
      const lastMessageAgeMs = Date.now() - this.lastMessageAt;
      const discardedQueuedMessages = this.deferDuringSearch ? this.queue.length : 0;
      if (this.deferDuringSearch) {
        this.queue = [];
        this.deferDuringSearch = false;
      }
      this.connected = false;
      this.authenticated = false;
      this.ws = null;
      this._stopHeartbeat();
      const details = {
        type: 'Disconnected',
        will_reconnect: shouldReconnect,
        code: event.code,
        reason: event.reason || '',
        was_clean: event.wasClean,
        last_message_age_ms: lastMessageAgeMs,
        discarded_queued_messages: discardedQueuedMessages,
      };
      console.warn('HexFish WebSocket disconnected', details);
      const handler = this.handlers.Disconnected;
      if (handler) handler(details);
      if (shouldReconnect) {
        this._scheduleReconnect();
      }
    };

    ws.onerror = () => {
      console.warn('HexFish WebSocket error', { readyState: ws.readyState });
      ws.close();
    };
  }

  send(msg) {
    const json = JSON.stringify(msg);
    if (this.deferDuringSearch && msg.type !== 'PauseSearch') {
      this._queueJson(json);
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
      this._queueJson(json);
    }
  }

  _queueJson(json) {
    if (this.queue.length >= WEBSOCKET_MAX_QUEUE_MESSAGES) {
      const dropIndex = this.queue.findIndex((queued) => (
        WEBSOCKET_LOW_PRIORITY_QUEUE_TYPES.has(this._queuedMessageType(queued))
      ));
      const removed = this.queue.splice(dropIndex >= 0 ? dropIndex : 0, 1)[0];
      console.warn('HexFish WebSocket send queue full; dropped queued message', {
        dropped_type: this._queuedMessageType(removed),
        queued_type: this._queuedMessageType(json),
        queue_limit: WEBSOCKET_MAX_QUEUE_MESSAGES,
      });
    }
    this.queue.push(json);
  }

  _queuedMessageType(json) {
    try {
      return JSON.parse(json)?.type || '';
    } catch (_error) {
      return '';
    }
  }

  _flushQueue() {
    if (!this.connected || !this.authenticated || !this.ws || this.ws.readyState !== WebSocket.OPEN) {
      return;
    }
    if (this.deferDuringSearch) return;
    const queued = this.queue;
    this.queue = [];
    for (const json of queued) {
      this.ws.send(json);
    }
  }

  disconnect(options = {}) {
    this.shouldReconnect = false;
    this._clearReconnectTimer();
    this.reconnectAttempts = 0;
    this.connected = false;
    this.authenticated = false;
    this.deferDuringSearch = false;
    this.queue = [];
    this._stopHeartbeat();
    if (this.ws) {
      const code = Number(options.code);
      const reason = String(options.reason || '').slice(0, 123);
      if (Number.isInteger(code) && code >= 3000 && code <= 4999) {
        this.ws.close(code, reason);
      } else {
        this.ws.close();
      }
      this.ws = null;
    }
  }

  on(type, handler) {
    this.handlers[type] = handler;
  }

  _startHeartbeat(ws) {
    this._stopHeartbeat();
    this.lastMessageAt = Date.now();
    this.heartbeatTimer = window.setInterval(() => {
      if (this.ws !== ws || !this.connected || !this.authenticated) {
        this._stopHeartbeat();
        return;
      }
      if (Date.now() - this.lastMessageAt > WEBSOCKET_HEARTBEAT_TIMEOUT_MS) {
        ws.close(WEBSOCKET_HEARTBEAT_TIMEOUT_CLOSE_CODE, 'heartbeat timeout');
        return;
      }
      try {
        ws.send(JSON.stringify({ type: 'Ping' }));
      } catch (_error) {
        ws.close(WEBSOCKET_HEARTBEAT_SEND_FAILED_CLOSE_CODE, 'heartbeat send failed');
      }
    }, WEBSOCKET_HEARTBEAT_INTERVAL_MS);
  }

  _stopHeartbeat() {
    if (!this.heartbeatTimer) return;
    window.clearInterval(this.heartbeatTimer);
    this.heartbeatTimer = null;
  }

  _scheduleReconnect() {
    this._clearReconnectTimer();
    const delay = this._nextReconnectDelayMs();
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
  }

  _nextReconnectDelayMs() {
    const attempt = this.reconnectAttempts++;
    const exponential = Math.min(
      WEBSOCKET_RECONNECT_MAX_MS,
      WEBSOCKET_RECONNECT_BASE_MS * (2 ** Math.min(attempt, 8))
    );
    const jitter = 1 + ((Math.random() * 2 - 1) * WEBSOCKET_RECONNECT_JITTER);
    return Math.max(250, Math.min(WEBSOCKET_RECONNECT_MAX_MS, Math.round(exponential * jitter)));
  }

  _clearReconnectTimer() {
    if (!this.reconnectTimer) return;
    window.clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
  }

  async _getAuthToken() {
    if (typeof this.getAuthToken !== 'function') return null;
    return this.getAuthToken();
  }

  _getAuthUsername() {
    const value = window.hexfishAuthSignedIn && typeof window.hexfishAuthUsername === 'function'
        ? window.hexfishAuthUsername()
        : '';
    const username = String(value || '')
      .replace(/[\u0000-\u001f\u007f]/g, '')
      .replace(/\s+/g, ' ')
      .trim()
      .slice(0, 24);
    return username || null;
  }

  _getAuthUserId() {
    if (!window.hexfishAuthSignedIn) return null;
    const clerk = window.Clerk;
    const value = clerk?.user?.id || clerk?.session?.user?.id || '';
    const userId = String(value || '')
      .trim()
      .slice(0, 128);
    return userId || null;
  }

  _loadAnonymousSessionId() {
    try {
      window.localStorage?.removeItem(ANONYMOUS_SESSION_KEY_V1);
      let id = window.localStorage && window.localStorage.getItem(ANONYMOUS_SESSION_KEY_V2);
      if (!id) {
        id = window.crypto && typeof window.crypto.randomUUID === 'function'
          ? window.crypto.randomUUID()
          : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
        window.localStorage && window.localStorage.setItem(ANONYMOUS_SESSION_KEY_V2, id);
      }
      return id;
    } catch (_error) {
      return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
    }
  }
}

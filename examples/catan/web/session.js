// WebSocket client for the analysis board.
//
// Dispatches incoming messages to registered handlers.

class Session {
  constructor() {
    this.ws = null;
    this.handlers = {};
    this.connected = false;
    this.queue = [];
    this.shouldReconnect = false;
  }

  connect() {
    if (this.ws && (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING)) {
      return;
    }
    this.shouldReconnect = true;
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${proto}//${location.host}/ws`;
    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      this.connected = true;
      for (const msg of this.queue) {
        this.ws.send(msg);
      }
      this.queue = [];
    };

    this.ws.onmessage = (event) => {
      const msg = JSON.parse(event.data);
      const handler = this.handlers[msg.type];
      if (handler) handler(msg);
    };

    this.ws.onclose = () => {
      this.connected = false;
      if (this.shouldReconnect) {
        setTimeout(() => this.connect(), 2000);
      }
    };

    this.ws.onerror = () => {
      this.ws.close();
    };
  }

  send(msg) {
    const json = JSON.stringify(msg);
    if (this.connected) {
      this.ws.send(json);
    } else {
      this.queue.push(json);
    }
  }

  disconnect() {
    this.shouldReconnect = false;
    this.connected = false;
    this.queue = [];
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  on(type, handler) {
    this.handlers[type] = handler;
  }
}

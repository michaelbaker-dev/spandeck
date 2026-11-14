// The Control Deck's connection to the station it was opened on.
// One WebSocket carries RPC, events, and every pane stream; other stations
// are reached through this one (peer.call, and attach with a station name).

const FRAME_OUTPUT = 1, FRAME_RESYNC = 2;

function clientId() {
  let id = localStorage.getItem('spandeck.client');
  if (!id) {
    id = 'web-' + Array.from(crypto.getRandomValues(new Uint8Array(8)), b => b.toString(16).padStart(2, '0')).join('');
    localStorage.setItem('spandeck.client', id);
  }
  return id;
}

// The localhost token arrives once in the URL fragment and is kept locally.
function takeToken() {
  const m = location.hash.match(/token=([0-9a-f]{32,})/);
  if (m) {
    localStorage.setItem('spandeck.token', m[1]);
    history.replaceState(null, '', location.pathname + '#/fleet');
  }
  return localStorage.getItem('spandeck.token') || undefined;
}

export class Api {
  constructor() {
    this.ws = null;
    this.pending = new Map();
    this.chans = new Map();
    this.nextId = 1;
    this.listeners = new Set();
    this.statusListeners = new Set();
    this.station = null;
    this.version = null;
    this.client = clientId();
    this.connected = false;
    this.authError = null;
  }

  onEvent(fn) { this.listeners.add(fn); return () => this.listeners.delete(fn); }
  onStatus(fn) { this.statusListeners.add(fn); return () => this.statusListeners.delete(fn); }
  _status() { for (const fn of this.statusListeners) fn(this.connected, this.authError); }

  connect() {
    return new Promise((resolve) => {
      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
      const ws = new WebSocket(`${proto}//${location.host}/api/ws`);
      ws.binaryType = 'arraybuffer';
      this.ws = ws;
      let opened = false;
      ws.onopen = () => ws.send(JSON.stringify({ auth: { token: takeToken(), client: this.client } }));
      ws.onmessage = (ev) => {
        if (typeof ev.data !== 'string') return this._binary(new Uint8Array(ev.data));
        const msg = JSON.parse(ev.data);
        if (msg.auth) {
          if (!msg.auth.ok) {
            this.authError = msg.auth.message || 'not allowed';
            this._status();
            resolve(false);
            return;
          }
          // The station was upgraded while this page was open: load the new
          // Control Deck, so a fix on the station reaches the page by itself.
          if (this.build && msg.auth.build && msg.auth.build !== this.build) { location.reload(); return; }
          this.build = msg.auth.build || this.build;
          this.authError = null;
          this.station = msg.auth.station;
          this.version = msg.auth.version;
          this.connected = true;
          opened = true;
          this.call('events.subscribe', {}).catch(() => {});
          this._status();
          resolve(true);
          return;
        }
        if (msg.event) { for (const fn of this.listeners) fn(msg.event); return; }
        if (msg.events_lost) { for (const fn of this.listeners) fn({ kind: 'events.lost', data: {} }); return; }
        if (msg.chan !== undefined && msg.id === undefined) {
          const c = this.chans.get(msg.chan);
          if (c && msg.exited !== undefined) c.onExit && c.onExit(msg.exited);
          if (c && msg.unreachable) c.onUnreachable && c.onUnreachable();
          return;
        }
        const p = this.pending.get(msg.id);
        if (!p) return;
        this.pending.delete(msg.id);
        if (msg.error) p.reject(Object.assign(new Error(msg.error.message || 'error'), { code: msg.error.code }));
        else p.resolve(msg.result);
      };
      ws.onclose = () => {
        const was = this.connected;
        this.connected = false;
        for (const p of this.pending.values()) p.reject(new Error('connection lost'));
        this.pending.clear();
        this.chans.clear();
        this._status();
        if (!opened) resolve(false);
        // An upgrade replaces the daemon in place: reconnect and carry on.
        if (!this.authError) setTimeout(() => this.connect().then(ok => ok && was !== undefined && this._reconnected()), 800);
      };
    });
  }

  _reconnected() { for (const fn of this.listeners) fn({ kind: 'connection.restored', data: {} }); }

  _binary(buf) {
    if (buf.length < 5) return;
    const chan = new DataView(buf.buffer, buf.byteOffset + 1, 4).getUint32(0, true);
    const c = this.chans.get(chan);
    if (!c) return;
    const payload = buf.subarray(5);
    if (buf[0] === FRAME_OUTPUT) c.onData(payload);
    else if (buf[0] === FRAME_RESYNC) c.onResync(payload);
  }

  call(method, params = {}) {
    return new Promise((resolve, reject) => {
      if (!this.connected) return reject(new Error('not connected'));
      const id = this.nextId++;
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  // Fire and forget (keystrokes).
  send(method, params) {
    if (this.connected) this.ws.send(JSON.stringify({ id: null, method, params }));
  }

  // Call a method on any station through this one.
  on(station, method, params = {}) {
    if (!station || station === this.station) return this.call(method, params);
    return this.call('peer.call', { station, method, params });
  }

  async attach(station, pane, mode, handlers) {
    const res = await this.call('pane.attach', { pane, mode, station: station === this.station ? undefined : station });
    this.chans.set(res.chan, handlers);
    const snapshot = Uint8Array.from(atob(res.snapshot_b64), c => c.charCodeAt(0));
    return { chan: res.chan, info: res.info, exited: res.exited, snapshot };
  }

  detach(chan) {
    this.chans.delete(chan);
    this.send('pane.detach', { chan });
  }
}

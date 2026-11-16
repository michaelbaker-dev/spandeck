// The Control Deck's connection to the station it was opened on.
// One WebSocket carries RPC, events, and every pane stream; other stations
// are reached through this one (peer.call, and attach with a station name).

const FRAME_OUTPUT = 1, FRAME_RESYNC = 2;

// Who this is, to the station: one name per browser, plus a part that is
// different in every tab or window. Two windows of the same browser showing the
// same pane used to look like one client, so neither gave way and they resized
// the pane back and forth without end (the screen flashed).
const hex = (bytes) => Array.from(crypto.getRandomValues(new Uint8Array(bytes)), b => b.toString(16).padStart(2, '0')).join('');
// `renew`: this tab's part is made again (another page turned out to hold it).
function clientId(renew = false) {
  let id = localStorage.getItem('spandeck.client');
  if (!id) {
    id = 'web-' + hex(8);
    localStorage.setItem('spandeck.client', id);
  }
  let tab = null;
  try { tab = sessionStorage.getItem('spandeck.tab'); } catch (e) { /* no session storage: one tab's worth */ }
  const had = !!tab;
  if (!tab || renew) {
    tab = hex(3);
    try { sessionStorage.setItem('spandeck.tab', tab); } catch (e) { /* as above */ }
  }
  return { id: `${id}-${tab}`, had };
}
// This page, for as long as it is loaded: made here, kept in memory only,
// so a reload or a duplicated tab is another page. The station closes a
// connection only for a newer one with the same client id AND instance:
// the same page come back.
const INSTANCE = hex(8);
// Where a page starts counting the names of its attachments: a place of its
// own below 2^30. A name is used once in the page's life, across every
// reconnect, so a number left over from an earlier connection is one the new
// connection was never given. (The audit: a key carrying pane B's old number
// was written to pane C.)
const firstName = () => 1 + crypto.getRandomValues(new Uint32Array(1))[0] % ((1 << 30) - 1);

// The localhost token arrives once in the URL fragment and is kept locally.
// Only the station's own, from `spandeck cc` on that machine: a browser's
// token for the fleet is never put in an address (see `hopOpen` in app.js).
function takeToken() {
  const m = location.hash.match(/token=([0-9a-f]{32,})/);
  if (m) {
    localStorage.setItem('spandeck.token', m[1]);
    history.replaceState(null, '', location.pathname + '#/fleet');
  }
  return localStorage.getItem('spandeck.token') || undefined;
}

// ---- the heartbeat's rules ----
//
// The audit: with this page's link half-open, the deck showed nothing at
// all at 1 s, 5 s, 30 s, 60 s, 120 s and 300 s, went on taking keys, and
// delivered them up to five minutes late. Nothing here pinged, nothing had a
// time limit, and the only way a lost connection was noticed was the socket
// closing, which a half-open one never does.
//
// The rules are plain functions of numbers, so that they can be checked one
// by one (crates/cli/tests/heartbeat.rs runs them in a real browser).

// The page's own clock ticks this often.
export const BEAT = 500;
// A tick this late means the page was not running (the machine slept, the
// tab was frozen, a long task): that time proves nothing about the link.
export const ASLEEP = 2000;
// With nothing heard for this long while the page is being looked at, it
// asks: a ping. A healthy idle link so carries one small frame a second.
export const PING_AFTER = 1000;
// Trying again after a lost connection: at once, then these, each 30% either
// way so that many pages do not come back in step, then every 5 s for ever.
export const RETRY = [0, 100, 200, 400, 800, 1600, 3200, 5000];
export function retryIn(attempt, rand = Math.random()) {
  return Math.round(RETRY[Math.min(Math.max(0, attempt), RETRY.length - 1)] * (0.7 + 0.6 * rand));
}
export function median(list) {
  if (!list.length) return null;
  const s = list.slice().sort((a, b) => a - b), m = s.length >> 1;
  return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
}
// How long a ping may go unanswered before the link is said not to be
// answering: 1.5 s, or four round trips of this connection when that is
// longer, and never more than 6 s. Typing is off from then; the connection is
// kept (a slow link that is alive must not be taken down for being slow).
export const slowAfter = (rtt) => Math.min(6000, Math.max(1500, 4 * (rtt || 0)));
// With nothing heard at all for this long, while the page was looking, the
// connection is given up and made again: 10 s, or three round trips and 2 s.
export const deadAfter = (rtt) => Math.min(30000, Math.max(10000, 3 * (rtt || 0) + 2000));
// How long a new connection is given to open: 5 s, twice that after each
// try that ran out of time, at most 10 s.
export const openWithin = (timeouts) => Math.min(10000, 5000 * 2 ** Math.min(timeouts, 4));
// And then to be signed in: 5 s, or three times what opening took.
export const signInWithin = (openedIn) => Math.max(5000, 3 * openedIn);
// What one tick of the page's clock means. `dt`: since the tick before.
//   'hidden'  nothing is sent and nothing is judged
//   'woke'    the page is looked at again, or was not running: nothing of
//             the time away counts, and one ping decides
//   'run'     the time counts
export function tickKind(visible, wasHidden, dt) {
  if (!visible) return 'hidden';
  return wasHidden || dt > ASLEEP ? 'woke' : 'run';
}
// What the screen says of the link. `state`: 'ok', 'silent' (open, and a ping
// is unanswered) or 'closed'. `gap`: for how long, of the time the page was
// looking, the link has not been known good. `wall`: since the last frame, by
// the clock on the wall.
//   0 nothing · 1 "reconnecting…" · 2 amber with the time, panes say typing
//   is off, what is shown is marked "as of" · 3 the bar · 4 the bar, "last
//   answered 1 min ago"
export function linkLevel(state, gap, wall) {
  if (state === 'ok') return 0;
  if (gap < 300 && wall < 10000) return 0;
  if (gap < 3000 && wall < 10000) return 1;
  if (gap < 10000) return 2;
  return wall < 60000 ? 3 : 4;
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
    const me = clientId();
    this.client = me.id;
    this.instance = INSTANCE;
    this.chanNext = firstName();
    this.attaching = new Map();
    // A duplicated tab is a client of its own (see `_twins`).
    this.renamed = 0;
    this._twins(me.had);
    this.connected = false;
    this.authError = null;
    // Refused for now, for a reason that is not this browser's (the station
    // cannot ask Tailscale who is connecting): shown, and tried again.
    this.waitWhy = null;
    // For the Performance page: how often this page lost its station.
    this.reconnects = 0;
    this.lastReconnect = 0;
    // Which of the owner's Tailscale devices the station takes this one for.
    this.device = null;

    // The heartbeat. `awake`: milliseconds this page has been looked at and
    // running; every judgement is made on that clock, so that a tab in the
    // background, a phone that was locked or a laptop that slept concludes
    // nothing from the time it was away.
    this.pings = false;      // the station answers pings (`caps`)
    this.awake = 0;
    this.beatAt = performance.now();
    this.wasHidden = document.visibilityState !== 'visible';
    this.frameAwake = 0;     // when the last frame came, on that clock
    this.frameWall = 0;      // and by the clock on the wall ("as of")
    this.pingOut = null;     // the ping that is unanswered: { n, t, awake }
    this.pingN = 0;
    this.deciding = false;   // that ping decides (the page has just woken)
    this.rtts = [];          // the last 8 round trips
    this.silent = false;     // open, and not answering
    this.gapFrom = null;     // since when (awake) the link is not known good
    this.tries = 0;          // connections tried in a row
    this.timeouts = 0;       // of them, how many ran out of time
    this.retryTimer = null;
    this.dropped = 0;        // connections this page gave up for silence
    this.silences = 0;       // times it said "no answer" of an open one
    this.lastGap = null;     // the last gap that ended: { ms, at }
    setInterval(() => this._beat(), BEAT);
    const wake = () => this._beat(true);
    document.addEventListener('visibilitychange', wake);
    addEventListener('pageshow', wake);
    addEventListener('online', wake);
  }

  // A duplicated tab copies `sessionStorage`, and with it the tab's part of
  // the client id: two pages would then be one client to every station, and
  // each would take the other's resize for its own (the sizing rule: each
  // tab is a client). Pages of one browser say who they are to each other;
  // of two with the same id, the one loaded later makes itself a new one and
  // connects again under it. (They never close each other's connection
  // meanwhile: that goes by the instance.) A page that finds its tab id
  // already there, and was not reloaded, waits a moment for another page to
  // say so before it first connects.
  _twins(had) {
    if (typeof BroadcastChannel !== 'function') return;
    const born = Date.now(), bc = new BroadcastChannel('spandeck.tab');
    const say = () => bc.postMessage({ tab: this.client, instance: this.instance, born });
    bc.onmessage = (ev) => {
      const m = ev.data;
      if (!m || m.tab !== this.client || m.instance === this.instance) return;
      if (born < m.born || (born === m.born && this.instance < m.instance)) return say();
      this.client = clientId(true).id;
      this.renamed += 1;
      const ws = this.ws;
      if (ws) { this._closed(ws); try { ws.close(); } catch (e) { /* gone already */ } }
    };
    say();
    let reloaded = false;
    try { reloaded = performance.getEntriesByType('navigation')[0]?.type === 'reload'; } catch (e) { /* not known: wait */ }
    if (had && !reloaded) this.settling = new Promise(r => setTimeout(r, 80));
  }

  // A name for a new attachment: never used before by this page.
  _name() { return this.chanNext++; }

  onEvent(fn) { this.listeners.add(fn); return () => this.listeners.delete(fn); }
  onStatus(fn) { this.statusListeners.add(fn); return () => this.statusListeners.delete(fn); }
  _status() { for (const fn of this.statusListeners) fn(this.connected, this.authError); }
  _emit(kind, data = {}) { for (const fn of this.listeners) fn({ kind, data }); }

  // Connected, and answering as far as this page knows: what is typed is
  // sent only then.
  get live() { return this.connected && !this.silent; }
  // This connection's round trip: the median of the last 8 pings.
  rtt() { return median(this.rtts); }
  // What the bounds are worked out from: that, or the latest one when it
  // is longer, so that a link that has just turned slow is given the time
  // of a slow link at once and not five pings later.
  _trip() { return Math.max(this.rtt() || 0, this.rtts[this.rtts.length - 1] || 0); }
  _now() { return this.awake + (this.wasHidden ? 0 : Math.min(performance.now() - this.beatAt, ASLEEP)); }
  // What the screen is drawn from (see linkLevel).
  link() {
    const state = !this.connected ? 'closed' : this.silent ? 'silent' : 'ok';
    const gap = this.gapFrom === null ? 0 : this._now() - this.gapFrom;
    const wall = this.frameWall ? Date.now() - this.frameWall : 0;
    return { state, gap, wall, asOf: this.frameWall, level: this.authError ? 0 : linkLevel(this.frameWall ? state : 'ok', gap, wall), why: this.waitWhy };
  }

  // Every half second, and at once when the page is shown again or the
  // browser says the network is back (`wake`).
  _beat(wake = false) {
    const now = performance.now(), dt = now - this.beatAt;
    const kind = tickKind(document.visibilityState === 'visible', this.wasHidden || wake, dt);
    this.beatAt = now;
    this.wasHidden = kind === 'hidden';
    if (kind === 'hidden') return;
    if (kind === 'woke') return this._woke();
    this.awake += dt;
    if (!this.connected || !this.pings) return;
    const at = this.awake, quiet = at - this.frameAwake;
    // A ping that other frames have overtaken, and whose own answer is
    // overdue, is let go of (a station drops the answer when the way out
    // to this page is full): the frames were the proof, and the next
    // quiet second asks again.
    if (this.pingOut && this.frameAwake > this.pingOut.awake && at - this.pingOut.awake >= slowAfter(this._trip())) this.pingOut = null;
    if (!this.pingOut && quiet >= PING_AFTER) this._ping();
    const unanswered = this.pingOut && this.frameAwake <= this.pingOut.awake ? at - this.pingOut.awake : 0;
    if (unanswered >= slowAfter(this._trip())) {
      // After waking, the one ping decides: the socket of a machine that
      // slept is usually dead, and a new one costs less than waiting.
      if (this.deciding) return this._drop();
      if (!this.silent) { this.silent = true; this.silences += 1; this.gapFrom ??= this.frameAwake; this._status(); }
    }
    if (quiet >= deadAfter(this._trip())) this._drop();
  }

  // The page is looked at again, or was not running for a while.
  _woke() {
    this.frameAwake = this.awake;
    this.pingOut = null;
    if (this.connected && this.pings) { this.deciding = true; this._ping(); }
    else if (!this.connected && this.retryTimer) { clearTimeout(this.retryTimer); this.retryTimer = null; this.connect(); }
    this._emit('page.woke');
  }

  _ping() {
    if (!this.connected) return;
    this.pingOut = { n: ++this.pingN, t: performance.now(), awake: this._now() };
    try { this.ws.send(JSON.stringify({ ping: this.pingOut.n, t: this.pingOut.t })); } catch (e) { /* closing: the close says so */ }
  }

  // Ask now, for a caller that wants to know whether the link answers (what
  // is typed, unit 03c): resolves with the round trip, or rejects when no
  // answer comes within `slowAfter`. The socket is not dropped by this.
  probe() {
    return new Promise((resolve, reject) => {
      if (!this.connected || !this.pings) return reject(new Error('not connected'));
      if (!this.pingOut) this._ping();
      const n = this.pingOut.n, limit = setTimeout(() => { this.probes.delete(n); reject(new Error('no answer')); }, slowAfter(this._trip()));
      (this.probes ||= new Map()).set(n, (ms) => { clearTimeout(limit); resolve(ms); });
    });
  }

  // Something arrived: the link is alive, whatever it was.
  _heard() {
    this.frameAwake = this._now();
    this.frameWall = Date.now();
    this.deciding = false;
    if (this.silent) { this.silent = false; this._back(); this._status(); }
  }

  // The link is known good again after a gap.
  _back() {
    if (this.gapFrom === null) return;
    this.lastGap = { ms: this._now() - this.gapFrom, at: Date.now() };
    this.gapFrom = null;
    this._emit('link.back', { ms: this.lastGap.ms });
  }

  // Nothing was heard for too long: this socket is given up, whether or not
  // the browser ever says it closed (a half-open one never does), and a new
  // one is made at once.
  _drop() {
    this.dropped += 1;
    this.gapFrom ??= this.frameAwake;
    const ws = this.ws;
    this._closed(ws);
    try { ws.close(); } catch (e) { /* gone already */ }
  }

  _closed(ws) {
    if (ws !== this.ws) return;
    this.ws = null;
    const was = this.connected;
    // A connection that held for a few seconds was a good one: the next
    // loss starts the schedule again.
    if (was && performance.now() - this.openedAt > 3000) { this.tries = 0; this.timeouts = 0; }
    this.connected = false;
    this.silent = false;
    this.pingOut = null;
    this.deciding = false;
    if (was) this.gapFrom ??= this._now();
    for (const p of this.pending.values()) { clearTimeout(p.limit); p.reject(Object.assign(new Error('connection lost'), { code: 'connection_lost' })); }
    this.pending.clear();
    // Every attachment was this connection's: its name names nothing now,
    // and whoever held it is told so before anything else can be typed.
    for (const c of this.chans.values()) c.onClosed && c.onClosed();
    this.chans.clear();
    for (const fn of this.settle || []) fn(false);
    this.settle = null;
    this._status();
    // An upgrade replaces the daemon in place: reconnect and carry on.
    if (this.authError) return;
    clearTimeout(this.retryTimer);
    this.retryTimer = setTimeout(() => { this.retryTimer = null; this.connect(); }, Math.max(retryIn(this.tries++), this.waitWhy ? 2000 : 0));
  }

  connect() {
    if (!this.settling) return this._open();
    return this.settling.then(() => { this.settling = null; return this._open(); });
  }

  _open() {
    return new Promise((resolve) => {
      if (this.ws) return resolve(this.connected);
      const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
      const ws = new WebSocket(`${proto}//${location.host}/api/ws`);
      ws.binaryType = 'arraybuffer';
      this.ws = ws;
      (this.settle ||= []).push(resolve);
      // Whoever opened the page hears of the first try; a later one that
      // succeeds is the connection coming back.
      const first = !this.tried;
      this.tried = true;
      const began = performance.now();
      // A connection that cannot be made on a half-open link neither opens
      // nor fails: it is given a time, and the next one is tried.
      const late = () => { if (ws === this.ws && !this.connected) { this.timeouts += 1; this._closed(ws); try { ws.close(); } catch (e) { /* gone */ } } };
      let limit = setTimeout(late, openWithin(this.timeouts));
      // `caps`: what this page understands that a page from before does not:
      // the error code `busy` (too many of this page's requests are waiting for
      // a station), and `input_refused` on a pane's channel.
      ws.onopen = () => {
        clearTimeout(limit);
        limit = setTimeout(late, signInWithin(performance.now() - began));
        // `instance`: this page (see INSTANCE). `names`: every pane this
        // page attaches is named by it.
        ws.send(JSON.stringify({ auth: { token: takeToken(), client: this.client, instance: this.instance, caps: ['busy', 'names'] } }));
      };
      ws.onmessage = (ev) => {
        if (ws !== this.ws) return;
        if (typeof ev.data !== 'string') { this._heard(); return this._binary(new Uint8Array(ev.data)); }
        const msg = JSON.parse(ev.data);
        if (msg.auth) {
          clearTimeout(limit);
          if (!msg.auth.ok) {
            const why = msg.auth.message || 'not allowed';
            // `again`: nothing is wrong with this browser. Said, and tried again.
            if (msg.auth.again) this.waitWhy = why; else this.authError = why;
            this._status();
            return;
          }
          // The station was upgraded while this page was open: load the new
          // Control Deck, so a fix on the station reaches the page by itself.
          if (this.build && msg.auth.build && msg.auth.build !== this.build) { location.reload(); return; }
          this.build = msg.auth.build || this.build;
          this.authError = null;
          this.waitWhy = null;
          this.station = msg.auth.station;
          this.version = msg.auth.version;
          this.device = typeof msg.auth.device === 'string' ? msg.auth.device : null;
          // A station from before, or one with the switch `deck_ping` off,
          // does not answer a ping: it is then not sent one, and nothing is
          // concluded from its silence, as before.
          this.pings = Array.isArray(msg.auth.caps) && msg.auth.caps.includes('ping');
          this.everConnected = true;
          this.connected = true;
          this.openedAt = performance.now();
          this.timeouts = 0;
          // Opening and signing in were two round trips: a first measure,
          // so that the first ping on a slow link is not judged by 1.5 s.
          if (!this.rtts.length) this.rtts.push((performance.now() - began) / 2);
          this._heard();
          this._back();
          this.call('events.subscribe', {}).catch(() => {});
          this._status();
          for (const fn of this.settle || []) fn(true);
          this.settle = null;
          if (!first) this._reconnected();
          return;
        }
        this._heard();
        if (msg.pong !== undefined) {
          if (this.pingOut && msg.pong === this.pingOut.n) {
            const ms = performance.now() - this.pingOut.t;
            this.rtts.push(ms);
            if (this.rtts.length > 8) this.rtts.shift();
            this.pingOut = null;
            const told = this.probes?.get(msg.pong);
            if (told) { this.probes.delete(msg.pong); told(ms); }
          }
          return;
        }
        if (msg.event) { for (const fn of this.listeners) fn(msg.event); return; }
        if (msg.events_lost) { for (const fn of this.listeners) fn({ kind: 'events.lost', data: {} }); return; }
        if (msg.chan !== undefined && msg.id === undefined) {
          const c = this.chans.get(msg.chan);
          if (c && msg.exited !== undefined) c.onExit && c.onExit(msg.exited);
          // The station this pane is on stopped answering the one serving
          // this page (`quiet_ms`: for how long it had said nothing), or
          // closed the connection. Numbers and a name from a station: a
          // whole number or nothing, a bounded name.
          if (c && msg.unreachable) c.onUnreachable && c.onUnreachable({ station: String(msg.station || '').slice(0, 64), quietMs: Number.isFinite(msg.quiet_ms) && msg.quiet_ms >= 0 ? Math.round(msg.quiet_ms) : null });
          // A pane on another station was resized there: who did it, and to what.
          if (c && msg.resized) c.onResized && c.onResized(msg.resized);
          // A stop threw away what this client had sent and the program had not been given.
          if (c && msg.input_discarded) c.onDiscarded && c.onDiscarded(Number(msg.input_discarded) || 0);
          // What was typed for a pane of another station was not sent: that
          // station is not taking it, and too much already waits.
          if (c && msg.input_refused) c.onRefused && c.onRefused(String(msg.station || '').slice(0, 64));
          // A watched conversation (chat.js): what is new, or that it ended.
          if (c && msg.conversation) c.onConversation && c.onConversation(msg.conversation);
          if (c && msg.conversation_end) c.onEnd && c.onEnd(msg.conversation_end);
          return;
        }
        const p = this.pending.get(msg.id);
        if (!p) return;
        this.pending.delete(msg.id);
        clearTimeout(p.limit);
        if (msg.error) p.reject(Object.assign(new Error(msg.error.message || 'error'), { code: msg.error.code }));
        else p.resolve(msg.result);
      };
      ws.onclose = () => { clearTimeout(limit); this._closed(ws); };
    });
  }

  _reconnected() { this.reconnects += 1; this.lastReconnect = Date.now(); this._emit('connection.restored'); }

  _binary(buf) {
    if (buf.length < 5) return;
    const chan = new DataView(buf.buffer, buf.byteOffset + 1, 4).getUint32(0, true);
    const c = this.chans.get(chan);
    if (!c) return;
    const payload = buf.subarray(5);
    if (buf[0] === FRAME_OUTPUT) c.onData(payload);
    else if (buf[0] === FRAME_RESYNC) c.onResync(payload);
  }

  // `within`: milliseconds this request is given. A question with no answer
  // by then is over, and says so ("alpha did not answer within 10 s"); an
  // answer that comes later is passed over. For what only reads: a change
  // may not be given up from this side alone (unit 03d).
  call(method, params = {}, { within, of } = {}) {
    return new Promise((resolve, reject) => {
      if (!this.connected) return reject(Object.assign(new Error('not connected'), { code: 'not_connected' }));
      const id = this.nextId++;
      const limit = within ? setTimeout(() => {
        if (!this.pending.delete(id)) return;
        reject(Object.assign(new Error(`${of || this.station} did not answer within ${Math.round(within / 1000)} s`), { code: 'no_answer' }));
      }, within) : null;
      this.pending.set(id, { resolve, reject, limit });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  // A file this station serves to its user (an uploaded font), over HTTP so
  // that the browser keeps it. The token goes in a header, as it never goes
  // in a URL.
  async file(path) {
    const res = await fetch(path, { headers: { authorization: 'Bearer ' + (localStorage.getItem('spandeck.token') || '') } });
    if (!res.ok) throw Object.assign(new Error(`${path}: ${res.status}`), { status: res.status });
    return res.arrayBuffer();
  }

  // Fire and forget (keystrokes).
  send(method, params) {
    if (this.connected) this.ws.send(JSON.stringify({ id: null, method, params }));
  }

  // Call a method on any station through this one. A question to another
  // station is given two seconds more than the same one here: the station
  // serving this page gives it up itself after ten (four to wait its turn,
  // six for the answer), and its words say more than this page could.
  on(station, method, params = {}, opts = {}) {
    if (!station || station === this.station) return this.call(method, params, opts);
    return this.call('peer.call', { station, method, params }, opts.within ? { within: opts.within + 2000, of: station } : opts);
  }

  // Attach to a pane under a name of this page's choosing (a station from
  // before gives a number of its own, and whichever the answer names is the
  // channel). `handlers.gone()`: whoever asked no longer wants it; it is let
  // go of here, and nothing is returned. One pane is attached by one request
  // at a time, each after the one before has been answered and, if unwanted,
  // let go of: so that at the pane's station this page's newer attachment
  // always follows its older one.
  async attach(station, pane, mode, handlers) {
    const key = `${station}/${pane}`, before = this.attaching.get(key);
    let done;
    const mine = new Promise(r => { done = r; });
    this.attaching.set(key, mine);
    try {
      if (before) await before;
      const res = await this.call('pane.attach', { pane, mode, station: station === this.station ? undefined : station, chan: this._name() });
      if (handlers.gone && handlers.gone()) { this.send('pane.detach', { chan: res.chan }); return null; }
      this.chans.set(res.chan, handlers);
      const snapshot = Uint8Array.from(atob(res.snapshot_b64), c => c.charCodeAt(0));
      // `client`: who this tab is at the pane's station. Through a gateway that
      // is a name the gateway made for this tab; here it is this tab's own.
      const client = typeof res.client === 'string' && res.client ? res.client : this.client;
      return { chan: res.chan, info: res.info, exited: res.exited, snapshot, client };
    } finally {
      if (this.attaching.get(key) === mine) this.attaching.delete(key);
      done();
    }
  }

  detach(chan) {
    this.chans.delete(chan);
    this.send('pane.detach', { chan });
  }
}

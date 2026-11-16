// The conversation view: a pane's second face (DECISIONS.md, "The
// conversation view"; the station's side is docs/conversation-api.md).
//
// A pane has its terminal, as always, and its conversation: what was said, as
// readable text, with the tool calls folded away. This file is that view, for
// the phone and for a pane's frame on a computer, and the phone's list of
// sessions that leads to it.
//
// Rules kept here:
// - Everything from a session file is data. It is put on the page as text
//   nodes (`h()`), and an agent's markdown goes through md.js, which builds
//   elements itself and never hands a string to the browser to parse.
// - This view never attaches to the pane, so it never sets the pane's size
//   ("One size per pane"): it only watches the conversation and sends what
//   the phone's message box already sends.
// - Buttons answer only what a hook holds (an approval). A question or a plan
//   is shown and answered in the terminal.
// - Times are the station's: an elapsed time is worked out from the clock the
//   station sent with its answer, plus the time since that answer arrived.

import { h, showMenu, toast, dialog, copyText, dot, stateOf, STATE_LABEL, fmtTokens, heldWords, RELEASE_SAYS } from './ui.js';
import { renderMarkdown } from './md.js';
import { sendAct } from './typed.js';
import { retryIn } from './api.js';

let env = null;
const convs = new Map();   // "station/pane id" -> Conv
const drafts = new Map();  // what was being written, when its view is not on the page
const termBoxes = new Map(); // the terminal face's message box on a phone, per pane
const titles = new Map();  // "station/pane id" -> the session's title, for the list
const noTitles = new Map(); // station -> when to ask it for titles again
let titlesAt = 0, titlesFor = '';

const keyOf = (st, p) => `${st}/${p.id}`;
const RUN = new Set(['tool', 'thinking', 'tasks']);
const STATUS = ['running', 'ok', 'done', 'error', 'denied', 'interrupted'];
const WAIT_KEYS = [['1', '1'], ['2', '2'], ['3', '3'], ['Enter', '\r'], ['Esc', '\x1b'], ['↑', '\x1b[A'], ['↓', '\x1b[B'], ['←', '\x1b[D'], ['→', '\x1b[C']];
const KEY_SAYS = { '↑': 'Up', '↓': 'Down', '←': 'Left', '→': 'Right' };
// Text commands an agent takes as a message (not its terminal-only pickers).
const SLASH = { claude: [['/compact', 'make room: summarise the conversation so far'], ['/clear', 'start a new conversation'], ['/context', 'what is filling the context'], ['/usage', 'how much of the plan is used'], ['/model ', 'change the model'], ['/effort ', 'how hard it thinks']] };
// The tests' stand-in agent speaks Claude Code's dialect (as in the archive).
SLASH.fake = SLASH.claude;
const NO_CONV = 'no conversation for this pane: ';
const canSpeak = () => 'speechSynthesis' in window && typeof window.SpeechSynthesisUtterance === 'function';

// ---- words ----

const dur = (ms) => {
  if (typeof ms !== 'number' || !(ms >= 0)) return '';
  const s = Math.round(ms / 1000);
  if (s < 1) return '<1 s';
  if (s < 60) return s + ' s';
  if (s < 3600) return `${Math.floor(s / 60)} m ${s % 60} s`;
  return `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} m`;
};
// How long ago, for a row of the list: "42 s", "5 m", "3 h", "2 d".
const since = (ms) => {
  if (!ms) return '';
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  return s < 60 ? s + ' s' : s < 3600 ? Math.floor(s / 60) + ' m' : s < 86400 ? Math.floor(s / 3600) + ' h' : Math.floor(s / 86400) + ' d';
};
const plural = (n, one, many) => `${n} ${n === 1 ? one : many || one + 's'}`;
const quiet = (text, cls) => h('div', { class: 'chat-quiet' + (cls ? ' ' + cls : '') }, text);
const idOrder = (id) => { const [a, b] = String(id).split('.'); return [Number(a) || 0, Number(b) || 0]; };

// Put `kids` in `parent` in this order, moving only what is out of place: a
// node that stays keeps its scroll position, its focus and what is typed in it.
function sync(parent, kids) {
  let at = parent.firstChild;
  for (const k of kids) {
    if (k === at) at = at.nextSibling;
    else parent.insertBefore(k, at);
  }
  while (at) { const next = at.nextSibling; at.remove(); at = next; }
}

// ---- which face a pane shows: this device's own choice ----

const FACES = 'spandeck.face';
let faces = {};
try { faces = JSON.parse(localStorage.getItem(FACES) || '{}') || {}; } catch (e) { faces = {}; }

// 'chat' or 'terminal'. A shell, and an agent whose station says it has no
// conversation, is its terminal. On a phone an agent opens as its
// conversation; on a computer a pane is its terminal until Chat is chosen.
export function faceOf(st, p) {
  if (!p || p.agent === 'shell') return 'terminal';
  const key = keyOf(st, p), c = convs.get(key);
  if (c && c.available === false) return 'terminal';
  return faces[key] === 'chat' || faces[key] === 'terminal' ? faces[key] : env.isPhone() ? 'chat' : 'terminal';
}

export function setFace(st, p, face) {
  const key = keyOf(st, p), c = convs.get(key), tb = termBoxes.get(key);
  // What was being written goes with you from one face to the other.
  if (c && face === 'terminal' && c.box.isConnected) {
    c.box.value ? drafts.set(key, c.box.value) : drafts.delete(key);
    if (tb) tb.value = c.box.value;
  }
  if (c && tb && face === 'chat' && tb.isConnected) { c.box.value = tb.value; c.typed(); }
  faces[key] = face;
  const keep = Object.entries(faces).slice(-200);
  try { localStorage.setItem(FACES, JSON.stringify(Object.fromEntries(keep))); } catch (e) { /* this device keeps no choice */ }
  env.update();
}

// The terminal face's message box on a phone, so a draft can follow the owner.
export function termBox(st, p, box) { termBoxes.set(keyOf(st, p), box); box.value = drafts.get(keyOf(st, p)) || ''; }

// "Chat" on a terminal's title bar, on a computer.
export function chatToggle(st, p) {
  if (!p || p.agent === 'shell') return null;
  return h('button', { class: 'mini face-btn', title: 'Show this pane as a conversation: what was said, with the tool calls folded away. The terminal is one click back.', onclick: () => setFace(st, p, 'chat') }, 'Chat');
}

// ---- what waits for the owner, across the fleet, in the order to take it ----

// Approvals oldest first, then agents waiting for something else, then
// errors, then agents that finished and have not been read.
export function queue(except) {
  const out = [], seen = new Set([except]);
  const add = (st, p, why) => { const key = keyOf(st, p); if (!seen.has(key)) { seen.add(key); out.push({ st, p, why }); } };
  for (const a of env.approvals().slice().sort((x, y) => (x.ts || 0) - (y.ts || 0))) {
    const p = env.findPane(a.station, a.pane);
    if (p) add(a.station, p, 'an approval');
  }
  const agents = env.panes().filter(x => x.p.agent !== 'shell').sort((x, y) => (x.p.state_since_ms || 0) - (y.p.state_since_ms || 0));
  for (const x of agents) if (x.p.state === 'needs_you') add(x.st, x.p, 'waiting for you');
  for (const x of agents) if (x.p.state === 'error') add(x.st, x.p, 'an error');
  for (const x of agents) if (x.p.unread && (x.p.state === 'done' || x.p.state === 'idle')) add(x.st, x.p, 'finished, not read');
  return out;
}

// ---- a link in a reply: shown before it is opened ----

function openLink(url) {
  let host = '';
  try { host = new URL(url).host; } catch (e) { return; }
  dialog('Open this link?', (close) => h('div', {},
    h('p', {}, 'The agent wrote a link to ', h('b', {}, host), '. It opens in a new tab:'),
    h('p', { class: 'pre link-dest' }, url),
    h('div', { class: 'row end' }, h('button', { onclick: close }, 'Cancel'),
      h('button', { onclick: () => copyText(url, 'Copied the address') }, 'Copy'),
      h('button', { class: 'primary', onclick: () => { close(); window.open(url, '_blank', 'noopener,noreferrer'); } }, 'Open'))));
}

// ---- one pane's conversation ----

class Conv {
  constructor(st, p) {
    this.st = st; this.p = p; this.key = keyOf(st, p);
    this.shell = p.agent === 'shell';
    this.items = []; this.byId = new Map(); this.ver = 0;
    this.session = null; this.cursor = 0; this.first = null; this.moreBefore = false;
    this.title = null; this.notUnderstood = 0; this.olderDropped = false;
    // null until the station has answered; a shell is known without asking.
    this.available = this.shell ? false : null;
    this.reason = this.shell ? NO_CONV + 'it runs a shell, not an agent' : '';
    this.live = null; this.liveAt = 0;
    this.chan = null; this.state = 'off'; this.problem = '';
    this.open = new Set(); this.revs = new Map(); this.nodes = new Map(); this.dockNodes = new Map();
    this.details = new Map(); this.full = new Map(); this.readTo = -1;
    this.stuck = true; this.top = 0; this.peekOpen = false; this.clocks = [];
    this.where = 'phone'; this.used = true; this.seeded = false;

    this.bar = h('header', { class: 'chat-bar' });
    this.log = h('div', { class: 'chat-log', role: 'log', 'aria-live': 'off', 'aria-label': 'Conversation', tabIndex: 0 });
    this.log.addEventListener('scroll', () => this.scrolled(), { passive: true });
    // Completed replies are said here, once each; the log itself is silent,
    // so a screen reader is not read every tool call as it happens.
    this.said = h('div', { class: 'sr-only', 'aria-live': 'polite' });
    this.acts = h('div', { class: 'chat-status' });
    this.peekEl = h('pre', { class: 'chat-peek', 'aria-label': 'The last lines of the terminal' });
    this.cards = h('div', { class: 'chat-cards' });
    this.buildComposer();
    this.dock = h('div', { class: 'chat-dock' }, this.acts, this.cards, this.composer);
    this.body = h('div', { class: 'chat-body' }, this.log, this.said, this.dock);
    this.draw();
  }

  // -------------------------------------------------------------- the stream

  async watch() {
    if (this.shell || this.disposed || this.state !== 'off' || !env.api.connected) return;
    this.state = 'opening';
    clearTimeout(this.retry);
    this.retry = null;
    const api = env.api, params = { pane: this.p.id, limit: 50 };
    if (this.st !== api.station) params.station = this.st;
    if (this.session) { params.session = this.session; params.after = this.cursor; }
    try {
      const r = await api.call('pane.conversation.watch', params);
      if (this.disposed) { api.send('pane.conversation.unwatch', { chan: r.chan }); return; }
      this.chan = r.chan;
      api.chans.set(r.chan, {
        onConversation: (a) => this.apply(a),
        onEnd: () => { this.unwatch(); this.problem = 'This pane was closed.'; this.draw(); },
        onUnreachable: () => { this.unwatch(); this.again(`${this.st} stopped answering. Trying again…`); },
      });
      this.state = 'watching';
      this.tries = 0;
      this.problem = '';
      this.apply(r);
    } catch (e) {
      this.state = 'off';
      if (this.disposed) return;
      // A station from before the conversation view has no such call.
      if (e.code === 'not_found' && /method/i.test(e.message || '')) {
        this.old = true;
        this.gone(`${NO_CONV}that station needs an update (${this.st} runs a spandeck from before the conversation view)`);
      } else if (e.code === 'not_found') this.gone(NO_CONV + 'it is not on its station any more');
      else this.again(`The conversation could not be loaded: ${e.message}. Trying again…`);
    }
  }

  gone(reason) {
    const was = this.available;
    this.available = false;
    this.reason = reason;
    this.draw();
    if (was !== false) settle(this);
  }

  again(text) {
    this.problem = text;
    this.draw();
    clearTimeout(this.retry);
    // By the schedule every reconnect uses (api.js): soon, then up to 5 s.
    this.retry = setTimeout(() => { this.retry = null; if (!this.disposed) this.watch(); }, Math.max(retryIn(this.tries = (this.tries || 0) + 1), 50));
  }

  unwatch() {
    if (this.chan !== null) { env.api.chans.delete(this.chan); env.api.send('pane.conversation.unwatch', { chan: this.chan }); }
    this.chan = null;
    this.state = 'off';
  }

  // The connection came back: ask for what came after the cursor, no more.
  restored() { this.chan = null; this.state = 'off'; this.watch(); }

  put(it) {
    if (!it || typeof it.id !== 'string') return;
    it._v = ++this.ver;
    const had = this.byId.get(it.id);
    this.byId.set(it.id, it);
    if (had) { this.items[this.items.indexOf(had)] = it; return; }
    const last = this.items[this.items.length - 1];
    this.items.push(it);
    const before = (a, b) => { const [x, y] = idOrder(a.id), [u, v] = idOrder(b.id); return x - u || y - v; };
    if (last && before(last, it) > 0) this.items.sort(before);
  }

  // One answer of the station: the first page, or what came after the cursor.
  apply(a) {
    if (!a || this.disposed) return;
    const was = this.available;
    if (a.available === false) {
      this.available = false;
      this.reason = typeof a.reason === 'string' ? a.reason : NO_CONV + 'the station did not say why';
      this.items = []; this.byId.clear(); this.session = null; this.cursor = 0; this.first = null; this.moreBefore = false; this.title = null;
    } else {
      const fresh = a.reset === true || was !== true || (this.session && a.session !== this.session);
      if (fresh) { this.items = []; this.byId.clear(); this.first = null; this.details.clear(); this.full.clear(); this.stuck = true; this.seeded = false; }
      this.available = true;
      this.session = a.session;
      const empty = !this.items.length;
      for (const it of a.items || []) this.put(it);
      for (const it of a.updated || []) this.put(it);
      if (typeof a.cursor === 'number') this.cursor = a.cursor;
      // Where the page above begins is known from the first page only.
      if (empty && (a.items || []).length) { this.first = a.first ?? null; this.moreBefore = a.more_before === true; }
      this.title = typeof a.title === 'string' && a.title ? a.title : null;
      this.notUnderstood = Number(a.not_understood) || 0;
      this.olderDropped = a.older_dropped === true;
    }
    if (a.live && typeof a.live === 'object') { this.live = a.live; this.liveAt = performance.now(); }
    this.draw();
    this.seeded = true;
    if (this.title) titles.set(this.key, this.title);
    // Whether there is a conversation decides which face is shown.
    if (was !== this.available) settle(this);
  }

  async older() {
    if (!this.moreBefore || this.paging || this.first === null || !this.session) return;
    this.paging = true;
    try {
      const a = await env.api.on(this.st, 'pane.conversation', { pane: this.p.id, session: this.session, before: this.first, limit: 50 });
      if (a.available !== true || a.reset || a.session !== this.session) return;
      const height = this.log.scrollHeight, top = this.log.scrollTop;
      for (const it of a.items || []) this.put(it);
      this.first = a.first ?? this.first;
      this.moreBefore = a.more_before === true && (a.items || []).length > 0;
      this.stuck = false;
      this.drawLog();
      // What was being read stays where it was: the page grew above it.
      this.log.scrollTop = top + (this.log.scrollHeight - height);
      this.top = this.log.scrollTop;
    } catch (e) { toast('Earlier messages could not be loaded: ' + e.message, 'error'); } finally { this.paging = false; }
  }

  // ----------------------------------------------------------------- reading

  scrolled() {
    if (!this.log.isConnected || !this.log.clientHeight) return;
    this.stuck = this.log.scrollHeight - this.log.scrollTop - this.log.clientHeight < 48;
    this.top = this.log.scrollTop;
    if (this.top < 160) this.older();
    this.maybeRead();
  }

  // After the page was drawn again: the log is where it was left, or at the
  // end if that is where the owner was.
  placed() {
    if (!this.log.isConnected) return;
    const to = this.stuck ? this.log.scrollHeight : this.top;
    if (Math.abs(this.log.scrollTop - to) > 1) this.log.scrollTop = to;
    this.maybeRead();
  }

  // The owner has seen the newest item: it is on screen and the page is in front.
  maybeRead() {
    if (this.available !== true || !this.live?.unread || !this.stuck || !this.cursor || this.readTo >= this.cursor) return;
    if (document.visibilityState !== 'visible' || !this.log.isConnected || !this.log.clientHeight) return;
    this.readTo = this.cursor;
    env.api.on(this.st, 'pane.read', { pane: this.p.id, upto: this.cursor }).catch(() => { this.readTo = -1; });
  }

  // ------------------------------------------------------------------- draw

  draw() { this.drawBar(); this.drawLog(); this.drawDock(); }

  rev(id) { return this.revs.get(id) || 0; }
  touch(id) { this.revs.set(id, this.rev(id) + 1); this.drawLog(); }
  toggle(flag, id) { this.open.has(flag) ? this.open.delete(flag) : this.open.add(flag); this.touch(id); }

  node(cache, used, key, sig, make) {
    let n = cache.get(key);
    if (!n || n.sig !== sig) { n = { sig, el: make() }; cache.set(key, n); }
    used.add(key);
    return n.el;
  }

  why() { return (this.reason || '').startsWith(NO_CONV) ? this.reason.slice(NO_CONV.length) : this.reason; }

  drawBar() {
    const p = this.p, phone = env.isPhone(), face = faceOf(this.st, p);
    const next = this.where === 'leaf' ? [] : queue(this.key);
    const title = this.title || titles.get(this.key) || p.name;
    const sub = [p.agent === 'shell' ? 'shell' : p.agent_name, this.st, p.project].filter(Boolean).join(' · ');
    const state = stateOf(p);
    const sig = JSON.stringify([phone, title, sub, state, p.needs_reason, face, this.available, this.reason, next.length, next[0] && keyOf(next[0].st, next[0].p), this.where, p.name]);
    if (sig === this.barSig) return;
    this.barSig = sig;
    const pick = (to) => h('button', { type: 'button', class: face === to ? 'on' : '', 'aria-pressed': String(face === to), onclick: (e) => { e.currentTarget.blur(); if (face !== to) setFace(this.st, p, to); } }, to === 'chat' ? 'Chat' : 'Terminal');
    const menu = (e) => {
      const b = e.currentTarget.getBoundingClientRect();
      env.paneMenu({ clientX: e.clientX || b.left, clientY: e.clientY || b.bottom + 2, preventDefault() {}, stopPropagation() {} }, this.st, this.p, this.where === 'phone' ? 'zoom' : this.where);
    };
    sync(this.bar, [
      phone ? h('a', { class: 'btn chat-back', href: '#/fleet', 'aria-label': 'Back to the list' }, '‹') : null,
      h('span', { class: 'dot ' + state, title: p.needs_reason || STATE_LABEL[state] || state }),
      h('div', { class: 'chat-title', title: `${p.name} · ${p.address}` }, h('b', {}, title), h('span', { class: 'muted' }, sub)),
      this.available === false
        ? h('span', { class: 'chat-why', role: 'note' }, 'Terminal only: ' + this.why())
        : h('div', { class: 'chat-switch', role: 'group', 'aria-label': 'Show this pane as' }, pick('chat'), pick('terminal')),
      next.length ? h('button', { type: 'button', class: 'chat-next', title: `Next: ${next[0].p.name} on ${next[0].st} (${next[0].why})`, 'aria-label': `Next of ${plural(next.length, 'pane')} that need you: ${next[0].p.name}, ${next[0].why}`,
        onclick: () => env.go(env.paneUrl(next[0].st, next[0].p.id)) }, 'Next ›', h('span', { class: 'count' }, next.length)) : null,
      h('button', { type: 'button', class: 'icon-btn chat-more', 'aria-label': 'Pane menu', title: 'Pane menu', 'aria-haspopup': 'menu', onclick: menu }, '⋯'),
    ].filter(Boolean));
  }

  drawLog() {
    const used = new Set(), out = [], cache = this.nodes;
    const one = (key, sig, make) => out.push(this.node(cache, used, key, String(sig), make));
    if (this.available === null) one('load', this.problem, () => quiet(this.problem || 'Loading the conversation…'));
    else if (this.available === false) one('none', this.reason, () => quiet(this.reason));
    else {
      if (this.problem) one('problem', this.problem, () => quiet(this.problem, 'warn'));
      if (this.moreBefore) one('older', '', () => h('button', { type: 'button', class: 'chat-older', onclick: () => this.older() }, 'Show earlier messages'));
      else if (this.olderDropped) one('dropped', '', () => quiet('The start of this conversation is too far back to show here.'));
      if (!this.items.length) one('empty', '', () => quiet('Nothing has been said in this conversation yet. Write below to start.'));
      const items = this.items, working = this.live?.state === 'working';
      let lastDone = null;
      for (let i = 0; i < items.length;) {
        const it = items[i];
        if (RUN.has(it.kind)) {
          let j = i;
          while (j < items.length && RUN.has(items[j].kind)) j += 1;
          const run = items.slice(i, j);
          one('run:' + it.id, run.map(x => `${x.id}.${x._v}.${this.rev(x.id)}`).join(','), () => this.runEl(run));
          i = j;
          continue;
        }
        // A reply is complete when the turn went on to something that is not
        // more of it, or the agent is no longer working.
        const next = items[i + 1];
        const done = it.kind === 'text' && (next ? !(RUN.has(next.kind) || next.kind === 'text') : !working);
        if (done) lastDone = it;
        one('i:' + it.id, `${it._v}.${this.rev(it.id)}.${done}`, () => this.itemEl(it, done));
        i += 1;
      }
      if (this.notUnderstood > 0) one('lost', this.notUnderstood, () => h('div', { class: 'chat-quiet warn' },
        `${plural(this.notUnderstood, 'record')} not shown: open the terminal to see everything. `,
        h('button', { type: 'button', class: 'mini', onclick: () => setFace(this.st, this.p, 'terminal') }, 'Open terminal')));
      // Said once, when it arrives; not for what was already there.
      if (lastDone && lastDone.id !== this.spoken) {
        if (this.seeded && this.spoken !== undefined) this.said.textContent = 'The agent replied: ' + String(lastDone.text || '').slice(0, 600);
        this.spoken = lastDone.id;
      } else if (!lastDone && this.spoken === undefined && this.available === true) this.spoken = null;
    }
    sync(this.log, out);
    for (const key of [...cache.keys()]) if (!used.has(key)) cache.delete(key);
    if (this.stuck) this.log.scrollTop = this.log.scrollHeight;
    this.maybeRead();
  }

  // A run of tool calls, folded into one line.
  runEl(run) {
    const tools = run.filter(x => x.kind !== 'thinking');
    const total = run.reduce((n, x) => n + (typeof x.duration_ms === 'number' ? x.duration_ms : 0), 0);
    if (!tools.length) return quiet('Thought' + (total ? ' ' + dur(total) : ''), 'thought');
    if (tools.length === 1) return this.toolEl(tools[0]);
    const flag = 'run:' + run[0].id, open = this.open.has(flag);
    const running = tools.some(x => x.status === 'running'), failed = tools.filter(x => x.status === 'error').length;
    const label = `${running ? 'Working: ' + plural(tools.length, 'tool') + ' so far' : 'Ran ' + plural(tools.length, 'tool')}${total ? ' · ' + dur(total) : ''}${failed ? ` · ${failed} failed` : ''}`;
    return h('div', { class: 'chat-run' + (open ? ' open' : '') },
      h('button', { type: 'button', class: 'chat-fold', 'aria-expanded': String(open), onclick: () => this.toggle(flag, run[0].id) }, h('span', {}, label), h('span', { class: 'chev', 'aria-hidden': 'true' }, open ? '▾' : '›')),
      open ? h('div', { class: 'chat-run-rows' }, tools.map(x => this.toolEl(x))) : null);
  }

  toolEl(it) {
    const flag = 't:' + it.id, open = this.open.has(flag);
    const status = STATUS.includes(it.status) ? it.status : 'other';
    const mark = { running: '…', ok: '✓', done: '✓', error: '✕', denied: '⊘', interrupted: '■', other: '·' }[status];
    const word = { running: 'running', error: 'failed', denied: 'denied', interrupted: 'stopped', other: String(it.status || '') }[status] || '';
    const name = it.kind === 'tasks' ? 'Task · ' + (it.name || 'agent') : String(it.name || 'tool');
    const changed = typeof it.added === 'number' || typeof it.removed === 'number' ? `+${it.added || 0} −${it.removed || 0}` : '';
    return h('div', { class: 'chat-tool ' + status },
      h('button', { type: 'button', class: 'chat-tool-row', 'aria-expanded': String(open), onclick: () => this.toggle(flag, it.id) },
        h('span', { class: 'tool-mark', 'aria-hidden': 'true' }, mark), h('b', {}, name), h('span', { class: 'tool-sum' }, it.summary || ''),
        changed ? h('span', { class: 'tool-note' }, changed) : null, word ? h('span', { class: 'tool-note ' + status }, word) : null,
        it.background ? h('span', { class: 'tool-note' }, 'in the background') : null,
        typeof it.duration_ms === 'number' ? h('span', { class: 'tool-note' }, dur(it.duration_ms)) : null,
        h('span', { class: 'chev', 'aria-hidden': 'true' }, open ? '▾' : '›')),
      open ? this.detailEl(it) : null);
  }

  // What a call was given and what came back, asked for when its row is opened.
  detailEl(it) {
    const d = this.details.get(it.id);
    if (!d || (d.v !== it._v && !d.asking)) this.askDetail(it);
    if (!d || (!d.res && !d.error)) return h('div', { class: 'chat-detail muted' }, 'Loading…');
    if (d.error) return h('div', { class: 'chat-detail warn-text' }, d.error);
    const r = d.res, parts = [];
    const fields = r.input && r.input.fields && typeof r.input.fields === 'object' ? Object.entries(r.input.fields) : [];
    const files = Array.isArray(r.diff?.files) ? r.diff.files : [];
    for (const [k, v] of fields) {
      if (files.length && ['file_path', 'old_string', 'new_string', 'content', 'edits', 'replace_all'].includes(k)) continue;
      if (k === 'command') parts.push(this.clamp(it, 'in:' + k, String(v), 'chat-cmd'));
      else parts.push(h('div', { class: 'chat-field' }, h('span', { class: 'muted small' }, k), this.clamp(it, 'in:' + k, String(v), 'chat-val')));
    }
    if (r.input?.more) parts.push(h('div', { class: 'muted small' }, 'Some of what it was given is too long to show here.'));
    if (files.length) parts.push(h('div', { class: 'chat-diff' },
      files.map(f => h('div', { class: 'diff-file' },
        h('div', { class: 'diff-name' }, String(f.file || ''), f.created ? ' (new file)' : f.deleted ? ' (deleted)' : ''),
        h('pre', { class: 'diff-lines' }, (Array.isArray(f.hunks) ? f.hunks : []).flatMap(k => [
          h('span', { class: 'diff-hunk' }, `@@ -${Number(k.old_start) || 0},${Number(k.old_lines) || 0} +${Number(k.new_start) || 0},${Number(k.new_lines) || 0} @@`),
          ...(Array.isArray(k.lines) ? k.lines : []).map(l => h('span', { class: String(l)[0] === '+' ? 'diff-add' : String(l)[0] === '-' ? 'diff-del' : 'diff-ctx' }, String(l) || ' '))])))),
      r.diff.more ? h('div', { class: 'muted small' }, 'The rest of this change is not shown here.') : null));
    if (r.output) {
      const pics = Number(r.output.images) || 0;
      parts.push(h('div', { class: 'muted small' }, 'Result' + (pics ? ` · ${plural(pics, 'picture')} not shown` : '')));
      parts.push(this.clamp(it, 'out', r.output.text || '(nothing was printed)', 'chat-out', r.output.next !== null && r.output.next !== undefined ? () => this.moreOutput(it) : null));
    } else if (it.status === 'running') parts.push(h('div', { class: 'muted small' }, 'No result yet.'));
    return h('div', { class: 'chat-detail' }, parts.length ? parts : h('span', { class: 'muted' }, 'Nothing more is recorded for this call.'));
  }

  // Long text is cut, with "Show more": first what is already here, then the
  // station's next page of it.
  clamp(it, what, text, cls, fetchMore) {
    const flag = `${what}:${it.id}`, open = this.open.has(flag);
    const lines = text.split('\n'), long = lines.length > 14 || text.length > 1600;
    const shown = open || !long ? text : lines.slice(0, 14).join('\n').slice(0, 1600);
    const more = long && !open ? () => this.toggle(flag, it.id) : fetchMore;
    return h('div', { class: 'chat-clamp' }, h('pre', { class: cls }, shown),
      more ? h('button', { type: 'button', class: 'mini', onclick: more }, 'Show more') : null);
  }

  async askDetail(it) {
    const d = this.details.get(it.id) || {};
    d.asking = true; d.v = it._v;
    this.details.set(it.id, d);
    try { d.res = await env.api.on(this.st, 'pane.conversation.detail', { pane: this.p.id, item: it.id, session: this.session }); d.error = null; } catch (e) { if (!d.res) d.error = 'The detail could not be loaded: ' + e.message; }
    d.asking = false;
    if (!this.disposed) this.touch(it.id);
  }

  async moreOutput(it) {
    const d = this.details.get(it.id), o = d?.res?.output;
    if (!o || o.next === null || o.next === undefined || d.asking) return;
    d.asking = true;
    try {
      const r = await env.api.on(this.st, 'pane.conversation.detail', { pane: this.p.id, item: it.id, session: this.session, offset: o.next });
      o.text = (o.text || '') + (r.output?.text || '');
      o.next = r.output ? r.output.next : null;
    } catch (e) { toast(e.message, 'error'); }
    d.asking = false;
    if (!this.disposed) this.touch(it.id);
  }

  // The whole of a text that was cut at the station's limit.
  async whole(it) {
    let text = '', offset = 0;
    try {
      for (let page = 0; page < 12 && offset !== null && offset !== undefined; page += 1) {
        const r = await env.api.on(this.st, 'pane.conversation.detail', { pane: this.p.id, item: it.id, session: this.session, offset });
        text += r.text?.text || '';
        offset = r.text ? r.text.next : null;
      }
      this.full.set(it.id, text);
      if (!this.disposed) this.touch(it.id);
    } catch (e) { toast(e.message, 'error'); }
  }

  itemEl(it, done) {
    const text = this.full.get(it.id) ?? (typeof it.text === 'string' ? it.text : '');
    const rest = it.more && !this.full.has(it.id) ? h('button', { type: 'button', class: 'mini', onclick: () => this.whole(it) }, 'Show all of it') : null;
    const md = (src) => renderMarkdown(src, { openLink, copy: (t) => copyText(t) });
    if (it.kind === 'user') {
      const from = { you: '', queued: 'sent when the agent was free', agent: `from ${it.from || 'another agent'}`, task: 'from a background task', schedule: 'a scheduled wake-up', system: 'from the agent program' };
      const mine = !it.origin || it.origin === 'you' || it.origin === 'queued';
      const label = it.origin in from ? from[it.origin] : String(it.origin || '');
      return h('div', { class: 'chat-user' + (mine ? '' : ' other') },
        label ? h('div', { class: 'chat-from' }, label) : null,
        h('div', { class: 'chat-bubble' }, text, it.images ? h('div', { class: 'chat-from' }, plural(Number(it.images) || 0, 'picture') + ' attached') : null), rest);
    }
    if (it.kind === 'text') {
      const body = md(text);
      const speak = (e) => readAloud(body, e.currentTarget);
      return h('div', { class: 'chat-text' + (it.phase === 'commentary' ? ' commentary' : '') }, body, rest,
        done ? h('div', { class: 'chat-acts' },
          h('button', { type: 'button', class: 'chat-act', onclick: () => copyText(text, 'Copied the reply') }, 'Copy'),
          canSpeak() ? h('button', { type: 'button', class: 'chat-act', 'aria-pressed': 'false', onclick: speak }, 'Read aloud') : null) : null);
    }
    if (it.kind === 'question') {
      const state = { waiting: ' · waiting for an answer', dismissed: ' · not answered' }[it.status] || '';
      return h('div', { class: 'chat-box-item' }, h('div', { class: 'chat-from' }, 'The agent asked' + state),
        (Array.isArray(it.questions) ? it.questions : []).map(q => h('div', { class: 'chat-q' },
          q.header ? h('span', { class: 'chip' }, String(q.header)) : null, h('b', {}, String(q.question || '')),
          h('ul', {}, (Array.isArray(q.options) ? q.options : []).map(o => h('li', {}, String(o.label || ''), o.description ? h('span', { class: 'muted' }, ' — ' + o.description) : null))))),
        (Array.isArray(it.answers) ? it.answers : []).map(a => h('div', { class: 'chat-answer' }, 'Answered: ', h('b', {}, String(Array.isArray(a) ? a[1] : a)))));
    }
    if (it.kind === 'plan') {
      const state = { waiting: 'waiting for your answer', approved: 'approved', rejected: 'not approved', dismissed: 'set aside' }[it.status] || String(it.status || '');
      return h('div', { class: 'chat-box-item' }, h('div', { class: 'chat-from' }, 'The agent proposed a plan' + (state ? ' · ' + state : '')), md(text), rest);
    }
    if (it.kind === 'notice') {
      const flag = 'n:' + it.id, open = this.open.has(flag);
      const fold = (label, body) => h('div', { class: 'chat-run' + (open ? ' open' : '') },
        h('button', { type: 'button', class: 'chat-fold', 'aria-expanded': String(open), onclick: () => this.toggle(flag, it.id) }, h('span', {}, label), h('span', { class: 'chev', 'aria-hidden': 'true' }, open ? '▾' : '›')),
        open ? body() : null);
      switch (it.what) {
        case 'turn_ended': return quiet('Turn ended' + (typeof it.duration_ms === 'number' ? ' · ' + dur(it.duration_ms) : ''));
        case 'interrupted': return quiet('Stopped before it finished');
        case 'compacted': return quiet('The conversation was compacted' + (it.trigger === 'auto' ? ' by itself' : '')
          + (typeof it.pre_tokens === 'number' && typeof it.post_tokens === 'number' ? ` · ${fmtTokens(it.pre_tokens)} to ${fmtTokens(it.post_tokens)} tokens` : ''));
        case 'summary': return fold('Summary of the conversation before this', () => h('div', { class: 'chat-detail' }, md(text), rest));
        case 'command_output': return fold('What the command printed', () => h('div', { class: 'chat-detail' }, h('pre', { class: 'chat-out' }, text)));
        case 'error': return quiet('Error: ' + text, 'err');
        case 'away_summary': return quiet('While you were away: ' + text);
        case 'task': return quiet(`A background task ${it.level || 'reported'}: ${text}`);
        case 'info': return quiet(text, it.level === 'warning' ? 'warn' : '');
        default: return quiet(String(it.what || 'notice'));
      }
    }
    // A kind from a later version: named, never an error.
    return quiet(String(it.kind || 'item'));
  }

  // The part above the message box: what the agent is doing, and what it
  // waits for. Each piece is rebuilt only when what it shows changes, so a
  // reason being typed into a card stays, and so does the keyboard.
  drawDock() {
    const L = this.live || {}, p = this.p, used = new Set(), cache = this.dockNodes;
    const exited = L.exited === true || p.exited;
    const working = L.state === 'working' && !exited;
    const subs = Array.isArray(L.subagents) ? L.subagents : [];
    const approvals = this.available === true && Array.isArray(L.approvals) ? L.approvals.filter(a => a && typeof a.approval === 'string') : [];
    const asked = this.available === true ? this.items.filter(x => (x.kind === 'question' || x.kind === 'plan') && x.status === 'waiting') : [];
    const blocked = !exited && this.available === true && !approvals.length && !asked.length && (L.state === 'needs_you' || L.state === 'error');
    // A banner that waits in the terminal shows the terminal's last lines.
    if (blocked && !this.wasBlocked) this.peekOpen = true;
    if (!blocked && this.wasBlocked && !working) this.peekOpen = false;
    this.wasBlocked = blocked;
    this.clocks = [];
    const status = [];
    if (working) {
      const running = [...this.items].reverse().find(x => x.kind === 'tool' && x.status === 'running');
      const what = L.tool ? `${L.tool}${running?.summary ? ': ' + running.summary : ''}` : '';
      const from = L.tool && L.tool_since_ms ? L.tool_since_ms : L.state_since_ms;
      status.push(this.node(cache, used, 'work', `${what}|${from}|${this.peekOpen}`, () => {
        const time = h('span', { class: 'act-time' });
        return h('div', { class: 'chat-activity', dataset: { from: String(from || '') } },
          h('span', { class: 'dot working' }), h('span', { class: 'act-text' }, 'Working' + (what ? ' · ' + what : '')), time,
          h('button', { type: 'button', class: 'mini act-peek', 'aria-expanded': String(this.peekOpen), title: 'The last lines of the terminal', onclick: () => { this.peekOpen = !this.peekOpen; this.drawDock(); if (this.peekOpen) this.peek(); } }, this.peekOpen ? 'Hide peek' : 'Peek'));
      }));
      const el = status[status.length - 1];
      if (from && L.now_ms) this.clocks.push({ el: el.querySelector('.act-time'), base: L.now_ms - from });
    }
    if (subs.length) status.push(this.node(cache, used, 'subs', subs.length, () => h('button', { type: 'button', class: 'chat-tasks', onclick: () => this.tasksSheet() }, plural(subs.length, 'running task'), ' ›')));
    if (this.peekOpen && (working || blocked)) { status.push(this.peekEl); if (!this.peeked) this.peek(); } else this.peeked = false;
    sync(this.acts, status);

    const cards = [];
    for (const a of approvals) cards.push(this.node(cache, used, 'ap:' + a.approval, a.summary, () => this.approvalEl(a)));
    for (const it of asked) cards.push(this.node(cache, used, 'ask:' + it.id, it._v, () => h('div', { class: 'chat-card ask' },
      h('b', {}, it.kind === 'plan' ? 'The agent proposed a plan and waits for your answer' : 'The agent asked you a question'),
      it.kind === 'plan' ? null : (Array.isArray(it.questions) ? it.questions : []).map(q => h('div', {}, String(q.question || ''))),
      h('div', { class: 'muted small' }, 'Answering from here is not built yet: the choices are in the terminal.'),
      h('button', { type: 'button', class: 'primary', onclick: () => setFace(this.st, p, 'terminal') }, 'Answer in the terminal'))));
    if (blocked) cards.push(this.node(cache, used, 'wait', `${L.state}|${L.needs_reason}`, () => h('div', { class: 'chat-card wait' + (L.state === 'error' ? ' err' : '') },
      h('b', {}, (L.state === 'error' ? 'Stopped on an error: ' : 'Waiting in the terminal: ') + (L.needs_reason || 'it needs you')),
      h('div', { class: 'wait-keys', role: 'group', 'aria-label': 'Keys for the terminal' },
        WAIT_KEYS.map(([label, bytes]) => h('button', { type: 'button', class: 'key', 'aria-label': KEY_SAYS[label] || label, onclick: () => this.key1(bytes) }, label)),
        h('button', { type: 'button', class: 'primary', onclick: () => setFace(this.st, p, 'terminal') }, 'Open terminal')))));
    // Held: stopped and staying stopped, or waiting for you after a restart.
    if (L.held && !exited) cards.push(this.node(cache, used, 'held', JSON.stringify(L.held), () => h('div', { class: 'chat-card wait' },
      h('b', {}, heldWords(L.held)),
      h('div', { class: 'muted small' }, 'It takes no mail from other agents until you release it. Sending it a message releases it too.'),
      h('button', { type: 'button', class: 'primary', title: RELEASE_SAYS, onclick: () => env.rpc(this.st, 'pane.release', { pane: p.id }).catch(() => {}) }, 'Release'))));
    if (exited) cards.push(this.node(cache, used, 'ended', '', () => quiet('This pane has ended. Its conversation is still here to read.')));
    sync(this.cards, cards);
    for (const key of [...cache.keys()]) if (!used.has(key)) cache.delete(key);
    this.composer.hidden = !!exited || this.available !== true;
    this.working = working;
    this.sendState();
    this.tick();
  }

  approvalEl(a) {
    const reason = h('input', { placeholder: 'reason (optional)', 'aria-label': 'Reason' });
    const answer = (allow) => env.rpc(this.st, 'approval.answer', { id: a.approval, allow, reason: reason.value || undefined }).then(() => toast(allow ? 'Allowed' : 'Denied'));
    const what = a.what === 'route' ? 'It asks to reach another station' : 'It asks before it runs this';
    return h('div', { class: 'approval chat-card' },
      h('div', { class: 'approval-what' }, h('b', {}, what), h('div', { class: 'mono' }, String(a.summary || ''))),
      h('div', { class: 'row' }, reason,
        h('button', { type: 'button', class: 'primary', onclick: () => answer(true) }, 'Allow'),
        h('button', { type: 'button', class: 'danger', onclick: () => answer(false) }, 'Deny')));
  }

  key1(bytes) {
    env.api.on(this.st, 'pane.input', { pane: this.p.id, data: bytes }).then(() => setTimeout(() => this.peek(), 250)).catch(e => toast(e.message, 'error'));
  }

  // The elapsed time, by the station's clock (docs/conversation-api.md).
  tick() {
    const waited = performance.now() - this.liveAt;
    for (const c of this.clocks) if (c.el) c.el.textContent = ' · ' + dur(Math.max(0, c.base + waited));
  }

  async peek() {
    this.peeked = true;
    try {
      const lines = (await env.api.on(this.st, 'pane.text', { pane: this.p.id })).lines.map(l => String(l).replace(/\s+$/, ''));
      while (lines.length && !lines[lines.length - 1]) lines.pop();
      this.peekEl.textContent = lines.slice(-8).join('\n') || '(the terminal is empty)';
      this.peekEl.scrollTop = this.peekEl.scrollHeight;
    } catch (e) { this.peekEl.textContent = 'The terminal could not be read: ' + e.message; }
  }

  tasksSheet() {
    const L = this.live || {}, now = (L.now_ms || 0) + (performance.now() - this.liveAt);
    const subs = Array.isArray(L.subagents) ? L.subagents : [];
    const asked = this.items.filter(x => x.kind === 'tasks' && x.status === 'running');
    const box = dialog('Running tasks', () => h('div', { class: 'list' },
      subs.map(s => h('div', { class: 'note row' }, h('span', { class: 'dot working' }), h('b', {}, String(s.kind || 'agent')), h('span', { class: 'grow' }), h('span', { class: 'muted' }, s.since_ms ? 'for ' + dur(now - s.since_ms) : ''))),
      asked.length ? h('h3', {}, 'What they were asked') : null,
      asked.map(t => h('div', { class: 'note' }, h('b', {}, String(t.name || 'agent')), ' ', String(t.summary || ''))),
      h('p', { class: 'muted small' }, `Sub-agents ${this.p.name} started itself. They stop when its turn is stopped.`)));
    box.body.parentNode.classList.add('sheet');
  }

  // ---------------------------------------------------------- the message box

  buildComposer() {
    const p = this.p;
    const Speech = window.SpeechRecognition || window.webkitSpeechRecognition;
    this.box = h('textarea', { class: 'chat-box', rows: 1, placeholder: Speech ? 'Message or hold to talk' : 'Message', 'aria-label': 'Message', enterkeyhint: 'send', dataset: { keep: 'chat:' + this.key } });
    this.box.value = drafts.get(this.key) || '';
    this.box.addEventListener('input', () => this.typed());
    // On a computer Enter sends and Shift+Enter starts a line; a phone's
    // keyboard has Send beside the box.
    this.box.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !env.isPhone()) { e.preventDefault(); this.send(); } });
    this.sendBtn = h('button', { type: 'submit', class: 'primary chat-send' }, 'Send');
    // A message that was not sent: why, while its text stays in the box.
    this.sendSaid = h('p', { class: 'send-said', role: 'alert', hidden: true });
    this.slash = h('div', { class: 'chat-slash', role: 'group', 'aria-label': 'Commands', hidden: true });

    // Dictation fills the box; nothing is sent until Send. A tap starts and
    // stops it; holding the button talks for as long as it is held.
    let rec = null, held = false, timer = null;
    const mic = h('button', { type: 'button', class: 'mic chat-mic', title: 'Dictate: tap to start and stop, or hold to talk', 'aria-label': 'Dictate', 'aria-pressed': 'false' }, '🎤');
    const stop = () => { if (rec) rec.stop(); };
    const start = () => {
      if (rec) return;
      if (!Speech) {
        toast(window.isSecureContext ? 'Dictation is not available here. Use the microphone on the keyboard.'
          : 'Dictation is not available here: it needs the HTTPS address of this station. Use the microphone on the keyboard.', 'error');
        return;
      }
      const r = new Speech(), before = this.box.value ? this.box.value.replace(/\s*$/, ' ') : '';
      r.lang = navigator.language || 'en-US';
      r.interimResults = true;
      r.continuous = true;
      r.onresult = (e) => { this.box.value = before + Array.from(e.results, x => x[0].transcript).join(''); this.typed(); };
      r.onerror = (e) => { if (e.error !== 'aborted' && e.error !== 'no-speech') toast(e.error === 'not-allowed' ? 'Dictation needs microphone permission, and the HTTPS address of this station.' : 'Dictation stopped: ' + e.error, 'error'); };
      r.onend = () => { rec = null; mic.classList.remove('on'); mic.setAttribute('aria-pressed', 'false'); };
      try { r.start(); rec = r; mic.classList.add('on'); mic.setAttribute('aria-pressed', 'true'); } catch (e) { toast('Dictation is not available here. Use the microphone on the keyboard.', 'error'); }
    };
    mic.addEventListener('pointerdown', () => { held = false; clearTimeout(timer); timer = setTimeout(() => { held = true; start(); }, 350); });
    const up = () => { clearTimeout(timer); if (held) stop(); };
    mic.addEventListener('pointerup', up);
    mic.addEventListener('pointercancel', up);
    mic.addEventListener('pointerleave', up);
    mic.addEventListener('contextmenu', (e) => e.preventDefault());
    // The tap, and the keyboard's way to the same thing.
    mic.addEventListener('click', () => { if (held) { held = false; return; } rec ? stop() : start(); });
    this.stopTalking = () => { clearTimeout(timer); if (rec) rec.abort(); };

    const plus = h('button', { type: 'button', class: 'chat-plus', title: 'Attach a photo or a file', 'aria-label': 'Attach a photo or a file', 'aria-haspopup': 'menu' }, '+');
    plus.onclick = () => {
      const b = plus.getBoundingClientRect();
      const pick = (camera) => {
        const inp = h('input', { type: 'file', class: 'copy-buffer', tabIndex: -1, 'aria-hidden': 'true' });
        if (camera) { inp.accept = 'image/*'; inp.setAttribute('capture', 'environment'); } else inp.multiple = true;
        inp.addEventListener('change', () => { [...(inp.files || [])].forEach(f => this.upload(f)); inp.remove(); });
        document.getElementById('overlays').append(inp);
        inp.click();
      };
      showMenu(b.left, b.top - 120, [{ title: `Send to ${p.name}` },
        { label: 'A photo or a file…', action: () => pick(false) },
        { label: 'Take a photo', action: () => pick(true) },
        { note: `It is saved on ${this.st} and its path is typed for the agent.` }]);
    };
    this.composer = h('form', { class: 'chat-composer', onsubmit: (e) => { e.preventDefault(); this.working && !this.box.value.trim() ? this.stopTurn() : this.send(); } },
      this.slash, this.sendSaid, h('div', { class: 'chat-row' }, plus, this.box, mic, this.sendBtn));
  }

  typed() {
    const v = this.box.value;
    v ? drafts.set(this.key, v) : drafts.delete(this.key);
    if (!this.sending) this.sendSaid.hidden = true;
    // As tall as what is written, up to a limit the style sheet sets.
    this.box.style.height = 'auto';
    this.box.style.height = Math.min(this.box.scrollHeight + 2, 240) + 'px';
    const cmds = SLASH[this.p.agent] || [];
    const show = /^\/[a-z-]*$/.test(v) ? cmds.filter(([c]) => c.startsWith(v)) : [];
    const sig = show.map(([c]) => c).join(' ');
    if (sig !== this.slashSig) {
      this.slashSig = sig;
      this.slash.hidden = !show.length;
      this.slash.replaceChildren(...show.map(([c, says]) => h('button', { type: 'button', class: 'chip slash', title: says, onclick: () => { this.box.value = c; this.typed(); this.box.focus(); } }, c.trim())));
    }
    this.sendState();
  }

  // While the agent works and nothing is written, the button stops the turn.
  sendState() {
    const stop = !!this.working && !this.box.value.trim();
    if (this.sendBtn.dataset.stop === String(stop)) return;
    this.sendBtn.dataset.stop = String(stop);
    this.sendBtn.textContent = stop ? 'Stop' : 'Send';
    this.sendBtn.className = (stop ? 'danger' : 'primary') + ' chat-send';
    this.sendBtn.title = stop ? 'Stop this turn. The agent stays.' : '';
  }

  // Sent the way the phone's message box sends: as one paste, then Enter.
  // One deliberate act (typed.js): sent now or not at all, and refused by
  // the station when it arrives more than ten seconds late. Not sent, or
  // not known to be: what was written stays in the box, which says why.
  async send() {
    const text = this.box.value.trim();
    if (!text || this.sending) return;
    this.sending = true;
    this.sendSaid.hidden = true;
    this.sendBtn.disabled = true;
    this.sendBtn.dataset.stop = 'sending';
    this.sendBtn.textContent = 'Sending…';
    let failed = null;
    try {
      await sendAct(env.api, this.st, 'pane.paste', { pane: this.p.id, text, enter: true });
      this.box.value = '';
      this.stuck = true;
      this.placed();
    } catch (e) { failed = e; } finally {
      this.sending = false;
      this.sendBtn.disabled = false;
      this.typed();
      this.sendState();
    }
    if (failed) { this.sendSaid.textContent = `${failed.message} Your message is still in the box.`; this.sendSaid.hidden = false; }
  }

  stopTurn() { env.rpc(this.st, 'pane.stop', { pane: this.p.id }).then(() => toast('Stopped. It takes no mail until you release it or send it a message.')).catch(() => {}); }

  // A photo or a file: the station writes it and types its path for the agent.
  async upload(file) {
    if (file.size > 64 * 1024 * 1024) return toast(`${file.name} is larger than 64 MB`, 'error');
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      let bin = '';
      for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
      const res = await env.api.on(this.st, 'pane.drop_file', { pane: this.p.id, name: file.name || 'photo.jpg', data_b64: btoa(bin) });
      toast(`Saved on ${this.st} and its path typed for the agent: ${res.path}`);
    } catch (e) { toast(`Could not send ${file.name}: ${e.message}`, 'error'); }
  }

  // -------------------------------------------------------------- on the page

  // The whole screen of a phone.
  phoneMain() {
    this.main ||= h('main', { class: 'phone phone-chat' });
    sync(this.main, [this.bar, this.body]);
    return this.main;
  }

  // A pane's frame on a computer: where its terminal would be.
  frame() {
    this.frameEl ||= h('div', { class: 'pane chat-pane', dataset: { pane: this.p.id } });
    sync(this.frameEl, [this.bar, this.body]);
    return this.frameEl;
  }

  dispose() {
    this.disposed = true;
    clearTimeout(this.retry);
    this.unwatch();
    this.stopTalking();
    if (this.box.value) drafts.set(this.key, this.box.value); else drafts.delete(this.key);
    convs.delete(this.key);
  }
}

// There is, or is no longer, a conversation for a pane: the page decides
// again which face to show. On a computer a pane that turned out to have none
// goes back to its terminal and says why.
function settle(c) {
  if (c.available === false && !env.isPhone() && faces[c.key] === 'chat') {
    delete faces[c.key];
    try { localStorage.setItem(FACES, JSON.stringify(faces)); } catch (e) { /* nothing kept */ }
    toast(c.reason, 'error');
  }
  env.update();
}

let speaking = null;
function readAloud(el, btn) {
  const synth = window.speechSynthesis, was = speaking;
  if (was) { synth.cancel(); was.done(); }
  if (was && was.btn === btn) return;
  // Code is not read out: a block of it is noise to listen to.
  const copy = el.cloneNode(true);
  for (const c of copy.querySelectorAll('.md-code')) c.replaceWith(document.createTextNode(' (a block of code) '));
  const u = new window.SpeechSynthesisUtterance(copy.textContent || '');
  u.lang = navigator.language || 'en-US';
  const me = { btn, done: () => { if (speaking === me) speaking = null; btn.textContent = 'Read aloud'; btn.setAttribute('aria-pressed', 'false'); } };
  u.onend = me.done;
  u.onerror = me.done;
  speaking = me;
  btn.textContent = 'Stop reading';
  btn.setAttribute('aria-pressed', 'true');
  synth.speak(u);
}

// ---- for the page (app.js) ----

export function initChat(e) {
  env = e;
  env.api.onEvent((ev) => { if (ev.kind === 'connection.restored') for (const c of convs.values()) c.restored(); });
  document.addEventListener('visibilitychange', () => { for (const c of convs.values()) c.maybeRead(); });
  setInterval(() => {
    for (const c of convs.values()) {
      if (!c.body.isConnected) continue;
      c.tick();
      if (c.peekOpen && c.peekEl.isConnected && document.visibilityState === 'visible') c.peek();
    }
  }, 1000);
}

// The conversation of a pane that is on the page in this draw. `where`:
// 'phone', 'zoom' (a pane at full size) or 'leaf' (a pane of a layout).
export function conversation(st, p, where) {
  const key = keyOf(st, p);
  let c = convs.get(key);
  if (!c) { c = new Conv(st, p); convs.set(key, c); }
  c.p = p;
  c.where = where;
  c.used = true;
  if (c.state === 'off' && !c.retry && !c.old) c.watch();
  c.drawBar();
  c.drawDock();
  return c;
}

export function chatBeforeRender() { for (const c of convs.values()) c.used = false; }
// After the page was drawn: a conversation that left the page is let go of,
// and each one still on it is where it was scrolled to.
export function chatAfterRender() {
  for (const c of [...convs.values()]) {
    if (!c.used) c.dispose();
    else c.placed();
  }
}

// ---- the phone's list of sessions ----

const HOME = 'spandeck.home';
export function homeMode() { try { return localStorage.getItem(HOME) === 'stations' ? 'stations' : 'recent'; } catch (e) { return 'recent'; } }
export function homeSwitch() {
  const mode = homeMode();
  const pick = (to, label) => h('button', { type: 'button', class: mode === to ? 'on' : '', 'aria-pressed': String(mode === to), dataset: { keep: 'home:' + to },
    onclick: () => { try { localStorage.setItem(HOME, to); } catch (e) { /* this time only */ } env.update(); } }, label);
  return h('div', { class: 'chat-switch home-switch', role: 'group', 'aria-label': 'List the sessions' }, pick('recent', 'Recent'), pick('stations', 'By station'));
}

// Each station is asked for its sessions' titles: the owner's call, since a
// title is made from what was said. A station from before this is not asked
// again for a while, and its panes go by their names.
function loadTitles() {
  // Asked when the sessions on the list change, and every few seconds
  // besides: an agent names its session a while after it starts.
  const now = env.panes().map(x => `${x.st}/${x.p.id}/${x.p.session_id || ''}`).join(' ');
  if (now === titlesFor && Date.now() - titlesAt < 4000) return;
  titlesFor = now;
  titlesAt = Date.now();
  for (const st of env.onlineStations()) {
    if ((noTitles.get(st) || 0) > Date.now()) continue;
    env.api.on(st, 'pane.titles', {}).then((r) => {
      let changed = false;
      const got = r && r.titles && typeof r.titles === 'object' ? r.titles : {};
      for (const [id, t] of Object.entries(got)) {
        if (typeof t !== 'string' || !t || titles.get(`${st}/${id}`) === t) continue;
        titles.set(`${st}/${id}`, t.slice(0, 200));
        changed = true;
      }
      if (changed) env.update();
    }).catch(() => noTitles.set(st, Date.now() + 600000));
  }
}

const rowDoes = (p) => {
  const s = stateOf(p);
  if (s === 'needs_you') return p.needs_reason ? 'needs you: ' + p.needs_reason : 'needs you';
  if (s === 'working') return 'working ' + since(p.state_since_ms);
  if (s === 'exited') return 'ended';
  if (s === 'error') return 'error' + (p.needs_reason ? ': ' + p.needs_reason : '');
  return `${STATE_LABEL[s] || s} ${since(p.state_since_ms)} ago`;
};

// One pane as a row of the list: its session's title when it has one, where
// it is and what it is doing, and a dot when it has said something not read.
export function sessionRow(st, p) {
  const title = titles.get(keyOf(st, p)) || p.name;
  const where = [st, p.project, rowDoes(p)].filter(Boolean).join(' · ');
  return h('a', { class: 'phone-row session-row' + (p.unread ? ' unread' : ''), href: env.paneUrl(st, p.id), dataset: { pane: p.id },
    'aria-label': `${title}, ${p.agent_name || p.agent}, ${where}${p.unread ? ', not read' : ''}` },
    dot(p), env.paneMark(p),
    h('div', { class: 'grow session-text' }, h('b', {}, title), h('div', { class: 'muted small' }, where)),
    p.unread ? h('span', { class: 'unread-dot', title: 'It has said something you have not read' }) : null,
    h('span', { 'aria-hidden': 'true' }, '›'));
}

// Every agent pane in the fleet, the one that changed last first.
export function recentRows() {
  loadTitles();
  const rows = env.panes().filter(x => x.p.agent !== 'shell').sort((a, b) => (b.p.state_since_ms || 0) - (a.p.state_since_ms || 0));
  return rows.length ? rows.map(x => sessionRow(x.st, x.p)) : [h('p', { class: 'muted' }, 'No agent is running. Start one below.')];
}

// A new session, as a sheet: which agent, where, and what to say first.
export function newSession() {
  const stations = env.stations(), up = env.onlineStations();
  if (!up.length) return toast('No station is answering', 'error');
  let lastAgent = null;
  try { lastAgent = localStorage.getItem('spandeck.newSession.agent'); } catch (e) { /* none */ }
  const agents = env.AGENTS.slice().sort((a, b) => (b[0] === lastAgent) - (a[0] === lastAgent));
  const S = { agent: agents[0][0], st: up.includes(env.api.station) ? env.api.station : up[0] };
  const box = dialog('New session', (close) => {
    const tiles = h('div', { class: 'tile-row', role: 'group', 'aria-label': 'Agent' });
    const chips = h('div', { class: 'tile-row', role: 'group', 'aria-label': 'Station' });
    const proj = h('select', { 'aria-label': 'Project' });
    const cwd = h('input', { placeholder: 'default: the project folder, or your home folder', 'aria-label': 'Folder' });
    const last = h('input', { type: 'checkbox', checked: env.pref('newPane.resumeLast', false) === true, style: { width: 'auto' } });
    last.addEventListener('change', () => env.setPref('newPane.resumeLast', last.checked));
    const lastNote = h('small', { class: 'muted' });
    const lastRow = h('div', {}, h('label', { class: 'row' }, last, 'Continue the last conversation in this folder'), lastNote);
    const first = h('textarea', { rows: 3, class: 'chat-first', placeholder: 'Optional: what to say first', 'aria-label': 'First message' });
    const firstRow = h('label', { class: 'field' }, h('span', {}, 'First message'), first);
    let asked = 0, canContinue = true, timer = null;
    const ask = async () => {
      const n = ++asked, agent = S.agent !== 'shell';
      lastRow.hidden = !(agent && canContinue);
      firstRow.hidden = !agent;
      if (!agent || !canContinue) return;
      lastNote.textContent = '';
      try {
        const r = await env.api.on(S.st, 'pane.last_session', { agent: S.agent, project: proj.value || undefined, cwd: cwd.value.trim() || undefined });
        if (n === asked) lastNote.textContent = env.lastSessionNote(r, env.AGENTS.find(([v]) => v === S.agent)[1]);
      } catch (e) { if (n === asked && (e.code === 'not_found' || /method pane\.last_session/.test(e.message || ''))) { canContinue = false; lastRow.hidden = true; } }
    };
    const draw = () => {
      tiles.replaceChildren(...agents.map(([v, label]) => h('button', { type: 'button', class: 'tile-pick' + (S.agent === v ? ' on' : ''), 'aria-pressed': String(S.agent === v), onclick: () => { S.agent = v; draw(); ask(); } }, env.agentMark(v), label)));
      chips.replaceChildren(...stations.map((s) => {
        const on = s.state === 'online';
        return h('button', { type: 'button', class: 'tile-pick station' + (S.st === s.station ? ' on' : ''), disabled: !on, 'aria-pressed': String(S.st === s.station), title: on ? '' : `${s.station} is not answering`,
          onclick: () => { S.st = s.station; projects(); draw(); ask(); } }, s.station, on ? null : h('span', { class: 'muted small' }, ' · away'));
      }));
    };
    const projects = () => proj.replaceChildren(h('option', { value: '' }, '(no project)'), ...env.projects(S.st).map(pr => h('option', { value: pr.name }, pr.name)));
    proj.addEventListener('change', ask);
    cwd.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(ask, 350); });
    projects();
    draw();
    ask();
    const start = async () => {
      clearTimeout(timer);
      const agent = S.agent !== 'shell', resume = agent && canContinue && last.checked;
      const name = env.freeName(S.agent, env.panesOf(S.st).filter(x => !x.exited && (x.project || '') === proj.value).map(x => x.name));
      const pane = await env.rpc(S.st, 'pane.spawn', { agent: S.agent, name, project: proj.value || undefined, cwd: cwd.value.trim() || undefined,
        resume_last: resume || undefined, prompt: agent && first.value.trim() ? first.value.trim() : undefined });
      try { localStorage.setItem('spandeck.newSession.agent', S.agent); } catch (e) { /* not kept */ }
      close();
      if (resume && !pane.resumed) toast('No saved conversation to continue here: a new one was started');
      env.go(env.paneUrl(S.st, pane.id));
    };
    return h('form', { class: 'new-session-form', onsubmit: (e) => { e.preventDefault(); start().catch(() => {}); } },
      h('h3', {}, 'Agent'), tiles, h('h3', {}, 'Station'), chips,
      h('label', { class: 'field' }, h('span', {}, 'Project'), proj), h('label', { class: 'field' }, h('span', {}, 'Folder'), cwd),
      lastRow, firstRow,
      h('div', { class: 'row end sheet-foot' }, h('button', { type: 'button', onclick: close }, 'Cancel'), h('button', { class: 'primary', type: 'submit' }, 'Start')));
  });
  box.body.parentNode.classList.add('sheet');
}

// The conversation view: a pane's second face (DECISIONS.md, "The
// conversation view"; the station's side is docs/conversation-api.md).
//
// A pane has its terminal, as always, and its conversation: what was said, as
// readable text, with the tool calls folded away. This file is that view, for
// the phone and for a pane's frame on a computer, and the phone's list of
// sessions that leads to it.
//
// Every pane has both faces (DECISIONS.md, "Chat for every pane"). Where the
// station can read the agent's own conversation, Chat is that. Where it
// cannot (a shell, an agent it cannot read, one whose session is not known
// yet), Chat is made from the screen: what was sent to the pane and what it
// printed after each, by the same code and with the same message box and
// cards. The station says which (`source`), and why.
//
// One chat for every pane (DECISIONS.md, 16 Nov 2026). There is one log,
// one renderer of items and one message box, whatever runs in the pane and
// wherever it is shown (a phone, a phone's layout, a cell of a layout on a
// computer, a pane at full size). What differs between agents is what the
// station sends: the items. What an agent printed on its screen comes as
// parts (`parts`), and is drawn as a reply is: prose through md.js, and rows
// whose layout is their meaning in the block a reply's code has.
//
// Rules kept here:
// - Everything from a session file is data. It is put on the page as text
//   nodes (`h()`), and an agent's markdown goes through md.js, which builds
//   elements itself and never hands a string to the browser to parse.
// - This view never attaches to the pane, so it never sets the pane's size
//   ("One size per pane"): it only watches the conversation and sends what
//   the phone's message box already sends.
// - Buttons answer only what a hook holds: an approval, and a question or a
//   plan the station says it holds (`live.questions`). An answer is made of
//   the choices the station sent, by their place, and of what the owner
//   types. A question or a plan nobody holds is answered in the terminal.
// - Times are the station's: an elapsed time is worked out from the clock the
//   station sent with its answer, plus the time since that answer arrived.

import { h, showMenu, toast, dialog, copyText, dot, stateOf, STATE_LABEL, fmtTokens, fmtCost, exact, modelShort, heldWords, RELEASE_SAYS, trustAnsweredWords, trustTick, trustCard, hooksTick } from './ui.js';
import { renderMarkdown } from './md.js';
import { sendAct } from './typed.js';
import { retryIn } from './api.js';

let env = null;
const convs = new Map();   // "station/pane id" -> Conv
const drafts = new Map();  // what was being written, when its view is not on the page
// How many message boxes hold something not sent, off the page (failover.js
// says "Your draft is kept" when the page moves to another station).
export const draftCount = () => drafts.size;
const termBoxes = new Map(); // the terminal face's message box on a phone, per pane
const titles = new Map();  // "station/pane id" -> the session's title, for the list
const noTitles = new Map(); // station -> when to ask it for titles again
let titlesAt = 0, titlesFor = '';

const keyOf = (st, p) => `${st}/${p.id}`;
const RUN = new Set(['tool', 'thinking', 'tasks']);
const STATUS = ['running', 'ok', 'done', 'error', 'denied', 'interrupted'];
// The keys under "Keys" in the message box, for a program that is at a
// prompt nothing here knows of. As on the terminal's face.
const SCREEN_KEYS = [['Esc', '\x1b'], ['Tab', '\t'], ['↑', '\x1b[A'], ['↓', '\x1b[B'], ['←', '\x1b[D'], ['→', '\x1b[C'], ['⏎', '\r'], ['^C', '\x03']];
const SCREEN_KEY_SAYS = { '↑': 'Up', '↓': 'Down', '←': 'Left', '→': 'Right', '⏎': 'Enter', '^C': 'Control C' };
// Lines of a printed block shown before "Show earlier lines".
const SCREEN_LINES = 30;
const WAIT_KEYS = [['1', '1'], ['2', '2'], ['3', '3'], ['Enter', '\r'], ['Esc', '\x1b'], ['↑', '\x1b[A'], ['↓', '\x1b[B'], ['←', '\x1b[D'], ['→', '\x1b[C']];
const KEY_SAYS = { '↑': 'Up', '↓': 'Down', '←': 'Left', '→': 'Right' };
// Text commands an agent takes as a message (not its terminal-only pickers).
const SLASH = { claude: [['/compact', 'make room: summarise the conversation so far'], ['/clear', 'start a new conversation'], ['/context', 'what is filling the context'], ['/usage', 'how much of the plan is used'], ['/model ', 'change the model'], ['/effort ', 'how hard it thinks']] };
// The tests' stand-in agent speaks Claude Code's dialect (as in the archive).
SLASH.fake = SLASH.claude;
const NO_CONV = 'no conversation for this pane: ';
// Notices that are something to read, and may be long: each is one quiet
// line that opens, and can be cleared (DECISIONS.md, 17 Nov 2026). Never an
// error, a turn's end, or anything that waits for the owner.
const NOTES = new Set(['away_summary', 'info', 'task', 'command_output', 'summary']);
// What Claude Code puts at the end of its own recap.
const RECAP_HINT = /\s*\(disable recaps in \/config\)\s*$/;
const TASK_KIND = { agent: ['◇', 'Agent'], shell: ['>_', 'Shell'], helper: ['▣', 'Helper pane'] };
// "claude-fable-5-2" is "Fable 5.2" on the chip; a name of another shape
// is shown as it is.
export function modelName(model) {
  const short = modelShort(model);
  const m = /^([a-z]+)((?:-\d+)+)$/.exec(short);
  return m ? m[1][0].toUpperCase() + m[1].slice(1) + ' ' + m[2].slice(1).replace(/-/g, '.') : short;
}
const cap1 = (s) => (s ? s[0].toUpperCase() + s.slice(1) : '');
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
// How much longer a held question waits here: "9 more minutes", "40 more seconds".
const moreTime = (ms) => {
  const s = Math.ceil(ms / 1000);
  return s >= 90 ? `${Math.round(s / 60)} more minutes` : `${plural(Math.max(s, 1), 'more second')}`;
};
const quiet = (text, cls) => h('div', { class: 'chat-quiet' + (cls ? ' ' + cls : '') }, text);
const idOrder = (id) => { const [a, b] = String(id).split('.'); return [Number(a) || 0, Number(b) || 0]; };

// Put `kids` in `parent` in this order, moving only what is out of place: a
// node that stays keeps its scroll position, its focus and what is typed in it.
export function sync(parent, kids) {
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

// 'chat' or 'terminal'. Every pane has both; one whose station cannot give
// a conversation at all (a station from before this) is its terminal. Until
// the owner chooses for a pane: an agent opens as its conversation and a
// shell as its terminal, on a phone and on a computer ("Chat default for
// agents terminal for terminals", the owner, 16 Nov 2026).
export function faceOf(st, p) {
  if (!p) return 'terminal';
  const key = keyOf(st, p), c = convs.get(key);
  if (c && c.available === false) return 'terminal';
  return faces[key] === 'chat' || faces[key] === 'terminal' ? faces[key] : p.agent !== 'shell' ? 'chat' : 'terminal';
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
  if (!p) return null;
  if (p.agent === 'shell') return h('button', { class: 'mini face-btn', title: 'Show this pane as a conversation: what you sent it and what it printed after each, as text. The terminal is one click back.', onclick: () => setFace(st, p, 'chat') }, 'Chat');
  // A question or a plan held for the owner is answered in the conversation:
  // while it is held, the terminal does not show the agent's own prompt.
  if (p.asks && !p.exited) return h('button', { class: 'mini face-btn attn', title: `${p.name} ${asksWords(p)}. It is held for you in its conversation; the terminal shows its own prompt only if nobody answers there in time.`, onclick: () => setFace(st, p, 'chat') }, 'Answer in Chat');
  return h('button', { class: 'mini face-btn', title: 'Show this pane as a conversation: what was said, with the tool calls folded away. The terminal is one click back.', onclick: () => setFace(st, p, 'chat') }, 'Chat');
}

// What a pane asks the owner, from the pane list: which kind, never what.
const asksWords = (p) => (p.asks && p.asks.kind === 'plan' ? 'asks you to approve its plan' : 'asks you a question');

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
  // A question or a plan held for the owner comes before what waits in a
  // terminal: it can be answered where the owner is.
  for (const x of agents) if (x.p.state === 'needs_you' && x.p.asks) add(x.st, x.p, x.p.asks.kind === 'plan' ? 'a plan to approve' : 'a question');
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
    this.items = []; this.byId = new Map(); this.ver = 0;
    this.session = null; this.cursor = 0; this.first = null; this.moreBefore = false;
    this.title = null; this.notUnderstood = 0; this.olderDropped = false;
    // null until the station has answered. `source`: 'transcript' (the
    // agent's own conversation) or 'screen' (made from the terminal's text).
    this.available = null;
    this.reason = '';
    this.source = 'transcript';
    this.live = null; this.liveAt = 0;
    this.chan = null; this.state = 'off'; this.problem = '';
    this.open = new Set(); this.revs = new Map(); this.nodes = new Map(); this.dockNodes = new Map();
    this.details = new Map(); this.full = new Map(); this.readTo = -1;
    this.stuck = true; this.top = 0; this.peekOpen = false; this.clocks = [];
    // What the owner has picked on a question's or a plan's card, by its id.
    this.asking = new Map(); this.waits = [];
    this.where = 'phone'; this.used = true; this.seeded = false;

    this.bar = h('header', { class: 'chat-bar' });
    this.log = h('div', { class: 'chat-log', role: 'log', 'aria-live': 'off', 'aria-label': 'Conversation', tabIndex: 0 });
    this.log.addEventListener('scroll', () => this.scrolled(), { passive: true });
    if (typeof ResizeObserver === 'function') { this.sized = new ResizeObserver(() => this.resized()); this.sized.observe(this.log); }
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
    if (this.disposed || this.state !== 'off' || !env.api.connected) return;
    this.state = 'opening';
    clearTimeout(this.retry);
    this.retry = null;
    // `screen`: this deck can show the screen as a conversation. A station
    // from before this does not know the word and answers as it always did.
    const api = env.api, params = { pane: this.p.id, limit: 50, screen: true };
    if (this.st !== api.station) params.station = this.st;
    if (this.session) { params.session = this.session; params.after = this.cursor; }
    try {
      const r = await api.call('pane.conversation.watch', params);
      // Let go of, or stepped away from, while the station was answering.
      if (this.disposed || this.parked) { api.send('pane.conversation.unwatch', { chan: r.chan }); this.state = 'off'; return; }
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
      if (this.disposed || this.parked) return;
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
  restored() { this.chan = null; this.state = 'off'; if (!this.parked) this.watch(); }

  // Stepped away from, in a phone's layout: nothing is watched and nothing
  // runs, and what was read, where it was scrolled to and what was being
  // written are kept for when the owner steps back. Watching again asks only
  // for what came after the cursor.
  park() {
    clearTimeout(this.retry);
    this.retry = null;
    if (this.state === 'watching') this.unwatch();
    this.stopTalking();
  }

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
      this.source = a.source === 'screen' ? 'screen' : 'transcript';
      this.reason = this.source === 'screen' && typeof a.reason === 'string' ? a.reason : '';
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
      const a = await env.api.on(this.st, 'pane.conversation', { pane: this.p.id, session: this.session, before: this.first, limit: 50, screen: true });
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

  // Where the log is now. The browser tells of a scroll only with its next
  // frame, and a page that is drawn again before then (or a window that
  // draws no frames) would put the log back where it was told of last.
  note() {
    if (!this.log.isConnected || !this.log.clientHeight) return false;
    // A log whose room changed has not been scrolled by the owner.
    this.resized();
    this.stuck = this.log.scrollHeight - this.log.scrollTop - this.log.clientHeight < 48;
    this.top = this.log.scrollTop;
    return true;
  }

  scrolled() {
    if (!this.note()) return;
    if (this.top < 160) this.older();
    this.maybeRead();
  }

  // After the page was drawn again: the log is where it was left, or at the
  // end if that is where the owner was.
  placed() {
    if (!this.log.isConnected) return;
    const to = this.stuck ? this.log.scrollHeight : this.top;
    if (Math.abs(this.log.scrollTop - to) > 1) this.log.scrollTop = to;
    this.top = this.log.scrollTop;
    this.tall = this.log.clientHeight || this.tall;
    this.maybeRead();
  }

  // The log's room changed (a phone's keyboard came up or went, a card
  // appeared, the keys were asked for): what was last in sight above the
  // message box still is. At the end, it stays at the end; further up, the
  // line at the bottom edge stays at the bottom edge.
  resized() {
    if (!this.log.isConnected) return;
    const tall = this.log.clientHeight, was = this.tall;
    this.tall = tall;
    if (!tall || !was || tall === was) return;
    // (Scrolled since it was last told of: where it was in its old room.
    // A log that only grew taller near its end was moved by the browser,
    // which keeps it within its end, and not by the owner.)
    const end = this.log.scrollHeight - tall, held = this.top > end && Math.abs(this.log.scrollTop - end) <= 1;
    if (this.log.scrollTop !== this.top && !held) {
      this.stuck = this.log.scrollHeight - this.log.scrollTop - was < 48;
      this.top = this.log.scrollTop;
    }
    this.log.scrollTop = this.stuck ? this.log.scrollHeight : Math.max(0, this.top + (was - tall));
    this.top = this.log.scrollTop;
  }

  // The owner has seen the newest item: it is on screen and the page is in front.
  maybeRead() {
    if (this.available !== true || this.source === 'screen' || !this.live?.unread || !this.stuck || !this.cursor || this.readTo >= this.cursor) return;
    if (document.visibilityState !== 'visible' || !this.log.isConnected || !this.log.clientHeight) return;
    this.readTo = this.cursor;
    env.api.on(this.st, 'pane.read', { pane: this.p.id, upto: this.cursor }).catch(() => { this.readTo = -1; });
  }

  // ------------------------------------------------------------------- draw

  // (The dock first: what it holds decides how much room the log has, and
  // the log goes to its end in the room it will have.)
  draw() { this.drawBar(); this.drawDock(); this.drawLog(); }

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
    const pct = [p.usage ? 1 : 0, p.usage?.context_pct ?? null, p.usage?.context_tokens ?? null];
    const sig = JSON.stringify([phone, title, sub, state, p.needs_reason, face, this.available, this.available === false ? this.reason : '', next.length, next[0] && keyOf(next[0].st, next[0].p), this.where, p.name, pct]);
    if (sig === this.barSig) return;
    this.barSig = sig;
    const pick = (to) => h('button', { type: 'button', class: face === to ? 'on' : '', 'aria-pressed': String(face === to), onclick: (e) => { e.currentTarget.blur(); if (face !== to) setFace(this.st, p, to); } }, to === 'chat' ? 'Chat' : 'Terminal');
    const menu = (e) => {
      const b = e.currentTarget.getBoundingClientRect();
      env.paneMenu({ clientX: e.clientX || b.left, clientY: e.clientY || b.bottom + 2, preventDefault() {}, stopPropagation() {} }, this.st, this.p, this.where === 'phone' ? 'zoom' : this.where);
    };
    this.bar.classList.toggle('has-next', next.length > 0);
    sync(this.bar, [
      phone ? h('a', { class: 'btn chat-back', href: '#/fleet', 'aria-label': 'Back to the list' }, '‹') : null,
      h('span', { class: 'dot ' + state, title: p.needs_reason || STATE_LABEL[state] || state }),
      h('div', { class: 'chat-title', title: `${p.name} · ${p.address}` }, h('b', {}, title), h('span', { class: 'muted' }, sub)),
      this.available === false
        ? h('span', { class: 'chat-why', role: 'note' }, 'Terminal only: ' + this.why())
        : h('div', { class: 'chat-switch', role: 'group', 'aria-label': 'Show this pane as' }, pick('chat'), pick('terminal')),
      next.length ? h('button', { type: 'button', class: 'chat-next', title: `Next: ${next[0].p.name} on ${next[0].st} (${next[0].why})`, 'aria-label': `Next of ${plural(next.length, 'pane')} that need you: ${next[0].p.name}, ${next[0].why}`,
        onclick: () => env.go(env.paneUrl(next[0].st, next[0].p.id)) }, 'Next ›', h('span', { class: 'count' }, next.length)) : null,
      this.ringEl(),
      h('button', { type: 'button', class: 'icon-btn chat-more', 'aria-label': 'Pane menu', title: 'Pane menu', 'aria-haspopup': 'menu', onclick: menu }, '⋯'),
    ].filter(Boolean));
  }

  // How full the agent's context is, as a small ring, in every agent's
  // chat. Filled only by what the agent's own session data says: with a
  // count and no known limit the ring is empty and says the count; with
  // nothing reported it is a dashed ring that says so. Never a made-up
  // share. (A shell has no context and no ring.)
  ringEl() {
    const u = this.p.usage;
    if (!u) return null;
    const pct = typeof u.context_pct === 'number' ? u.context_pct : null;
    const known = u.context_tokens !== null && u.context_tokens !== undefined;
    const level = pct === null ? (known ? ' count' : ' none') : pct >= 90 ? ' full' : pct >= 70 ? ' high' : '';
    const says = pct !== null ? `Context ${pct}% used` : known ? `Context ${fmtTokens(u.context_tokens)} tokens used, its limit is not known` : 'Context not reported';
    const ring = h('span', { class: 'ring', 'aria-hidden': 'true' });
    ring.style.setProperty('--pct', String(Math.max(0, Math.min(100, pct || 0))));
    return h('button', { type: 'button', class: 'chat-ring' + level, 'aria-label': says, title: says, 'aria-haspopup': 'dialog', dataset: { pct: pct === null ? '' : String(pct) },
      onclick: (e) => { e.currentTarget.blur(); this.usageSheet(); } }, ring);
  }

  // What is known of the agent's model and what it has used. Only what
  // the station was told: a line that is not known is not there.
  usageSheet() {
    const u = this.p.usage || {}, M = this.live?.model || {};
    const fact = (label, value) => (value ? h('div', { class: 'fact' }, h('span', { class: 'muted' }, label), h('b', {}, value)) : null);
    const known = (n) => n !== null && n !== undefined;
    const now = (this.live?.now_ms || 0) + (performance.now() - this.liveAt);
    const packed = [...this.items].reverse().find(x => x.kind === 'notice' && x.what === 'compacted' && typeof x.ts_ms === 'number');
    const box = dialog('Context and usage', (close) => h('div', { class: 'usage-sheet' },
      h('div', { class: 'facts' },
        fact('Model', u.model || M.model || ''),
        fact('Effort', M.effort ? cap1(M.effort) : ''),
        fact('Context', known(u.context_tokens) ? (u.context_window ? `${fmtTokens(u.context_tokens)} of ${fmtTokens(u.context_window)} (${u.context_pct}% used)` : `${fmtTokens(u.context_tokens)} tokens (the model's window is not known)`) : ''),
        fact('Tokens in', known(u.input_tokens) ? exact(u.input_tokens) : ''),
        fact('Tokens out', known(u.output_tokens) ? exact(u.output_tokens) : ''),
        fact('Estimated cost', known(u.cost_estimate) ? fmtCost(u.cost_estimate) + ' at list prices' : ''),
        fact('Last compacted', packed && now > packed.ts_ms ? dur(now - packed.ts_ms) + ' ago' : '')),
      h('p', { class: 'muted small usage-why' }, u.note === 'not reported by this agent' ? `${this.p.agent_name || 'This agent'} keeps no usage where spandeck can read it: nothing is shown rather than a guess.`
        : !known(u.total_tokens) ? 'Nothing is reported yet: the numbers come with the agent\'s first reply.'
          : !u.context_window && known(u.context_tokens) ? 'The limit of this model\'s context is not known (it is not in the session data, and the price list does not name it), so no share is drawn.'
            : 'From the agent\'s own session data, for this conversation. A cost is an estimate at list prices, never a bill.'),
      h('div', { class: 'row end sheet-foot' },
        h('button', { type: 'button', onclick: close }, 'Close'),
        env.usage ? h('button', { type: 'button', class: 'primary usage-more', onclick: () => { close(); env.usage(this.st, this.p); } }, 'All usage for this pane') : null)));
    box.body.parentNode.classList.add('sheet', 'chat-sheet');
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
      // Made from the screen: said at the top, with why, where "Terminal
      // only" used to be said. A tap shows all of a long reason.
      if (this.source === 'screen') one('src', `${this.reason}|${this.open.has('src')}`, () => h('button', { type: 'button', class: 'chat-src' + (this.open.has('src') ? ' open' : ''), 'aria-expanded': String(this.open.has('src')), title: 'Chat is made from what this pane\'s terminal shows: what you sent, and what it printed after each. The terminal has everything.',
        onclick: () => { this.open.has('src') ? this.open.delete('src') : this.open.add('src'); this.drawLog(); } }, 'From the screen: ' + this.why()));
      if (!this.items.length) one('empty', this.source, () => quiet(this.source === 'screen' ? 'Nothing has been sent or printed here yet. Write below to start.' : 'Nothing has been said in this conversation yet. Write below to start.'));
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
        // A note the owner cleared: left out, here and on every device.
        if (it.kind === 'notice' && NOTES.has(it.what) && this.isCleared(it.id)) { i += 1; continue; }
        const next = items[i + 1];
        const said = it.kind === 'text' || it.kind === 'screen';
        const done = said && (next ? !(RUN.has(next.kind) || next.kind === 'text') : !working);
        if (done && it.kind === 'text') lastDone = it;
        const heldId = it.kind === 'question' || it.kind === 'plan' ? this.heldFor(it.id)?.id || '' : '';
        one('i:' + it.id, `${it._v}.${this.rev(it.id)}.${done}.${heldId}`, () => this.itemEl(it, done));
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
    // (Where it was put is where it is known to be: otherwise the next
    // redraw takes the log for moved by the owner, and a log that has since
    // grown, as fonts arrive, for one no longer at its end.)
    if (this.stuck) { this.log.scrollTop = this.log.scrollHeight; this.top = this.log.scrollTop; }
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

  // Under what an agent said, once it is whole: copy it, and hear it. Small
  // marks, with their words for a screen reader and on a long press.
  // (Copy is under every reply, as soon as it is there, and copies that
  // reply's own source: the markdown as the agent wrote it. Read aloud is
  // under a reply that is whole.)
  actsEl(text, body, copied, speak = true) {
    return h('div', { class: 'chat-acts' },
      h('button', { type: 'button', class: 'chat-act act-copy', title: 'Copy', onclick: () => copyText(text, copied || 'Copied') }, 'Copy'),
      speak && canSpeak() ? h('button', { type: 'button', class: 'chat-act act-speak', title: 'Read aloud', 'aria-pressed': 'false', onclick: (e) => readAloud(body, e.currentTarget) }, 'Read aloud') : null);
  }

  isCleared(id) {
    if (this.showCleared) return false;
    return this.hid?.has(id) || (Array.isArray(this.live?.cleared) && this.live.cleared.includes(id));
  }

  // Clear a note: it is hidden in this conversation on every device of
  // the owner's (the pane's station keeps which). Nothing is deleted from
  // the agent's transcript or the archive.
  clearNote(it) {
    (this.hid ||= new Set()).add(it.id);
    this.drawLog();
    env.rpc(this.st, 'pane.notes.clear', { pane: this.p.id, session: this.session, item: it.id })
      .catch(() => { this.hid.delete(it.id); this.drawLog(); });
  }

  // "Show cleared notes", from the pane's menu.
  restoreNotes() {
    this.hid = new Set();
    env.rpc(this.st, 'pane.notes.clear', { pane: this.p.id, session: this.session, restore: true }).then(() => toast('The cleared notes are back')).catch(() => {});
  }

  // Something to read that is not part of the talk: one quiet line, which
  // opens, and a small mark that clears it.
  noteEl(it, label, text, body) {
    const flag = 'n:' + it.id, open = this.open.has(flag);
    const line = label + (label && text ? ': ' : '') + text.replace(/\s+/g, ' ');
    return h('div', { class: 'chat-note' + (open ? ' open' : ''), dataset: { note: it.what } },
      h('div', { class: 'chat-note-row' },
        h('button', { type: 'button', class: 'chat-note-line', 'aria-expanded': String(open), onclick: () => this.toggle(flag, it.id) },
          h('span', { class: 'note-text' }, open ? label : line), h('span', { class: 'chev', 'aria-hidden': 'true' }, open ? '▾' : '▸')),
        this.source === 'screen' && !this.session ? null
          : h('button', { type: 'button', class: 'chat-note-x', 'aria-label': 'Clear this note', title: 'Clear this note: it is hidden here on all your devices. The pane\'s menu brings cleared notes back.', onclick: () => this.clearNote(it) }, '×')),
      open ? h('div', { class: 'chat-note-body' }, body()) : null);
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
        h('div', { class: 'chat-bubble' }, text, it.images ? h('div', { class: 'chat-from' }, plural(Number(it.images) || 0, 'picture') + ' attached') : null), rest,
        text ? h('div', { class: 'chat-acts mine' }, h('button', { type: 'button', class: 'chat-act act-copy', title: 'Copy', onclick: () => copyText(text, 'Copied') }, 'Copy')) : null);
    }
    // What the pane printed, from its terminal. An agent's comes as parts:
    // prose, drawn like any reply, and rows that are a layout, in the block
    // a reply's code has. A shell's is its rows, in that block. The block
    // never wraps and scrolls sideways inside itself.
    if (it.kind === 'screen') {
      // A block that turned out to hold nothing (only the prompt the next
      // message was typed at, which is the next block's): not drawn.
      if (!text) return h('div', { hidden: true });
      const flag = 's:' + it.id, open = this.open.has(flag);
      const mono = (t, says) => h('div', { class: 'md-code chat-mono' }, h('pre', { tabIndex: 0, 'aria-label': says }, h('code', {}, t)));
      const says = it.open ? 'What the pane has printed since' : 'What the pane printed';
      const parts = !this.full.has(it.id) && Array.isArray(it.parts) ? it.parts.filter(x => x && typeof x.text === 'string' && x.text) : null;
      const label = it.whole ? (it.open ? 'The screen now' : 'The screen then') : '';
      let body, copy = text, more = null;
      if (parts) {
        // Nothing on its screen but its own frame and message box.
        if (!parts.length) return it.open ? quiet('Nothing to read on its screen yet.') : h('div', { hidden: true });
        body = h('div', { class: 'chat-parts' }, parts.map(x => (x.prose ? md(x.text) : mono(x.text, says))));
        copy = parts.map(x => x.text).join('\n\n');
      } else {
        const lines = text.split('\n'), long = lines.length > SCREEN_LINES;
        body = mono(open || !long ? text : lines.slice(-SCREEN_LINES).join('\n'), says);
        if (long && !open) more = h('button', { type: 'button', class: 'mini', onclick: () => this.toggle(flag, it.id) }, `Show ${plural(lines.length - SCREEN_LINES, 'earlier line')}`);
      }
      // A block still being printed is cut by the station to its last
      // lines: all of it is in the terminal.
      more ||= !it.more || this.full.has(it.id) ? null
        : it.open ? h('button', { type: 'button', class: 'mini', onclick: () => setFace(this.st, this.p, 'terminal') }, 'Earlier lines are in the terminal')
          : h('button', { type: 'button', class: 'mini', onclick: () => { this.open.add(flag); this.whole(it); } }, 'Show all of it');
      return h('div', { class: 'chat-text chat-screen' + (it.open ? ' open' : '') },
        label ? h('div', { class: 'chat-from' }, label) : null, more, body,
        done || !it.open ? this.actsEl(copy, body, 'Copied', done) : null);
    }
    if (it.kind === 'text') {
      const body = md(text);
      return h('div', { class: 'chat-text' + (it.phase === 'commentary' ? ' commentary' : '') }, body, rest,
        this.actsEl(text, body, 'Copied', done));
    }
    if (it.kind === 'question') {
      const held = it.status === 'waiting' && this.heldFor(it.id);
      const state = held ? ' · waiting for your answer, below' : { waiting: ' · waiting for an answer', dismissed: ' · not answered' }[it.status] || '';
      const qs = Array.isArray(it.questions) ? it.questions : [], answers = Array.isArray(it.answers) ? it.answers : [];
      // Held: the card below has the question and its choices; not twice.
      if (held) return h('div', { class: 'chat-box-item held-item' }, h('div', { class: 'chat-from' }, 'The agent asked' + state), qs.map(q => h('div', { class: 'chat-q' }, h('b', {}, String(q.question || '')))));
      return h('div', { class: 'chat-box-item' }, h('div', { class: 'chat-from' }, 'The agent asked' + state),
        qs.map(q => h('div', { class: 'chat-q' },
          q.header ? h('span', { class: 'chip' }, String(q.header)) : null, h('b', {}, String(q.question || '')),
          h('ul', {}, (Array.isArray(q.options) ? q.options : []).map(o => h('li', {}, String(o.label || ''), o.description ? h('span', { class: 'muted' }, ' — ' + o.description) : null))))),
        // With several questions, each answer says which one it is for.
        answers.map(a => h('div', { class: 'chat-answer' }, answers.length > 1 && Array.isArray(a) ? String(a[0]) + ' ' : 'Answered: ', h('b', {}, String(Array.isArray(a) ? a[1] : a)))));
    }
    if (it.kind === 'plan') {
      const held = it.status === 'waiting' && this.heldFor(it.id);
      const state = held ? 'waiting for your answer, below' : { waiting: 'waiting for your answer', approved: 'approved', rejected: 'sent back: keep planning', dismissed: 'set aside' }[it.status] || String(it.status || '');
      // Claude Code may leave the plan out of the call it writes to the
      // session file; what the hook was given has it.
      const plan = text || (held && typeof held.text === 'string' ? held.text : '');
      // Held: the plan is in the card below, with its buttons; not twice.
      if (held) return h('div', { class: 'chat-box-item held-item' }, h('div', { class: 'chat-from' }, 'The agent proposed a plan · ' + state));
      return h('div', { class: 'chat-box-item' }, h('div', { class: 'chat-from' }, 'The agent proposed a plan' + (state ? ' · ' + state : '')), plan ? md(plan) : h('div', { class: 'muted small' }, 'The text of this plan is not in the session file: it was shown in the terminal.'), rest);
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
        case 'summary': return this.noteEl(it, 'Summary of the conversation before this', '', () => [md(text), rest]);
        case 'command_output': return this.noteEl(it, 'What the command printed', text, () => h('pre', { class: 'chat-out' }, text));
        case 'error': return quiet('Error: ' + text, 'err');
        // Claude Code's own recap (a `system` record of the subtype
        // `away_summary`). Its hint is kept for the opened note, with
        // where that setting is: it is a screen of Claude Code's own.
        case 'away_summary': return this.noteEl(it, 'While you were away', text.replace(RECAP_HINT, ''), () => [
          h('div', {}, text.replace(RECAP_HINT, '')),
          RECAP_HINT.test(text) ? h('div', { class: 'muted small' }, 'This recap is Claude Code\'s own. To turn recaps off, type /config in the terminal and switch them off there: it is a screen of Claude Code\'s, so spandeck does not press its keys for you.') : null]);
        case 'task': return this.noteEl(it, `A background task ${it.level || 'reported'}`, text, () => h('div', {}, text));
        // A short line is said as it is; a long one folds like the rest.
        case 'info': return text.length <= 120 ? quiet(text, it.level === 'warning' ? 'warn' : '') : this.noteEl(it, '', text, () => h('div', {}, text));
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
    // A question or a plan the station holds for the owner: answered here.
    const held = this.available === true && !exited && Array.isArray(L.questions) ? L.questions.filter(q => q && typeof q.id === 'string' && (q.kind === 'question' || q.kind === 'plan')) : [];
    for (const id of [...this.asking.keys()]) if (!held.some(q => q.id === id)) this.asking.delete(id);
    // One that waits and is not held is answered in the terminal: it was
    // not answered here in time, or its session cannot be held. Not shown
    // in the moment between the call being written and its hook asking.
    const now = (L.now_ms || 0) + (performance.now() - this.liveAt);
    const settled = (x) => L.state === 'needs_you' || typeof x.ts_ms !== 'number' || now - x.ts_ms > 4000;
    const waiting = this.available === true ? this.items.filter(x => (x.kind === 'question' || x.kind === 'plan') && x.status === 'waiting'
      && !held.some(q => q.item === x.id || (q.item === null && q.kind === x.kind))) : [];
    const asked = waiting.filter(settled);
    this.askSoon = waiting.length > asked.length;
    // The program asks whether to switch its model (the station read its
    // screen exactly): a card of its own, answered with the keys it takes.
    const switching = !exited && L.prompt && L.prompt.kind === 'model_switch' ? L.prompt : null;
    const blocked = !switching && !exited && this.available === true && !approvals.length && !asked.length && !held.length && (L.state === 'needs_you' || L.state === 'error');
    // A banner that waits in the terminal shows the terminal's last lines.
    // (Made from the screen, the conversation already ends with them.)
    if (blocked && !this.wasBlocked && this.source !== 'screen') this.peekOpen = true;
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
    const tasks = this.tasksNow();
    if (tasks.running.length) status.push(this.node(cache, used, 'subs', tasks.running.length, () => h('button', { type: 'button', class: 'chat-tasks', 'aria-haspopup': 'dialog', onclick: () => this.tasksSheet() }, plural(tasks.running.length, 'running task'))));
    if (this.tasksBox) this.tasksBox();
    if (this.peekOpen && (working || blocked)) { status.push(this.peekEl); if (!this.peeked) this.peek(); } else this.peeked = false;
    sync(this.acts, status);

    const cards = [];
    for (const a of approvals) cards.push(this.node(cache, used, 'ap:' + a.approval, a.summary, () => this.approvalEl(a)));
    for (const q of held) {
      const s = this.askState(q);
      cards.push(this.node(cache, used, 'held:' + q.id, `${s.rev}|${q.attached}|${q.until_ms}`, () => (q.kind === 'plan' ? this.planEl(q, s) : this.questionEl(q, s))));
    }
    for (const it of asked) {
      // Why it is the terminal's: the station says so when it let it go.
      const moved = typeof L.needs_reason === 'string' && /in the terminal now/.test(L.needs_reason) ? L.needs_reason : '';
      cards.push(this.node(cache, used, 'ask:' + it.id, `${it._v}|${moved}`, () => h('div', { class: 'chat-card ask terminal' },
        h('b', {}, it.kind === 'plan' ? 'The agent proposed a plan and waits for your answer in the terminal' : 'The agent asked you a question, in the terminal'),
        it.kind === 'plan' ? null : (Array.isArray(it.questions) ? it.questions : []).map(x => h('div', {}, String(x.question || ''))),
        h('div', { class: 'muted small' }, moved ? `It is not held here any more: ${moved.replace(/^its (question|plan) is in the terminal now \(?/, '').replace(/\)$/, '')}. The choices are in the terminal.`
          : 'It is not held here: this session was started before spandeck could hold a question, its station runs an older spandeck, or holding is switched off there. The choices are in the terminal.'),
        h('div', { class: 'wait-keys', role: 'group', 'aria-label': 'Keys for the terminal' },
          WAIT_KEYS.map(([label, bytes]) => h('button', { type: 'button', class: 'key', 'aria-label': KEY_SAYS[label] || label, onclick: () => this.key1(bytes) }, label)),
          h('button', { type: 'button', class: 'primary', onclick: () => setFace(this.st, p, 'terminal') }, 'Answer in the terminal')))));
    }
    // The program asks whether to trust the folder: the owner's one tap,
    // with the choice to remember it, or the terminal.
    const trustP = { agent_name: p.agent_name, trust: L.trust };
    const trustAsks = !exited && L.trust?.asks;
    if (trustAsks) cards.push(this.node(cache, used, 'trust', JSON.stringify(L.trust.asks), () => trustCard(trustP,
      (remember) => env.rpc(this.st, 'pane.trust_folder', { pane: p.id, remember }).catch(() => {}), () => setFace(this.st, p, 'terminal'))));
    if (trustAnsweredWords(trustP)) cards.push(this.node(cache, used, 'trusted', '', () => quiet(trustAnsweredWords(trustP) + '. It is in the ship\'s log.')));
    if (blocked && !trustAsks) cards.push(this.node(cache, used, 'wait', `${L.state}|${L.needs_reason}`, () => h('div', { class: 'chat-card wait' + (L.state === 'error' ? ' err' : '') },
      h('b', {}, (L.state === 'error' ? 'Stopped on an error: ' : 'Waiting in the terminal: ') + (L.needs_reason || 'it needs you')),
      h('div', { class: 'wait-keys', role: 'group', 'aria-label': 'Keys for the terminal' },
        WAIT_KEYS.map(([label, bytes]) => h('button', { type: 'button', class: 'key', 'aria-label': KEY_SAYS[label] || label, onclick: () => this.key1(bytes) }, label)),
        h('button', { type: 'button', class: 'primary', onclick: () => setFace(this.st, p, 'terminal') }, 'Open terminal')))));
    if (switching) cards.push(this.node(cache, used, 'switch', JSON.stringify(switching), () => h('div', { class: 'chat-card wait model-ask' },
      h('b', {}, `${p.agent_name || 'The agent'} asks: switch model to ${String(switching.name || '')}?`),
      h('div', { class: 'muted small' }, 'It says the next reply will be slower and use more tokens: the whole conversation is read again by the new model. Until it is answered, nothing you send is typed into the pane.'),
      h('div', { class: 'wait-keys', role: 'group', 'aria-label': 'Answer' },
        // Enter takes the choice its marker is on, so "Yes" is offered
        // only while the marker is on it.
        switching.on_yes ? h('button', { type: 'button', class: 'primary model-yes', onclick: () => this.key1('\r') }, 'Yes, switch') : null,
        h('button', { type: 'button', class: 'model-no', onclick: () => this.key1('\x1b') }, 'No, go back'),
        h('button', { type: 'button', onclick: () => setFace(this.st, p, 'terminal') }, 'Open terminal')),
      switching.on_yes ? null : h('div', { class: 'muted small' }, 'Its marker is on "No": to switch, choose Yes in the terminal.'))));
    // Held: stopped and staying stopped, or waiting for you after a restart.
    if (L.held && !exited) cards.push(this.node(cache, used, 'held', JSON.stringify(L.held), () => h('div', { class: 'chat-card wait' },
      h('b', {}, heldWords(L.held)),
      h('div', { class: 'muted small' }, 'It takes no mail from other agents until you release it. Sending it a message releases it too.'),
      h('button', { type: 'button', class: 'primary', title: RELEASE_SAYS, onclick: () => env.rpc(this.st, 'pane.release', { pane: p.id }).catch(() => {}) }, 'Release'))));
    if (exited) cards.push(this.node(cache, used, 'ended', '', () => quiet('This pane has ended. Its conversation is still here to read.')));
    sync(this.cards, cards);
    for (const key of [...cache.keys()]) if (!used.has(key)) cache.delete(key);
    this.composer.hidden = !!exited || this.available !== true;
    // The keys are shown when the owner asks for them. (A pane that waits
    // in its terminal has a card with keys of its own.)
    this.keysEl.hidden = !this.keysOpen;
    this.keysBtn.setAttribute('aria-expanded', String(!!this.keysOpen));
    this.keysBtn.classList.toggle('on', !!this.keysOpen);
    const says = this.p.agent === 'shell' ? 'Type or paste a command' : this.speech ? 'Message or hold to talk' : 'Message';
    if (this.box.placeholder !== says) this.box.placeholder = says;
    this.working = working;
    this.sendState();
    this.modelState();
    // Until when each held card waits here, by the station's clock.
    this.waits = held.map(q => ({ el: cache.get('held:' + q.id)?.el.querySelector('.ask-wait'), until: q.until_ms }));
    this.tick();
    // What the dock holds may have changed the log's room.
    this.resized();
  }

  // ---- a question or a plan the station holds for the owner ----

  heldFor(id) {
    const qs = this.live?.questions;
    return (Array.isArray(qs) && qs.find(q => q && q.item === id && typeof q.id === 'string')) || null;
  }

  // What the owner has picked so far: kept while the card is redrawn.
  askState(q) {
    let s = this.asking.get(q.id);
    if (!s) {
      const n = Array.isArray(q.questions) ? q.questions.length : 0;
      s = { step: 0, picks: Array.from({ length: n }, () => new Set()), other: Array(n).fill(''), otherOpen: Array(n).fill(false), note: '', rev: 0, busy: false };
      this.asking.set(q.id, s);
    }
    return s;
  }

  // `question.answer`: the owner's only, at the pane's station. The card
  // stays, saying it was sent, until the station says the question is gone.
  answerHeld(q, s, params, said) {
    if (s.busy) return;
    s.busy = true; s.rev += 1;
    this.drawDock();
    env.rpc(this.st, 'question.answer', { id: q.id, ...params })
      .then((r) => toast(r && r.delivered === false ? said + '. The agent is finding its way back after a restart: it is given your answer when it is back.' : said))
      .catch(() => { s.busy = false; s.rev += 1; this.drawDock(); });
  }

  // The foot of a held card: how long it waits here, and the way out.
  heldFoot(q, s) {
    const toTerminal = () => {
      if (s.busy) return;
      s.busy = true; s.rev += 1;
      this.drawDock();
      env.rpc(this.st, 'question.answer', { id: q.id, release: true })
        .then(() => { toast('It is in the terminal now'); setFace(this.st, this.p, 'terminal'); })
        .catch(() => { s.busy = false; s.rev += 1; this.drawDock(); });
    };
    return [
      q.attached === false ? h('div', { class: 'muted small' }, 'The agent is finding its way back after a restart. Your answer is kept for it.') : null,
      h('div', { class: 'ask-foot' },
        h('span', { class: 'muted small ask-wait', role: 'note' }),
        h('button', { type: 'button', class: 'mini ask-terminal', disabled: s.busy, title: 'Let it go: the agent then shows its own prompt in the terminal, at once. Nothing is answered for you.', onclick: toTerminal }, 'Answer in the terminal instead')),
    ];
  }

  // A question, or several asked together (one step each): every option a
  // large button with what it means; several may be ticked where the agent
  // allows it; "Other…" is a field for an answer of the owner's own. An
  // answer is sent as the places of the options picked, never their text.
  questionEl(q, s) {
    const qs = Array.isArray(q.questions) ? q.questions : [], many = qs.length > 1;
    const n = Math.max(0, Math.min(s.step, qs.length - 1)), cur = qs[n] || {}, last = n >= qs.length - 1;
    const opts = Array.isArray(cur.options) ? cur.options : [];
    const has = (i) => s.picks[i].size > 0 || s.other[i].trim() !== '';
    const send = () => this.answerHeld(q, s, { answers: qs.map((_, i) => ({ picked: [...s.picks[i]].sort((a, b) => a - b), other: s.other[i].trim() || undefined })) }, many ? 'Answers sent' : 'Answer sent');
    const redraw = (then) => { s.rev += 1; this.drawDock(); if (then) then(); };
    const pick = (i) => {
      const set = s.picks[n];
      if (cur.multi) { set.has(i) ? set.delete(i) : set.add(i); return redraw(); }
      set.clear(); set.add(i);
      s.other[n] = ''; s.otherOpen[n] = false;
      // One question with one choice: the tap is the answer. With several
      // questions it leads to the next, and the last step sends them all.
      if (!many) return send();
      if (!last) s.step = n + 1;
      redraw();
    };
    let submit = null;
    const ready = () => (many && !last ? has(n) : qs.every((_, i) => has(i)));
    const other = h('input', { class: 'ask-other', placeholder: 'Your own answer', 'aria-label': 'Your own answer', maxLength: 4000, disabled: s.busy, enterkeyhint: 'send', dataset: { keep: `ask:${q.id}:${n}` } });
    other.value = s.other[n];
    const list = h('div', { class: 'ask-opts', role: cur.multi ? 'group' : 'radiogroup', 'aria-label': String(cur.question || 'Choices') },
      opts.map((o, i) => h('button', { type: 'button', class: 'ask-opt' + (s.picks[n].has(i) ? ' on' : ''), role: cur.multi ? 'checkbox' : 'radio', 'aria-checked': String(s.picks[n].has(i)), disabled: s.busy, onclick: () => pick(i) },
        h('span', { class: 'ask-mark' + (cur.multi ? ' box' : ''), 'aria-hidden': 'true' }, s.picks[n].has(i) ? '✓' : ''),
        h('span', { class: 'ask-label' }, h('b', {}, String(o.label || '')), o.description ? h('span', { class: 'muted' }, String(o.description)) : null))),
      h('button', { type: 'button', class: 'ask-opt other' + (s.otherOpen[n] ? ' on' : ''), 'aria-expanded': String(!!s.otherOpen[n]), disabled: s.busy,
        onclick: () => { s.otherOpen[n] = !s.otherOpen[n]; if (!s.otherOpen[n]) s.other[n] = ''; redraw(() => { if (s.otherOpen[n]) this.cards.querySelector('.ask-other')?.focus(); }); } },
        h('span', { class: 'ask-mark' + (cur.multi ? ' box' : ''), 'aria-hidden': 'true' }, s.otherOpen[n] && cur.multi ? '✓' : ''), h('span', { class: 'ask-label' }, h('b', {}, 'Other…'))),
      s.otherOpen[n] ? other : null);
    other.addEventListener('input', () => {
      s.other[n] = other.value;
      // Where one answer is taken, an answer of the owner's own is that one.
      if (!cur.multi && other.value.trim()) {
        s.picks[n].clear();
        for (const b of list.querySelectorAll('.ask-opt.on:not(.other)')) { b.classList.remove('on'); b.setAttribute('aria-checked', 'false'); b.firstChild.textContent = ''; }
      }
      if (submit) submit.disabled = s.busy || !ready();
    });
    other.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.isComposing) { e.preventDefault(); if (submit && !submit.disabled) submit.click(); } });
    if (many || cur.multi || s.otherOpen[n]) {
      submit = h('button', { type: 'button', class: 'primary ask-send', disabled: s.busy || !ready(), onclick: () => { if (many && !last) { s.step = n + 1; redraw(); } else send(); } },
        s.busy ? 'Sent…' : many ? (last ? 'Send answers' : 'Next') : 'Send answer');
    }
    return h('div', { class: 'chat-card ask held', dataset: { held: q.id } },
      h('div', { class: 'ask-head' },
        cur.header ? h('span', { class: 'chip' }, String(cur.header)) : null,
        h('span', { class: 'muted small' }, many ? `The agent asks · question ${n + 1} of ${qs.length}` : 'The agent asks'),
        cur.multi ? h('span', { class: 'muted small' }, ' · choose any') : null),
      h('b', { class: 'ask-q' }, String(cur.question || '')),
      list,
      many || submit ? h('div', { class: 'row ask-nav' },
        many && n > 0 ? h('button', { type: 'button', disabled: s.busy, onclick: () => { s.step = n - 1; redraw(); } }, 'Back') : null,
        h('span', { class: 'grow' }), submit) : null,
      s.busy && !submit ? h('div', { class: 'muted small' }, 'Sent…') : null,
      this.heldFoot(q, s));
  }

  // A plan: drawn like a reply (md.js: text, never HTML), then Approve, or
  // Keep planning with a note for the agent.
  planEl(q, s) {
    const note = h('input', { class: 'ask-note', placeholder: 'What to change (optional, with Keep planning)', 'aria-label': 'A note for the agent, sent with Keep planning', maxLength: 4000, disabled: s.busy, dataset: { keep: `ask:${q.id}:note` } });
    note.value = s.note;
    note.addEventListener('input', () => { s.note = note.value; });
    const item = typeof q.item === 'string' ? this.byId.get(q.item) : null;
    const text = typeof q.text === 'string' && q.text ? q.text : String(item?.text || '');
    return h('div', { class: 'chat-card ask held plan', dataset: { held: q.id } },
      h('div', { class: 'ask-head' }, h('span', { class: 'muted small' }, 'The agent proposes a plan and waits for your answer')),
      h('div', { class: 'ask-plan' + (s.tall ? ' tall' : ''), tabIndex: 0, role: 'document', 'aria-label': 'The plan' }, renderMarkdown(text || '(the plan is empty)', { openLink, copy: (t) => copyText(t) })),
      h('button', { type: 'button', class: 'mini ask-tall', 'aria-pressed': String(!!s.tall), onclick: () => { s.tall = !s.tall; s.rev += 1; this.drawDock(); } }, s.tall ? 'Show less of the plan' : 'Show the whole plan'),
      q.more ? h('div', { class: 'muted small' }, 'The plan is longer than is shown here: the rest is in the terminal.') : null,
      note,
      h('div', { class: 'row ask-nav' },
        h('button', { type: 'button', class: 'primary ask-approve', disabled: s.busy, onclick: () => this.answerHeld(q, s, { approve: true }, 'Plan approved') }, s.busy ? 'Sent…' : 'Approve'),
        h('button', { type: 'button', class: 'ask-keep', disabled: s.busy, onclick: () => this.answerHeld(q, s, { approve: false, text: s.note.trim() || undefined }, 'Sent back: it keeps planning') }, 'Keep planning')),
      this.heldFoot(q, s));
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
    // How much longer a held question waits here: the station's deadline,
    // by the station's clock.
    const now = (this.live?.now_ms || 0) + waited;
    for (const w of this.waits) {
      if (!w.el || typeof w.until !== 'number') continue;
      const left = w.until - now;
      w.el.textContent = left > 0 ? `Waits here for ${moreTime(left)}, then moves to the terminal.` : 'Its time here is up: it moves to the terminal.';
    }
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

  // What the agent has running beside its turn, and what of it is over:
  // as the station says (`live.tasks`: from the agent's own session file,
  // and the panes it started through spandeck). Sub-agents its hooks
  // report and its file does not (Codex; a station from before) are rows
  // too, with what the hook said: a kind and since when.
  tasksNow() {
    const L = this.live || {}, T = L.tasks && typeof L.tasks === 'object' ? L.tasks : {};
    const rows = (a) => (Array.isArray(a) ? a.filter(t => t && typeof t.id === 'string') : []);
    const running = rows(T.running), finished = rows(T.finished);
    if (!running.some(t => t.kind === 'agent')) {
      for (const [i, s] of (Array.isArray(L.subagents) ? L.subagents : []).entries()) running.push({ id: 'hook:' + i, kind: 'agent', name: String(s.kind || ''), title: '', status: 'running', started_ms: s.since_ms });
    }
    return { running, finished, total: Math.max(Number(T.finished_total) || 0, finished.length) };
  }

  tasksSheet() {
    const list = h('div', { class: 'task-list' });
    let showDone = false;
    const row = (t, now) => {
      const [mark, kind] = TASK_KIND[t.kind] || ['·', String(t.kind || 'task')];
      const run = t.status === 'running';
      const took = typeof t.started_ms === 'number' ? (run ? now - t.started_ms : typeof t.ended_ms === 'number' ? t.ended_ms - t.started_ms : null) : null;
      const how = run ? '' : { lost: 'no longer running (its program ended or was started again)', ok: 'finished', completed: 'completed', done: 'finished', error: 'failed', failed: 'failed', interrupted: 'stopped', ended: 'its pane has ended' }[t.status] || String(t.status || '');
      const title = String(t.title || '') || (t.kind === 'helper' ? String(t.name || '') : t.name ? `${t.name} sub-agent` : t.from ? 'A sub-agent\'s conversation' : kind);
      return h('div', { class: 'task-row' + (run ? ' running' : ''), dataset: { task: t.id, kind: String(t.kind || '') } },
        h('span', { class: 'task-mark', 'aria-hidden': 'true' }, mark),
        h('div', { class: 'task-what' },
          h('b', {}, title),
          h('span', { class: 'muted' }, [t.kind === 'helper' && t.title ? `${kind} ${t.name || ''}`.trim() : t.kind === 'agent' && t.name && t.title ? `${kind} · ${t.name}` : kind, took !== null && took >= 0 ? dur(took) : '', how, t.sure === false ? (run ? 'written to just now' : 'not written to lately') : ''].filter(Boolean).join(' · ')),
          t.kind === 'agent' && t.transcript ? h('button', { type: 'button', class: 'mini task-open', onclick: () => this.sideSheet(t) }, 'View transcript') : null,
          t.kind === 'helper' && t.pane ? h('button', { type: 'button', class: 'mini task-open', onclick: () => { this.tasksClose?.(); env.go(env.paneUrl(this.st, t.pane)); } }, 'Open its pane') : null),
        // Only a helper's turn can be stopped by itself: it is a pane,
        // and the station stops a pane's turn. An agent's own sub-agents
        // and commands stop with its turn.
        t.kind === 'helper' && t.can_stop && t.pane ? h('button', { type: 'button', class: 'task-stop', 'aria-label': `Stop ${t.name || 'this helper'}'s turn`, title: 'Stop this helper\'s turn. Its pane stays.',
          onclick: () => env.rpc(this.st, 'pane.stop', { pane: t.pane }).then(() => toast(`Stopped ${t.name || 'the helper'}`)).catch(() => {}) }, '■') : null);
    };
    const draw = () => {
      if (!list.isConnected && list.dataset.drawn) { this.tasksBox = null; return; }
      list.dataset.drawn = '1';
      const T = this.tasksNow(), now = (this.live?.now_ms || 0) + (performance.now() - this.liveAt);
      sync(list, [
        h('h3', {}, `Running${T.running.length ? ' · ' + T.running.length : ''}`),
        ...(T.running.length ? T.running.map(t => row(t, now)) : [h('p', { class: 'muted' }, 'Nothing is running in the background.')]),
        T.total ? h('button', { type: 'button', class: 'task-fold', 'aria-expanded': String(showDone), onclick: () => { showDone = !showDone; draw(); } }, `Finished · ${T.total}`, h('span', { class: 'chev', 'aria-hidden': 'true' }, showDone ? '▾' : '›')) : null,
        ...(showDone ? T.finished.map(t => row(t, now)) : []),
        showDone && T.total > T.finished.length ? h('p', { class: 'muted small' }, `The ${T.finished.length} newest are listed.`) : null,
        h('p', { class: 'muted small' }, `What ${this.p.name} left running, its sub-agents, and the panes it started through spandeck. Its own sub-agents and commands stop when its turn is stopped; a helper pane has a stop of its own.`),
      ].filter(Boolean));
    };
    const box = dialog('Background tasks', () => list, { onClose: () => { this.tasksBox = null; this.tasksClose = null; } });
    box.body.parentNode.classList.add('sheet', 'chat-sheet');
    this.tasksBox = draw;
    this.tasksClose = box.close;
    draw();
  }

  // A sub-agent's own conversation, drawn by what draws every
  // conversation, to read only: nothing can be sent to it or answered.
  async sideSheet(t) {
    const log = h('div', { class: 'chat-log side-log', role: 'log', 'aria-label': 'The sub-agent\'s conversation' }, quiet('Loading…'));
    const box = dialog(`${t.name || 'Sub-agent'} · ${String(t.title || '').slice(0, 60)}`, () => h('div', { class: 'chat-body side-body' }, log));
    box.body.parentNode.classList.add('sheet', 'chat-sheet');
    let r;
    try { r = await env.api.on(this.st, 'pane.conversation.detail', { pane: this.p.id, task: t.id, session: this.session }); } catch (e) { log.replaceChildren(quiet('Its transcript could not be read: ' + e.message, 'warn')); return; }
    const items = (Array.isArray(r.items) ? r.items : []).filter(x => x && typeof x.id === 'string');
    // The same drawing, with nothing behind it to ask: a row opens to
    // nothing more, and no note is cleared from here.
    const v = Object.create(this);
    Object.assign(v, { open: new Set(), revs: new Map(), full: new Map(), details: new Map(), showCleared: true, heldFor: () => null, whole: () => {},
      detailEl: () => h('div', { class: 'chat-detail muted' }, 'What a sub-agent\'s call was given and what came back are in its file, not shown here.'),
      clearNote: () => {} });
    const draw = () => {
      const out = [];
      if (r.cut) out.push(quiet('The start of this transcript is too far back to show here.'));
      if (!items.length) out.push(quiet('Nothing is written in its transcript yet.'));
      for (let i = 0; i < items.length;) {
        if (RUN.has(items[i].kind)) { let j = i; while (j < items.length && RUN.has(items[j].kind)) j += 1; out.push(v.runEl(items.slice(i, j))); i = j; continue; }
        out.push(v.itemEl(items[i], true));
        i += 1;
      }
      log.replaceChildren(...out);
    };
    v.touch = (id) => { v.revs.set(id, (v.revs.get(id) || 0) + 1); draw(); };
    v.drawLog = draw;
    draw();
    box.body.scrollTop = box.body.scrollHeight;
  }

  // ---- the model, and how hard it thinks ----

  // The chip says what the agent last reported, never what was asked for.
  modelState() {
    const M = this.live?.model, b = this.modelBtn;
    b.hidden = !M || typeof M !== 'object' || this.available !== true;
    if (b.hidden) return;
    const model = M.model || this.p.usage?.model || '';
    // (A model a command has just set is named by the program before the
    // agent has answered on it: `set`.)
    const shown = typeof M.set === 'string' && M.set ? M.set : model ? modelName(model) : '';
    const label = shown ? [shown, M.effort ? cap1(String(M.effort)) : ''].filter(Boolean).join(' · ') : 'Model';
    const waits = this.modelQueued ? ' · queued' : '';
    if (b.textContent !== label + waits) b.textContent = label + waits;
    b.title = shown ? `Model: ${M.set || model}${M.effort ? ', effort ' + M.effort : ''}. Tap to change.` : 'Choose the model';
    b.setAttribute('aria-label', (shown ? `Model ${label}` : 'Choose the model') + (waits ? ', a change is queued' : ''));
    // A choice made in the middle of a turn goes when the turn is over.
    if (this.modelQueued && this.live.state !== 'working') { const q = this.modelQueued; this.modelQueued = null; this.setModel(q, true); }
  }

  // `pane.model`: the station types the agent program's own command into
  // the pane, as the owner would, and only while the agent is idle.
  setModel(params, late) {
    return env.rpc(this.st, 'pane.model', { pane: this.p.id, ...params }).then((r) => {
      if (r && r.restarted) toast(`${this.p.name} was started again for this pane alone and resumes its conversation.`);
      else if (r && r.sent) toast(`Sent ${r.command} to ${this.p.name}. The chip follows what it reports.`);
      else if (r && r.busy && !late) { this.modelQueued = params; toast('It is in the middle of a turn. It will be sent when this turn ends, while this page stays open.'); }
      else toast(`Not sent: ${this.p.name} is ${r?.why || 'not ready'}.`, 'error');
      this.modelState();
    }).catch(() => {});
  }

  modelSheet() {
    const M = this.live?.model || {}, name = this.p.agent_name || 'The agent';
    const current = String(M.model || this.p.usage?.model || '');
    const models = Array.isArray(M.models) ? M.models.filter(m => m && typeof m.id === 'string') : [];
    const efforts = Array.isArray(M.efforts) ? M.efforts.filter(e => typeof e === 'string') : [];
    // An alias is ticked when the model in use is of its family
    // ("fable" for claude-fable-5-2); "default" is a way back, not a model.
    const is = (m) => m.id !== 'default' && current !== '' && (current === m.id || modelShort(current).split(/[-.]/).includes(m.id));
    const box = dialog('Model', (close, body) => {
      const go = (params) => { close(); this.setModel(params); };
      // Said before anything is typed: what the program's own command
      // changes beyond this pane, and the way that changes this pane only.
      const only = M.only_pane && typeof M.only_pane === 'object' ? M.only_pane : {};
      const pick = (params) => {
        const what = params.model ? 'model' : 'effort', value = params.model || cap1(params.effort);
        if (!M.saves && !only[what]) return go(params);
        body.replaceChildren(h('div', { class: 'model-sheet model-confirm' },
          h('b', { class: 'ask-q' }, params.model ? `Switch ${this.p.name} to ${value}?` : `Set ${this.p.name}'s effort to ${value}?`),
          M.saves ? h('p', { class: 'model-saves' }, String(M.saves)) : null,
          h('button', { type: 'button', class: 'primary model-go', onclick: () => go(params) }, M.saves ? 'Switch, and keep it as the default' : 'Switch'),
          only[what] ? h('button', { type: 'button', class: 'model-only', onclick: () => go({ ...params, only_pane: true }) }, 'Only this pane: start it again and resume') : null,
          only[what] ? h('p', { class: 'muted small' }, `"Only this pane" types nothing and changes no default: ${name} is started again on ${value} and picks its conversation up where it was. What it left running in the background stops, and it must be idle.`) : null,
          h('button', { type: 'button', class: 'model-back', onclick: close }, 'Cancel')));
      };
      const other = h('input', { class: 'ask-other model-other', placeholder: 'A model\'s name, as the program takes it', 'aria-label': 'Another model\'s name', maxLength: 120, autocapitalize: 'off', autocorrect: 'off', spellcheck: false, enterkeyhint: 'send' });
      const set = h('button', { type: 'button', class: 'primary model-set', onclick: () => { const v = other.value.trim(); if (v) pick({ model: v }); } }, 'Set');
      other.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.isComposing) { e.preventDefault(); set.click(); } });
      const toPicker = () => { close(); env.rpc(this.st, 'agents.command', { pane: this.p.id, command: M.picker }).then((r) => {
        const skipped = r?.stations?.[0]?.skipped?.[0];
        if (skipped) toast(`${name} is ${skipped.why}: its picker was not opened. Type ${M.picker} in the terminal when it is free.`, 'error');
        this.keysOpen = true;
        setFace(this.st, this.p, 'terminal');
      }).catch(() => {}); };
      return h('div', { class: 'model-sheet' },
        current ? h('p', { class: 'muted small model-now' }, `In use now, as ${name} last reported: ${current}${M.effort ? ' · effort ' + M.effort : ''}`) : h('p', { class: 'muted small model-now' }, `${name} has not reported its model yet.`),
        M.can_set ? h('div', { class: 'ask-opts model-list', role: 'radiogroup', 'aria-label': 'Models' },
          models.map(m => h('button', { type: 'button', class: 'ask-opt model-opt' + (is(m) ? ' on' : ''), role: 'radio', 'aria-checked': String(is(m)), dataset: { model: m.id }, onclick: () => pick({ model: m.id }) },
            h('span', { class: 'ask-label' }, h('b', {}, String(m.name || m.id)), m.says ? h('span', { class: 'muted' }, String(m.says)) : null),
            h('span', { class: 'model-tick', 'aria-hidden': 'true' }, is(m) ? '✓' : '')))) : null,
        M.can_set ? h('div', { class: 'model-other-row' }, h('label', { class: 'muted small' }, 'Other…'), h('div', { class: 'row' }, other, set)) : null,
        !M.can_set && M.picker ? h('button', { type: 'button', class: 'primary model-picker', onclick: toPicker }, `Open its picker in the terminal (${M.picker})`) : null,
        efforts.length ? h('div', { class: 'model-effort' }, h('h3', {}, 'Effort'),
          h('div', { class: 'chat-switch effort-switch', role: 'radiogroup', 'aria-label': 'Effort' },
            efforts.map(e => h('button', { type: 'button', class: M.effort === e ? 'on' : '', role: 'radio', 'aria-checked': String(M.effort === e), dataset: { effort: e }, onclick: () => pick({ effort: e }) }, cap1(e))))) : null,
        M.note ? h('p', { class: 'muted small' }, String(M.note)) : null,
        h('p', { class: 'muted small' }, `spandeck types ${name}'s own command into the pane, as you would, and only while it is idle.`));
    });
    box.body.parentNode.classList.add('sheet', 'chat-sheet');
  }

  // ---------------------------------------------------------- the message box

  buildComposer() {
    const p = this.p;
    const Speech = window.SpeechRecognition || window.webkitSpeechRecognition;
    this.speech = !!Speech;
    this.keysEl = h('div', { class: 'wait-keys chat-keys', role: 'group', 'aria-label': 'Keys for the terminal', hidden: true },
      SCREEN_KEYS.map(([label, bytes]) => h('button', { type: 'button', class: 'key', 'aria-label': SCREEN_KEY_SAYS[label] || label, onclick: () => this.key1(bytes) }, label)));
    this.box = h('textarea', { class: 'chat-box', rows: 1, placeholder: Speech ? 'Message or hold to talk' : 'Message', 'aria-label': 'Message', enterkeyhint: 'send', dataset: { keep: 'chat:' + this.key } });
    this.box.value = drafts.get(this.key) || '';
    this.box.addEventListener('input', () => this.typed());
    // On a computer Enter sends and Shift+Enter starts a line; a phone's
    // keyboard has Send beside the box.
    this.box.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !env.isPhone()) { e.preventDefault(); this.send(); } });
    this.sendBtn = h('button', { type: 'submit', class: 'primary chat-send' }, 'Send');
    // Esc, Tab, the arrows, Enter and Ctrl-C for the pane: one tap away, in
    // every pane's message box, and out of the way until asked for.
    // The model in use and how hard it thinks, as the agent reports them.
    this.modelBtn = h('button', { type: 'button', class: 'chat-model', hidden: true, 'aria-haspopup': 'dialog', onclick: (e) => { e.currentTarget.blur(); this.modelSheet(); } }, 'Model');
    this.keysBtn = h('button', { type: 'button', class: 'chat-keys-btn', title: 'Keys for the terminal: Esc, Tab, the arrows, Enter, Ctrl-C', 'aria-label': 'Keys for the terminal', 'aria-expanded': 'false',
      onclick: (e) => { e.currentTarget.blur(); this.keysOpen = !this.keysOpen; this.drawDock(); this.resized(); } }, 'Keys');
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
    // One card: what is written above, and under it what can be added on
    // the left and how it is sent on the right. The same in every pane.
    this.composer = h('form', { class: 'chat-composer', onsubmit: (e) => { e.preventDefault(); this.working && !this.box.value.trim() ? this.stopTurn() : this.send(); } },
      this.keysEl, this.slash, this.sendSaid,
      h('div', { class: 'chat-row', onclick: (e) => { if (e.target === e.currentTarget || e.target.classList.contains('chat-tools')) this.box.focus(); } },
        this.box, h('div', { class: 'chat-tools' }, plus, this.keysBtn, this.modelBtn, h('span', { class: 'grow' }), mic, this.sendBtn)));
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
    this.sendBtn.title = stop ? 'Stop this turn. The agent stays.' : 'Send';
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
    if (this.sized) this.sized.disconnect();
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
  const me = { btn, done: () => { if (speaking === me) speaking = null; btn.textContent = btn.title = 'Read aloud'; btn.setAttribute('aria-pressed', 'false'); } };
  u.onend = me.done;
  u.onerror = me.done;
  speaking = me;
  btn.textContent = btn.title = 'Stop reading';
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
      // A question that waits and is not held (yet): its card is due.
      if (c.askSoon) c.drawDock();
      c.tick();
      if (c.peekOpen && c.peekEl.isConnected && document.visibilityState === 'visible') c.peek();
    }
  }, 1000);
}

// What the conversation adds to its pane's menu.
export function chatMenu(st, p) {
  const c = convs.get(keyOf(st, p));
  if (!c || !c.body.isConnected) return [];
  const T = c.tasksNow(), cleared = Array.isArray(c.live?.cleared) ? c.live.cleared.length : 0;
  return [
    T.running.length || T.total ? { label: 'Background tasks', hint: T.running.length ? `${T.running.length} running` : `${T.total} finished`, action: () => c.tasksSheet() } : null,
    cleared || c.showCleared ? { label: c.showCleared ? 'Hide cleared notes again' : 'Show cleared notes', hint: cleared ? String(cleared) : undefined, action: () => { c.showCleared = !c.showCleared; c.drawLog(); } } : null,
    cleared ? { label: 'Bring cleared notes back', action: () => c.restoreNotes() } : null,
    { label: 'About this screen', hint: 'layout numbers', action: () => aboutScreen(c) },
  ].filter(Boolean);
}

// The numbers that say where the message box is and where the browser
// says the screen ends, for when the two do not agree on a phone (the
// owner's report of 17 Nov 2026: a gap under the message box with the
// keyboard up). Shown, copied on a tap, and sent to the station's log
// (`client.report`), where it is read with
// spandeck rpc log.query '{"kind":"client.report"}'. Temporary.
export function screenNumbers(c) {
  const vv = window.visualViewport, r = (el) => { if (!el || !el.isConnected) return null; const b = el.getBoundingClientRect(); return [Math.round(b.top), Math.round(b.bottom), Math.round(b.height)]; };
  const probe = h('div', { style: { position: 'fixed', visibility: 'hidden', paddingTop: 'env(safe-area-inset-top)', paddingBottom: 'env(safe-area-inset-bottom)', paddingLeft: 'env(safe-area-inset-left)', paddingRight: 'env(safe-area-inset-right)' } });
  document.body.append(probe);
  const cs = getComputedStyle(probe), safe = [cs.paddingTop, cs.paddingRight, cs.paddingBottom, cs.paddingLeft].map(x => Math.round(parseFloat(x) || 0));
  probe.remove();
  const app = document.getElementById('app'), dock = c?.dock, card = c?.composer;
  return {
    what: 'layout-numbers', build: env.api.build,
    inner: [window.innerWidth, window.innerHeight], client: [document.documentElement.clientWidth, document.documentElement.clientHeight], screen: [window.screen.width, window.screen.height], dpr: window.devicePixelRatio,
    vv: vv ? { w: Math.round(vv.width), h: Math.round(vv.height * 10) / 10, top: Math.round(vv.offsetTop * 10) / 10, left: Math.round(vv.offsetLeft), page_top: Math.round(vv.pageTop), scale: vv.scale } : null,
    scroll: [window.scrollX, window.scrollY], app: r(app), bar: r(c?.bar), log: r(c?.log), dock: r(dock), card: r(card), box: r(c?.box),
    dock_pad_bottom: dock && dock.isConnected ? getComputedStyle(dock).paddingBottom : null,
    under_card: vv && card && card.isConnected ? Math.round((vv.offsetTop + vv.height - card.getBoundingClientRect().bottom) * 10) / 10 : null,
    safe, kbd: [app.classList.contains('kbd'), document.documentElement.classList.contains('kbd')],
    focus: document.activeElement ? document.activeElement.tagName + '.' + String(document.activeElement.className).split(' ')[0] : null,
    standalone: !!(navigator.standalone || window.matchMedia?.('(display-mode: standalone)').matches), secure: window.isSecureContext, ua: navigator.userAgent.slice(0, 160),
  };
}

function aboutScreen(c) {
  const out = h('pre', { class: 'chat-out about-out', tabIndex: 0 });
  let n = null;
  // Read while the keyboard is up too: the box below can be typed in, and
  // the numbers are taken again each second.
  const take = () => { n = screenNumbers(c); out.textContent = JSON.stringify(n, null, 1); };
  const send = () => { copyText(JSON.stringify(n), 'Copied the numbers'); env.api.call('client.report', n).catch(() => {}); };
  const timer = setInterval(() => { if (!out.isConnected) return clearInterval(timer); take(); }, 1000);
  const box = dialog('About this screen', (close) => h('div', { class: 'about-screen' },
    h('p', { class: 'muted small' }, 'Where the browser says the screen is, and where the message box is. "under_card" is the room between the message box and the bottom of what the browser says is in sight: it should be 0 with the keyboard up. Tap the numbers to copy them; they are also sent to this station\'s log.'),
    h('input', { class: 'ask-other', placeholder: 'Tap here to bring the keyboard up, then tap the numbers', 'aria-label': 'A box to bring the keyboard up' }),
    out,
    h('div', { class: 'row end' }, h('button', { type: 'button', class: 'primary about-copy', onclick: send }, 'Copy and send'), h('button', { type: 'button', onclick: close }, 'Close'))));
  out.addEventListener('click', send);
  take();
  return box;
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
  c.parked = false;
  if (c.state === 'off' && !c.retry && !c.old) c.watch();
  c.drawBar();
  c.drawDock();
  return c;
}

// What is in sight changed its size (a phone's keyboard): every log on the
// page keeps its place. Called by the page as soon as it knows, since a
// browser tells a log of its new size only with its next frame.
export function chatResized() {
  for (const c of convs.values()) c.resized();
}

// (Only a log that moved since it was last told of: one that grew under an
// owner reading its end stays at its end.)
export function chatBeforeRender() {
  for (const c of convs.values()) {
    if (c.log.isConnected && c.log.scrollTop !== c.top) c.note();
    c.used = false;
  }
}
// After the page was drawn: a conversation that left the page is let go of,
// and each one still on it is where it was scrolled to.
export function chatAfterRender() {
  for (const c of [...convs.values()]) {
    if (!c.used) c.dispose();
    else if (c.parked) c.park();
    else c.placed();
  }
}

// A pane of the layout a phone is stepping through, while another of its
// panes is the one on screen: its conversation is kept (what was read, where,
// and the draft) and not watched. It is let go of with the layout's view.
export function chatPark(st, p) {
  const c = convs.get(keyOf(st, p));
  if (c) { c.p = p; c.used = true; c.parked = true; }
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
  // Held for the owner in its conversation: said short, so that it fits.
  if (s === 'needs_you' && p.asks) return asksWords(p);
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
    const trust = trustTick((q) => env.api.on(S.st, 'foldertrust.choice', q), () => ({ agent: S.agent, project: proj.value, cwd: cwd.value.trim() }));
    const hooks = hooksTick((q) => env.api.on(S.st, 'hookstrust.choice', q), () => ({ agent: S.agent }));
    const first = h('textarea', { rows: 3, class: 'chat-first', placeholder: 'Optional: what to say first', 'aria-label': 'First message' });
    const firstRow = h('label', { class: 'field' }, h('span', {}, 'First message'), first);
    let asked = 0, canContinue = true, timer = null;
    const ask = async () => {
      const n = ++asked, agent = S.agent !== 'shell';
      trust.refresh();
      hooks.refresh();
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
        trust_folder: trust.value(), trust_hooks: hooks.value(), resume_last: resume || undefined, prompt: agent && first.value.trim() ? first.value.trim() : undefined });
      try { localStorage.setItem('spandeck.newSession.agent', S.agent); } catch (e) { /* not kept */ }
      close();
      if (resume && !pane.resumed) toast('No saved conversation to continue here: a new one was started');
      env.go(env.paneUrl(S.st, pane.id));
    };
    return h('form', { class: 'new-session-form', onsubmit: (e) => { e.preventDefault(); start().catch(() => {}); } },
      h('h3', {}, 'Agent'), tiles, h('h3', {}, 'Station'), chips,
      h('label', { class: 'field' }, h('span', {}, 'Project'), proj), h('label', { class: 'field' }, h('span', {}, 'Folder'), cwd),
      lastRow, trust.row, hooks.row, firstRow,
      h('div', { class: 'row end sheet-foot' }, h('button', { type: 'button', onclick: close }, 'Cancel'), h('button', { class: 'primary', type: 'submit' }, 'Start')));
  });
  box.body.parentNode.classList.add('sheet');
}

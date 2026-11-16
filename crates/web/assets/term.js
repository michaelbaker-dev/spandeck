// One pane on screen: the activity strip and a live terminal.
//
// Sizing rule: one size per pane, set by whoever has control. A client in
// control fits the pane to its view and tells the station. Anyone else shows
// the pane at its real size, scaled down and letterboxed. The rule is the
// pane's station's, and is the same for a pane on another station shown
// through this one: each tab is a client of its own there (`this.me`), and
// that station's resizes arrive on the pane's channel (`onResized`).
//
// How a terminal looks (colours, font, cursor) is a look (look.js). A font
// change follows the sizing rule: the client in control fits the pane again
// and tells the station; anyone else only scales what it shows. A font that
// finishes loading after the terminal was drawn (an uploaded font, the
// symbols font) is the same case: the terminal measured its cells with the
// font that stood in for it, so it measures again and then does as above.

import { h, ago, dot, STATE_LABEL, stateOf, toast, copyText, usageGauge, heldWords, RELEASE_SAYS } from './ui.js';
import { paneIcon, iconName, hideIcon } from './icons.js';
import { keyTyped, paneWrote, keysNotSent } from './perf.js';
import { HOLD_MS, isTyping, isPaste, holdTake, heldTooLong, keysWords, wasWere, offWords, droppedWords, sendAct } from './typed.js';
import { retryIn } from './api.js';
import { BASE, termOptions, applyPaneFrame, SIZING } from './look.js';

// A terminal as every pane has it. Emoji take two cells, as the programs
// that print them expect: the terminal's own table (Unicode 6) gives them
// one, and the next character was drawn on top of them.
export function newTerminal(options) {
  const term = new Terminal({ allowProposedApi: true, ...options });
  term.loadAddon(new Unicode11Addon.Unicode11Addon());
  term.unicode.activeVersion = '11';
  return term;
}

export function remeasureTerminal(term) {
  const family = term.options.fontFamily;
  term.options.fontFamily = 'monospace';
  term.options.fontFamily = family;
}

export class PaneTerm {
  // opts: {mode: 'control'|'watch', onMenu(ev, pane), onFocus(), compact}
  constructor(api, station, pane, opts = {}) {
    this.api = api;
    this.station = station;
    this.pane = pane;
    this.opts = opts;
    this.mode = opts.mode || 'control';
    this.control = this.mode === 'control';
    this.chan = null;
    this.disposed = false;
    this.size = { cols: pane.cols, rows: pane.rows };
    // Who this tab is at the pane's station; attach says, for another station.
    this.me = api.client;

    this.strip = h('div', { class: 'strip', oncontextmenu: (e) => this._menu(e) });
    this.body = h('div', { class: 'term-body' });
    // Slides in from the right: the agents working under this one.
    this.drawer = h('aside', { class: 'agents-drawer', 'aria-label': 'Agents under this one' });
    this.agentsOpen = false;
    this.note = h('div', { class: 'term-note', hidden: true });
    // The page's own link to its station is down: said on every pane.
    this.linkNote = h('div', { class: 'term-note link-note', role: 'status', hidden: true });
    // What was typed here and not sent: how many keys, and why.
    this.typeNote = h('div', { class: 'term-note type-note', role: 'alert', hidden: true });
    this.tries = 0;
    // What is typed (typed.js). `iseq`: the number of the last thing sent on
    // this attachment. `hold`: typed while this terminal could not send, for
    // at most 1.5 s. `off`: after that, until it can send again, every key is
    // refused at once and counted: { keys, pastes, why }. `sent`: the last
    // things sent, to say which of them a station did not take, or which may
    // not have arrived when the link went silent.
    this.iseq = 0;
    this.hold = [];
    this.off = null;
    this.sent = [];
    this.el = h('div', {
      class: 'pane', dataset: { pane: pane.id },
      onmousedown: () => opts.onFocus && opts.onFocus(this),
      oncontextmenu: (e) => this._menu(e),
      ondragover: (e) => { if (this.control) { e.preventDefault(); this.el.classList.add('dropping'); } },
      ondragleave: () => this.el.classList.remove('dropping'),
      ondrop: (e) => this._drop(e),
    }, this.strip, h('div', { class: 'pane-main' }, this.body, this.drawer), this.note, h('div', { class: 'term-notes' }, this.linkNote, this.typeNote));
    // The element leads back to its terminal (the pane menu and tests use it).
    this.el.paneTerm = this;

    this.term = newTerminal({
      ...termOptions(BASE, opts.compact), scrollback: 5000,
      disableStdin: !this.control && !opts.typeWhileWatching, macOptionIsMeta: true,
      // A full-screen agent listens for the mouse, so a plain drag goes to it
      // and selects nothing. Option (Mac) or Shift makes the drag a selection.
      // A right-click opens our menu and leaves the selection as it is.
      macOptionClickForcesSelection: true, rightClickSelectsWord: false,
    });
    this.fit = new FitAddon.FitAddon();
    this.term.loadAddon(this.fit);
    this.term.onData((data) => this._typed(data));
    // The link changed, or the station has read this page's clock: what was
    // held may go now. The page was not running: what was held is too old.
    this._offStatus = api.onStatus(() => this._linkChanged());
    this._offEvents = api.onEvent((ev) => {
      if (ev.kind === 'input.ready') this._flush();
      else if (ev.kind === 'page.woke') this._refuseHeld('asleep');
    });
    this.term.onResize(() => {});
    // Ctrl+Shift+C copies what is selected, and Ctrl+Shift+V is left to the
    // browser, which pastes. (Cmd-C and Cmd-V on a Mac need nothing here.)
    this.term.attachCustomKeyEventHandler((e) => {
      if (e.type !== 'keydown' || !e.ctrlKey || !e.shiftKey || e.altKey || e.metaKey) return true;
      const k = e.key.toLowerCase();
      if (k === 'c' && this.hasSelection()) { e.preventDefault(); this.copySelection(); return false; }
      return k !== 'v';
    });
    // Paste of images or files goes to the station as a file drop.
    this.body.addEventListener('paste', (e) => {
      const files = [...(e.clipboardData?.files || [])];
      if (files.length && this.control) { e.preventDefault(); e.stopPropagation(); files.forEach(f => this._upload(f)); }
    }, true);

    // A finger drags the history up and down. The terminal library has its
    // own touch handling, which sees the finger first and did not scroll on a
    // real iPhone. So this listens ahead of it (capture) and scrolls the
    // history directly; the library never sees a drag. A tap still reaches
    // it, to focus the terminal and bring up the keyboard.
    let touchY = null, carry = 0, dragging = false, moved = 0, events = 0, from = 0, lastReport = 0;
    const lineHeight = () => {
      const row = this.term.element?.querySelector('.xterm-rows')?.firstElementChild;
      return (row && row.getBoundingClientRect().height) || 17;
    };
    this.body.addEventListener('touchstart', (e) => {
      touchY = e.touches.length === 1 ? e.touches[0].clientY : null;
      carry = 0; dragging = false; moved = 0; events = 0; from = this.term.buffer.active.viewportY;
    }, { capture: true, passive: true });
    this.body.addEventListener('touchmove', (e) => {
      if (touchY === null || e.touches.length !== 1) return;
      const y = e.touches[0].clientY;
      carry += touchY - y;
      touchY = y;
      if (!dragging && Math.abs(carry) < 6) return;
      dragging = true;
      const line = lineHeight();
      const lines = Math.trunc(carry / line);
      events += 1;
      if (lines) { this._scrollBy(lines); carry -= lines * line; moved += lines; }
      e.preventDefault();
      e.stopPropagation();
    }, { capture: true, passive: false });
    const endTouch = (e) => {
      // The end of a drag is not a tap: do not let it move the cursor or focus.
      if (dragging) { e.preventDefault(); e.stopPropagation(); }
      // Touch scrolling cannot be tested on a real phone from the station, so
      // the page says what a drag did. Read with: spandeck rpc log.query '{"kind":"client.report"}'
      if (events && Date.now() - lastReport > 4000) {
        lastReport = Date.now();
        const b = this.term.buffer.active;
        this.api.call('client.report', { what: 'touch-drag', build: this.api.build, mode: this.mode, control: this.control,
          events, asked: moved, from, to: b.viewportY, base: b.baseY, rows: this.term.rows, alt: b.type,
          mouse: this.term.modes?.mouseTrackingMode, line: Math.round(lineHeight() * 10) / 10, ua: navigator.userAgent.slice(0, 120) }).catch(() => {});
      }
      touchY = null; dragging = false;
    };
    this.body.addEventListener('touchend', endTouch, { capture: true, passive: false });
    this.body.addEventListener('touchcancel', endTouch, { capture: true, passive: false });

    this.ro = new ResizeObserver(() => { clearTimeout(this._rt); this._rt = setTimeout(() => this.layout(), 60); });
    this.renderStrip();
    this._tick = setInterval(() => this.renderStrip(), 1000);
  }

  mount() {
    this.term.open(this.body);
    // Whoever is typed into last is who gets the keyboard back after a
    // reconnect, however it got it (a click, Tab, the layout's own keys).
    this.term.textarea?.addEventListener('focus', () => this.opts.onFocus && this.opts.onFocus(this));
    this.ro.observe(this.body);
    this.attach();
    return this;
  }

  _silentWords() {
    const s = Math.round((Date.now() - this.silentSince) / 1000);
    return `${this.station} has not answered for ${s < 60 ? s + ' s' : Math.floor(s / 60) + ' min'}: typing is off. Trying again…`;
  }

  // What the page says of its own link, or nothing.
  setLink(text) {
    this.linkNote.hidden = !text;
    if (text && this.linkNote.textContent !== text) this.linkNote.textContent = text;
    this.el.classList.toggle('link-down', !!text);
  }

  // The page has a new connection: the channel of the one before names
  // nothing on it (it was let go of when that connection ended, `onClosed`).
  reattach() { this.chan = null; return this.attach(); }

  // ---- what is typed (DECISIONS.md, "What is typed") ----
  //
  // A key is sent at once when this terminal can send: the page is
  // connected and answering, the station has read this page's clock, and
  // this terminal's own attach has been answered. Otherwise it is held for
  // at most 1.5 s and 256 bytes and goes out, in order, the moment the
  // terminal can send; and when that does not come in time it is refused
  // where the owner can see it, with how many keys, and never sent later.
  // (The audit: keys typed in a gap were dropped without a word, or
  // delivered a minute late in one burst.)

  // Can this terminal send what is typed, now?
  _canSend() {
    if (!this.api.ready) return false;
    return this.control ? this.chan !== null : !!this.opts.typeWhileWatching;
  }
  // Why not: the page's own link, or this pane not being attached.
  _offWhy() { return this.api.offWhy || 'pane'; }

  _typed(data) {
    const typing = isTyping(data);
    if (this._canSend()) return this._put({ data, t: performance.now() }, typing);
    // What the terminal says by itself (focus, the mouse, an answer to
    // the program) has nowhere to go and is nobody's typing.
    if (!typing) return;
    if (!this.control && !this.opts.typeWhileWatching) return;
    const paste = isPaste(data);
    // Already told that typing is off: counted, with a flash, at once.
    if (this.off) return this._refuse(paste ? 0 : 1, paste ? 1 : 0, this.off.why);
    // A station that does not judge what is typed takes it however late:
    // nothing is held to be sent to it later.
    const took = this.api.stamps ? holdTake(this.hold, { data, t: performance.now(), wall: Date.now() }) : 'full';
    if (took !== 'held') return this._refuse(this.hold.length + (paste ? 0 : 1), paste ? 1 : 0, this._offWhy());
    if (!this._holdTimer) this._holdTimer = setTimeout(() => { this._holdTimer = null; this._flush(true); }, HOLD_MS + 10);
  }

  // Send what was held, in the order it was typed, if this terminal can
  // send now; `due`: the oldest has waited its 1.5 s, so what cannot go now
  // is refused.
  _flush(due = false) {
    const can = this._canSend();
    // It can send again: what the pane said of keys not sent stays for a
    // few seconds, without "typing is off".
    if (can && this.off) {
      const o = this.off;
      this.off = null;
      this._say('off', `${keysWords(o.keys, o.pastes)} typed into ${this.pane.name} ${wasWere(o.keys, o.pastes)} not sent. Typing works again.`);
    }
    if (!this.hold.length) return;
    const now = performance.now(), wall = Date.now();
    if (can) {
      const held = this.hold;
      this.hold = [];
      clearTimeout(this._holdTimer); this._holdTimer = null;
      // Never one that waited too long, nor anything typed after it.
      const stale = held.findIndex(x => heldTooLong(x, now, wall));
      const go = stale < 0 ? held : held.slice(0, stale);
      for (const x of go) this._put(x, true);
      if (stale >= 0) this._refuse(held.length - stale, 0, 'asleep');
    } else if (due || this.hold.some(x => heldTooLong(x, now, wall))) this._refuseHeld(this._offWhy());
  }

  _refuseHeld(why) {
    if (this.hold.length) this._refuse(this.hold.length, 0, why);
  }

  // Keys (and pastes) that were typed here are not sent, now or later:
  // counted, said on the pane, and every further key flashes the pane until
  // it can send again.
  _refuse(keys, pastes, why) {
    this.hold = [];
    clearTimeout(this._holdTimer); this._holdTimer = null;
    if (!this.off) this.off = { keys: 0, pastes: 0, why };
    this.off.keys += keys; this.off.pastes += pastes;
    keysNotSent(keys + pastes);
    const o = this.off, name = this.pane.name;
    // A page that was asleep can send again as soon as it is awake.
    if (o.why === 'asleep') this.off = null;
    const lead = o.why === 'asleep' ? 'This page was not running for a moment' : offWords(o.why, this.api.station, this.station);
    this._say('off', `${lead}: ${keysWords(o.keys, o.pastes)} typed into ${name} ${wasWere(o.keys, o.pastes)} not sent.${o.why === 'asleep' ? '' : ' Typing is off until it is back.'}`, o.why !== 'asleep');
    this.el.classList.remove('bell');
    void this.el.offsetWidth;
    this.el.classList.add('bell');
  }

  // What the pane says of what was typed: one sentence of each kind (keys
  // refused here, keys a station refused, keys that may not have arrived),
  // each for twelve seconds, or (`stay`) until typing works again.
  _say(kind, text, stay = false) {
    const says = this.says || (this.says = new Map()), was = says.get(kind);
    if (was) clearTimeout(was.timer);
    if (!text) says.delete(kind);
    else says.set(kind, { text, timer: stay ? null : setTimeout(() => this._say(kind, null), 12000) });
    this.typeNote.hidden = !says.size;
    this.typeNote.replaceChildren(...[...says.values()].map(x => h('div', {}, x.text)));
    this.el.classList.toggle('type-off', !!this.off);
  }

  // One thing typed goes to the pane: numbered on its attachment, stamped
  // with the time it was typed (the station refuses it when it arrives too
  // late), and never sent a second time.
  _put(item, typing) {
    const stamp = this.api.stamps ? { t: item.t } : {};
    if (this.control && this.chan !== null) {
      if (typing) keyTyped(this);
      const iseq = ++this.iseq;
      this.api.send('pane.input', { chan: this.chan, data: item.data, ...(this.api.stamps ? { iseq } : {}), ...stamp });
      this.sent.push({ iseq, at: performance.now(), typing, paste: isPaste(item.data), chan: this.chan });
      if (this.sent.length > 512) this.sent.splice(0, 256);
      if (typing) this.api.typed();
    } else if (this.opts.typeWhileWatching) {
      // On a phone the pane is watched at its own size, and can still be
      // typed into: the keys go as a request of their own, without taking
      // the size. A refusal is an answer, and is said like any other.
      const paste = isPaste(item.data), sent = { iseq: null, at: performance.now(), typing, paste, chan: null };
      this.sent.push(sent);
      if (this.sent.length > 512) this.sent.splice(0, 256);
      if (typing) this.api.typed();
      const answered = () => { const i = this.sent.indexOf(sent); if (i >= 0) this.sent.splice(i, 1); };
      this.api.on(this.station, 'pane.input', { pane: this.pane.id, data: item.data, ...stamp }).then(answered, (e) => {
        answered();
        // The connection ended first: whether it arrived is not known, and
        // the pane has said so ("may not have been sent").
        if (!typing || e.code === 'connection_lost') return;
        keysNotSent(1);
        if (this.maybe) { this.maybe -= 1; if (this.maybe <= 0) { this.maybe = 0; this._say('maybe', null); } }
        // Several in a row are one sentence with their count.
        const b = this._lost || (this._lost = { keys: 0, pastes: 0 });
        if (paste) b.pastes += 1; else b.keys += 1;
        clearTimeout(this._lostTimer);
        this._lostTimer = setTimeout(() => { this._lost = null; }, 3000);
        // How late: the latest of them.
        if (e.late) b.late = { ...e.late, ageMs: Math.max(e.late.ageMs, b.late ? b.late.ageMs : 0), station: e.late.station || this.api.station };
        this._say('lost', e.late ? droppedWords('late', b.keys, b.pastes, this.pane.name, b.late)
          : `${keysWords(b.keys, b.pastes)} typed into ${this.pane.name} ${wasWere(b.keys, b.pastes)} not sent: ${String(e.message || 'refused').replace(/^not sent: /i, '').replace(/\.$/, '')}.`);
      });
    }
  }

  // A station did not take something typed here (it arrived too late, or
  // the station it was for was silent): which, and why. Several in a row
  // are one sentence with their count.
  _dropped(d) {
    const i = this.sent.findIndex(x => x.iseq === d.iseq && x.chan === this.chan);
    const item = i >= 0 ? this.sent.splice(i, 1)[0] : null;
    // Something the terminal said by itself was not typed: nothing to say.
    if (item && !item.typing) return;
    const b = this._late && this._late.why === d.why ? this._late : (this._late = { why: d.why, keys: 0, pastes: 0, d });
    if (item && item.paste) b.pastes += 1; else b.keys += 1;
    b.d = { ...d, ageMs: Math.max(d.ageMs || 0, b.d.ageMs || 0), station: d.station || this.station };
    keysNotSent(1);
    // It is known now: not "may not have been sent" any more.
    if (this.maybe) { this.maybe -= 1; if (this.maybe <= 0) { this.maybe = 0; this._say('maybe', null); } }
    this._say('late', droppedWords(b.why, b.keys, b.pastes, this.pane.name, b.d));
    clearTimeout(this._lateTimer);
    this._lateTimer = setTimeout(() => { this._late = null; }, 3000);
  }

  // The page's link changed. Gone silent, or closed: what was typed in the
  // moment before, and has had nothing come back since, may not have
  // arrived, and the pane says how many keys that is. Answering again: what
  // was held goes.
  _linkChanged() {
    const api = this.api;
    if (api.live) {
      // Answering again on the connection that went silent: every key that
      // was in it has by now been taken or refused, and a refusal is said.
      // With nothing refused, "may not have been sent" is over.
      if (api.ready && this._wasDown && this.maybe && this._downAt === api.reconnects) {
        setTimeout(() => { if (this.maybe) { this.maybe = 0; this._say('maybe', null); } }, 1500);
      }
      if (api.ready) this._wasDown = false;
      return this._flush();
    }
    if (this._wasDown) return;
    this._wasDown = true;
    this._downAt = api.reconnects;
    // Sent after the last thing heard from the station.
    const n = this.sent.filter(x => x.typing && x.at > api.framePerf).length;
    if (!n || this.off) return;
    this.maybe = n;
    this._say('maybe', `${api.station} stopped answering: the last ${keysWords(n)} typed into ${this.pane.name} may not have been sent. Look at the pane before typing ${n === 1 ? 'it' : 'them'} again.`);
  }

  async attach() {
    if (this.disposed || this.attaching) return;
    this.attaching = true;
    try { await this._attach(); } finally { this.attaching = false; }
  }

  async _attach() {
    if (!this.api.connected) return;
    try {
      const handlers = {
        // This terminal was closed while the pane was being attached.
        gone: () => this.disposed,
        // The connection this attachment was on has ended: from now until
        // this terminal's own next attach is answered it has no channel,
        // and nothing typed into it is sent.
        onClosed: () => { this.chan = null; },
        onDropped: (d) => this._dropped(d),
        onData: (b) => { paneWrote(this); this.term.write(b); },
        onResync: (b) => { this.term.reset(); this.term.write(b); },
        onExit: (info) => this._note(`This pane's process ended (${describeExit(info)}).`),
        // The pane's station said nothing to the one serving this page
        // for six seconds (or closed the connection): the channel is over.
        // Typing is off until the pane is attached again, which is tried by
        // itself: at once, then after 0.1, 0.2 … 5 s, then every 5 s.
        onUnreachable: (why) => {
          if (this.chan !== null) this.api.detach(this.chan);
          this.chan = null;
          // Since when, for what is said while it is tried again.
          this.silentSince = why && why.quietMs !== null ? Date.now() - why.quietMs : null;
          this._note(this.silentSince ? this._silentWords() : `${this.station} closed the connection: typing is off. Trying again…`, () => this.attach());
        },
        // Numbers from another station: whole and within a terminal's range, or passed over.
        onResized: (r) => {
          if (Number.isInteger(r.cols) && Number.isInteger(r.rows) && r.cols >= 2 && r.cols <= 500 && r.rows >= 1 && r.rows <= 300) this.resizedBy(String(r.controller || ''), r.cols, r.rows);
        },
        onDiscarded: (n) => { if (n > 0) toast(`Not sent: the pane was stopped before ${n === 1 ? 'what you typed' : n + ' things you typed'} reached its program.`, 'error'); },
        // Said once in a while, not for every key: the station names itself as text.
        onRefused: (station) => {
          if (Date.now() - (this.refusedAt || 0) < 4000) return;
          this.refusedAt = Date.now();
          toast(`Not sent: ${station || this.station} is not taking what is typed. What you type now is not kept for later.`, 'error');
        },
      };
      const res = await this.api.attach(this.station, this.pane.id, this.mode, handlers);
      if (!res) return;
      this.chan = res.chan;
      // A new attachment counts what is typed from 1.
      this.iseq = 0;
      this.silentSince = null;
      this.tries = 0;
      this.setLink(null);
      this.me = res.client;
      this.size = { cols: res.info.cols, rows: res.info.rows };
      this.term.reset();
      this.term.resize(res.info.cols, res.info.rows);
      this.term.write(res.snapshot);
      this._ended(res.exited);
      this.layout();
      // This terminal's own attach is answered: what was held goes now,
      // and whoever had the keyboard has it again.
      this._flush();
      if (this.opts.onAttached) this.opts.onAttached(this);
    } catch (e) {
      // A pane that is gone does not come back by asking again: said once,
      // with a button, and not asked for every three seconds for ever.
      if (/no such pane/i.test(String(e.message))) this._note('This pane is no longer on its station.', () => this.attach(), 'manual');
      // Its station is still silent: what the pane said stays, with the time.
      else if (this.silentSince && ['not_answering', 'unreachable', 'busy', 'no_answer'].includes(e.code)) this._note(this._silentWords(), () => this.attach());
      else this._note(`Cannot show this pane: ${e.message}`, () => this.attach());
    }
  }

  // The pane's program has ended: why, and for one that ended with a restart
  // of spandeck (or was not started again after three), the way back.
  _ended(exit) {
    clearTimeout(this._retry);
    if (!exit) return this._note(null);
    const p = this.pane, why = endedWhy(p);
    if (!why) return this._note(`This pane's process ended (${describeExit(exit)}).`);
    const back = () => this.api.on(this.station, 'pane.restart', { pane: p.id })
      .then(() => { if (this.chan !== null) this.api.detach(this.chan); this.chan = null; this.attach(); })
      .catch(e => toast(e.message, 'error'));
    this.note.hidden = false;
    this.note.replaceChildren(why[0].toUpperCase() + why.slice(1) + '. ', h('button', { onclick: back }, 'Bring back'));
  }

  // `how` "manual": the button only, no timer.
  _note(text, retry, how) {
    this.note.hidden = !text;
    this.note.replaceChildren(...(text ? [text, retry ? h('button', { onclick: retry }, 'Retry') : null].filter(Boolean) : []));
    clearTimeout(this._retry);
    // Tried again by the schedule every reconnect uses (api.js).
    if (retry && text && how !== 'manual') this._retry = setTimeout(() => !this.disposed && retry(), Math.max(retryIn(this.tries++), 50));
  }

  // Keys and the like, sent to the pane whether this client holds its size or not.
  // Nobody typed these (a finger's drag turned into wheel turns): they are
  // sent when they can be, and are nothing when they cannot.
  _send(data) {
    if (this._canSend()) this._put({ data, t: performance.now() }, false);
  }

  // Scroll by a finger drag. A full-screen program (Claude Code, an editor, a
  // pager) keeps its own history, so there is nothing of ours to move: it is
  // told to scroll, with wheel turns if it listens for the mouse, else with
  // arrow keys, as a desktop terminal does.
  _scrollBy(lines) {
    const b = this.term.buffer.active;
    if (b.type !== 'alternate') { this.term.scrollLines(lines); return; }
    // A wheel turn moves most programs a few lines, so fewer turns than lines.
    const n = Math.min(Math.max(1, Math.round(Math.abs(lines) / 2)), 30);
    const mouse = this.term.modes?.mouseTrackingMode;
    let one;
    if (mouse && mouse !== 'none') {
      one = `\x1b[<${lines < 0 ? 64 : 65};${Math.ceil(this.term.cols / 2)};${Math.ceil(this.term.rows / 2)}M`;
    } else {
      const app = this.term.modes?.applicationCursorKeysMode;
      one = (app ? '\x1bO' : '\x1b[') + (lines < 0 ? 'A' : 'B');
    }
    this._send(one.repeat(n));
  }

  // Fit (in control) or letterbox (watching).
  layout() {
    if (this.disposed || !this.term.element) return;
    const xterm = this.term.element;
    if (this.control) {
      xterm.style.transform = '';
      xterm.style.width = '';
      this.body.classList.remove('letterbox');
      const dims = this.fit.proposeDimensions();
      if (!dims || !dims.cols || !dims.rows) return;
      if (dims.cols !== this.term.cols || dims.rows !== this.term.rows) this.term.resize(dims.cols, dims.rows);
      if (this.chan !== null && (dims.cols !== this.size.cols || dims.rows !== this.size.rows)) {
        this.size = { cols: dims.cols, rows: dims.rows };
        this.api.send('pane.resize', { chan: this.chan, cols: dims.cols, rows: dims.rows });
      }
    } else {
      if (this.term.cols !== this.size.cols || this.term.rows !== this.size.rows) this.term.resize(this.size.cols, this.size.rows);
      this.body.classList.add('letterbox');
      xterm.style.transform = '';
      const screen = xterm.querySelector('.xterm-screen');
      const w = screen?.offsetWidth || 1, hh = screen?.offsetHeight || 1;
      const scale = Math.min(1, this.body.clientWidth / w, this.body.clientHeight / hh);
      xterm.style.width = w + 'px';
      xterm.style.transform = `scale(${scale})`;
    }
  }

  // The look this pane is shown in. `page` is the look of the page around
  // it when the pane has one of its own (its frame then carries its colours),
  // else nothing. Only what changed is given to the terminal, and the pane is
  // fitted again only when the change moves how many cells fit.
  setLook(look, page) {
    applyPaneFrame(this.el, look, page);
    const next = termOptions(look, this.opts.compact);
    const sig = JSON.stringify(next);
    if (sig === this._lookSig) return;
    const was = this._lookSig ? JSON.parse(this._lookSig) : termOptions(BASE, this.opts.compact);
    this._lookSig = sig;
    let sizing = false;
    for (const [k, v] of Object.entries(next)) {
      if (JSON.stringify(v) === JSON.stringify(was[k])) continue;
      this.term.options[k] = v;
      sizing ||= SIZING.includes(k);
    }
    // In control: fit again and tell the station. Watching: scale again.
    // Another window in control of the same pane does the same and arrives at
    // the same size, which `resizedBy` does not take for a loss of control.
    if (sizing && this.term.element) { clearTimeout(this._rt); this._rt = setTimeout(() => this.layout(), 60); }
  }

  // A font this terminal names has just finished loading. The terminal
  // measures a cell when its font option changes and at no other time, and
  // keeps the width of every character it has drawn; so the option is
  // changed and put back, which makes it do both again. Then the sizing
  // rule, exactly as for a font change.
  remeasure() {
    if (this.disposed || !this.term.element) return;
    remeasureTerminal(this.term);
    this.layout();
  }

  // Someone else resized the pane: they have control now, we letterbox.
  // Unless they made it the size this client already holds it at (two windows
  // that fit it alike, after a font change reached both): nothing changed.
  resizedBy(controller, cols, rows) {
    const same = cols === this.size.cols && rows === this.size.rows;
    this.size = { cols, rows };
    if (controller !== this.me && this.control && !same) {
      this.control = false;
      this.term.options.disableStdin = true;
      this._lostControl = true;
    }
    this.layout();
    this.renderStrip();
  }

  takeControl() {
    if (this.mode === 'watch') return;
    this.control = true;
    this._lostControl = false;
    this.term.options.disableStdin = false;
    this.size = { cols: 0, rows: 0 };
    this.layout();
    this.renderStrip();
    this.term.focus();
  }

  focus() { if (this.control) this.term.focus(); }

  // ---- copy and paste ----
  hasSelection() { return this.term.hasSelection(); }
  // True while the program in the pane takes the mouse for itself.
  tracksMouse() { const m = this.term.modes?.mouseTrackingMode; return !!m && m !== 'none'; }
  copySelection() {
    const text = this.term.getSelection();
    if (!text) { toast('Nothing is selected in this pane', 'error'); return Promise.resolve(false); }
    return copyText(text);
  }
  // Sent as one paste, so several lines arrive together and nothing runs
  // until Enter is pressed.
  pasteText(text) {
    if (!text) return Promise.resolve(false);
    if (!this.control && !this.opts.typeWhileWatching) { toast('This pane is only being watched here. Take control to paste into it.', 'error'); return Promise.resolve(false); }
    return this.pasteNow(text).then(() => true).catch((e) => { toast(`${e.message} (pasting into ${this.pane.name})`, 'error'); return false; });
  }
  // One deliberate act: sent now or not at all, and refused by the station
  // when it arrives more than ten seconds late. Rejects with what to show.
  pasteNow(text) { return sendAct(this.api, this.station, 'pane.paste', { pane: this.pane.id, text, enter: false }); }

  update(pane) { this.pane = pane; this.renderStrip(); }

  // Shown in a layout that is not its own project's, or not on its station:
  // the strip says where the pane is from ("from web", "from web on spark2").
  setGuest(from) {
    if ((this.guest || null) === (from || null)) return;
    this.guest = from || null;
    this.renderStrip();
  }

  // In a layout a pane can be hidden: `hide` does it, and is nothing where
  // that is not offered (the pane on its own, or the last one showing).
  setHide(hide) {
    const had = !!this.onHide;
    this.onHide = hide || null;
    if (had !== !!hide) this.renderStrip();
  }

  renderStrip() {
    const p = this.pane, s = stateOf(p);
    const activity = p.exited ? (endedWhy(p) || 'ended')
      : p.tool ? `${p.tool} · ${ago(p.tool_since_ms)}`
      : `${STATE_LABEL[s] || s}${s === 'working' || s === 'needs_you' ? ' · ' + ago(p.state_since_ms) : ''}`;
    const rpc = (m, extra = {}) => this.api.on(this.station, m, { pane: p.id, ...extra }).catch(e => toast(e.message, 'error'));
    this.strip.className = 'strip ' + s;
    const agents = this.opts.agentsOf ? this.opts.agentsOf(p) : [];
    this.el.classList.toggle('agents-open', this.agentsOpen);
    if (this.agentsOpen) {
      this.drawer.replaceChildren(
        h('div', { class: 'row' }, h('b', {}, 'Agents under ' + p.name), h('span', { class: 'grow' }),
          h('button', { class: 'mini', 'aria-label': 'Slide the agents panel away', onclick: () => { this.agentsOpen = false; this.renderStrip(); } }, '›')),
        agents.length ? h('div', { class: 'agent-rows' }, agents.map(a => h(a.open ? 'button' : 'div', { class: 'agent-row', style: { marginLeft: a.depth * 16 + 'px' }, ...(a.open ? { type: 'button', onclick: a.open, title: 'Open this pane' } : {}) },
          h('span', { class: 'dot ' + a.state }), h('b', {}, a.name), h('span', { class: 'muted grow' }, a.detail), h('span', { class: 'muted' }, a.activity))))
          : h('p', { class: 'muted' }, 'No agents are working under this one. Helpers it gives tasks to, and its own sub-agents, appear here.'));
    }
    // replaceChildren would print a missing piece as the word "null".
    this.strip.replaceChildren(...[
      dot(p),
      h('span', { class: 'ico-wrap', title: iconName(p.icon) }, paneIcon(p.icon)),
      h('span', { class: 'strip-name', title: p.address }, p.name),
      p.agent !== 'shell' ? h('span', { class: 'chip' }, p.agent_name) : null,
      p.role ? h('span', { class: 'chip' }, p.role) : null,
      this.guest ? h('span', { class: 'chip guest', title: `A guest in this layout: this pane is ${this.guest}` }, this.guest) : null,
      h('span', { class: 'strip-activity' }, activity),
      p.needs_reason && s === 'needs_you' ? h('span', { class: 'strip-reason', title: p.needs_reason }, p.needs_reason) : null,
      h('span', { class: 'grow' }),
      // The model and how full its context is. The first things a narrow strip drops.
      ...usageGauge(p.usage, this.opts.onUsage ? () => this.opts.onUsage(this) : null),
      p.paused ? h('span', { class: 'chip warn' }, 'paused') : null,
      p.held && !p.exited ? h('span', { class: 'strip-reason', title: heldWords(p.held) }, heldWords(p.held)) : null,
      p.held && !p.exited ? h('button', { class: 'mini', title: RELEASE_SAYS, onclick: () => rpc('pane.release') }, 'Release') : null,
      p.task ? h('span', { class: 'chip', title: 'Working on task ' + p.task }, 'task') : null,
      this.mode === 'watch' ? h('span', { class: 'chip' }, 'watching') : null,
      this._lostControl ? h('button', { class: 'mini', title: 'Another client set this pane\'s size', onclick: () => this.takeControl() }, 'Take control') : null,
      agents.length || this.agentsOpen ? h('button', { class: 'mini' + (this.agentsOpen ? ' on' : ''), 'aria-pressed': String(this.agentsOpen), title: 'The agents working under this one',
        onclick: () => { this.agentsOpen = !this.agentsOpen; this.renderStrip(); } }, this.agentsOpen ? 'Agents ›' : `‹ Agents ${agents.length}`) : null,
      // The pane's other face, its conversation (chat.js), where it has one.
      this.opts.face ? this.opts.face(this) : null,
      p.agent !== 'shell' && !p.exited && (s === 'working' || s === 'needs_you')
        ? h('button', { class: 'mini danger', title: 'Stop: interrupt this turn. The process stays.', onclick: () => rpc('pane.stop') }, 'Stop') : null,
      this.onHide ? h('button', { class: 'icon-btn strip-hide', title: 'Hide this pane. It keeps running, and stays as a chip beside the layout\'s name.', 'aria-label': `Hide ${p.name}`, onclick: () => this.onHide && this.onHide() }, hideIcon()) : null,
      h('button', { class: 'icon-btn', title: 'Pane menu', 'aria-label': 'Pane menu', onclick: (e) => this._menu(e) }, '⋮'),
    ].filter(Boolean));
  }

  _menu(e) {
    e.preventDefault();
    e.stopPropagation();
    if (this.opts.onMenu) this.opts.onMenu(e, this);
  }

  _drop(e) {
    this.el.classList.remove('dropping');
    const files = [...(e.dataTransfer?.files || [])];
    if (!files.length || !this.control) return;
    e.preventDefault();
    files.forEach(f => this._upload(f));
  }

  // Image and file drop: the station writes the file and types its path.
  async _upload(file) {
    if (file.size > 64 * 1024 * 1024) return toast(`${file.name} is larger than 64 MB`, 'error');
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      let bin = '';
      for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
      const res = await this.api.on(this.station, 'pane.drop_file', { pane: this.pane.id, name: file.name || 'pasted.png', data_b64: btoa(bin) });
      toast(`Saved on ${this.station}: ${res.path}`);
    } catch (err) { toast(`Could not send ${file.name}: ${err.message}`, 'error'); }
  }

  dispose() {
    this.disposed = true;
    clearInterval(this._tick);
    clearTimeout(this._retry);
    clearTimeout(this._holdTimer);
    for (const x of (this.says || new Map()).values()) clearTimeout(x.timer);
    clearTimeout(this._lateTimer);
    clearTimeout(this._lostTimer);
    this._offStatus();
    this._offEvents();
    this.ro.disconnect();
    if (this.chan !== null) this.api.detach(this.chan);
    this.term.dispose();
    this.el.remove();
  }
}

// Why a pane that has no program is still listed, in words, or null for a
// pane that simply ended. Times are shown in the browser's own time zone.
export function endedWhy(p) {
  if (!p || !p.exited) return null;
  if (p.held_back) return 'not started again: spandeck restarted 3 times in 10 minutes';
  if (p.ended_by !== 'restart') return null;
  const at = Number(p.ended_ms) > 0 ? ' at ' + new Date(Number(p.ended_ms)).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '';
  return `ended when spandeck restarted${at}`;
}

function describeExit(info) {
  if (!info) return 'ended';
  if (info.Code !== undefined) return info.Code === 0 ? 'exit 0' : `exit code ${info.Code}`;
  if (info.Signal !== undefined) return `signal ${info.Signal}`;
  return 'status unknown';
}

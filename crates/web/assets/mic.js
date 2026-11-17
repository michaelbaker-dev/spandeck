// The microphone of a message box (DECISIONS.md, "The microphone", 17 Nov
// 2026). The owner, from an iPhone: "I tapped it on a spark2 session and it
// locked up spandeck I had to close the web page and restart."
//
// What is said becomes text in the box, and nothing more: it is never sent
// from here. Two ways to the text:
//
//   'speech'  the browser's own dictation (the Web Speech API), where the
//             page is a secure one and the browser has it;
//   'record'  a short recording, turned into text by a program of the
//             owner's on a station that says it can (`caps`: `speech`).
//
// Nothing here trusts the browser to answer. This file's own state decides
// what the button is, never an event that may not come:
//
//   - a start that throws, or that is answered by nothing within 1.5 s, is
//     given up and said in words (30 s while the browser may be asking the
//     owner for permission, with Stop on the button meanwhile);
//   - the button stops whatever is going on, at once, in every state;
//   - a session ends by itself after two minutes, when the page is hidden
//     or loses its window, when the finger that held the button is lifted
//     or lost, and when the owner types in the box;
//   - what is heard is written at most eight times a second, however fast
//     the browser reports it;
//   - nothing is ever disabled, covered or focused from here.
//
// Each attempt is told to the station (`client.report`, `what: "mic"`),
// with what was tried, which events came and when. One that the page did
// not survive is told by the next page that opens. Read with:
//   spandeck rpc log.query '{"kind":"client.report"}'

import { h } from './ui.js';

// In milliseconds. A test may shorten them (`window.spandeckMicTimes`).
export const TIMES = {
  hold: 350,       // held this long, the button talks until it is let go
  start: 1500,     // a start is answered within this, or given up
  ask: 30000,      // … or within this while the browser may be asking
  cap: 120000,     // a session of dictation ends by itself
  stop: 1500,      // after Stop, the last words are waited for this long
  flush: 120,      // what is heard is written this often at most
  said: 12000,     // a line of explanation stays this long
  record: 60000,   // a recording ends by itself
  answer: 75000,   // a station is given this long to turn it into text
  stall: 4000,     // the page's own clock missed this much while in sight
};
const times = () => ({ ...TIMES, ...(window.spandeckMicTimes || {}) });

const CRUMB = 'spandeck.mic.crumb';
const KEYBOARD = 'Use the microphone key on the keyboard.';
const WORDS = {
  insecure: `The microphone is not available on this address: a browser gives it only to a secure (HTTPS) page. ${KEYBOARD}`,
  absent: `This browser has no dictation. ${KEYBOARD}`,
  'not-allowed': 'Dictation was not allowed. On an iPhone: Settings → Apps → Safari → Microphone, or "Website Settings" in the address bar\'s menu.',
  'service-not-allowed': `The phone's speech service refused. It needs Siri & Dictation turned on (Settings → General → Keyboard → Enable Dictation), and Safari does not offer it to a deck opened from the home screen. ${KEYBOARD}`,
  'audio-capture': 'No microphone was found.',
  network: `The browser could not reach its speech service. ${KEYBOARD}`,
  'language-not-supported': `The browser's dictation does not know this language. ${KEYBOARD}`,
  'no-speech': 'Nothing was heard.',
};

let ctx = null;
const live = new Set();   // the buttons with something going on
const now = () => performance.now();

export const standalone = () => !!(navigator.standalone || window.matchMedia?.('(display-mode: standalone)').matches);

// "ios-safari 26.0", "chrome 141": enough to tell the browsers apart.
export function uaFamily(ua = navigator.userAgent || '') {
  const v = (re) => (ua.match(re) || [])[1] || '';
  const ios = /iPhone|iPad|iPod/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1);
  if (ios) return /CriOS|FxiOS|EdgiOS/.test(ua) ? `ios-other ${v(/OS (\d+(?:_\d+)?)/)}` : `ios-safari ${v(/Version\/([\d.]+)/)}`;
  if (/Edg\//.test(ua)) return `edge ${v(/Edg\/(\d+)/)}`;
  if (/Chrome\//.test(ua)) return `chrome ${v(/Chrome\/(\d+)/)}`;
  if (/Firefox\//.test(ua)) return `firefox ${v(/Firefox\/(\d+)/)}`;
  if (/Safari\//.test(ua)) return `safari ${v(/Version\/([\d.]+)/)}`;
  return 'other';
}

const where = () => ({ secure: window.isSecureContext === true, standalone: standalone(), shell: typeof window.spandeckShell === 'object' && !!window.spandeckShell, ua: uaFamily(), lang: navigator.language || '' });

function tell(report) {
  try { ctx?.api?.call('client.report', { build: ctx.api.build, ...report, ...where() }).catch(() => {}); } catch (e) { /* told next time */ }
}

// The station that turns a recording into text: the pane's own, else the
// one this page works through, else any that says it can.
function recordStation(st) {
  if (!ctx?.hasCap) return null;
  const all = [st, ctx.api?.station, ...(ctx.stationsWith ? ctx.stationsWith('speech') : [])];
  return all.find(s => s && ctx.hasCap(s, 'speech')) || null;
}

// What a tap would do here, without doing it.
export function plan(st) {
  const secure = window.isSecureContext === true;
  if (!secure) return { engine: null, why: 'insecure' };
  const station = typeof window.MediaRecorder === 'function' && navigator.mediaDevices?.getUserMedia ? recordStation(st) : null;
  if (station) return { engine: 'record', station };
  const Speech = window.SpeechRecognition || window.webkitSpeechRecognition;
  if (typeof Speech !== 'function') return { engine: null, why: 'absent' };
  return { engine: 'speech', Speech };
}

const crumb = (s) => {
  try {
    if (!s) return localStorage.removeItem(CRUMB);
    localStorage.setItem(CRUMB, JSON.stringify({ t: s.wall, alive: Date.now(), engine: s.engine, how: s.how, state: s.state, events: s.events, results: s.n }));
  } catch (e) { /* not kept: the report of a page that lived says it */ }
};

// One microphone button for one message box. `box`: the textarea.
// `changed`: called after the box was written. `st`: the pane's station.
export function micFor({ box, changed = () => {}, st = null, chat = false }) {
  const IDLE = 'Dictate: tap to start and stop, or hold to talk';
  const btn = h('button', { type: 'button', class: 'mic' + (chat ? ' chat-mic' : ''), title: IDLE, 'aria-label': 'Dictate', 'aria-pressed': 'false' }, '🎤');
  // What is going on, and why it could not: a line by the box.
  const said = h('p', { class: 'send-said mic-said', role: 'status', hidden: true });
  let S = null, held = false, holdTimer = null, saidTimer = null, skipClick = -1e9;

  const say = (words, action) => {
    clearTimeout(saidTimer);
    said.replaceChildren(...(words ? [words] : []), ...(action ? [' ', h('button', { type: 'button', class: 'mini', onclick: () => { say(''); action.run(); } }, action.label)] : []));
    said.hidden = !words;
    said.classList.toggle('live', !!S && !S.closing);
    if (words && !(S && !S.closing)) saidTimer = setTimeout(() => { said.hidden = true; }, times().said * (action ? 2 : 1));
  };
  const face = (state) => {
    const on = !!state;
    btn.classList.toggle('on', on);
    btn.setAttribute('aria-pressed', String(on));
    btn.setAttribute('aria-label', on ? 'Stop dictating' : 'Dictate');
    btn.title = on ? 'Stop' : IDLE;
    if (on) btn.dataset.mic = state; else delete btn.dataset.mic;
    if (!chat) btn.textContent = on ? '■' : '🎤';
  };
  const STATE_WORDS = {
    starting: () => 'Starting the microphone… Tap it again to stop.',
    asking: () => 'Waiting for the browser to give the microphone: answer its question, or tap the microphone to stop.',
    listening: () => 'Listening. Tap the microphone to stop. Nothing is sent until you send it.',
    recording: (s) => `Recording, for ${s.station} to turn into text. Tap the microphone when you are done. Nothing is sent until you send it.`,
    transcribing: (s) => `Transcribing on ${s.station}… Tap the microphone to cancel.`,
  };
  const mark = (s, name) => { if (s.events.length < 40) s.events.push([name, Math.round(now() - s.t0)]); crumb(s); };
  const to = (s, state) => { s.state = state; mark(s, state); face(state); say(STATE_WORDS[state](s)); };

  // The end of a session, whatever it was doing: this file's state first,
  // then the browser is told, and what it does about it is not waited for.
  const close = (why, words, action) => {
    const s = S;
    if (!s) return;
    if (s.engine === 'speech' && why !== 'typed') flush(s);
    S = null;
    live.delete(ctl);
    for (const t of [s.wd, s.capT, s.stopT, s.flushT]) clearTimeout(t);
    if (s.r) {
      for (const k of ['onstart', 'onaudiostart', 'onspeechstart', 'onresult', 'onerror', 'onend', 'onnomatch']) { try { s.r[k] = null; } catch (e) { /* its own */ } }
      try { s.r.abort(); } catch (e) { /* already ended */ }
    }
    if (s.rec) { try { s.rec.ondataavailable = s.rec.onstop = s.rec.onerror = null; if (s.rec.state !== 'inactive') s.rec.stop(); } catch (e) { /* already ended */ } }
    if (s.stream) { try { s.stream.getTracks().forEach(t => t.stop()); } catch (e) { /* gone */ } }
    face(null);
    say(words || '', action);
    crumb(null);
    tell({ what: 'mic', engine: s.engine, how: s.how, ended: why, state: s.state, error: s.error || undefined, perm: s.perm, station: s.station, ms: Math.round(now() - s.t0), events: s.events, results: s.n, first_result_ms: s.first, chars: s.chars, bytes: s.bytes });
  };

  // ---- the browser's own dictation

  // What was heard so far is written to the box: here and nowhere else,
  // and not more often than `flush`.
  function flush(s) {
    clearTimeout(s.flushT);
    s.flushT = null;
    if (!s.results) return;
    // The owner typed meanwhile: what is in the box is theirs.
    if (box.value !== s.wrote) return;
    let text = '';
    try {
      const n = Math.min(s.results.length, 400);
      for (let i = 0; i < n && text.length < 20000; i++) text += s.results[i]?.[0]?.transcript || '';
    } catch (e) { return; }
    const next = s.before + text;
    s.chars = text.length;
    if (next === s.wrote) return;
    box.value = s.wrote = next;
    changed();
  }

  function speech(s, Speech) {
    s.before = box.value ? box.value.replace(/\s*$/, ' ') : '';
    s.wrote = box.value;
    const mine = (f) => (e) => { if (S === s) f(e); };
    const heard = () => { if (s.state === 'starting' || s.state === 'asking') { clearTimeout(s.wd); to(s, 'listening'); s.capT = setTimeout(() => S === s && soft('cap', 'Dictation stops by itself after two minutes. Tap the microphone to carry on.'), times().cap); } };
    try {
      const r = s.r = new Speech();
      r.lang = navigator.language || 'en-US';
      r.interimResults = true;
      r.continuous = true;
      r.onstart = mine(() => { mark(s, 'onstart'); heard(); });
      r.onaudiostart = mine(() => { mark(s, 'onaudiostart'); heard(); });
      r.onspeechstart = mine(() => mark(s, 'onspeechstart'));
      r.onnomatch = mine(() => mark(s, 'onnomatch'));
      // As little as can be done per result: a browser may report hundreds.
      r.onresult = mine((e) => {
        s.n += 1;
        s.results = e.results;
        if (s.n === 1) { s.first = Math.round(now() - s.t0); mark(s, 'onresult'); heard(); }
        if (!s.flushT) s.flushT = setTimeout(() => { if (S !== s) return; if (box.value !== s.wrote) return close('typed', 'Dictation stopped when you typed.'); flush(s); }, times().flush);
      });
      r.onerror = mine((e) => {
        const err = String(e?.error || 'error').slice(0, 40);
        mark(s, 'onerror:' + err);
        if (err === 'aborted') return close(s.why || 'aborted');
        s.error = err;
        close('error', WORDS[err] || `Dictation stopped: ${err}.`);
      });
      r.onend = mine(() => { mark(s, 'onend'); close(s.why || 'ended', s.words); });
      r.start();
      mark(s, 'start()');
    } catch (e) {
      s.error = String(e?.name || e).slice(0, 40);
      mark(s, 'threw:' + s.error);
      return close('threw', s.error === 'NotAllowedError' ? WORDS['not-allowed'] : `Dictation could not start here (${s.error}). ${KEYBOARD}`);
    }
  }

  // Stop, and the last words: the box is the owner's again at once, and
  // what the browser still has to say is taken for a moment.
  function soft(why, words) {
    const s = S;
    if (!s || s.closing) return;
    if (s.engine !== 'speech' || s.state !== 'listening') return close(why, words);
    s.closing = true; s.why = why; s.words = words;
    mark(s, 'stop()');
    clearTimeout(s.capT);
    face(null);
    say(words || '');
    try { s.r.stop(); } catch (e) { return close(why, words); }
    s.stopT = setTimeout(() => S === s && close(why, words), times().stop);
  }

  // ---- a recording, for a station to turn into text

  async function record(s) {
    let stream;
    try { stream = await navigator.mediaDevices.getUserMedia({ audio: true }); } catch (e) {
      if (S !== s) return;
      s.error = String(e?.name || e).slice(0, 40);
      mark(s, 'refused:' + s.error);
      return close('error', s.error === 'NotAllowedError' ? WORDS['not-allowed'] : s.error === 'NotFoundError' ? WORDS['audio-capture'] : `The microphone could not be opened (${s.error}).`);
    }
    // Given up on while the browser was asking: the microphone is let go.
    if (S !== s) { try { stream.getTracks().forEach(t => t.stop()); } catch (e) { /* gone */ } return; }
    clearTimeout(s.wd);
    s.stream = stream;
    try {
      const kind = ['audio/mp4', 'audio/webm;codecs=opus', 'audio/webm'].find(m => window.MediaRecorder.isTypeSupported?.(m));
      const rec = s.rec = new window.MediaRecorder(stream, kind ? { mimeType: kind, audioBitsPerSecond: 64000 } : {});
      s.kind = kind || '';
      s.chunks = [];
      rec.ondataavailable = (e) => { if (S === s && e.data && e.data.size) s.chunks.push(e.data); };
      rec.onerror = (e) => { if (S !== s) return; s.error = String(e?.error?.name || 'error').slice(0, 40); close('error', `The recording stopped (${s.error}).`); };
      rec.onstop = () => { if (S === s && s.state === 'transcribing') { mark(s, 'onstop'); clearTimeout(s.stopT); upload(s); } };
      rec.start();
    } catch (e) {
      s.error = String(e?.name || e).slice(0, 40);
      return close('threw', `This browser could not record (${s.error}). ${KEYBOARD}`);
    }
    s.recAt = now();
    to(s, 'recording');
    s.capT = setTimeout(() => S === s && finish('cap'), times().record);
  }

  // Done talking: the recording goes to the station.
  function finish(why) {
    const s = S;
    if (!s || s.state !== 'recording') return;
    clearTimeout(s.capT);
    s.secs = (now() - s.recAt) / 1000;
    s.why = why;
    to(s, 'transcribing');
    try { s.rec.stop(); } catch (e) { /* what there is, is sent */ }
    // A recorder that never says it stopped: what it gave is sent.
    s.stopT = setTimeout(() => S === s && upload(s), times().stop);
  }

  async function upload(s) {
    if (s.sent) return;
    s.sent = true;
    try { s.stream.getTracks().forEach(t => t.stop()); } catch (e) { /* gone */ }
    const blob = new Blob(s.chunks, { type: s.rec.mimeType || s.kind || 'audio/mp4' });
    s.bytes = blob.size;
    if (!blob.size || s.secs < 0.3) return close('empty', 'Nothing was recorded: tap, speak, and tap again.');
    try {
      const data = await new Promise((ok, no) => { const f = new FileReader(); f.onload = () => ok(String(f.result).split(',')[1] || ''); f.onerror = () => no(new Error('the recording could not be read')); f.readAsDataURL(blob); });
      if (S !== s) return;
      mark(s, 'sent');
      let timer;
      const res = await Promise.race([
        ctx.api.on(s.station, 'speech.transcribe', { data_b64: data, mime: blob.type, seconds: Math.round(s.secs * 10) / 10 }),
        new Promise((_, no) => { timer = setTimeout(() => no(new Error(`no answer from ${s.station} within ${Math.round(times().answer / 1000)} s`)), times().answer); }),
      ]).finally(() => clearTimeout(timer));
      // Cancelled meanwhile: what comes back is dropped.
      if (S !== s) return;
      const text = String(res?.text || '').trim();
      s.chars = text.length;
      if (text) {
        // After what is in the box now: the owner may have typed meanwhile.
        box.value = (box.value ? box.value.replace(/\s*$/, ' ') : '') + text;
        changed();
      }
      close(s.why === 'cap' ? 'cap' : 'done', text ? (s.why === 'cap' ? 'A recording stops by itself after a minute. Read it over before you send it.' : '') : WORDS['no-speech']);
    } catch (e) {
      if (S !== s) return;
      s.error = 'transcribe';
      close('error', `Not turned into text: ${e.message}`);
    }
  }

  // ---- the button

  function start(how) {
    if (S) close('again');
    const p = plan(st && typeof st === 'function' ? st() : st);
    if (!p.engine) {
      const deck = p.why === 'insecure' && ctx?.secureDeck ? ctx.secureDeck() : null;
      say(WORDS[p.why], deck ? { label: 'Open the secure deck', run: () => ctx.hopTo(deck.origin, deck.what) } : null);
      tell({ what: 'mic', engine: 'none', how, ended: p.why, offered: !!deck });
      return;
    }
    const s = S = { engine: p.engine, station: p.station, how, t0: now(), wall: Date.now(), events: [], n: 0, state: 'starting' };
    live.add(ctl);
    to(s, 'starting');
    const t = times();
    // No answer: given up, unless the browser may be asking the owner.
    s.wd = setTimeout(() => {
      if (S !== s || s.state !== 'starting') return;
      if (s.perm === 'denied') return close('watchdog', WORDS['not-allowed']);
      if (s.perm !== 'prompt' && !s.blurred) return close('watchdog', `The microphone did not start: the browser gave no answer within ${t.start / 1000} s. ${KEYBOARD}`);
      to(s, 'asking');
      s.wd = setTimeout(() => S === s && s.state === 'asking' && close('unanswered', `The microphone was not given within ${Math.round(t.ask / 1000)} s. ${KEYBOARD}`), Math.max(0, t.ask - t.start));
    }, t.start);
    try { navigator.permissions?.query({ name: 'microphone' }).then((q) => { if (S === s) { s.perm = q.state; mark(s, 'perm:' + q.state); } }).catch(() => {}); } catch (e) { /* not known */ }
    if (p.engine === 'speech') speech(s, p.Speech); else record(s);
  }

  // The button, while something is going on: it stops it.
  function tapStop() {
    const s = S;
    if (!s) return;
    if (s.state === 'listening') return soft('tap');
    if (s.state === 'recording') return finish('tap');
    close(s.state === 'transcribing' ? 'cancelled' : 'tap', s.state === 'transcribing' ? 'Cancelled: nothing was added.' : '');
  }
  // The finger that held it was lifted, or lost.
  function release(why) {
    const s = S;
    if (!s || s.closing) return;
    if (s.state === 'listening') return soft(why);
    if (s.state === 'recording') return finish(why);
    if (s.state !== 'transcribing') close(why, 'Let go before the microphone started. Hold it until it listens, or tap it once.');
  }
  const up = (why) => { clearTimeout(holdTimer); if (held) { held = false; skipClick = now(); release(why); } };
  btn.addEventListener('pointerdown', () => {
    clearTimeout(holdTimer);
    held = false;
    // Something is going on: this touch is the tap that stops it.
    if (S && !S.closing) return;
    holdTimer = setTimeout(() => { held = true; start('hold'); }, times().hold);
  });
  btn.addEventListener('pointerup', () => up('release'));
  btn.addEventListener('pointercancel', () => up('pointercancel'));
  btn.addEventListener('pointerleave', () => up('pointerleave'));
  btn.addEventListener('contextmenu', (e) => e.preventDefault());
  // The tap, and the keyboard's way to the same thing.
  btn.addEventListener('click', () => { if (now() - skipClick < 500) return; S && !S.closing ? tapStop() : start('tap'); });

  const ctl = {
    button: btn,
    said,
    can: () => !!plan(st && typeof st === 'function' ? st() : st).engine,
    // Away from this box: whatever was going on is over, and nothing more
    // is written.
    stop: (why = 'left') => { clearTimeout(holdTimer); held = false; close(why); },
    // The page is no longer in front (`hidden`), or its window is not.
    away: (why) => {
      const s = S;
      if (!s) return;
      if (why === 'blur' && (s.state === 'starting' || s.state === 'asking')) { if (!s.blurred) { s.blurred = true; mark(s, 'blur'); } return; }
      if (why === 'blur' && s.state === 'transcribing') return;
      held = false;
      if (why === 'blur') return s.state === 'recording' ? finish('blur') : soft('blur', 'Dictation stopped when the page was left.');
      close(why, 'Dictation stopped when the page was left.');
    },
    lifted: () => up('lifted'),
    beat: () => { if (S && !btn.isConnected) return close('gone'); if (S) crumb(S); },
    state: () => (S ? S.state : null),
  };
  return ctl;
}

// Once, for the page: what ends every session, the page's own clock, and
// what a page that did not live left behind.
export function initMic(c) {
  ctx = c;
  let sawHidden = document.visibilityState !== 'visible', last = now(), lastWall = Date.now(), stalls = 0, input = null;
  document.addEventListener('visibilitychange', () => { sawHidden = true; if (document.visibilityState !== 'visible') for (const m of [...live]) m.away('hidden'); });
  addEventListener('pagehide', () => { for (const m of [...live]) m.away('pagehide'); });
  addEventListener('blur', () => { for (const m of [...live]) m.away('blur'); });
  // A finger lifted anywhere ends a hold, when the button never heard of it.
  for (const ev of ['pointerup', 'pointercancel', 'touchend', 'touchcancel']) addEventListener(ev, () => { for (const m of [...live]) m.lifted(); }, { capture: true, passive: true });
  for (const ev of ['pointerdown', 'keydown']) addEventListener(ev, (e) => { const t = e.target; input = [ev, now(), t && t.tagName ? `${t.tagName.toLowerCase()}.${String(t.className || '').split(' ')[0]}` : '']; }, { capture: true, passive: true });
  // The page's own clock. A second that took more than four, with the page
  // in sight all the while, is a page that stopped answering: said when it
  // answers again, with what it was doing.
  setInterval(() => {
    const t = now(), wall = Date.now(), gap = t - last, limit = times().stall;
    const doing = [...live].map(m => m.state());
    for (const m of [...live]) m.beat();
    if (gap > limit && gap < 600000 && !sawHidden && document.visibilityState === 'visible' && stalls < 5) {
      stalls += 1;
      const a = document.activeElement;
      tell({ what: 'page-stall', gap_ms: Math.round(gap), wall_gap_ms: wall - lastWall, mic: doing, route: location.hash.slice(0, 80),
        focus: a && a.tagName ? `${a.tagName.toLowerCase()}.${String(a.className || '').split(' ')[0]}` : '', last_input: input ? [input[0], Math.round(last - input[1]), input[2]] : null });
    }
    last = t; lastWall = wall; sawHidden = document.visibilityState !== 'visible';
  }, 1000);
  // A session that never ended: its page was closed, or stopped for good.
  // (Another window that is still at it keeps its note fresh.)
  const left = (again) => {
    let was = null;
    try { was = JSON.parse(localStorage.getItem(CRUMB) || 'null'); } catch (e) { /* nothing left */ }
    if (!was || typeof was !== 'object') return;
    if (Date.now() - (was.alive || 0) < 5000) { if (again) setTimeout(() => left(false), 6000); return; }
    try { localStorage.removeItem(CRUMB); } catch (e) { /* told twice, then */ }
    tell({ what: 'mic', died: true, engine: was.engine, how: was.how, state: was.state, events: Array.isArray(was.events) ? was.events.slice(0, 40) : [], results: was.results, alive_ms: (was.alive || 0) - (was.t || 0), ago_s: Math.round((Date.now() - (was.alive || 0)) / 1000) });
  };
  setTimeout(() => left(true), 3000);
}

// What is typed into a pane, on the page's side (DECISIONS.md, "What is
// typed"): the rules as plain functions, so that each can be checked by
// itself (crates/cli/tests/typed.rs runs them in a real browser), and the
// one way a message box sends.
//
// The audit: keys typed during a gap were dropped without a word (about
// 0.8 s at every upgrade or reconnect) or delivered much later in one burst
// (232 keys up to 60 s late). The promise now: what is typed reaches its
// pane within two seconds plus the path's round trip, or the owner is told
// how many keys did not; never another pane, never later, never twice.

// Keys typed while this terminal cannot send (not connected, not answering,
// or its pane not attached yet) are held this long at most, and this many
// bytes: a reconnect at an upgrade is over in less, and what was held goes
// out in order after the terminal's own attach is answered. Under the two
// seconds a key may take, so that a held key still arrives on time.
export const HOLD_MS = 1500;
export const HOLD_BYTES = 256;
// One thing typed that is larger than this is a paste: never held.
export const PASTE_BYTES = 64;
// A key was sent and nothing came back for this long: the page asks at once
// whether the link answers (api.js, `typed`).
export const PROBE_AFTER = 300;
// One deliberate act (a message from the message box, a paste from the
// menu): the limit of a click.
export const ACT_MS = 10000;

const enc = new TextEncoder();
export const bytesOf = (s) => enc.encode(s).length;

// Is this something a person typed, or only what a terminal says by itself:
// that the window gained or lost focus, the mouse, its answers to the
// program's questions (where the cursor is, what kind of terminal this is)?
// The same rule as the station's (crates/core/src/holds.rs, `is_typing`):
// none of those is counted as a key or held, and none is ever "not sent".
export function isTyping(data) {
  let rest = data;
  while (rest.length) {
    const n = reportLen(rest);
    if (!n) return true;
    rest = rest.slice(n);
  }
  return false;
}
function reportLen(b) {
  if (b.length < 3 || b[0] !== '\x1b') return 0;
  if (b[1] === '[') {
    // The old mouse report: three characters follow.
    if (b[2] === 'M') return b.length >= 6 ? 6 : 0;
    let end = -1;
    for (let i = 2; i < b.length; i++) { const c = b.charCodeAt(i); if (c >= 0x40 && c <= 0x7e) { end = i; break; } }
    if (end < 0) return 0;
    const params = b.slice(2, end), plain = /^[0-9;]*$/.test(params);
    const f = b[end];
    const report = f === 'I' || f === 'O' ? params === ''
      : f === 'R' || f === 'n' ? params !== '' && (plain || params[0] === '?')
      : f === 'c' ? params[0] === '?' || params[0] === '>'
      : f === 't' ? params !== '' && plain
      : f === 'y' ? params[0] === '?'
      : f === 'M' || f === 'm' ? params[0] === '<'
      : false;
    return report ? end + 1 : 0;
  }
  // An answer that is a string (the terminal's colours, its name): to its end.
  if (b[1] === ']' || b[1] === 'P') {
    const bel = b.indexOf('\x07'), st = b.indexOf('\x1b\\', 2);
    if (bel < 0 && st < 0) return 0;
    return bel >= 0 && (st < 0 || bel < st) ? bel + 1 : st + 2;
  }
  return 0;
}

// A paste: marked as one by the terminal, or simply large.
export const isPaste = (data) => data.startsWith('\x1b[200~') || bytesOf(data) > PASTE_BYTES;

// A terminal's hold: what was typed and could not be sent yet, oldest first.
// `item`: { data, t (the page's clock), wall (the clock on the wall) }.
// Returns what becomes of the new item:
//   'held'   kept, for at most HOLD_MS
//   'paste'  a paste is never held: it, and what was held, is refused
//   'full'   more than HOLD_BYTES would be held: all of it is refused
export function holdTake(hold, item) {
  if (isPaste(item.data)) return 'paste';
  const bytes = hold.reduce((n, x) => n + bytesOf(x.data), 0) + bytesOf(item.data);
  if (bytes > HOLD_BYTES) return 'full';
  hold.push(item);
  return 'held';
}
// Is a held item too old to send? By the page's clock and by the clock on
// the wall, whichever says older: a page that was not running (a laptop that
// slept, a phone that was locked) may find its own clock stood still.
export const heldTooLong = (item, now, wall) => Math.max(now - item.t, wall - item.wall) > HOLD_MS;

// "1 key", "7 keys", "7 keys and a paste", "a paste".
export function keysWords(keys, pastes = 0) {
  const k = keys ? `${keys} key${keys === 1 ? '' : 's'}` : '';
  const p = pastes ? (pastes === 1 ? 'a paste' : `${pastes} pastes`) : '';
  return k && p ? `${k} and ${p}` : k || p || 'nothing';
}
export const wasWere = (keys, pastes = 0) => (keys + pastes === 1 ? 'was' : 'were');
// "5.2 s", from milliseconds.
export const secsWords = (ms) => `${(Math.max(0, ms) / 1000).toFixed(1)} s`;

// Why what was typed cannot be sent now, in words that begin a sentence.
// `why`: 'closed' (the page has no connection), 'silent' (it has one that
// does not answer), 'clock' (the station has not answered this page's first
// ping yet), 'pane' (this pane is not attached: its station is being asked).
export function offWords(why, serving, paneStation) {
  if (why === 'closed') return `Not connected to ${serving}`;
  if (why === 'silent') return `${serving} is not answering`;
  if (why === 'clock') return `${serving} has not answered this page yet`;
  return paneStation && paneStation !== serving ? `${paneStation} is not showing this pane yet` : 'This pane is not attached yet';
}

// What the station said of something typed that it did not take.
export function droppedWords(why, n, pastes, pane, d) {
  const what = `${keysWords(n, pastes)} typed into ${pane} ${wasWere(n, pastes)} not sent`;
  if (why === 'late') return `${what}: ${n + pastes === 1 ? 'it' : 'they'} reached ${d.station} ${secsWords(d.ageMs)} after you typed ${n + pastes === 1 ? 'it' : 'them'}, and nothing typed is delivered more than ${secsWords(d.limitMs)} late.`;
  if (why === 'silent') return `${what}: ${d.station} had not answered for ${secsWords(d.quietMs)}.`;
  if (why === 'not_ready') return `${what}: ${d.station} had not yet read this page's clock.`;
  return `${what}.`;
}

// One deliberate act: a message from the message box, a paste from the
// menu. It is stamped, so the station refuses it when it arrives more than
// ten seconds after it was sent; the page waits that long and the path's
// round trip. Resolves when it was taken. Rejects with an error whose
// `message` is what to show, and `unknown` when the page cannot tell
// whether it arrived (then it must not be said to be "not sent", and is
// not sent again by itself).
export async function sendAct(api, station, method, params) {
  const to = station || api.station;
  if (!api.live) throw Object.assign(new Error(`Not sent: ${api.connected ? `${api.station} is not answering` : `not connected to ${api.station}`}.`), { unknown: false });
  const stamped = api.stamps ? { ...params, t: performance.now() } : params;
  const within = ACT_MS + 2 * Math.round(api.rtt() || 0) + 1000;
  try {
    return await api.on(to, method, stamped, { within });
  } catch (e) {
    const said = String(e.message || 'it was refused');
    if (['no_answer', 'connection_lost', 'outcome_unknown'].includes(e.code) || /could not confirm/.test(said)) {
      const why = e.code === 'no_answer' ? `No answer from ${to} within ${Math.round(within / 1000)} s`
        : e.code === 'connection_lost' ? `The connection to ${api.station} was lost before it answered`
        : `${to} could not confirm that it arrived`;
      throw Object.assign(new Error(`${why}: this may or may not have been sent. Look at the pane before sending it again.`), { unknown: true });
    }
    throw Object.assign(new Error(/^not sent/i.test(said) ? said[0].toUpperCase() + said.slice(1) + '.' : `Not sent: ${said}.`), { unknown: false });
  }
}

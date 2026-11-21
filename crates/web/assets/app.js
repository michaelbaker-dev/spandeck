// The Control Deck. The list on the left is the same on every station; what
// is selected in it is shown on the right, and each selection has a URL:
//   #/fleet                      every station
//   #/s/<station>                one station: every running pane as a tile
//   #/s/<station>/p/<pane>       one pane, full size
//   #/s/<station>/l/<project>/<layout>   a project's layout
//   #/s/<station>/w/<project>    Watch: a project's agents and who is talking
//   #/l/<layout>                 a global layout (kept by the whole fleet)
// A layout's leaf is a pane, or a view that runs nothing: Files or Watch
// (viewpanes.js).
// On a phone the same URLs show a small view: what needs you, the sessions,
// one pane to talk to, and a layout stepped through a pane at a time.
// A pane has two faces, its terminal and its conversation (chat.js): on a
// phone an agent's pane opens as its conversation.

import { Api } from './api.js';
import { PaneTerm } from './term.js';
import { h, showMenu, closeMenu, toast, dialog, confirmBox, keepInView, field, ago, dot, stateOf, stateRank, STATE_LABEL, copyText, fmtTokens, fmtCost, exact, usageBrief, usageTitle, contextText, heldWords, rapidLine, hooksTick, hooksCard, hooksAnsweredWords, trustTick, trustCard, trustAnsweredWords, TRUST_WARNING, kindTag, KINDS } from './ui.js';
import { paneIcon, iconName, layoutIcon, watchIcon, stationIcon, pinIcon } from './icons.js';
import { lookState, setDoc, pageLook, paneLook, applyPage, applyChatFrame, applyRemembered, rememberPage, loadFonts, fontListeners, termSize } from './look.js';
import { lookEditor } from './lookedit.js';
import { phoneLook } from './phonelook.js';
import { toolsPanel } from './tools.js';
import { rolesPanel, rolePick } from './roles.js';
import { initFiles, openFiles, filesSync, filesBusy } from './files.js';
import { initViews, isView, viewKind, cleanView, viewLabel, viewName, viewState, viewCell, viewIcon, viewsBeforeRender, viewsAfterRender, addPaneDialog, dotKeyDialog } from './viewpanes.js';
import { perfPage, gather, anyTrouble, keyInfo, keySteps, signInWords, lanOf, TS_MACHINES, clockWords, refreshTook, refreshAsked, refreshCorrected } from './perf.js';
import { qrSvg } from './qr.js';
import { initFailover } from './failover.js';
import { sendAct } from './typed.js';
import { initMic, micFor } from './mic.js';
import { initChips } from './chips.js';
import { initChat, chatMenu, chatLooks, draftCount, conversation, faceOf, setFace, chatToggle, termBox, chatBeforeRender, chatAfterRender, chatPark, chatResized, sync as syncKids, homeMode, homeSwitch, sessionRow, recentRows, newSession } from './chat.js';

// The page opens in the look this browser last had, before anything is asked.
applyRemembered();

const api = new Api();
const root = document.getElementById('app');
// The element leads back to the connection (tests read it; the deck does not).
root.api = api;
const isPhone = () => matchMedia('(max-width: 760px)').matches;

const S = {
  route: { view: 'fleet' },
  fleet: { stations: [], stops: [] },
  panes: new Map(),      // station -> [pane]
  busy: new Map(),       // station -> what is shown of it is from before: as of when (its last answer)
  tasks: new Map(),      // station -> [active task]
  msgs: new Map(),       // station -> [recent message]
  projects: new Map(),   // station -> [project]
  deck: { layouts: [], pins: [] }, // global layouts and pins: one document for the fleet
  loaded: false,         // the first answers have arrived
  approvals: [],         // across the fleet
  adding: null,          // a machine being added to the fleet from this station
  notes: [],             // notification inbox (this station)
  terms: new Map(),      // key -> PaneTerm
  focused: null,
};

// ------------------------------------------------------------------ routing

function parseRoute() {
  const parts = location.hash.replace(/^#\/?/, '').split('/').map(decodeURIComponent);
  if (parts[0] === 'l' && parts[1]) return { view: 'layout', layout: 'global:' + parts[1] };
  if (parts[0] === 'perf') return { view: 'perf' };
  if (parts[0] === 's' && parts[1]) {
    // A bookmark or a notice's link from before a station was renamed.
    const station = stationNow(parts[1]);
    if (parts[2] === 'p' && parts[3]) return { view: 'pane', station, pane: parts[3] };
    if (parts[2] === 'l' && parts[3] && parts[4]) return { view: 'layout', station, layout: `${station}/${parts[3]}/${parts[4]}` };
    if (parts[2] === 'w' && parts[3]) return { view: 'watch', station, project: parts[3] };
    return { view: 'station', station };
  }
  return { view: 'fleet' };
}
const go = (hash) => { if (location.hash !== hash) location.hash = hash; else render(); };
// What a station is called now, by a name it had before it was renamed
// (the station serving this page says which names those are). A name from
// another machine: only ever compared, and put in as text.
function stationNow(name) {
  const former = S.fleet && S.fleet.former;
  const now = former && Object.prototype.hasOwnProperty.call(former, name) ? former[name] : null;
  return typeof now === 'string' && now ? now : name;
}
// What this browser keeps by a pane's station (which face a pane shows, a
// phone's fit) follows a station to its new name, once.
let renamedSeen = '';
function renamedHere() {
  const former = (S.fleet && S.fleet.former) || {};
  const key = JSON.stringify(former);
  if (key === renamedSeen) return;
  renamedSeen = key;
  for (const store of ['spandeck.face', 'spandeck.phonefit.off']) {
    try {
      const kept = JSON.parse(localStorage.getItem(store) || 'null');
      if (!kept || typeof kept !== 'object' || Array.isArray(kept)) continue;
      let changed = false;
      for (const k of Object.keys(kept)) {
        const at = k.indexOf('/');
        const now = at > 0 ? stationNow(k.slice(0, at)) : null;
        if (now && now !== k.slice(0, at)) { const to = now + k.slice(at); if (!(to in kept)) kept[to] = kept[k]; delete kept[k]; changed = true; }
      }
      if (changed) localStorage.setItem(store, JSON.stringify(kept));
    } catch (e) { /* no storage: nothing is kept */ }
  }
}
const stationUrl = (s) => `#/s/${encodeURIComponent(s)}`;
const paneUrl = (s, p) => `#/s/${encodeURIComponent(s)}/p/${encodeURIComponent(p)}`;
const layoutUrl = (s, project, name) => `#/s/${encodeURIComponent(s)}/l/${encodeURIComponent(project)}/${encodeURIComponent(name)}`;
const watchUrl = (s, project) => `#/s/${encodeURIComponent(s)}/w/${encodeURIComponent(project)}`;
const globalUrl = (name) => `#/l/${encodeURIComponent(name)}`;

// --------------------------------------------------------------------- data

const panesOf = (st) => S.panes.get(st) || [];
const liveAgents = (st) => panesOf(st).filter(p => !p.exited);
const findPane = (st, id) => panesOf(st).find(p => p.id === id || p.name === id);
const online = (st) => S.fleet.stations.find(s => s.station === st)?.state === 'online';
const stationsByName = () => S.fleet.stations.slice().sort((a, b) => a.station.localeCompare(b.station));
const onlineStations = () => stationsByName().filter(s => s.state === 'online').map(s => s.station);
const byState = (a, b) => stateRank(a) - stateRank(b) || a.name.localeCompare(b.name);
// The projects a project is linked with. A station that does not know about
// links yet says nothing, which is the same as none.
const linksOf = (pr) => Array.isArray(pr?.links) ? pr.links.filter(l => l && l.with) : [];
const linkName = (st, l) => l.station && l.station !== st ? `${l.with} on ${l.station}` : l.with;
const linkedText = (st, pr) => { const ls = linksOf(pr); return ls.length ? 'Linked with ' + linkName(st, ls[0]) + (ls.length > 1 ? ` +${ls.length - 1}` : '') : ''; };
// `short` is for a row of the list, where there is room for little: "⇄ web".
const linkedChip = (st, pr, short) => linksOf(pr).length ? h('span', { class: 'chip linked', 'aria-label': linkedText(st, pr), title: `Linked: agents in ${pr.name} can message agents in ${linksOf(pr).map(l => linkName(st, l)).join(', ')}, and they share their findings boards. Files, history and keys stay separate.` },
  short ? '⇄ ' + linksOf(pr)[0].with + (linksOf(pr).length > 1 ? ` +${linksOf(pr).length - 1}` : '') : linkedText(st, pr)) : null;

// ---- what the deck holds of each station, and how it is asked ----
//
// The owner's rule (15 Nov 2026): "the slow or absent station has to be fixed
// with the right architecture for a system like this, never to slow everyone
// else down." So each station is asked by itself, and its answer is put on
// the page when it comes: no station's answer waits for another station's,
// and the page waits for none. A station is never asked while its last
// question is unanswered (one more is asked when that ends, if something
// asked meanwhile), so questions for a station that is slow do not pile up.
// What is shown of a station whose answer is late, or did not come, stays,
// and is marked as from before (S.busy: as of when).
//
// A station that says it can (`deck_snapshot` in its caps) is asked one
// question, `deck.snapshot`, with the versions of what the page holds:
// sections that have not changed come back empty. A station from before
// (0.4.5 and earlier) is asked the seven lists.
const SECTIONS = ['panes', 'projects', 'tasks', 'msgs', 'approvals', 'notes', 'stops'];
const DOCS = ['trust', 'deck', 'look', 'prefs'];
const held = new Map(); // station -> { v, raw, sig, got, at, asking, askedAt, next, failed, old }
const slot = (st) => held.get(st) || held.set(st, { v: {}, raw: {}, sig: {}, got: {}, at: 0, asking: null, askedAt: 0, next: null, failed: false, old: false }).get(st);
const canSnapshot = (st) => !slot(st).old && (S.fleet.stations.find(s => s.station === st)?.caps || []).includes('deck_snapshot');
// A question of the refresh, counted for the Performance page.
// (`data-asked` and `data-corrections` are read by nothing in the deck: a
// test counts with them.)
let askedTotal = 0, correctedTotal = 0;
// Each has ten seconds: a question with no answer by then is over, and its
// station is asked again at the next round (the audit: no request in the page
// had a time limit, so one that was never answered held its place for ever).
const ask = (st, method, params) => { refreshAsked(); root.dataset.asked = String(++askedTotal); return api.on(st, method, params, { within: 10000 }); };
const corrected = (st, name) => { refreshCorrected(st, name); root.dataset.corrections = String(++correctedTotal); };
// What a section says, for comparing two copies of it: without the two
// fields that change with the clock alone (the station leaves them out of
// the version too).
const SKIP = { panes: 'last_output_ms', stops: 'age_ms' };
const sigOf = (name, value) => JSON.stringify(value, SKIP[name] ? (k, v) => (k === SKIP[name] ? undefined : v) : undefined);

function take(st, k, name, value) {
  // The first list of notices from a station is what it held already.
  if (name === 'notes' && !k.raw.notes) for (const n of value) announced.add(`${st}/${n.id}`);
  k.raw[name] = value;
  k.sig[name] = sigOf(name, value);
  k.got[name] = Date.now();
  if (name === 'panes') S.panes.set(st, value);
  else if (name === 'projects') S.projects.set(st, value);
  else if (name === 'tasks') S.tasks.set(st, value);
  else if (name === 'msgs') S.msgs.set(st, value);
  else if (name === 'trust') S.trust = value;
  else if (name === 'deck') S.deck = value;
  else if (name === 'look') lookTaken(value);
  else if (name === 'prefs') prefsTaken(value);
}

// One question. `full`: the check, every fourth round: everything is asked
// for, whatever the page holds, and compared with it. A section that came
// back different under the version the page holds is a correction: the page
// would have gone on showing the old one. It is repaired and counted
// (Performance page, "this browser"); it should stay 0.
async function askSnapshot(st, k, full) {
  const docs = st === api.station;
  let r;
  try { r = await ask(st, 'deck.snapshot', { have: full ? {} : k.v, docs }); } catch (e) {
    // It turned out not to know the question after all: the seven, from now on.
    if (/method deck\.snapshot/.test(e.message || '')) { k.old = true; return askLists(st, k); }
    throw e;
  }
  const v = r && typeof r.v === 'object' && r.v ? r.v : {};
  for (const name of [...SECTIONS, ...(docs ? DOCS : [])]) {
    if (r[name] === null || r[name] === undefined) continue;
    if (full && k.v[name] !== undefined && k.v[name] === v[name] && k.sig[name] !== sigOf(name, r[name])) corrected(st, name);
    take(st, k, name, r[name]);
    k.v[name] = v[name];
  }
}

// A station from before `deck.snapshot`: the seven lists, as the deck always
// asked them. A list that fails is kept as it was; the station's panes and
// projects failing is the station not answering.
async function askLists(st, k) {
  k.v = {};
  const keep = (name) => () => k.raw[name];
  const [panes, projects, tasks, msgs, approvals, notes, stops] = await Promise.all([ask(st, 'pane.list'), ask(st, 'project.list'),
    ask(st, 'task.list', { active: true, limit: 100 }).catch(() => []), ask(st, 'msg.recent', { limit: 60 }).catch(() => []),
    ask(st, 'approval.list').catch(keep('approvals')), ask(st, 'notify.list', { limit: 60 }).catch(keep('notes')), ask(st, 'stop.list').catch(keep('stops'))]);
  const lists = { panes, projects, tasks, msgs, approvals, notes, stops };
  for (const name of SECTIONS) if (lists[name] !== undefined) take(st, k, name, lists[name]);
  if (st !== api.station) return;
  // The station serving the deck, with its switch off: what it alone is asked.
  for (const [name, method] of [['trust', 'trust.status'], ['deck', 'deck.get'], ['look', 'look.get'], ['prefs', 'prefs.get']]) {
    try { take(st, k, name, await ask(st, method)); } catch (e) { /* keep */ }
  }
}

// Ask one station, unless it is being asked. `timer`: the 2.5 s round, which
// does not queue a second question behind one that is late; it marks the
// station as shown from before. Anything else that asks meanwhile gets one
// more question when this one ends, and is answered by that.
function askStation(st, { full = false, timer = false } = {}) {
  const k = slot(st);
  if (k.asking) {
    if (Date.now() - k.askedAt > 2000) stale(st, k);
    if (timer) return k.asking;
    if (!k.next) { let done; k.next = new Promise((r) => { done = r; }); k.next.done = done; }
    return k.next;
  }
  k.askedAt = Date.now();
  k.asking = (canSnapshot(st) ? askSnapshot(st, k, full) : askLists(st, k))
    .then(() => { k.at = Date.now(); k.failed = false; S.busy.delete(st); }, () => { k.failed = true; stale(st, k); })
    .then(() => {
      k.asking = null;
      settled();
      const next = k.next;
      k.next = null;
      if (next) askStation(st).then(next.done);
    });
  return k.asking;
}
function stale(st, k) {
  if (!S.busy.has(st)) { S.busy.set(st, k.at || k.askedAt); settled(); }
}

// The fleet's list, from the station serving the deck: one question at a
// time, like every station's.
let fleetAsking = null, fleetNext = null;
function askFleet() {
  if (fleetAsking) { fleetNext ||= fleetAsking.then(() => askFleet()); return fleetNext; }
  fleetAsking = ask(null, 'fleet.view').then((v) => {
    S.fleet = v;
    renamedHere();
    // This device's own list of the fleet's stations (failover.js).
    fo.keep(v);
    // What was held of a station that is no longer listed is let go.
    for (const st of held.keys()) if (!v.stations.some(s => s.station === st)) { held.delete(st); S.busy.delete(st); for (const m of [S.panes, S.projects, S.tasks, S.msgs]) m.delete(st); }
    for (const s of v.stations) if (s.state !== 'online') S.busy.delete(s.station);
  }, () => {}).then(() => { fleetAsking = null; fleetNext = null; });
  return fleetAsking;
}
// A machine being added from this station: asked when the deck opens, when
// the station says it changed, and at every round while one is being added.
// One question at a time, so that an older answer never comes after a newer.
let addAsking = null, addAgain = false;
function askAdding() {
  if (addAsking) { addAgain = true; return addAsking; }
  addAsking = ask(null, 'fleet.add_status').then((a) => { S.adding = a; addChanged(); }, () => {})
    .then(() => { addAsking = null; if (addAgain) { addAgain = false; askAdding(); } });
  return addAsking;
}

// Put together what the stations said. Run when any of them answers.
function merged() {
  const on = onlineStations();
  // How many agents are working or waiting on a station is counted from the
  // pane list it has just given, as "needs you" in the top bar is. What the
  // serving station last heard of another one can be ten seconds old, and
  // differs from one serving station to the next.
  for (const s of S.fleet.stations) {
    const live = s.state === 'online' && S.panes.has(s.station) ? liveAgents(s.station) : null;
    if (live) Object.assign(s, { working: live.filter(p => p.state === 'working').length, needs_you: live.filter(p => p.state === 'needs_you').length });
  }
  // A pane whose state changes more often than a program changes it says so
  // in one line, wherever it is drawn (`stateFlood`).
  for (const p of panesOf(api.station)) { const f = flooding.get(`${api.station}/${p.id}`); if (f) p.rapid = f.line; else delete p.rapid; }
  const of = (st, name) => held.get(st)?.raw[name] || [];
  S.approvals = on.flatMap(st => of(st, 'approvals'));
  mergeNotes(on);
  // A stop is kept by the station it was issued at, which is also the one
  // that delivers it to a station that was away. The deck shows the stops of
  // every station that answers, so the same ones wherever it is opened.
  // `got`: when this page was told, to count each stop's age on from.
  const seen = new Set();
  const stops = on.flatMap(st => of(st, 'stops').map(x => ({ from: st, ...x, got: held.get(st).got.stops })));
  if (on.some(st => held.get(st)?.raw.stops)) S.stops = stops.filter(x => !seen.has(x.id) && seen.add(x.id)).sort((a, b) => b.issued_ms - a.issued_ms);
}

// An answer arrived (or was given up on): the page is drawn with it. Several
// that arrive together are drawn once.
let settleTimer = null, afterTimer = null, afterRan = false;
function settled() {
  if (settleTimer) return;
  settleTimer = setTimeout(() => {
    settleTimer = null;
    merged();
    // The page has loaded when the station serving it has answered. A view
    // of another station says "Loading…" until that station has (`heard`).
    if (held.get(api.station)?.at) S.loaded = true;
    updateSoon();
    // What follows new data, once the serving station's documents are here:
    // at once the first time, then at most every 200 ms.
    if (!S.loaded) return;
    clearTimeout(afterTimer);
    afterTimer = setTimeout(() => { loadPerf(); loadSignIn(); vaultAtOpen(); migrateWorkspaces(); bindGlobalLeaves(); }, afterRan ? 200 : 0);
    afterRan = true;
  }, 15);
}
const heard = (st) => S.panes.has(st);
// What a view of a station says while it has nothing of it to show.
const waitWords = (st) => (S.busy.has(st) ? `${st} is not answering yet.` : 'Loading…');

// Everything: the fleet's list, then every station that is on, each by
// itself. Resolves when each has answered or been given up on, or after two
// seconds, whichever is first: a caller that waits for the page to be up to
// date does not wait for a station that is slow.
let rounds = 0, firstRound = true;
async function refresh({ timer = false } = {}) {
  if (!api.connected) return;
  const began = performance.now();
  const full = timer && ++rounds % 4 === 0;
  await askFleet();
  const asked = onlineStations().map(st => askStation(st, { full, timer }));
  if (firstRound || full || addActive() || addDlg) askAdding();
  firstRound = false;
  const all = Promise.all(asked).then(() => { if (timer) refreshTook(performance.now() - began); });
  await Promise.race([all, new Promise(r => setTimeout(r, 2000))]);
}

// The Performance page's numbers, and Tailscale key expiry for the tiles.
// Asked of every station that answers: every 4 s while the page is open, and
// once a minute otherwise, which is what the dot on its button is drawn from.
// Not waited for: nothing else on the deck depends on it.
let perfAsking = false;
async function loadPerf(now) {
  const every = S.route.view === 'perf' ? 4000 : 60000;
  if (perfAsking || (!now && S.perf && Date.now() - S.perf.at < every)) return;
  perfAsking = true;
  try {
    S.perf = await gather(api, S.fleet);
    // Each station's key as the station serving this deck knows it. A
    // station from before this has no such call: then nothing is shown.
    try { S.tailnet = await api.call('tailnet.view'); } catch (e) { S.tailnet = null; }
  } finally { perfAsking = false; }
  if (S.route.view === 'perf' || now) updateSoon();
}
// A station's Tailscale key in words, or null when nothing is known of it.
const stationKey = (st) => keyInfo(S.tailnet?.stations?.[st]);
const keyName = (st) => S.tailnet?.stations?.[st]?.name || st;
// A step's parts: text, a link that opens beside the deck, or a command.
const stepPart = (x) => (typeof x === 'string' ? x : x.code ? h('code', { class: 'cmd' }, x.code) : h('a', { class: 'out', href: x.a, target: '_blank', rel: 'noopener noreferrer' }, x.text));
// What a key's state means and what to do about it. The steps are the deck's
// own words; the name is text from a station. `more`: { station, os, lan }.
function keyDialog(name, info, more = {}) {
  const steps = keySteps(name, info, more);
  const lead = info.expired ? (more.station ? ' It cannot be reached over Tailscale until that is put right. Its agents may still be running.' : ' It is off your Tailscale network, and cannot open the Control Deck, until that is put right.')
    : more.station ? ' When it expires the machine drops off your Tailscale network, and spandeck cannot reach it.' : ' When it expires the device drops off your Tailscale network and can no longer open the Control Deck.';
  dialog(`Tailscale key of ${name}`, (close) => h('div', { class: 'consequences key-steps' },
    h('p', {}, h('b', {}, `${name}'s Tailscale key ${info.text}.`), lead),
    h('p', { class: 'muted small' }, 'The Tailscale key is the machine\'s sign-in to Tailscale, which Tailscale ends after 180 days unless told otherwise.'),
    steps.groups.map(g => [h('h3', {}, g.title), h('ol', {}, g.steps.map(step => h('li', {}, step.map(stepPart))))]),
    steps.notes.map(n => h('p', { class: 'small' }, n)),
    h('div', { class: 'row end' }, h('a', { class: 'btn out', href: TS_MACHINES, target: '_blank', rel: 'noopener noreferrer' }, 'Open Tailscale\'s admin console'), h('button', { class: 'primary', onclick: close }, 'Close'))), { wide: true });
  // The keyboard starts on Close, at the foot: a phone still opens the
  // steps at their first line.
  for (const back of document.querySelectorAll('#overlays .backdrop')) if (back.querySelector('.key-steps')) back.scrollTop = 0;
}
const stationMore = (st) => ({ station: true, lan: lanOf(S.perf, st, S.tailnet) });
const keyChip = (st) => { const k = stationKey(st); return k && k.level !== 'ok' ? h('button', { class: 'chip key-chip ' + k.level, title: 'What to do', onclick: (e) => { e.preventDefault(); e.stopPropagation(); keyDialog(keyName(st), k, stationMore(st)); } }, 'Tailscale key ' + k.text) : null; };
// On the machine itself: is its Tailscale client signed out, and where does
// it say to sign in? Asked of the station serving this deck, which answers
// only the holder of its own token; a deck opened from another device is
// refused once and does not ask again. Every 5 s while a sign-in is needed.
let signAsked = 0, signRefused = false;
async function loadSignIn() {
  if (signRefused || Date.now() - signAsked < (S.signin?.needed ? 5000 : 60000)) return;
  signAsked = Date.now();
  const was = JSON.stringify(S.signin || null);
  try { S.signin = await api.call('tailnet.signin'); } catch (e) { S.signin = null; signRefused = true; }
  if (JSON.stringify(S.signin || null) !== was) updateSoon();
}
let signBox = null, signKey = '';
function signInBar() {
  const s = S.signin;
  if (!s || !s.needed) { signBox = null; signKey = ''; return null; }
  const key = JSON.stringify([api.station, s.state, s.expired, s.url]);
  if (key === signKey) return signBox;
  // An https address or nothing: it becomes a link.
  const url = typeof s.url === 'string' && /^https:\/\/[\x21-\x7e]{1,292}$/.test(s.url) ? s.url : null;
  const w = signInWords(api.station, s, !!url);
  signKey = key;
  signBox = h('div', { class: 'vault-prompt signin-bar', role: 'region', 'aria-label': 'Tailscale needs a sign-in on this machine' },
    h('div', { class: 'grow' }, h('b', {}, w.title), h('div', { class: 'muted' }, w.why),
      h('div', {}, w.how.map(stepPart), url ? h('a', { class: 'out', href: url, target: '_blank', rel: 'noopener noreferrer' }, url) : null),
      h('div', { class: 'muted small' }, w.after)),
    url ? qrSvg(url) : null);
  return signBox;
}
// Why a station that is listed cannot be reached, when that is known: its
// clock, or its Tailscale key. Otherwise null, and it is just unreachable.
function awayWhy(s) {
  if (s.state !== 'unreachable') return null;
  if (s.why === 'clock') return `Its clock differs from ${api.station}'s by ${clockWords(s.clock_diff_ms || 0)}. Calls between them are refused until the clocks agree: set both from the network.`;
  const k = stationKey(s.station);
  return k?.expired ? `Its Tailscale key ${k.text}, so it cannot be reached.` : null;
}
// The device this browser is on, when its own key is within 14 days.
function ownKey() {
  const st = Object.entries(S.tailnet?.stations || {}).find(([, k]) => k.id && k.id === api.device);
  const d = st ? { name: st[1].name || st[0], k: st[1], more: stationMore(st[0]) } : (S.tailnet?.devices || []).filter(x => x.id && x.id === api.device).map(x => ({ name: String(x.name || 'this device'), k: { known: true, ...x }, more: { os: String(x.os || '') } }))[0];
  const info = d ? keyInfo(d.k) : null;
  return info && (info.expired || info.left <= 14) ? { name: d.name, info, more: d.more } : null;
}

// The inbox is the whole fleet's: each station keeps the notices it raised,
// and every station that answers is asked for them. A station that does not
// answer is named as not counted (S.notesAway), never passed over in silence.
// A notice every station raised for the same thing (a request to be let in)
// is one entry that names them all.
const fleetAway = () => S.fleet.stations.filter(s => s.state !== 'online' && s.state !== 'not_linked').map(s => s.station);
const announced = new Set();
function mergeNotes(stations) {
  // A station that is on and has not answered, or whose last question
  // failed: what it last said is kept on screen; it is named as not counted.
  const failed = stations.filter(st => !held.get(st)?.raw.notes || held.get(st).failed || S.busy.has(st));
  const lists = stations.map(st => (held.get(st)?.raw.notes || []).map(n => ({ ...n, station: st })));
  // A pane's notice raised again for the same reason within a few seconds
  // of the last (`sameNotice`; a state that flips raises one at each flip)
  // is one entry: the newest, which is read, and counted as unread, once.
  const merged = new Map(), chain = new Map();
  for (const n of lists.flat().sort((a, b) => b.ts - a.ts)) {
    if (n.pane) {
      const same = sameNotice(n), last = chain.get(same);
      if (last && last.ts - n.ts < NOTE_QUIET) { last.ts = n.ts; last.n.at.push({ station: n.station, id: n.id }); last.n.read = last.n.read && n.read; continue; }
      const one = { ...n, at: [{ station: n.station, id: n.id }] };
      chain.set(same, { ts: n.ts, n: one });
      merged.set(`${n.station}/${n.id}`, one);
      continue;
    }
    const key = `${n.kind}\n${n.title}\n${n.body}`;
    const had = merged.get(key);
    if (had) { if (!had.at.some(a => a.station === n.station)) had.at.push({ station: n.station, id: n.id }); had.read = had.read && n.read; }
    else merged.set(key, { ...n, at: [{ station: n.station, id: n.id }] });
  }
  S.notes = [...merged.values()].slice(0, 120);
  S.notesAway = [...new Set([...fleetAway(), ...failed])].sort();
  // A notice from a station that does not tell the others (one from before
  // it could) is announced when it first shows in the list. What a station
  // held when this page first heard from it is not news (`take`).
  for (const n of S.notes) {
    const fresh = n.at.filter(a => !announced.has(`${a.station}/${a.id}`));
    for (const a of n.at) announced.add(`${a.station}/${a.id}`);
    if (!n.read && fresh.length === n.at.length) notifyBrowser(n);
  }
}
// Mark a notice read at every station that holds it.
const readNote = (n) => Promise.all(n.at.map(a => api.on(a.station, 'notify.read', { id: a.id }).catch(() => {})));

// The look document: themes, and what the deck, a layout or a pane uses. Like
// the deck document it is asked of the station the deck was opened on.
async function loadLook() {
  try { lookTaken(await api.call('look.get')); } catch (e) { /* keep */ }
}
// The look document arrived (with the serving station's answer, or asked for
// by the Appearance editor).
function lookTaken(doc) {
  setDoc(doc);
  // What is being tried in the editor is not what to open with next time.
  if (!lookState.preview) rememberPage(pageLook(null));
  // The fonts the owner uploaded. Not waited for: a terminal shows in the
  // next font of its list until one arrives. The serving station has the
  // file unless it has not fetched it yet (or cannot keep fonts); then any
  // station that has it is asked, so a font does not show on one station's
  // deck and not on another's.
  loadFonts(async (id) => {
    try { return await api.file('/api/font/' + id); } catch (e) {
      // (While this page works through another station, that station is
      // asked first: every station holds a copy.)
      for (const st of onlineStations().filter(x => api.target || x !== api.station).sort((a, b) => (b === api.station) - (a === api.station))) {
        try { return Uint8Array.from(atob((await api.on(st, 'look.font.fetch', { id })).data_b64), c => c.charCodeAt(0)).buffer; } catch (e2) { /* the next station */ }
      }
      throw e;
    }
  });
}
// A font arrived after terminals were drawn with another in its place.
fontListeners.add(() => { for (const t of S.terms.values()) t.remeasure(); });

// Show every look again: the page's (the deck's, or the layout's on screen)
// and each pane's, in whichever face the pane is shown: its terminal, its
// conversation, or its picture on a tile. Called on every redraw and by the
// Appearance editor as a value changes.
function applyLook() {
  const layout = S.route.view === 'layout' ? S.route.layout : null;
  const page = pageLook(layout);
  applyPage(page);
  for (const t of S.terms.values()) {
    const { look, own } = paneLook(layout, `${t.station}/${t.pane.id}`);
    t.setLook(look, own ? page : null);
  }
  chatLooks((st, p) => ({ look: paneLook(layout, paneLookKey(st, p)).look, page }));
  for (const el of document.querySelectorAll('.thumb[data-key]')) applyChatFrame(el, paneLook(layout, el.dataset.key).look, page);
}
// The editor for the deck, a layout ({key, name}) or a pane ({key, name}).
// At a phone's width it is a sheet (phonelook.js): the deck's theme and font,
// and this device's own text sizes. The editor is made for a wide screen.
function appearance(level, scope = {}) {
  const layoutKey = S.route.view === 'layout' ? S.route.layout : null;
  if (isPhone()) return phoneLook({ api, apply: applyLook, reload: loadLook, update }, { layoutKey, paneKey: level === 'pane' ? scope.key : null, name: scope.name });
  lookEditor({ api, apply: applyLook, reload: async () => { await loadLook(); } },
    { level, ...scope, layoutKey: S.route.view === 'layout' ? S.route.layout : null });
}
const paneLookKey = (st, p) => `${st}/${p.id}`;

// Everything, soon; or one station, soon. The round every 2.5 s is the
// repair for whatever no event told this page of (and every fourth one is
// the check: see askSnapshot).
let refreshTimer = null;
function refreshSoon(ms = 120) { clearTimeout(refreshTimer); refreshTimer = setTimeout(refresh, ms); }
const stationTimers = new Map();
function stationSoon(st, ms = 120) {
  if (!online(st)) return refreshSoon(ms);
  // One that is due sooner stands: events that keep coming do not put the
  // question off.
  const due = Date.now() + ms, had = stationTimers.get(st);
  if (had && had.due <= due) return;
  if (had) clearTimeout(had.timer);
  stationTimers.set(st, { due, timer: setTimeout(() => { stationTimers.delete(st); if (api.connected) askStation(st); }, ms) });
}
setInterval(() => { if (document.visibilityState === 'visible') refresh({ timer: true }); }, 2500);

// A pane's strip changed, and the event says how: the pane is brought up to
// date from the event and nothing is asked. If its station's answer is on
// its way, one more is asked after it, since that answer may be older than
// the event.
function panePatched(id, change) {
  const p = panesOf(api.station).find(x => x.id === id);
  if (!p) return stationSoon(api.station);
  Object.assign(p, change);
  if (slot(api.station).asking) askStation(api.station);
  settled();
}
api.onEvent((ev) => {
  // This station's panes. A pane on another station hears of its resizes on
  // its own channel, through the gateway (term.js, onResized).
  if (ev.kind === 'pane.resized') {
    for (const t of S.terms.values()) if (t.station === api.station && t.pane.id === ev.pane) t.resizedBy(ev.data.controller, ev.data.cols, ev.data.rows);
    return;
  }
  // A line from a machine being added: shown as it arrives, nothing re-read.
  if (ev.kind === 'fleet.add.output') { if (addDlg) addDlg.line(ev.data); return; }
  // Raised here, or at another station of the fleet, which told this one.
  if (ev.kind === 'notify.new') {
    announced.add(`${ev.data.station}/${ev.data.id}`);
    notifyBrowser({ pane: ev.pane, ...ev.data });
    // (One more of a pane whose state is flipping: the round shows it.)
    if (ev.pane && flooding.has(`${api.station}/${ev.pane}`)) return;
  }
  // A pane's state changed. One that changes it over and over is no longer
  // followed change by change (`stateFlood`).
  if (ev.kind === 'pane.state' && ev.pane && stateFlood(ev)) return;
  if (ev.kind === 'connection.restored') { for (const t of S.terms.values()) t.reattach(); }
  // The page is looked at again (it was in the background, or the machine
  // slept): what it shows is brought up to date now, not at the next round.
  if (ev.kind === 'page.woke') { if (api.connected) refresh(); return; }
  if (ev.kind === 'link.back') { linkBack(ev.data.ms); return; }
  // The strip: from the event itself.
  if (ev.kind === 'agent.tool_start' && ev.pane) return panePatched(ev.pane, { tool: ev.data?.tool ?? null, tool_since_ms: ev.ts });
  if (ev.kind === 'agent.tool' && ev.pane) return panePatched(ev.pane, { tool: null });
  // New numbers for a pane (the event carries none): its station is asked,
  // twice a second at most however many agents are working.
  if (ev.kind === 'pane.usage') return stationSoon(api.station, 500);
  if (ev.kind.startsWith('fleet.add.')) askAdding();
  // Events may have been missed, or what the list of stations says changed
  // (who is in the fleet; a station's vault): everything.
  if (/^(fleet|station|connection|events|trust|vault)\./.test(ev.kind)) return refreshSoon();
  // Anything else is this station's (its events are the ones this page
  // hears), and that of a station the event names: only they are asked.
  if (/^(pane|agent|approval|project|stop|notify|task|recipe|msg|deck|look|prefs)\./.test(ev.kind)) {
    stationSoon(api.station);
    const other = ev.data && typeof ev.data.station === 'string' ? ev.data.station : null;
    if (other && other !== api.station && online(other)) stationSoon(other);
  }
});

// ---- a pane that changes state over and over ----
//
// What happened (18 Nov 2026, the owner's iPhone): a fault on the station
// flipped one pane between "working" and "needs you" about once a second for
// minutes (55 to 63 `pane.state` a minute in its log, against 0 to 10). Each
// flip was followed by a question to the station, a drawing of the whole
// page and a toast whose words differed only by a timer; the owner: "trying
// to scroll doesn't always respond… the terminal here is flashing every
// second". The station was idle: the page was what was slow. So:
//   - the page is drawn for new data twice a second at most, kept up, and
//     not under a finger that is scrolling (`updateSoon`, by `update`);
//   - a notice for the same pane and reason that repeats is said once
//     (`sameNotice`);
//   - a pane with FLOOD_N changes or more in FLOOD_MS is not followed
//     change by change: the round (2.5 s) shows where it is, the pane says
//     in one line that its state is changing rapidly, and the station's log
//     is told once (`client.report`, `what: "state-flood"`), to be read with
//     spandeck rpc log.query '{"kind":"client.report"}'.
// Six in ten seconds: the busiest minute of the log without the fault had
// ten changes for the whole station, under two in any ten seconds; an agent
// whose prompts are answered as fast as a person can makes two changes per
// answer, so six in ten seconds is three answers in ten seconds, kept up.
// The fault made nine or ten. The line goes when two or fewer are left in
// the ten seconds, so that it does not come and go at the edge. Only the
// panes of the station serving the page are counted: its events are the
// ones a page hears, and another station's pane is looked at by the round.
const FLOOD_N = 6, FLOOD_MS = 10000, FLOOD_OVER = 2, FLOOD_TELL_MS = 5 * 60000;
const stateTimes = new Map(); // station/pane -> when its state changed, the last ten seconds
const flooding = new Map();   // station/pane -> { line: { says, more } }
const floodTold = new Map();  // station/pane -> when the station's log was told
let floodTimer = null;
const lately = (key, now) => (stateTimes.get(key) || []).filter(t => now - t < FLOOD_MS);
// True while the pane is not to be followed change by change.
function stateFlood(ev) {
  const key = `${api.station}/${ev.pane}`, now = Date.now(), at = lately(key, now);
  at.push(now);
  stateTimes.set(key, at);
  if (stateTimes.size > 400) for (const [k, v] of stateTimes) if (!v.some(t => now - t < FLOOD_MS)) stateTimes.delete(k);
  const was = flooding.get(key);
  if (at.length < FLOOD_N) return !!was;
  const word = (s) => STATE_LABEL[s] || String(s || 'unknown').slice(0, 20);
  const pair = [...new Set([ev.data?.from, ev.data?.state].map(word))].sort();
  const name = String(ev.data?.name || findPane(api.station, ev.pane)?.name || ev.pane).slice(0, 60);
  const line = { says: `state is changing rapidly (${pair.join(' ⇄ ')})`,
    more: `${name} changed state ${FLOOD_N} times or more in ten seconds. A program does not do that by itself: something on ${api.station} keeps flipping it. This page looks at it every few seconds instead of at every change, and ${api.station}'s log was told (client.report, "state-flood").` };
  if (was && was.line.says === line.says) return true;
  flooding.set(key, { line });
  floodTimer ||= setInterval(floodWatch, 1000);
  if (now - (floodTold.get(key) || 0) > FLOOD_TELL_MS) {
    floodTold.set(key, now);
    api.call('client.report', { what: 'state-flood', build: api.build, station: api.station, pane: ev.pane, name, changes: at.length, span_ms: now - at[0],
      per_min: Math.round((at.length - 1) * 60000 / Math.max(1, now - at[0])), states: [ev.data?.from ?? null, ev.data?.state ?? null],
      reason: String(ev.data?.reason || '').slice(0, 200), route: location.hash.slice(0, 80), phone: isPhone() }).catch(() => {});
  }
  settled();
  return true;
}
// The flipping stopped: the pane is followed again, and asked for now.
function floodWatch() {
  const now = Date.now();
  for (const key of [...flooding.keys()]) {
    if (lately(key, now).length > FLOOD_OVER) continue;
    flooding.delete(key);
    stationSoon(api.station, 0);
    settled();
  }
  if (!flooding.size) { clearInterval(floodTimer); floodTimer = null; }
}

// Whether two notices of a pane say the same thing: the same pane, kind
// and words, with every number and length of time in the words read as one
// ("Working (1m 50s • esc to interrupt)" and "(52s • …)" are the same;
// "asks to run Bash" and "asks to edit a file" are not). One that repeats
// within NOTE_QUIET of the last, said or not, is not said again: no toast,
// no browser notification, no second entry or count in the Inbox. Eight
// seconds: longer than a state that flickers, shorter than it takes the
// owner to answer a prompt and the agent to reach its next.
const NOTE_QUIET = 8000;
const TIMES = /\d[\d.,:]*(?:\s*(?:ms|hrs?|h|mins?|secs?|m|s)\b)?(?:\s+\d[\d.,:]*\s*(?:ms|hrs?|h|mins?|secs?|m|s)\b)*/gi;
const paneOfNote = (n) => n.pane || (typeof n.url === 'string' ? (n.url.match(/\/p\/([^/?#]+)/) || [])[1] : '') || '';
const sameNotice = (n) => `${n.station || api.station}\n${paneOfNote(n)}\n${n.kind}\n${`${n.title} — ${n.body || ''}`.toLowerCase().replace(TIMES, '#').replace(/\s+/g, ' ')}`;
const lastOfPane = new Map(); // sameNotice -> when it was last raised

const lastSaid = new Map(); // what was announced -> when, so one thing every station raised is said once
function notifyBrowser(n) {
  const same = `${n.kind}\n${n.title}\n${n.body}`, now = Date.now();
  for (const [k, at] of lastSaid) if (now - at > 15000) lastSaid.delete(k);
  if (lastSaid.has(same)) return;
  lastSaid.set(same, now);
  if (paneOfNote(n)) {
    for (const [k, at] of lastOfPane) if (now - at > NOTE_QUIET) lastOfPane.delete(k);
    const key = sameNotice(n), again = lastOfPane.has(key);
    lastOfPane.set(key, now);
    if (again) return;
  }
  const from = n.station && n.station !== api.station ? ` · ${n.station}` : '';
  toast(n.title + from + (n.body ? ' — ' + n.body : ''), n.kind === 'error' ? 'error' : 'info');
  if (document.visibilityState !== 'visible' && 'Notification' in window && Notification.permission === 'granted') {
    const note = new Notification(n.title + from, { body: n.body || '', tag: n.id });
    note.onclick = () => { window.focus(); if (n.url) location.hash = n.url.replace(/^\/#/, '#'); };
  }
}

// You are "at a keyboard" while you use a full client: pushes are held back.
let lastTouch = 0;
function touch() {
  if (isPhone() || Date.now() - lastTouch < 20000) return;
  lastTouch = Date.now();
  api.call('presence.touch').catch(() => {});
}
addEventListener('keydown', touch, true);
addEventListener('mousedown', touch, true);

// A change the owner made: its station is asked again at once. One that
// can change more than its station (a stop for the fleet, the fleet's
// members) asks everything.
const ONE_STATION = /^(pane|task|msg|approval|notify|recipe|signoff|note|agents)\./;
const rpc = (st, method, params) => api.on(st, method, params).then((r) => { if (ONE_STATION.test(method)) stationSoon(st || api.station, 60); else refreshSoon(60); return r; }).catch((e) => { toast(e.message, 'error'); throw e; });

// ------------------------------------------------------------- pane menus

// Paste into a pane. A browser that will hand over the clipboard is asked for
// it; one that will not (plain http, or permission refused) gets a box to
// paste into by hand.
async function pasteInto(term) {
  if (navigator.clipboard?.readText) {
    try {
      const text = await navigator.clipboard.readText();
      if (text) return term.pasteText(text);
      return toast('There is no text on the clipboard');
    } catch (e) { /* not allowed: ask for the text instead */ }
  }
  dialog(`Paste into ${term.pane.name}`, (close) => {
    const box = h('textarea', { rows: 5, 'aria-label': 'Text to paste', placeholder: 'Press Cmd-V or Ctrl-V here, then Paste' });
    // One deliberate act: sent now or not at all. When it is not sent the
    // dialog stays, with the text in it, and says why.
    const said = h('p', { class: 'send-said', role: 'alert', hidden: true });
    const go = h('button', { class: 'primary', type: 'submit' }, 'Paste');
    const submit = async (e) => {
      e.preventDefault();
      if (!box.value || go.disabled) return;
      go.disabled = true; go.textContent = 'Sending…'; said.hidden = true;
      try { await term.pasteNow(box.value); close(); term.focus(); }
      catch (err) { said.textContent = `${err.message} Your text is still here.`; said.hidden = false; go.disabled = false; go.textContent = 'Paste'; }
    };
    return h('form', { onsubmit: submit },
      h('p', { class: 'muted' }, 'This browser does not let a page read the clipboard here. Press Cmd-V or Ctrl-V in the box, then Paste. Nothing runs until you press Enter in the pane.'),
      box, said, h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Cancel'), go));
  });
}

async function copyScreen(st, p) {
  try {
    const lines = (await api.on(st, 'pane.text', { pane: p.id })).lines.map(l => l.replace(/\s+$/, ''));
    while (lines.length && !lines[lines.length - 1]) lines.pop();
    await copyText(lines.join('\n'), `Copied what is on ${p.name}'s screen`);
  } catch (e) { toast(e.message, 'error'); }
}

// `term` is the terminal the menu was opened on, when it was opened on one.
// A pane's program was started again: every terminal showing it attaches to the new one.
function reattach(st, id) { for (const t of S.terms.values()) if (t.station === st && t.pane.id === id) { if (t.chan !== null) api.detach(t.chan); t.chan = null; t.attach(); } }
// The file explorer (files.js), opened at a pane's folder on its station.
const filesFor = (st, p) => openFiles(st, p);

function paneMenu(e, st, p, where, term) {
  const s = stateOf(p), agent = p.agent !== 'shell', alive = !p.exited;
  const act = (m, extra) => () => rpc(st, m, { pane: p.id, ...extra }).then((r) => {
    // A closed pane's own look goes with it. Its station sees to that; one
    // from before themes does not know how, so it is also asked for here.
    if (m === 'pane.close' && lookState.doc.panes[paneLookKey(st, p)]) api.call('look.set', { level: 'pane', key: paneLookKey(st, p), choice: null }).catch(() => {});
    // A restarted pane is a new program behind the same name: show it.
    if (m === 'pane.restart') reattach(st, p.id);
    return r;
  });
  const ctx = isPhone() ? null : layCtx, inLay = ctx && ctx.has(st, p), pin = panePin(st, p);
  // In the layout on screen a pane can be hidden: it keeps running and keeps
  // its place, and stays in sight as a chip beside the layout's name.
  const leaf = inLay ? ctx.leafOf(st, p) : null, seen = ctx ? ctx.shown().length : 0;
  const overTop = isPhone() && faceOf(st, p) === 'chat';
  showMenu(e.clientX, e.clientY, [
    { title: `${p.name} · ${p.address}` },
    // On a phone a conversation has the screen to itself: the deck's top
    // bar is not above it, and what that bar holds is here.
    overTop && { label: 'Approvals', hint: S.approvals.length ? `${S.approvals.length} waiting` : 'none waiting', action: approvalsPanel },
    overTop && { label: 'Inbox', action: inboxPanel },
    overTop && { label: 'Stop all…', danger: true, action: () => stopAllMenu({ clientX: e.clientX, clientY: e.clientY }) },
    overTop && '-',
    term && { label: 'Copy', hint: 'Cmd+C · Ctrl+Shift+C', disabled: !term.hasSelection(), action: () => term.copySelection() },
    term && alive && { label: 'Paste', hint: 'Cmd+V · Ctrl+Shift+V', action: () => pasteInto(term) },
    term && { label: 'Copy everything on screen', action: () => copyScreen(st, p) },
    term && term.tracksMouse() && { note: 'To select text here, hold Option (Mac) or Shift while dragging.' },
    term && '-',
    where !== 'zoom' && { label: 'Open full size', action: () => go(paneUrl(st, p.id)) },
    // With a layout on screen a pane goes beside the one that has the
    // keyboard; with none, into a layout you choose.
    ctx && !inLay && alive && { label: `Add to ${ctx.lay.name}, to the right`, hint: 'Ctrl+Shift+E', action: () => addToLayout(ctx.lay, st, p, 'row') },
    ctx && !inLay && alive && { label: `Add to ${ctx.lay.name}, below`, hint: 'Ctrl+Shift+O', action: () => addToLayout(ctx.lay, st, p, 'col') },
    leaf && !leaf.hidden && { label: 'Hide', hint: seen < 2 ? 'the only pane showing' : 'it keeps running', disabled: seen < 2, action: () => ctx.hide(leaf, true) },
    leaf && leaf.hidden && { label: 'Show', hint: 'where it was', action: () => ctx.hide(leaf, false) },
    leaf && (leaf.hidden || seen > 1) && { label: 'Show only this', hint: 'hides the others', action: () => ctx.only(leaf) },
    ctx && inLay && { label: 'Take out of this layout', hint: where === 'leaf' ? 'Ctrl+Shift+W' : undefined, action: () => { ctx.remove(ctx.sideRef(st, p)); render(); } },
    !ctx && alive && !isPhone() && { label: 'Add to a layout…', action: () => addToLayoutDialog(st, p) },
    { label: isPinned(pin) ? 'Unpin' : 'Pin', action: () => togglePin(pin) },
    '-',
    agent && alive && { label: 'Stop this turn', action: act('pane.stop'), disabled: !(s === 'working' || s === 'needs_you') },
    agent && alive && { label: p.paused ? 'Unpause (deliver work again)' : 'Pause (no new work)', action: act('pane.pause', { paused: !p.paused }) },
    agent && alive && p.held && { label: 'Release (take mail again)', hint: heldWords(p.held), action: act('pane.release') },
    agent && alive && p.stuck && s === 'needs_you' && { label: 'It is fine (not stuck)', hint: 'stop saying so for this pane', action: act('pane.stuck_fine') },
    agent && alive && p.trust?.asks?.button && { label: 'Trust this folder', hint: 'answers its question, this once', action: act('pane.trust_folder') },
    agent && alive && p.trust?.asks?.button && { label: `Trust this folder and remember it for ${p.agent_name}`, action: act('pane.trust_folder', { remember: true }) },
    agent && alive && p.hooks_trust?.asks?.button && { label: 'Trust spandeck\'s hooks', hint: 'answers its review of hooks, this once', action: act('pane.trust_hooks') },
    agent && { label: p.watch ? 'Stop notifying when it finishes' : 'Notify me when it finishes', action: act('pane.set', { watch: !p.watch }) },
    { label: p.muted ? 'Unmute notifications' : 'Mute notifications', action: act('pane.set', { muted: !p.muted }) },
    '-',
    agent && { label: 'Usage', action: () => usageDialog(st, p) },
    // What the pane's conversation adds: its background tasks, its cleared
    // notes, and the numbers of the screen (chat.js).
    ...chatMenu(st, p),
    hasCap(st, 'files') && { label: 'Files…', hint: 'this station\'s files, at its folder', action: () => filesFor(st, p) },
    { label: 'Show changes (diff)', action: () => diffPanel(st, p) },
    agent && { label: 'Standing orders for this pane', action: () => ordersPanel(st, p) },
    agent && p.project && alive && { label: 'Start a review loop on its work', action: () => rpc(st, 'recipe.review', { pane: p.id }).then(() => toast('Review loop started')) },
    agent && alive && { label: 'Run a command in it…', action: () => commandDialog(st, p) },
    { label: 'Send it a message', action: () => messageDialog(st, p) },
    // Its role's prompt, again (after a compaction, or after the owner
    // changed the role): only where the station sends roles.
    agent && alive && p.role && hasCap(st, 'roles') && { label: 'Send its role again', hint: p.role, action: () => rpc(st, 'roles.send', { pane: p.id })
      .then(r => toast(r.sent ? `Sent ${p.name} its role (${r.role}, the ${r.text} text)` : `Nothing was sent to ${p.name}: ${r.why}`, r.sent ? 'info' : 'error')) },
    { label: 'Rename', action: () => renameDialog(st, p) },
    { label: 'Appearance…', hint: lookState.doc.panes[paneLookKey(st, p)] ? 'its own' : undefined, action: () => appearance('pane', { key: paneLookKey(st, p), name: p.name }) },
    { label: 'Copy address', action: () => copyText(p.address, 'Copied ' + p.address) },
    '-',
    alive && { label: 'Kill the process', danger: true, action: () => confirmBox('Kill ' + p.name + '?', 'The process ends now. An agent can be resumed later by its session.', 'Kill', act('pane.kill')) },
    agent && { label: alive ? 'Restart and resume the conversation' : 'Start again and resume the conversation', action: () => alive
      ? confirmBox('Restart ' + p.name + '?', 'The agent is stopped and started again in this pane. It goes back to its conversation; a turn that is running now is lost.', 'Restart', act('pane.restart'))
      : act('pane.restart')() },
    agent && { label: 'Restart with a new conversation', action: () => confirmBox('Start ' + p.name + ' afresh?', 'The agent is started again in this pane with an empty conversation. The old one stays in the archive.', 'Start afresh', act('pane.restart', { fresh: true })) },
    !agent && { label: alive ? 'Restart the shell' : 'Start the shell again', action: () => alive ? confirmBox('Restart ' + p.name + '?', 'What is running in this shell is stopped.', 'Restart', act('pane.restart')) : act('pane.restart')() },
    { label: alive ? 'Kill and remove' : 'Remove from the list', danger: true, action: () => confirmBox('Remove ' + p.name + '?', 'The pane is closed and removed from this station.', 'Remove', act('pane.close')) },
  ]);
}

// Choose a running pane, from any station, that is not in the layout yet.
function pickPane(then, ctx) {
  const options = onlineStations().flatMap(st => liveAgents(st).filter(p => !ctx || !ctx.has(st, p)).map(p => ({ st, p })));
  dialog('Choose a pane', (close) => h('div', { class: 'list' },
    options.map(({ st, p }) => h('button', { class: 'list-row', onclick: () => { close(); then(st, p); } }, dot(p), paneMark(p), h('b', {}, p.name), h('span', { class: 'muted' }, `${st}${p.project ? ' · ' + p.project : ''}`))),
    options.length ? null : h('p', { class: 'muted' }, 'Every running pane is already in this layout.')));
}

// ---- folder trust: what the owner chose, per station ----
const hasCap = (st, cap) => (S.fleet.stations.find(s => s.station === st)?.caps || []).includes(cap);

// The folders trusted for agents on one station, each with Remove.
// `under`: only those in this folder (a project's own).
function trustedFolders(st, list, redraw, under) {
  const rows = (list.folders || []).filter(f => !under || f.folder === under || f.folder.startsWith(under + '/'));
  if (!rows.length) return h('p', { class: 'muted small trusted-none' }, under ? 'No folder of this project is on the list.' : `No folder is trusted for an agent on ${st}.`);
  return h('table', { class: 'table trusted-folders' }, h('tbody', {}, rows.map(f => h('tr', {},
    h('td', { class: 'mono' }, f.folder, f.there === false ? h('span', { class: 'muted' }, ' (not there now)') : null),
    h('td', {}, f.agent_name),
    h('td', { class: 'muted' }, `${st} · since ${new Date(Number(f.at_ms) || 0).toLocaleDateString()}`),
    h('td', {}, h('button', { class: 'mini danger', 'aria-label': `Stop trusting ${f.folder} for ${f.agent_name}`,
      onclick: async () => { await rpc(st, 'foldertrust.remove', { folder: f.folder, agent: f.agent }); toast(`${f.agent_name} will be left to ask about ${f.folder} again. What it already recorded for itself is its own.`); redraw(); } }, 'Remove'))))));
}

// A project's switch, and its folders on the list.
async function folderTrustDialog(st, project) {
  let list = await api.on(st, 'foldertrust.list', {});
  const path = (S.projects.get(st) || []).find(p => p.name === project)?.path;
  dialog(`Folder trust: ${project} on ${st}`, (close, body) => {
    const draw = () => {
      const on = (list.projects || []).some(p => p.project === project);
      const box = h('input', { type: 'checkbox', checked: on, style: { width: 'auto' } });
      box.addEventListener('change', async () => {
        try { await rpc(st, 'foldertrust.project', { project, on: box.checked }); } catch (e) { /* said by rpc */ }
        list = await api.on(st, 'foldertrust.list', {});
        body.replaceChildren(draw());
      });
      const redraw = async () => { list = await api.on(st, 'foldertrust.list', {}); body.replaceChildren(draw()); };
      return h('div', {},
        h('label', { class: 'row' }, box, h('b', {}, 'Agents may be started in this project\'s folders without asking me to trust them')),
        h('p', { class: 'muted small' }, `Off until you turn it on. On: when an agent's program started in ${path || 'the project\'s folder'}, a folder under it, or a worktree spandeck made for one of this project's helpers, asks whether to trust the folder, spandeck answers yes for you, once, and writes it in the ship's log. That goes for helpers an agent asks for too. It is kept on ${st}, not in the project's files: a repository cannot switch it on for itself.`),
        h('p', { class: 'muted small' }, (list.programs || []).map(p => `${p.agent_name}: ${p.allows} ${p.reach}`).join(' ')),
        h('p', { class: 'small trust-warn' }, TRUST_WARNING.replace('tick it', 'turn it on')),
        h('h3', {}, 'Folders of this project you ticked'),
        trustedFolders(st, list, redraw, path),
        h('div', { class: 'row end' }, h('button', { class: 'primary', onclick: close }, 'Done')));
    };
    return draw();
  }, { wide: true });
}

// ------------------------------------------------------------------ dialogs

const AGENT_CHOICES = [['claude', 'Claude Code'], ['codex', 'Codex'], ['opencode', 'OpenCode'], ['antigravity', 'Antigravity'], ['shell', 'Shell']];

// `preset.agent`: what Run starts at (a shell, when a shell was asked for).
function newPaneDialog(st, project, then, preset = {}) {
  dialog('New pane on ' + st, (close) => {
    const agent = h('select', {}, AGENT_CHOICES.map(([v, label]) => h('option', { value: v, selected: v === preset.agent }, label)));
    const name = h('input', { 'aria-label': 'Name' });
    const proj = h('select', {}, h('option', { value: '' }, '(no project)'), (S.projects.get(st) || []).map(p => h('option', { value: p.name, selected: p.name === project }, p.name)));
    const cwd = h('input', { placeholder: 'default: the project folder, or your home folder' });
    // A pane's name is used once in its project (and once among the panes
    // with none): offer one that is free, until a name is typed.
    let typed = false;
    const suggest = () => { if (!typed) name.value = freeName(agent.value, panesOf(st).filter(p => !p.exited && (p.project || '') === proj.value).map(p => p.name)); };
    name.addEventListener('input', () => { typed = name.value !== ''; });
    agent.addEventListener('change', suggest);
    proj.addEventListener('change', suggest);
    suggest();
    // An agent can go back to the last conversation it saved in the folder.
    // The station is asked whether there is one, so the dialog can say so
    // before the pane is started. The tick is one of the owner's
    // preferences: remembered for every station and every browser.
    const last = h('input', { type: 'checkbox', checked: pref('newPane.resumeLast', false) === true, style: { width: 'auto' } });
    last.addEventListener('change', () => setPref('newPane.resumeLast', last.checked));
    const lastNote = h('small', { class: 'muted', style: { display: 'block', margin: '2px 0 0 24px' } });
    const lastRow = h('div', { style: { marginBottom: '10px' } }, h('label', { class: 'row' }, last, 'Continue the last conversation in this folder'), lastNote);
    // "Trust this folder for <agent> if it asks": off for a folder never
    // used, on where the owner ticked it before or switched it on for the
    // project. The station says which, and what it allows.
    const trust = trustTick((q) => api.on(st, 'foldertrust.choice', q), () => ({ agent: agent.value, project: proj.value, cwd: cwd.value.trim() }));
    // "Trust spandeck's own hooks in <agent> if it asks": on unless the
    // owner turned it off on that station.
    const hooks = hooksTick((q) => api.on(st, 'hookstrust.choice', q), () => ({ agent: agent.value }));
    // "Role": none unless chosen. The agent is sent the role's prompt once
    // it has started (roles.js).
    const role = rolePick({ api, hasCap, station: () => st, isAgent: () => agent.value !== 'shell' });
    let canContinue = true, asked = 0, askTimer = null;
    const ask = async () => {
      const n = ++asked;
      const isAgent = agent.value !== 'shell';
      trust.refresh();
      hooks.refresh();
      role.refresh();
      lastRow.style.display = isAgent && canContinue ? '' : 'none';
      if (!isAgent || !canContinue) return;
      lastNote.textContent = '';
      let r;
      try { r = await api.on(st, 'pane.last_session', { agent: agent.value, project: proj.value || undefined, cwd: cwd.value.trim() || undefined }); }
      catch (e) {
        // A station from before this could be asked cannot continue one either.
        if (n === asked && (e.code === 'not_found' || /method pane\.last_session/.test(e.message || ''))) { canContinue = false; lastRow.style.display = 'none'; }
        return;
      }
      if (n !== asked) return;
      lastNote.textContent = lastSessionNote(r, AGENT_CHOICES.find(([v]) => v === agent.value)[1]);
    };
    agent.addEventListener('change', ask);
    proj.addEventListener('change', ask);
    cwd.addEventListener('input', () => { clearTimeout(askTimer); askTimer = setTimeout(ask, 350); });
    ask();
    const submit = async () => {
      clearTimeout(askTimer);
      const resumeLast = agent.value !== 'shell' && canContinue && last.checked;
      const params = { agent: agent.value, name: name.value.trim() || undefined, project: proj.value || undefined, cwd: cwd.value || undefined, resume_last: resumeLast || undefined, trust_folder: trust.value(), trust_hooks: hooks.value(), role: role.value() };
      const pane = await rpc(st, 'pane.spawn', params);
      close();
      if (params.role) toast(`Started ${pane.name}. It is being sent its role: ${role.title() || params.role}`);
      if (resumeLast && !pane.resumed) toast('No saved conversation to continue here: a new one was started');
      then ? then(pane.id) : go(paneUrl(st, pane.id));
    };
    return h('form', { onsubmit: (e) => { e.preventDefault(); submit(); } },
      field('Run', agent), field('Name', name, 'A name is used once in a project.'), field('Project', proj), field('Folder', cwd), role.row, lastRow, trust.row, hooks.row,
      h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Cancel'), h('button', { class: 'primary', type: 'submit' }, 'Start')));
  });
}

// ---- the owner's preferences ----
// Small choices that follow the owner: the fleet keeps them in one document
// (prefs.get, prefs.set), so they are the same on every station and in every
// browser. Each station is its own web address, so what a browser keeps for
// one it does not have for the next; the browser's copy is only what the
// page opens with before the station has answered, and what is used on a
// station from before preferences. What is the device's own is not here:
// which pane a phone shows, drafts, fit-to-screen, the sign-in token.
const PREF_LOCAL = { 'side.folded': 'spandeck.side.folded', 'newPane.resumeLast': 'spandeck.newPane.resumeLast' };
function prefLocal(key) {
  try {
    const v = localStorage.getItem(PREF_LOCAL[key]);
    if (v === null) return undefined;
    return key === 'newPane.resumeLast' ? v === '1' : (JSON.parse(v) ?? undefined);
  } catch (e) { return undefined; }
}
function prefKeep(key, value) {
  try { localStorage.setItem(PREF_LOCAL[key], key === 'newPane.resumeLast' ? (value ? '1' : '0') : JSON.stringify(value)); } catch (e) { /* not kept, then */ }
}
const pref = (key, fallback) => S.prefs?.values?.[key] ?? prefLocal(key) ?? fallback;
let prefSaving = 0;
function setPref(key, value) {
  prefKeep(key, value);
  if (!S.prefs) return;
  S.prefs.values = { ...S.prefs.values, [key]: value };
  prefSaving += 1;
  api.call('prefs.set', { key, value }).catch(() => {}).then(() => { prefSaving -= 1; });
}
function prefsTaken(doc) {
  if (prefSaving || !doc) return; // a change of this page's is on its way: not undone by an older answer
  doc.values ||= {};
  S.prefs = doc;
  for (const key of Object.keys(PREF_LOCAL)) {
    // What this browser chose before preferences followed the owner becomes
    // the fleet's, once, if the fleet has none.
    const mine = prefLocal(key);
    if (doc.values[key] === undefined) { if (mine !== undefined) setPref(key, mine); } else prefKeep(key, doc.values[key]);
  }
  folded.clear();
  for (const k of pref('side.folded', [])) folded.add(k);
}

// "2 hours ago", for a time that may be days back.
function longAgo(ms) {
  const mins = Math.floor(Math.max(0, Date.now() - ms) / 60000);
  const say = (n, unit) => `${n} ${unit}${n === 1 ? '' : 's'} ago`;
  if (mins < 1) return 'just now';
  if (mins < 60) return say(mins, 'minute');
  if (mins < 1440) return say(Math.floor(mins / 60), 'hour');
  return say(Math.floor(mins / 1440), 'day');
}

// What `pane.last_session` answered, in a line beside the tick.
function lastSessionNote(r, agentName) {
  if (!r) return '';
  if (r.can_find === false) return `spandeck cannot find ${agentName}'s saved conversations; a new one will start`;
  if (r.found) return `Last used ${longAgo(r.last_used_ms)}` + (r.newer_in_use ? '. A more recent one is open in another pane' : '');
  return r.newer_in_use ? 'The last one is open in another pane; a new one will start' : 'None saved here yet; a new one will start';
}

function newProjectDialog(st) {
  dialog('New project on ' + st, (close) => {
    const path = h('input', { placeholder: '/path/to/the/repo', required: true });
    const name = h('input', { placeholder: 'default: the folder name' });
    const rows = h('div', { class: 'slots' });
    const addRow = (n = '', a = 'claude', r = '') => rows.append(h('div', { class: 'slot' },
      h('input', { placeholder: 'pane name', value: n, 'aria-label': 'Pane name' }),
      h('select', { 'aria-label': 'Agent' }, AGENT_CHOICES.map(([x, label]) => h('option', { value: x, selected: x === a }, label))),
      h('input', { placeholder: 'role (optional)', value: r, 'aria-label': 'Role' }),
      h('button', { type: 'button', class: 'icon-btn', title: 'Remove', onclick: (e) => e.target.closest('.slot').remove() }, '✕')));
    addRow('dev', 'claude', 'author');
    const submit = async () => {
      const panes = [...rows.children].map(r => { const [n, a, ro] = r.querySelectorAll('input,select'); return { name: n.value.trim(), agent: a.value, role: ro.value.trim() || undefined }; }).filter(p => p.name);
      const v = await rpc(st, 'project.new', { path: path.value.trim(), name: name.value.trim() || undefined, panes, open: true });
      close();
      toast(`Project ${v.name} is open`);
      go(stationUrl(st));
    };
    return h('form', { onsubmit: (e) => { e.preventDefault(); submit(); } },
      field('Folder on ' + st, path, '~ is your home folder. A folder that is not there yet is created. One that already has .spandeck/project.toml is opened as it is.'),
      field('Name', name),
      h('div', { class: 'field' }, h('span', {}, 'Panes to start'), rows, h('button', { type: 'button', class: 'mini', onclick: () => { const used = [...rows.querySelectorAll('input[aria-label="Pane name"]')].map(i => i.value.trim()); let n = 2; while (used.includes('dev' + n)) n += 1; addRow(used.includes('dev') ? 'dev' + n : 'dev'); } }, '＋ Add a pane')),
      h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Cancel'), h('button', { class: 'primary', type: 'submit' }, 'Create and open')));
  });
}

function renameDialog(st, p) {
  dialog('Rename pane', (close) => {
    const name = h('input', { value: p.name });
    return h('form', { onsubmit: (e) => { e.preventDefault(); rpc(st, 'pane.rename', { pane: p.id, name: name.value.trim() }).then(close); } },
      field('Name', name), h('div', { class: 'row end' }, h('button', { class: 'primary', type: 'submit' }, 'Rename')));
  });
}

// Link two projects, or undo it. The other project can be on any station.
function linkDialog(st, pr) {
  dialog(`Link ${pr.name} with another project`, (close) => {
    const mine = linksOf(pr);
    const others = [];
    for (const [st2, list] of S.projects) {
      if (!online(st2)) continue;
      for (const o of list || []) if (!(st2 === st && o.name === pr.name)) others.push({ st: st2, name: o.name });
    }
    // A link to a project on a station that is away can still be undone.
    for (const l of mine) if (!others.some(o => o.name === l.with && o.st === (l.station || st))) others.push({ st: l.station || st, name: l.with, away: true });
    const params = (o) => ({ project: pr.name, with: o.name, ...(o.st !== st ? { station: o.st } : {}) });
    const row = (o) => {
      const linked = mine.some(l => l.with === o.name && (l.station || st) === o.st);
      return h('div', { class: 'link-row' }, h('div', { class: 'grow' }, h('b', {}, o.name), h('span', { class: 'muted' }, o.st === st ? ' · this station' : ` · on ${o.st}`), o.away ? h('span', { class: 'muted' }, ' · not reachable now') : null),
        linked ? h('span', { class: 'chip linked' }, 'Linked') : null,
        linked
          ? h('button', { type: 'button', 'aria-label': `Unlink ${pr.name} from ${o.name}`, onclick: () => rpc(st, 'project.unlink', params(o)).then(() => { close(); toast(`${pr.name} and ${o.name} are no longer linked`); }) }, 'Unlink')
          : h('button', { type: 'button', class: 'primary', 'aria-label': `Link ${pr.name} with ${o.name}`, onclick: () => rpc(st, 'project.link', params(o)).then(() => { close(); toast(`${pr.name} is linked with ${o.name}`); }) }, 'Link'));
    };
    return h('div', {},
      h('p', {}, 'Agents in linked projects can message each other, and the projects share their findings boards.'),
      h('p', { class: 'muted' }, 'Files, history and keys stay separate: a link does not let one project read or change the other\'s folder, search its past sessions or use its keys. You can unlink at any time.'),
      others.length ? h('div', { class: 'list' }, others.sort((a, b) => (a.st === st ? 0 : 1) - (b.st === st ? 0 : 1) || a.name.localeCompare(b.name)).map(row))
        : h('p', { class: 'muted' }, 'There is no other project on a station that is online. Add one with ＋ Project.'),
      h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Close')));
  });
}

function messageDialog(st, p) {
  dialog('Message to ' + p.name, (close) => {
    const body = h('textarea', { rows: 4, placeholder: 'It lands in the agent\'s inbox; a one-line nudge wakes it if it is idle.' });
    return h('form', { onsubmit: (e) => { e.preventDefault(); rpc(st, 'msg.send', { to: p.id, body: body.value, kind: 'note' }).then(() => { close(); toast('Sent'); }); } },
      field('Message', body), h('div', { class: 'row end' }, h('button', { class: 'primary', type: 'submit' }, 'Send')));
  });
}

async function diffPanel(st, p) {
  let d;
  try { d = await api.on(st, 'diff', { pane: p.id }); } catch (e) { return toast(e.message, 'error'); }
  dialog(`Changes in ${p.name}`, () => {
    const pre = h('pre', { class: 'diff' });
    for (const line of (d.diff || '').split('\n')) {
      const cls = line.startsWith('+') && !line.startsWith('+++') ? 'add' : line.startsWith('-') && !line.startsWith('---') ? 'del' : line.startsWith('@@') ? 'hunk' : line.startsWith('diff ') ? 'file' : '';
      pre.append(h('span', { class: cls }, line + '\n'));
    }
    return h('div', {},
      h('p', { class: 'muted' }, `Since ${d.task ? 'the task started' : 'the last commit'} (${String(d.base).slice(0, 10)}). Read-only.`),
      d.stat ? h('pre', { class: 'stat' }, d.stat) : h('p', {}, 'No changes to tracked files.'),
      d.untracked?.length ? h('p', {}, 'New files: ' + d.untracked.join(', ')) : null,
      d.diff ? pre : null,
      d.truncated ? h('p', { class: 'muted' }, 'The diff was cut short at 400 KB.') : null);
  }, { wide: true });
}

async function ordersPanel(st, p) {
  let v, table;
  try { [v, table] = await Promise.all([api.on(st, 'orders.view', { pane: p.id }), api.on(st, 'orders.table')]); } catch (e) { return toast(e.message, 'error'); }
  dialog(`Standing orders for ${p.name}`, () => h('div', {},
    v.orders.length === 0 ? h('p', {}, 'No standing orders apply to this pane.') :
      h('table', { class: 'table' },
        h('thead', {}, h('tr', {}, h('th', {}, 'Order'), h('th', {}, 'Layer'), h('th', {}, 'For this agent'))),
        h('tbody', {}, v.orders.map(o => h('tr', {},
          h('td', {}, o.text),
          h('td', {}, o.layer),
          h('td', { title: o.why_guidance || '' }, h('span', { class: 'chip ' + (o.enforced ? 'ok' : '') }, o.shown_as), o.why_guidance ? h('small', { class: 'muted block' }, o.why_guidance) : null))))),
    v.problems?.length ? h('p', { class: 'warn-text' }, v.problems.join(' · ')) : null,
    h('h3', {}, 'What each agent can block'),
    h('table', { class: 'table' },
      h('thead', {}, h('tr', {}, h('th', {}, 'Agent'), h('th', {}, 'Rules are'), h('th', {}, 'Note'))),
      h('tbody', {}, table.map(r => h('tr', {}, h('td', {}, r.name), h('td', {}, h('span', { class: 'chip ' + (r.rules_shown_as === 'enforced' ? 'ok' : '') }, r.rules_shown_as)), h('td', { class: 'muted' }, r.note)))))), { wide: true });
}

function approvalRow(a, close) {
  const reason = h('input', { placeholder: 'reason (optional)', 'aria-label': 'Reason' });
  const answer = (allow) => rpc(a.station, 'approval.answer', { id: a.id, allow, reason: reason.value || undefined }).then(() => { toast(allow ? 'Allowed' : 'Denied'); close && close(); });
  const p = findPane(a.station, a.pane);
  return h('div', { class: 'approval' },
    h('div', { class: 'approval-what' }, h('b', {}, p?.name || a.pane), h('span', { class: 'muted' }, ` on ${a.station} · ${ago(a.ts)} ago`), h('div', { class: 'mono' }, a.summary)),
    h('div', { class: 'row' }, reason,
      h('button', { class: 'primary', onclick: () => answer(true) }, 'Allow'),
      h('button', { class: 'danger', onclick: () => answer(false) }, 'Deny')));
}

// " and 2 other stations (a, b)": the fleet-mates a station says come with
// it. Names from another machine: only ever put in as text.
const cameWith = (w) => !(w || []).length ? '' : ` and ${w.length} other station${w.length === 1 ? '' : 's'} (${w.join(', ')})`;
// Stations and browsers asking to be let in, at any station of the fleet.
// Approving one gives it control of every station, so the code shown here
// must match the one it shows.
function trustPanel() {
  dialog('Stations and browsers', (close) => {
    const t = S.trust || {};
    // A station is let in under the name in its card's box, when there is
    // one: its own name may be a station's already.
    const boxes = new Map();
    const answer = (r, yes) => rpc(api.station, yes ? 'trust.approve' : 'trust.deny', yes && boxes.get(r.id) ? { request: r.id, as: boxes.get(r.id).value.trim() || undefined } : { request: r.id }).then(close);
    return h('div', {},
      h('h3', {}, 'Asking to be let in'),
      (t.requests || []).length ? h('div', { class: 'list' }, t.requests.map(r => h('div', { class: 'note unread' },
        h('div', {}, h('b', {}, r.kind === 'station' ? `Station ${r.name}${cameWith(r.with)}` : r.name), h('span', { class: 'muted' }, ` · from ${r.from}${r.at && r.at !== t.station ? ` · asked at ${r.at}` : ''}`)),
        h('div', {}, 'It shows the code ', h('b', { class: 'code' }, r.code), '. Approve only if you started this and the codes match: it will be able to control every station.',
          (r.with || []).length ? ` Its whole fleet joins this one: every station that holds its key, whatever names it gives. Browsers signed in to those stations sign in again; browsers signed in to this fleet are not affected.` : '',
          r.at && r.at !== t.station ? ` Your answer is passed to ${r.at}, the station that was asked, which hands over the key.` : ''),
        r.kind === 'station' ? joinName(r, boxes) : null,
        h('div', { class: 'row end' }, h('button', { class: 'mini', onclick: () => answer(r, false) }, 'Refuse'), h('button', { class: 'mini primary', onclick: () => answer(r, true) }, 'Approve')))))
        : h('p', { class: 'muted' }, 'Nothing is waiting.'),
      t.outgoing ? h('p', { class: t.outgoing.problem ? 'warn-text' : '' }, t.outgoing.problem
        ? `Joining the fleet ${t.outgoing.station} is in did not finish: ${t.outgoing.problem}.`
        : `Waiting for an approval of the request that shows the code ${t.outgoing.code}, at any station of that fleet (${t.outgoing.station} was the one asked). ${(t.outgoing.with || []).length ? `This station${cameWith(t.outgoing.with)} then move into that fleet.` : 'This station then joins that fleet.'}`) : null,
      h('h3', {}, 'Browsers signed in to this fleet'),
      (t.devices || []).length ? h('div', { class: 'list' }, t.devices.map(d => h('div', { class: 'note' },
        h('div', { class: 'row' }, h('b', {}, d.name), h('span', { class: 'muted' }, [d.from ? `from ${d.from}` : '', d.approved_ms ? `approved${d.at ? ` at ${d.at}` : ''} ${ago(d.approved_ms)} ago` : d.at ? `first seen at ${d.at}` : ''].filter(Boolean).map(x => ' · ' + x).join('')), h('span', { class: 'grow' }),
          h('button', { class: 'mini danger', title: 'Every station of the fleet refuses it from now on; a station that is off, as soon as it is back', onclick: () => rpc(api.station, 'device.revoke', { device: d.id }).then(close) }, 'Sign out')),
        d.unconfirmed ? h('div', { class: 'muted small' }, `Listed since before this list was shared between stations. If it was approved before ${t.station} last changed fleets, it is no longer signed in.`) : null)))
        : h('p', { class: 'muted' }, 'None. A browser on another device asks to sign in the first time it opens the Control Deck.'),
      h('p', { class: 'muted small' }, 'The same list on every station of the fleet: a browser approved at one is accepted by all, and signing one out here signs it out everywhere.'),
      h('p', { class: 'muted' }, `Fleet ${t.fleet || ''}. To add a station that is running, choose "Add … to this fleet" on its tile; to add a machine that has no spandeck yet, use Add a machine. Either way you approve its code here or at any station of this fleet. If it is in a fleet of its own, every station of that fleet joins with it: there is one fleet afterwards.`));
  });
}

// The name a station joins under, in its request's card: a box, filled
// with a free name when the station's own is a member's already (capitals
// aside), and the fact said. Every name here is text from another machine.
function joinNameWords(r) {
  const c = r.clash;
  const taken = !c ? '' : c.exact ? `A station of this fleet is already called ${c.with}.`
    : String(c.with).toLowerCase() === String(r.name).toLowerCase() ? `${r.name} has the same name as ${c.with}, a station of this fleet, capitals aside.`
      : `${r.name} was the name of ${c.with}, a station of this fleet, and still finds it.`;
  const mates = (r.with_clash || []).map(m => `${m.name}, which comes with it, and ${m.with} of this fleet have one name, capitals aside: rename one afterwards.`).join(' ');
  return { taken, mates };
}
function joinName(r, boxes, preset) {
  const { taken, mates } = joinNameWords(r);
  if (!r.can_rename) {
    return taken || mates ? h('p', { class: 'warn-text join-name-fact' }, [taken, taken ? `${r.name} runs a spandeck from before a station could be named as it is let in: it can only be let in as ${r.name} and renamed once it is updated, or refused.` : '', mates].filter(Boolean).join(' ')) : null;
  }
  const input = h('input', { class: 'join-name', autocapitalize: 'none', autocorrect: 'off', spellcheck: false, autocomplete: 'off', maxLength: 64,
    value: preset || (r.clash ? r.suggest || '' : ''), placeholder: r.name, 'aria-label': `Name for ${r.name} in this fleet` });
  boxes.set(r.id, input);
  return h('div', { class: 'join-name-row' },
    taken || mates ? h('p', { class: 'warn-text join-name-fact' }, [taken, taken ? 'It is let in under another name, or refused.' : '', mates].filter(Boolean).join(' ')) : null,
    field(r.clash ? 'Its name in this fleet' : 'Its name in this fleet (leave empty to keep its own)', input,
      r.clash ? 'Letters, digits, - _ and . only. It takes the name as it joins.' : r.tailscale ? `It calls itself ${r.name}; Tailscale calls that machine ${r.tailscale}.` : null));
}

// "RTX6000 and rtx6000 differ only by capitals": a fleet from before names
// had to differ keeps working as it did, and one quiet line offers to end
// the doubt.
function twinsLine() {
  const twins = (S.fleet.twins || []).filter(p => Array.isArray(p) && p.length === 2);
  if (!twins.length) return null;
  const station = (name) => S.fleet.stations.find(s => s.station === name);
  return h('div', { class: 'twins' }, twins.map(([a, b]) => h('p', { class: 'muted hint twins-line' },
    `${a} and ${b} differ only by capitals. Each is found by its own spelling. `,
    [a, b].map(name => { const s = station(name); return s && s.state === 'online' && (s.caps || []).includes('rename')
      ? h('button', { type: 'button', class: 'mini', onclick: () => renameStationDialog(s) }, `Rename ${name}…`) : null; }))));
}

// Rename a station. It keeps running: the dialog says what follows it to
// the new name and what does not, and the station says no in a sentence.
function renameStationDialog(s) {
  const st = s.station;
  const input = h('input', { class: 'rename-name', autocapitalize: 'none', autocorrect: 'off', spellcheck: false, autocomplete: 'off', maxLength: 64, value: st, 'aria-label': `New name for ${st}` });
  const why = h('p', { class: 'warn-text rename-why', role: 'alert' });
  const others = S.fleet.stations.filter(x => x.station !== st && x.state !== 'not_linked');
  const old = others.filter(x => x.state === 'online' && !(x.caps || []).includes('rename')).map(x => x.station);
  const d = dialog(`Rename ${st}`, (close) => {
    const ok = h('button', { class: 'primary', type: 'submit' }, 'Rename');
    const submit = (e) => {
      e.preventDefault();
      const name = input.value.trim();
      if (!name || name === st) return input.focus();
      ok.disabled = true; why.textContent = '';
      api.on(st, 'station.rename', { name }).then((r) => { close(); toast(r.says || `${st} is now ${name}`); refreshSoon(2500); },
        (e2) => { ok.disabled = false; why.textContent = e2.message; input.focus(); });
    };
    return h('form', { class: 'consequences rename-station', onsubmit: submit },
      field(`New name for ${st}`, input, 'Letters, digits, - _ and . only, and not the name of another station, capitals aside.'),
      why,
      h('ul', {},
        h('li', {}, `${st} keeps running: every pane, agent, conversation and terminal stays open. spandeck there restarts in place to take the name, as for an update, so this page reconnects for a moment.`),
        h('li', {}, `Layouts, pins, looks, messages, stops and links between projects that name ${st} name it by its new name, on every station. ${st} still finds it for a year: an address an agent remembers, a bookmark, a notice's link.`),
        h('li', {}, 'Its address for secure access (HTTPS) is its Tailscale name and does not change. A vault token it holds keeps working and keeps its old label at the vault.'),
        old.length ? h('li', { class: 'warn-text' }, `${old.join(', ')} ${old.length === 1 ? 'runs' : 'run'} a spandeck from before stations could be renamed: ${old.length === 1 ? 'it' : 'they'} will list ${st} as well, as a station that does not answer, until updated.`) : null),
      h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Cancel'), ok));
  });
  setTimeout(() => { input.focus(); input.select(); }, 0);
  return d;
}

function approvalsPanel() {
  dialog('Approval requests', (close) => S.approvals.length
    ? h('div', { class: 'list' }, S.approvals.map(a => approvalRow(a, close)))
    : h('p', {}, 'Nothing is waiting for your approval.'));
}

// One inbox for the fleet: what agents wrote to you and what was raised for
// you on every station that answers, each entry with the station it is from.
async function inboxPanel() {
  const stations = onlineStations(), failed = [];
  const msgs = (await Promise.all(stations.map(st => api.on(st, 'msg.inbox', { all: true, peek: true })
    .then(list => list.map(m => ({ ...m, station: st }))).catch(() => { failed.push(st); return []; })))).flat().sort((a, b) => b.ts - a.ts);
  const away = [...new Set([...(S.notesAway || []), ...failed])].sort();
  // Each notice is marked at the station that holds it; so is each message.
  const readAll = () => Promise.all(stations.flatMap(st => [api.on(st, 'notify.read', {}).catch(() => {}), api.on(st, 'msg.inbox', {}).catch(() => {})])).then(() => refreshSoon(0));
  // One entry, a message or a notice: a title, what it says, and a line of
  // where it is from and when. Every entry is built here, so every one has
  // the same sizes (app.css, "The Inbox"). What it says is cut to a few
  // lines; a tap on it shows all of it in place, and reads it. What it can
  // do (its pane, "Release all") is a button of its own beside it. The words
  // are an agent's or another station's: text, never markup.
  const entry = (cls, unread, dot, title, body, meta, acts, onRead) => {
    const main = h('button', { type: 'button', class: 'inbox-main', 'aria-expanded': 'false', onclick: () => {
      const open = item.classList.toggle('open');
      main.setAttribute('aria-expanded', String(open));
      if (onRead && item.classList.contains('unread')) { item.classList.remove('unread'); onRead(); }
    } },
      dot ? h('span', { class: 'dot ' + dot }) : null,
      h('span', { class: 'inbox-title', title }, title),
      h('span', { class: 'inbox-body', title: body }, body),
      h('span', { class: 'inbox-meta', title: meta }, meta));
    const item = h('div', { class: `inbox-item ${cls} note` + (unread ? ' unread' : '') + (dot ? '' : ' no-dot') }, main,
      acts.length ? h('div', { class: 'inbox-acts' }, acts) : null);
    return item;
  };
  dialog('Inbox', (close) => h('div', { class: 'inbox' },
    h('div', { class: 'row end inbox-top' }, h('button', { class: 'mini', onclick: () => readAll().then(close) }, 'Mark all read'),
      'Notification' in window && Notification.permission === 'default' ? h('button', { class: 'mini', onclick: () => Notification.requestPermission() }, 'Enable browser notifications') : null),
    away.length ? h('p', { class: 'muted small' }, `Not counted, because ${away.length === 1 ? 'it is' : 'they are'} not reachable: ${away.join(', ')}.`) : null,
    h('h3', {}, 'Messages to you'),
    // A message is read with "Mark all read", as before: opening it to see
    // the rest of it does not take it from the agent's list of what it sent.
    msgs.length ? h('div', { class: 'list' }, msgs.map(m => entry('inbox-msg', !m.read, null, String(m.from), String(m.body ?? ''), `${m.station} · ${m.kind} · ${ago(m.ts)} ago`, [], null)))
      : h('p', { class: 'muted' }, 'No messages.'),
    h('h3', {}, 'Notifications'),
    S.notes.length ? h('div', { class: 'list' }, S.notes.map(n => {
      const read = () => readNote(n).then(() => refreshSoon(0));
      return entry('inbox-note', !n.read, n.kind === 'approval' ? 'needs_you' : n.kind, String(n.title), String(n.body ?? ''),
        [[...new Set(n.at.map(a => a.station))].join(', '), n.pushed === 2 ? 'held (you were here)' : n.pushed === 1 ? 'pushed' : '', ago(n.ts)].filter(Boolean).join(' · '), [
          n.url ? h('button', { class: 'mini', title: 'Open the pane this is about', onclick: () => { read(); close(); location.hash = n.url.replace(/^\/#/, '#'); } }, 'Open') : null,
          // The recovery notice carries the one action that lets the restarted sessions take mail again.
          ...(n.kind === 'station.recovered' ? n.at.map(a => a.station).filter(st => (S.fleet.stations.find(s => s.station === st)?.caps || []).includes('holds'))
            .map(st => h('button', { class: 'mini', onclick: () => releaseAll(st) }, `Release all on ${st}`)) : [])].filter(Boolean), read);
    })) : h('p', { class: 'muted' }, 'No notifications.')), { wide: true });
}

// "Release all on this station": every held agent there takes mail again.
// Pause is not changed by it, and the answer says how many stay paused.
function releaseAll(st) {
  return rpc(st, 'pane.release', { all: true }).then((r) => toast(`Released ${plural(r.released, 'pane')} on ${st}` + (r.still_paused ? ` (${r.still_paused} still paused)` : '')));
}

function stopAllMenu(e) {
  const stop = (scope, label) => () => confirmBox('Stop ' + label + '?', 'Every running agent turn is interrupted. Processes stay, and nothing is lost.', 'Stop', () => rpc(api.station, 'stop.all', { scope }).then(() => toast('Stop sent')));
  showMenu(e.clientX, e.clientY, [
    { label: `Stop every agent on ${api.station}`, danger: true, action: stop('station', 'this station') },
    { label: 'Stop every agent in the fleet', danger: true, action: stop('fleet', 'the whole fleet') },
  ]);
}

async function tasksPanel(st, project) {
  const [tasks, recipes, sign] = await Promise.all([
    api.on(st, 'task.list', { project, limit: 30 }), api.on(st, 'recipe.list', { project }), api.on(st, 'signoff.status', { project }).catch(() => null)]);
  // A failed task that kept its resume information can be sent back to its
  // agent, on a station that knows how (one from before this does not say so).
  const canContinue = (S.fleet.stations.find(s => s.station === st)?.caps || []).includes('task_continue');
  const again = (t, close) => canContinue && t.state === 'failed' && t.resume?.pane
    ? h('button', { class: 'mini', title: 'The task is running again, and its agent is told it was interrupted and to check its last step', onclick: () => rpc(st, 'task.continue', { task: t.id }).then(() => { close(); toast('The task is running again'); }) }, 'Continue this task') : null;
  dialog(`Tasks in ${project}`, (close) => h('div', {},
    h('h3', {}, 'Sign-off'),
    sign ? h('p', {}, sign.signed ? `Commit ${String(sign.commit).slice(0, 10)} was signed off ${ago(sign.ts)} ago. ` : 'No sign-off yet. ',
      sign.stale ? h('span', { class: 'warn-text' }, `Stale: ${sign.why_stale}. `) : null,
      h('button', { class: 'mini', onclick: () => rpc(st, 'signoff.give', { project }).then(() => { close(); toast('Signed off'); }) }, 'Sign off the current commit')) : null,
    h('h3', {}, 'Review loops'),
    recipes.length ? h('div', { class: 'list' }, recipes.map(r => h('div', { class: 'note' }, h('b', {}, `Round ${r.data.round} of ${r.data.max_rounds} · ${r.data.phase.replace('_', ' ')}`), h('div', { class: 'muted' }, r.data.note || ''),
      r.state === 'active' ? h('button', { class: 'mini', onclick: () => rpc(st, 'recipe.cancel', { id: r.id }).then(close) }, 'Cancel') : null))) : h('p', { class: 'muted' }, 'None.'),
    h('h3', {}, 'Tasks'),
    tasks.length ? h('table', { class: 'table' }, h('thead', {}, h('tr', {}, h('th', {}, 'State'), h('th', {}, 'Role'), h('th', {}, 'Goal'), h('th', {}, 'Result'))),
      h('tbody', {}, tasks.map(t => h('tr', {}, h('td', {}, h('span', { class: 'chip' }, t.state.replaceAll('_', ' '))), h('td', {}, t.role || ''), h('td', {}, t.goal), h('td', { class: 'muted' }, t.outputs?.summary || t.resume?.reason || '', ' ', again(t, close)))))) : h('p', { class: 'muted' }, 'No tasks yet.')), { wide: true });
}

function palette() {
  const here = online(S.route.station) ? S.route.station : api.station;
  const actions = [
    { label: 'Go to the fleet', run: () => go('#/fleet') },
    ...S.fleet.stations.map(s => ({ label: `Go to station ${s.station}`, run: () => go(stationUrl(s.station)) })),
    ...S.fleet.stations.flatMap(s => liveAgents(s.station).map(p => ({ label: `Open pane ${p.name} on ${s.station}`, run: () => go(paneUrl(s.station, p.id)) }))),
    ...allLayouts().map(x => ({ label: `Open layout ${x.name} (${layShort(x)})`, run: () => go(layUrl(x)) })),
    { label: 'New global layout…', run: () => newLayoutDialog(null, null) },
    // An action on one station names it: which one it is depends on what
    // is on screen, or on the station serving this deck.
    { label: `New pane on ${here}…`, run: () => newPaneDialog(here) },
    { label: `New project on ${here}…`, run: () => newProjectDialog(here) },
    ...(hasCap(here, 'files') ? [{ label: `Files on ${here}…`, run: () => openFiles(here, null) }] : []),
    { label: 'Approval requests', run: approvalsPanel },
    { label: 'Inbox', run: inboxPanel },
    { label: 'Usage: tokens and estimated cost', run: fleetUsageDialog },
    { label: 'Performance: stations, links and Tailscale keys', run: () => go('#/perf') },
    { label: 'Appearance: themes, colours and fonts', run: () => appearance('deck') },
    { label: `Stop every agent on ${api.station}`, run: () => rpc(api.station, 'stop.all', { scope: 'station' }) },
    { label: `Check ${here} (doctor)`, run: () => doctorPanel(here) },
    { label: 'Agent setup: MCP servers, skills, models', run: setupPanel },
    { label: 'Tools for agents: add, roll out, upgrade', run: openTools },
    { label: 'Roles for agents: view and change each role\'s prompt', run: openRoles },
    { label: 'Vault: connect stations and set token rotation', run: vaultPanel },
    { label: 'Run a command in every agent…', run: () => commandDialog(api.station, null) },
  ];
  dialog('Go to or do…', (close) => {
    const input = h('input', { placeholder: 'Type to filter', 'aria-label': 'Filter' });
    const list = h('div', { class: 'list' });
    const draw = () => {
      const q = input.value.toLowerCase();
      const hits = actions.filter(a => a.label.toLowerCase().includes(q)).slice(0, 12);
      list.replaceChildren(...hits.map((a, i) => h('button', { class: 'list-row' + (i === 0 ? ' first' : ''), onclick: () => { close(); a.run(); } }, a.label)));
    };
    input.addEventListener('input', draw);
    input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); list.firstChild?.click(); } });
    draw();
    return h('div', {}, input, list);
  });
}

async function doctorPanel(where) {
  const st = (typeof where === 'string' && where) || S.route.station || api.station;
  let d;
  try { d = await api.on(st, 'station.doctor'); } catch (e) { return toast(e.message, 'error'); }
  dialog('Station check: ' + st, () => h('table', { class: 'table' }, h('tbody', {}, d.checks.map(c => h('tr', {},
    h('td', {}, h('span', { class: 'chip ' + (c.ok ? 'ok' : c.required ? 'bad' : 'warn') }, c.ok ? 'ok' : c.required ? 'fail' : 'warn')), h('td', {}, c.name), h('td', { class: 'muted' }, c.detail))))), { wide: true });
}

// ----------------------------------------------------------- agent setup

const COMMAND_PRESETS = [
  ['/login', 'Sign in again (finish in the terminal)'],
  ['/model ', 'Change the model'],
  ['/effort ', 'Change the effort level'],
  ['/compact', 'Compact the conversation'],
  ['/clear', 'Start a fresh conversation'],
  ['/mcp', 'Show MCP servers'],
  ['/status', 'Show the agent\'s status'],
];

// Run a command in one agent (pane given) or in many.
function commandDialog(st, pane) {
  dialog(pane ? `Run a command in ${pane.name}` : 'Run a command in agents', (close) => {
    const cmd = h('input', { placeholder: '/model opus', 'aria-label': 'Command' });
    const scope = h('select', { 'aria-label': 'Where' },
      h('option', { value: 'station' }, `Every agent on ${st}`), h('option', { value: 'fleet' }, 'Every agent in the fleet'));
    const kind = h('select', { 'aria-label': 'Which agents' }, h('option', { value: '' }, 'All agents'), h('option', { value: 'claude' }, 'Claude Code only'), h('option', { value: 'codex' }, 'Codex only'));
    const force = h('input', { type: 'checkbox', style: { width: 'auto' } });
    const result = h('div', { class: 'list' });
    const run = async () => {
      const command = cmd.value.trim();
      if (!command) return;
      const params = pane ? { command, pane: pane.id, force: true } : { command, fleet: scope.value === 'fleet', agent: kind.value || undefined, force: force.checked };
      const out = await rpc(pane ? st : api.station, 'agents.command', params);
      if (pane) {
        close();
        // Commands like /login are finished by hand: bring the terminal up
        // (an agent's pane opens as its conversation otherwise).
        setFace(st, pane, 'terminal');
        go(paneUrl(st, pane.id));
        return;
      }
      result.replaceChildren(...out.stations.map(x => h('div', { class: 'note' }, h('b', {}, x.station), x.error ? h('span', { class: 'warn-text' }, ' ' + x.error) :
        h('span', {}, ` sent to ${x.sent.length}`, x.skipped.length ? `, skipped ${x.skipped.length} (${x.skipped.map(k => `${k.name}: ${k.why}`).join('; ')})` : ''))));
    };
    return h('form', { onsubmit: (e) => { e.preventDefault(); run(); } },
      field('Command', cmd, 'Typed into the agent exactly as you would type it.'),
      h('div', { class: 'row', style: { flexWrap: 'wrap' } }, COMMAND_PRESETS.map(([c, title]) => h('button', { type: 'button', class: 'mini', title, onclick: () => { cmd.value = c; cmd.focus(); } }, c.trim()))),
      pane ? null : h('div', { class: 'row', style: { marginTop: '10px' } }, scope, kind),
      pane ? null : h('label', { class: 'row', style: { marginTop: '8px' } }, force, 'Also send to agents that are in the middle of a turn'),
      h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Close'), h('button', { class: 'primary', type: 'submit' }, pane ? 'Run and open the terminal' : 'Run')),
      result);
  });
}

// MCP servers, skills and model defaults: set once, applied on every station.
// Which agent programs each station has, whether each one starts, at what
// version, whether that is the newest published, and installing or updating
// them. Everything a program or a station said (an error line, a path, a
// command, what an updater printed) is put in as text.
//
// The owner, 17 Nov 2026, from an iPhone: "I picked update all on
// m4-MacBook, no feedback, don't know if it's doing it, if it worked, don't
// know if the version listed is the latest or not"; and "I can't press
// update twice in a row, it should grey out until the update process is
// complete". So (DECISIONS.md, "Agent programs: every action says what it
// did"):
// - An update or install is a job its station keeps (`job` in each row of
//   `agents.status`). What a row shows, and whether its buttons are greyed,
//   comes from that record, not from this page's memory: it is the same on
//   another device, and after this dialog is closed and opened again.
// - Each station is asked by itself and drawn when it answers: a slow one
//   holds up only its own rows.
// - Every card is the same three lines in the same order, at any width.
const AP_RECENT_MS = 10 * 60 * 1000;
const AP_RULE = 'After an update that changes the version, an idle agent of this program restarts and goes back to its conversation; one in the middle of a turn, or waiting for you, restarts when the turn is over. A reinstall does not restart it.';
// What a job is doing, or how it ended. `row` is the station's row.
function apJobWords(r) {
  const j = r.job;
  if (!j) return null;
  const doing = j.action === 'install' ? 'Installing' : 'Updating';
  if (j.state === 'running') return j.step === 'running' && j.command ? `${doing}… running \`${j.command}\`` : `${doing}… reading its version first`;
  const from = j.from || '?', to = j.to || '?';
  let s;
  if (j.outcome === 'updated') s = `Updated ${from} → ${to}`;
  else if (j.outcome === 'latest') s = r.latest?.state === 'available' && r.latest.version ? `Still ${to}: the updater finished without installing ${r.latest.version}` : `Already the latest: ${to}`;
  else if (j.outcome === 'installed') s = `${j.from ? 'Reinstalled' : 'Installed'} ${j.to || ''}`.trim();
  else if (j.outcome === 'failed') s = `Could not ${j.action === 'install' ? 'install' : 'update'}: ${j.reason || 'it did not say why'}`;
  else if (j.outcome === 'interrupted') s = `Not known whether it finished: ${j.reason || ''}`;
  else return null;
  const now = (j.restarted || []).length, later = (j.restart_when_idle || []).length;
  if (now) s += ` · ${now} restarted into ${now === 1 ? 'its conversation' : 'their conversations'}`;
  if (later) s += ` · ${later} ${later === 1 ? 'restarts' : 'restart'} when the turn ends`;
  return s;
}
const apPlural = (n, one, many) => `${n} ${n === 1 ? one : many || one + 's'}`;

function agentsPanel() {
  const stations = S.fleet.stations.filter(s => s.state === 'online').map(s => s.station);
  const openedAt = Date.now();
  const view = new Map(stations.map(st => [st, { rows: null, problem: null }]));   // what each station last said
  const picked = new Map();    // "station agent" -> the way to install the owner picked here
  const asked = new Map();     // "station agent" -> what this page is asking of the station, until it answers
  const errs = new Map();      // "station agent" (or "station") -> a request that could not be made, in the station's words
  const shown = new Set();     // "station agent": "Show what it said" is open
  const timers = new Map();
  let open = true, drawn = '', draw = () => {};
  const key = (st, agent) => st + ' ' + agent;
  const knows = (st) => hasCap(st, 'agent_jobs');
  // A station that does not say (an older spandeck) only knows there or not.
  const healthOf = (r) => r.health || (r.installed ? 'ok' : 'missing');
  const running = (st, r) => r.job ? r.job.state === 'running' : !!(r.updating || r.installing);
  const busy = (st, r) => running(st, r) || asked.has(key(st, r.agent));
  const updatable = (r) => ['ok', 'no_answer'].includes(healthOf(r)) && r.can_update !== false;
  const rowsOf = (st) => view.get(st)?.rows || [];

  // One look at a station at a time; a look asked for meanwhile is made
  // straight after, and whoever asked waits for that one.
  const looking = new Map();
  const look = (st) => {
    if (looking.has(st)) { looking.get(st).again = true; return looking.get(st).done; }
    const mine = { again: false };
    mine.done = (async () => {
      clearTimeout(timers.get(st));
      const v = view.get(st);
      do {
        mine.again = false;
        try { v.rows = await api.on(st, 'agents.status', {}, { within: 9000 }); v.problem = null; } catch (e) { v.problem = e.message; }
      } while (mine.again && open);
      looking.delete(st);
      if (!open) return;
      draw();
      // Look again soon while something is going on there; now and then
      // otherwise, since another device may start something.
      const live = v.problem || rowsOf(st).some(r => running(st, r) || r.reading || r.latest?.checking);
      timers.set(st, setTimeout(() => look(st), live ? 1000 : 6000));
    })();
    looking.set(st, mine);
    return mine.done;
  };
  const check = (st, again) => {
    if (!knows(st)) return;
    api.on(st, 'agents.check', again ? { again: true } : {}).then(() => look(st)).catch(() => {});
  };

  // Ask a station to start something, for some of its rows. From the press
  // until the station has answered, those rows say so and their buttons
  // are greyed; after that the station's own record does both.
  const ask = async (st, agents, words, method, params) => {
    for (const a of agents) { asked.set(key(st, a), words); errs.delete(key(st, a)); }
    errs.delete(st);
    draw();
    const jobOf = (a) => rowsOf(st).find(x => x.agent === a)?.job?.id;
    const before = new Map(agents.map(a => [a, jobOf(a)]));
    const where = agents.length === 1 ? key(st, agents[0]) : st;
    let failed = null, unsure = false;
    try {
      const r = await api.on(st, method, params);
      // Not started, and no job of its own to show: the station's reason.
      for (const s of r.skipped || []) if (!s.job && (agents.length === 1 || s.why !== 'not installed on this station')) errs.set(key(st, s.agent), `Not started: ${s.why}`);
    } catch (e) {
      // No answer in time is not "no": the station may have started. Its
      // own record says, and is read next.
      if (/did not answer|could not confirm/.test(e.message)) unsure = true; else failed = `Could not start: ${e.message}`;
    }
    await look(st);
    for (const a of agents) asked.delete(key(st, a));
    const began = agents.some(a => { const r = rowsOf(st).find(x => x.agent === a); return r && (running(st, r) || jobOf(a) !== before.get(a)); });
    if (failed) errs.set(where, failed);
    else if (unsure && !began) errs.set(where, `${st} did not confirm in time, and nothing is running there now: it may not have started. Look at the rows, then try again.`);
    draw(true);
  };
  const update = (st, agent) => {
    const rows = rowsOf(st).filter(r => (agent ? r.agent === agent : updatable(r)) && !busy(st, r));
    if (!rows.length) return;
    return ask(st, rows.map(r => r.agent), `Asking ${st} to update it…`, 'agents.update', agent ? { agent } : {});
  };
  const wayOf = (st, r) => {
    const ways = r.methods || [];
    return ways.find(m => m.id === picked.get(key(st, r.agent)) && m.available) || ways.find(m => m.id === r.method) || ways.find(m => m.available);
  };
  const install = (st, r) => {
    const way = wayOf(st, r), again = healthOf(r) !== 'missing';
    if (!way || busy(st, r)) return;
    dialog(`${again ? 'Reinstall' : 'Install'} ${r.name} on ${st}?`, (shut) => h('div', {},
      h('p', {}, `This runs on ${st}, and is remembered as the way ${r.name} is installed there:`),
      h('div', { class: 'note mono' }, way.command),
      h('p', { class: 'muted' }, 'Whether it worked is judged by the program answering afterwards. Agents that are running are not restarted.'),
      h('div', { class: 'row end' }, h('button', { onclick: shut }, 'Cancel'),
        h('button', { class: 'primary', onclick: () => { shut(); ask(st, [r.agent], `Asking ${st} to ${again ? 'reinstall' : 'install'} it…`, 'agents.install', { agent: r.agent, method: way.id }); } }, again ? 'Reinstall' : 'Install'))));
  };

  // ---- one card: three lines, the same in every card ----
  const version = (r) => {
    const health = healthOf(r);
    if (health === 'broken') return h('span', { class: 'chip bad' }, 'Installed but not working');
    if (health === 'no_answer') return h('span', { class: 'warn-text', title: 'It was asked its version and did not answer in time. It was left running, not stopped. This is not known to be a fault.' }, r.reading ? 'reading its version…' : 'did not answer when asked its version');
    if (health === 'missing') return h('span', { class: 'muted' }, 'not installed');
    return h('span', {}, r.version || 'version not read');
  };
  const latestMark = (st, r) => {
    const l = r.latest;
    if (!l || healthOf(r) === 'missing') return null;
    const when = l.checked_ms ? `Checked ${ago(l.checked_ms)} ago${l.asked ? ', asking ' + l.asked : ''}.` : '';
    if (l.state === 'latest') return h('span', { class: 'ap-mark ap-latest', title: `${when} ${l.version && l.version !== r.version ? `The newest published is ${l.version}.` : 'Nothing newer is published.'}` }, '✓ latest');
    if (l.state === 'available') return h('span', { class: 'ap-mark ap-available', title: when }, `${l.version} available`);
    if (l.checking || l.state === 'not_checked') return h('span', { class: 'ap-mark muted' }, l.checking ? 'checking for a newer one…' : 'not checked yet');
    if (l.state === 'off') return h('span', { class: 'ap-mark muted', title: `[agents] check_latest = false in ${st}'s config.toml: it asks no release host.` }, 'checks are off');
    return h('span', { class: 'ap-mark ap-unknown', title: when }, `could not check: ${l.why || 'no reason given'}`);
  };
  const lastMark = (st, r, quiet) => {
    if (!quiet) return null;
    const j = r.job;
    if (j && j.state !== 'running' && j.ended_ms) return h('span', { class: 'ap-mark muted', title: apJobWords(r) || '' }, `${j.outcome === 'installed' ? 'installed' : j.outcome === 'updated' ? 'updated' : 'checked by its updater'} ${ago(j.ended_ms)} ago`);
    // A station from before: what its log says.
    if (!j && r.last_update) return h('span', { class: r.last_update.ok ? 'ap-mark muted' : 'ap-mark warn-text', title: r.last_update.message || '' }, r.last_update.ok ? `updated ${ago(r.last_update.at_ms)} ago` : 'the last update failed');
    return null;
  };
  const status = (st, r) => {
    const k = key(st, r.agent), j = r.job, out = [];
    const health = healthOf(r);
    if (health === 'broken') {
      const way = wayOf(st, r);
      out.push(h('div', { class: 'agent-problem' },
        h('div', { class: 'muted' }, 'Found at ', h('span', { class: 'mono' }, r.path || ''), '. When started it said:'),
        h('div', { class: 'mono warn-text' }, r.problem || 'nothing'),
        way ? h('div', { class: 'muted' }, 'Reinstall runs ', h('span', { class: 'mono' }, way.command)) : h('div', { class: 'muted' }, 'spandeck knows no install command for it.')));
    }
    if (asked.has(k)) out.push(h('div', { class: 'ap-job running' }, h('span', { class: 'ap-spin' }), h('span', {}, asked.get(k))));
    else if (running(st, r)) {
      out.push(h('div', { class: 'ap-job running' }, h('span', { class: 'ap-spin' }),
        h('span', {}, apJobWords(r) || (r.installing ? 'Installing…' : 'Updating…'), j?.started_ms ? h('span', { class: 'muted' }, ` · started ${ago(j.started_ms)} ago${j.by ? ' by ' + j.by : ''}`) : null)));
      if (j?.last_line) out.push(h('div', { class: 'ap-line mono' }, j.last_line));
    } else if (j && j.ended_ms && !quietJob(j)) {
      const kind = j.outcome === 'failed' ? 'failed' : j.outcome === 'interrupted' ? 'unsure' : 'good';
      out.push(h('div', { class: 'ap-job ' + kind }, h('span', {}, apJobWords(r)), h('span', { class: 'muted' }, ` · ${ago(j.ended_ms)} ago`)));
      // What it printed is behind a tap when it did not end well.
      if (kind !== 'good' && (j.output || '').trim()) out.push(h('details', { class: 'ap-output', open: shown.has(k), ontoggle: (e) => { e.target.open ? shown.add(k) : shown.delete(k); } },
        h('summary', {}, 'Show what it said'), h('pre', { class: 'mono' }, j.output)));
    }
    if (errs.has(k)) out.push(h('div', { class: 'ap-job failed' }, errs.get(k)));
    return out;
  };
  // A result stays on its row while it is news: ten minutes, or since this
  // dialog was opened; a failure stays until something else is done.
  const quietJob = (j) => !['failed', 'interrupted'].includes(j.outcome) && j.ended_ms < openedAt && Date.now() - j.ended_ms > AP_RECENT_MS;
  const row = (st, r) => {
    const health = healthOf(r), ways = r.methods || [], way = wayOf(st, r), off = busy(st, r);
    const available = r.latest?.state === 'available';
    const quiet = !asked.has(key(st, r.agent)) && !running(st, r) && (!r.job || quietJob(r.job));
    const updating = running(st, r) ? (r.job ? r.job.action === 'update' : !!r.updating) : /update/.test(asked.get(key(st, r.agent)) || '');
    const button = (label, cls, on, title) => h('button', { class: cls, disabled: off, 'aria-disabled': off ? 'true' : 'false', 'aria-busy': off ? 'true' : null, title, onclick: on }, label);
    return h('div', { class: 'note agent-program', 'data-agent': r.agent, 'data-health': health, 'data-busy': off ? '1' : null, 'data-available': available ? '1' : null },
      h('div', { class: 'ap-line1' },
        paneIcon(r.agent), h('b', { class: 'ap-name' }, r.name), h('span', { class: 'ap-version' }, version(r)),
        latestMark(st, r),
        r.panes ? h('span', { class: 'ap-mark muted ap-running', title: AP_RULE }, `${r.panes} running: ${r.panes === 1 ? 'it restarts' : 'they restart'} after an update`) : null,
        lastMark(st, r, quiet)),
      h('div', { class: 'ap-status' }, status(st, r)),
      h('div', { class: 'ap-line2' },
        h('span', { class: 'ap-label muted' }, health === 'missing' ? 'Install with' : 'Installed with'),
        ways.length > 1 ? h('select', { class: 'ap-way', 'aria-label': `How ${r.name} is installed on ${st}`, title: way ? way.command : '', disabled: off,
          onchange: (e) => { picked.set(key(st, r.agent), e.target.value); draw(true); } },
          ways.map(m => h('option', { value: m.id, selected: way && m.id === way.id, disabled: !m.available }, m.available ? m.label : `${m.label} (not on this station)`)))
          : h('span', { class: 'ap-way', title: way ? way.command : '' }, way ? way.label : 'no way known')),
      h('div', { class: 'ap-line3' },
        way && health !== 'missing' ? button('Reinstall', health === 'broken' ? 'primary' : 'quiet', () => install(st, r), way.command) : null,
        way && health === 'missing' ? button('Install', 'primary', () => install(st, r), way.command) : null,
        updatable(r) ? button(updating ? 'Updating…' : 'Update', available ? 'primary ap-update' : 'ap-update', () => update(st, r.agent), r.update_command ? `Runs ${r.update_command} on ${st}` : '') : null));
  };

  // ---- "Update all here": the newest batch of a station's jobs ----
  const batchLine = (st) => {
    const rows = rowsOf(st);
    const pending = rows.filter(r => asked.has(key(st, r.agent)));
    const inBatch = rows.filter(r => r.job?.batch);
    const newest = inBatch.reduce((m, r) => (!m || r.job.started_ms > m.started_ms ? r.job : m), null);
    const out = [];
    if (errs.has(st)) out.push(h('div', { class: 'ap-job failed' }, errs.get(st)));
    if (pending.length > 1) { out.push(h('div', { class: 'ap-job running' }, h('span', { class: 'ap-spin' }), `Asking ${st} to update ${apPlural(pending.length, 'program')}…`)); return out; }
    if (!newest) return out;
    const mine = inBatch.filter(r => r.job.batch === newest.batch);
    const done = mine.filter(r => r.job.state !== 'running');
    if (done.length < mine.length) { out.push(h('div', { class: 'ap-job running' }, h('span', { class: 'ap-spin' }), `Updating ${apPlural(mine.length, 'program')} on ${st}… ${done.length} of ${mine.length}`)); return out; }
    const ended = Math.max(...mine.map(r => r.job.ended_ms || 0));
    if (ended < openedAt && Date.now() - ended > AP_RECENT_MS) return out;
    const n = (o) => mine.filter(r => r.job.outcome === o);
    const bad = mine.filter(r => !['updated', 'latest'].includes(r.job.outcome));
    const parts = [];
    if (n('updated').length) parts.push(`${n('updated').length} updated`);
    if (n('latest').length) parts.push(`${n('latest').length} already the latest`);
    if (bad.length) parts.push(`${bad.length} failed: ${bad.map(r => r.name).join(', ')}`);
    out.push(h('div', { class: 'ap-job ' + (bad.length ? 'failed' : 'good') }, parts.join(', '), h('span', { class: 'muted' }, ` · ${ago(ended)} ago`)));
    return out;
  };
  const station = (st) => {
    const v = view.get(st), rows = rowsOf(st), info = S.fleet.stations.find(s => s.station === st);
    const any = rows.some(r => busy(st, r));
    return h('div', { class: 'ap-station', 'data-station': st },
      h('div', { class: 'ap-head' }, h('h3', {}, st), h('span', { class: 'grow' }),
        rows.some(updatable) ? h('button', { class: 'ap-all', disabled: any, 'aria-disabled': any ? 'true' : 'false', onclick: () => update(st) }, any ? 'Updating…' : 'Update all here') : null),
      h('div', { class: 'ap-batch' }, batchLine(st)),
      v.problem ? h('p', { class: 'warn-text ap-problem' }, rows.length ? `${v.problem}. Showing what it last said.` : v.problem) : null,
      !v.rows && !v.problem ? h('p', { class: 'muted' }, `Asking ${st}…`) : null,
      v.rows && !knows(st) ? h('p', { class: 'muted ap-old' }, `${st} runs spandeck ${info?.version || 'from before'}: it cannot say whether a version is the latest, and shows no progress while it updates. Update spandeck there to get both.`) : null,
      h('div', { class: 'list' }, rows.map(r => row(st, r))));
  };

  // ---- the whole fleet in a line ----
  const fleetLine = () => {
    const rows = stations.flatMap(st => rowsOf(st).filter(r => healthOf(r) !== 'missing').map(r => ({ st, r })));
    const of = (state) => rows.filter(x => x.r.latest?.state === state);
    const avail = of('available'), unknown = of('unknown'), off = [...new Set(of('off').map(x => x.st))];
    const old = stations.filter(st => view.get(st).rows && !knows(st));
    const checking = rows.some(x => x.r.latest?.checking), waiting = stations.some(st => !view.get(st).rows && !view.get(st).problem);
    const any = stations.some(st => rowsOf(st).some(r => busy(st, r)));
    const checked = rows.map(x => x.r.latest?.checked_ms).filter(Boolean);
    let words, cls = 'muted';
    if (avail.length) { words = `${apPlural(avail.length, 'update')} available on ${apPlural(new Set(avail.map(x => x.st)).size, 'station')}`; cls = 'ap-available'; }
    else if (waiting || checking) words = 'Checking for newer versions…';
    else if (rows.length && of('latest').length === rows.length) { words = 'Everything is the latest'; cls = 'ap-latest'; }
    else if (of('latest').length) words = `${of('latest').length} of ${rows.length} are the latest`;
    else words = 'Whether these are the latest is not known';
    const notes = [];
    if (unknown.length) notes.push(`${unknown.length} could not be checked`);
    if (off.length) notes.push(`checks are off on ${off.join(', ')}`);
    if (old.length) notes.push(`${old.join(', ')} cannot say (older spandeck)`);
    if (checked.length && !checking) notes.push(`checked ${ago(Math.min(...checked))} ago`);
    return h('div', { class: 'ap-fleet' },
      h('div', { class: 'ap-fleet-words' }, h('b', { class: cls }, words), notes.length ? h('span', { class: 'muted' }, ' · ' + notes.join(' · ')) : null),
      h('div', { class: 'ap-fleet-actions' },
        h('button', { class: 'quiet ap-check', disabled: checking, onclick: () => stations.forEach(st => check(st, true)) }, 'Check again'),
        h('button', { class: (avail.length ? 'primary ' : '') + 'ap-fleet-all', disabled: any, 'aria-disabled': any ? 'true' : 'false', title: 'Each station updates its own programs', onclick: () => stations.forEach(st => update(st)) }, any ? 'Updating…' : 'Update all')));
  };

  // What each program does about updates without being asked, as its
  // station's catalogue has it from the vendor's own pages.
  const byItself = () => {
    const said = new Map();
    for (const st of stations) for (const r of rowsOf(st)) if (r.by_itself && !said.has(r.name)) said.set(r.name, r.by_itself);
    if (!said.size) return null;
    return h('details', { class: 'ap-itself', open: shown.has('itself'), ontoggle: (e) => { e.target.open ? shown.add('itself') : shown.delete('itself'); } },
      h('summary', {}, 'What each program does about updates by itself'),
      [...said].map(([name, text]) => h('p', { class: 'small' }, h('b', {}, name + ': '), text)));
  };

  const { close } = dialog('Agent programs', (shut) => {
    const body = h('div', { class: 'agent-programs' });
    draw = (force) => {
      if (!open) return;
      // Drawn again only when something to show has changed: a select that
      // is open, or text being read, is not replaced under the finger
      // every second. Times ("2 min ago") move on with the next change.
      const now = JSON.stringify([[...view], [...asked], [...errs], [...picked], Math.floor(Date.now() / 20000)]);
      if (now === drawn && !force) return;
      drawn = now;
      const at = body.closest('.dialog-body')?.scrollTop;
      body.replaceChildren(
        fleetLine(),
        h('p', { class: 'muted small ap-rule' }, 'An update restarts that program\'s agents and they go back to their conversations; an agent in the middle of a turn is restarted when it finishes, not interrupted. Starting an agent never checks for or installs an update. A program that is not installed, or does not start, is installed the first time you start a pane for it, or here.'),
        ...stations.map(station),
        byItself(),
        h('div', { class: 'row end' }, h('button', { onclick: shut }, 'Close')));
      if (at !== undefined) body.closest('.dialog-body').scrollTop = at;
    };
    draw();
    return body;
  }, { wide: true, onClose: () => { open = false; for (const t of timers.values()) clearTimeout(t); } });
  // Each station by itself; and each is asked whether its programs are the
  // newest (it reuses an answer under three hours old).
  for (const st of stations) look(st).then(() => check(st, false));
  return close;
}

// The permission mode of each program, in Agent setup: how far it goes
// without asking the owner (`permission_mode` of the station's setup.rs,
// which passes it when a pane starts). Codex's three are the ones the
// station takes for it (`validate`), with what each means (`launch_args`).
// Claude Code and Antigravity are passed the word as it is
// (`--permission-mode`, `--mode`) and nothing in spandeck lists their
// modes: none is made up here, the owner types the program's own word.
const PERMISSION = {
  codex: { choices: [
    ['on-request', 'Ask me', 'Codex asks you before a step that needs your say.'],
    ['auto', 'Review for itself', 'Codex\'s own reviewer decides what it would otherwise ask you, inside its sandbox for the folder. You are not asked.'],
    ['never', 'Never ask', 'Codex never asks you: a step that fails or is not allowed goes back to the model, which carries on by itself.']] },
  claude: { flag: '--permission-mode' },
  antigravity: { flag: '--mode' },
};
// What the station takes as a setting's value (setup.rs, `validate`).
const SETTING_WORD = /^[A-Za-z0-9._[\]-]+$/;
const permTyped = new Set();

// Tools for agents (tools.js): added once, set up by each station chosen.
const openTools = () => toolsPanel({ api, hasCap, stations: () => S.fleet.stations });
// Roles for agents (roles.js): what each kind of agent is asked to be.
const openRoles = () => rolesPanel({ api, hasCap });

async function setupPanel() {
  let view;
  try { view = await api.call('setup.get'); } catch (e) { return toast(e.message, 'error'); }
  // What the owner chose about folder trust, on each station that answers.
  const trustLists = new Map();
  const loadTrust = () => Promise.all(onlineStations().filter(st => hasCap(st, 'folder_trust')).map(st =>
    api.on(st, 'foldertrust.list', {}).then(l => trustLists.set(st, l)).catch(() => trustLists.delete(st))));
  await loadTrust();
  // And whether each station answers a review of spandeck's own hooks.
  const hooksOn = new Map();
  const loadHooks = () => Promise.all(onlineStations().filter(st => hasCap(st, 'hooks_trust')).map(st =>
    api.on(st, 'hookstrust.choice', {}).then(c => hooksOn.set(st, c)).catch(() => hooksOn.delete(st))));
  await loadHooks();
  const setup = view.setup;
  setup.mcp_servers ||= {}; setup.skills ||= {}; setup.defaults ||= {};
  dialog('Agent setup', (close, body) => {
    const draw = () => body.replaceChildren(content());
    // The permission mode of one program (PERMISSION). A program whose
    // modes spandeck knows is given them as a choice, each with what it
    // means; another is given its own setting or a word typed as the
    // program spells it, checked as the station checks it. `permTyped`:
    // the programs whose box is open, kept while the dialog is drawn again.
    const permRow = (a) => {
      const known = PERMISSION[a.agent];
      if (!known) return null;
      const now = () => setup.defaults[a.agent]?.permission_mode || '';
      const put = (v) => { setup.defaults[a.agent] ||= {}; setup.defaults[a.agent].permission_mode = v || null; };
      const says = h('small', { class: 'perm-says' });
      const NOT_SET = `Not set (${a.name}'s own setting)`;
      if (known.choices) {
        // (A word kept from before, which is none of the program's: shown, so that it can be changed.)
        const odd = now() && !known.choices.some(c => c[0] === now()) ? [[now(), `${now()} (set before; not one of ${a.name}'s)`, `The station refuses this word for ${a.name}: choose another.`]] : [];
        const all = [['', NOT_SET, `spandeck passes nothing: ${a.name} uses its own setting.`], ...known.choices, ...odd];
        const tell = () => says.replaceChildren(...all.map(c => h('span', { class: c[0] === now() ? 'on' : '' }, h('b', {}, c[1] + ': '), c[2])));
        tell();
        return h('div', { class: 'perm-row', dataset: { agent: a.agent } }, h('label', { class: 'field' }, h('span', {}, a.name),
          h('select', { 'aria-label': `${a.name} permission mode`, onchange: (e) => { put(e.target.value); tell(); } }, all.map(c => h('option', { value: c[0], selected: c[0] === now() }, c[1])))), says);
      }
      const box = h('input', { 'aria-label': `${a.name} permission mode, as ${a.name} spells it`, placeholder: `a mode, as ${a.name} spells it`, value: now(), autocapitalize: 'off', spellcheck: false,
        oninput: (e) => { put(e.target.value.trim()); tell(); } });
      const pick = h('select', { 'aria-label': `${a.name} permission mode`, onchange: (e) => { if (e.target.value) permTyped.add(a.agent); else { permTyped.delete(a.agent); box.value = ''; put(''); } tell(); } },
        [['', NOT_SET], ['typed', 'A mode I type']].map(([v, l]) => h('option', { value: v, selected: (v === 'typed') === (!!now() || permTyped.has(a.agent)) }, l)));
      const tell = () => {
        const typed = pick.value === 'typed', bad = typed && now() && !SETTING_WORD.test(now());
        box.hidden = !typed;
        box.setAttribute('aria-invalid', String(!!bad));
        says.className = 'perm-says' + (bad ? ' warn-text' : '');
        says.textContent = bad ? 'Letters, digits and - _ . [ ] only: the station refuses anything else.'
          : !typed ? `spandeck passes nothing: ${a.name} uses its own setting.`
            : `Passed to ${a.name} as "${known.flag} ${now() || '…'}" when a pane starts. spandeck does not know ${a.name}'s list of modes: type one exactly as ${a.name} names it. A word it does not know may stop it from starting.`;
      };
      tell();
      return h('div', { class: 'perm-row', dataset: { agent: a.agent } }, h('label', { class: 'field' }, h('span', {}, a.name), pick), box, says);
    };
    const save = async () => {
      const badMode = Object.entries(setup.defaults).find(([, d]) => d?.permission_mode && !SETTING_WORD.test(d.permission_mode));
      if (badMode) return toast(`${badMode[0]}: "${badMode[1].permission_mode}" is not a permission mode the station takes (letters, digits and - _ . [ ] only)`, 'error');
      const out = await rpc(api.station, 'setup.set', { setup, fleet: true });
      // A station that kept its own, newer setup did not take this one, and
      // its copy replaces it everywhere: that is not "Saved".
      const kept = out.stations.filter(x => x.kept), bad = out.stations.filter(x => !x.ok && !x.kept);
      if (kept.length) toast(`Not saved: ${kept.map(x => x.station).join(', ')} had a newer agent setup, which is the one in use now. Open Agent setup again and make your change there.`, 'error');
      else toast(bad.length ? `Saved. Not yet on: ${bad.map(x => x.station).join(', ')} (they catch up when they are back)` : `Saved on ${out.stations.length} station${out.stations.length === 1 ? '' : 's'}`);
      const problems = out.stations.flatMap(x => x.problems || []);
      if (problems.length) toast(problems.join(' · '), 'error');
      close();
    };
    const agentsBox = (item) => h('select', { 'aria-label': 'Which agents', onchange: (e) => { item.agents = e.target.value ? [e.target.value] : []; } },
      [['', 'All agents'], ['claude', 'Claude Code'], ['codex', 'Codex']].map(([v, l]) => h('option', { value: v, selected: (item.agents?.[0] || '') === v }, l)));
    const content = () => {
      const mName = h('input', { placeholder: 'name', 'aria-label': 'Server name' });
      const mCmd = h('input', { placeholder: 'npx -y some-mcp-server   or   https://mcp.example.com', 'aria-label': 'Command or URL' });
      const sName = h('input', { placeholder: 'name', 'aria-label': 'Skill name' });
      const sDesc = h('input', { placeholder: 'When the agent should use it (one line)', 'aria-label': 'Skill description' });
      const sBody = h('textarea', { rows: 5, placeholder: 'The instructions (the body of SKILL.md)', 'aria-label': 'Skill content' });
      const def = (agent, key, placeholder) => h('input', { placeholder, value: setup.defaults[agent]?.[key] || '', 'aria-label': `${agent} ${key}`,
        oninput: (e) => { setup.defaults[agent] ||= {}; setup.defaults[agent][key] = e.target.value.trim() || null; } });
      return h('div', {},
        h('p', { class: 'muted' }, `One list for every station${setup.updated_by ? ` · last changed by ${setup.updated_by}` : ''}. New agent panes pick it up when they start.`),
        h('h3', {}, 'MCP servers'),
        h('p', { class: 'muted small setup-tools' }, 'These are given to every agent on every station, and are listed in Tools too. To have a tool installed for you, kept to some stations, or upgraded: ',
          h('button', { type: 'button', class: 'mini', onclick: () => { close(); openTools(); } }, 'Open Tools')),
        h('table', { class: 'table' }, h('tbody', {}, Object.entries(setup.mcp_servers).map(([name, sv]) => h('tr', {},
          h('td', {}, h('b', {}, name)), h('td', { class: 'mono' }, sv.url || [sv.command, ...(sv.args || [])].join(' ')), h('td', {}, agentsBox(sv)),
          h('td', {}, h('button', { class: 'mini danger', onclick: () => { delete setup.mcp_servers[name]; draw(); } }, 'Remove')))),
          h('tr', {}, h('td', {}, mName), h('td', {}, mCmd), h('td', {}), h('td', {}, h('button', { class: 'mini', onclick: () => {
            const name = mName.value.trim(), line = mCmd.value.trim();
            if (!name || !line) return toast('Give the server a name and a command or URL', 'error');
            if (/^https?:\/\//.test(line)) setup.mcp_servers[name] = { url: line, agents: [] };
            else { const parts = line.split(/\s+/); setup.mcp_servers[name] = { command: parts[0], args: parts.slice(1), env: {}, agents: [] }; }
            draw();
          } }, 'Add'))))),
        h('h3', {}, 'Skills'),
        h('table', { class: 'table' }, h('tbody', {}, Object.entries(setup.skills).map(([name, sk]) => h('tr', {},
          h('td', {}, h('b', {}, name)), h('td', {}, sk.description), h('td', {}, agentsBox(sk)),
          h('td', {}, h('button', { class: 'mini danger', onclick: () => { delete setup.skills[name]; draw(); } }, 'Remove')))))),
        h('div', { class: 'slot', style: { gridTemplateColumns: '1fr 2fr auto' } }, sName, sDesc, h('button', { class: 'mini', onclick: () => {
          const name = sName.value.trim();
          if (!name || !sDesc.value.trim() || !sBody.value.trim()) return toast('A skill needs a name, a description and content', 'error');
          setup.skills[name] = { description: sDesc.value.trim(), content: sBody.value, agents: [] };
          draw();
        } }, 'Add')), sBody,
        h('h3', {}, 'Defaults for new agent panes'),
        h('table', { class: 'table' },
          h('thead', {}, h('tr', {}, h('th', {}, 'Agent'), h('th', {}, 'Model'), h('th', {}, 'Effort'), h('th', {}, 'Installed here'), h('th', {}, 'Running here'))),
          h('tbody', {}, view.agents.map(a => h('tr', {}, h('td', {}, a.name), h('td', {}, def(a.agent, 'model', 'default')), h('td', {}, def(a.agent, 'effort', 'default')),
            h('td', { class: 'muted' }, a.installed_version || ({ missing: 'not installed', broken: 'installed but not working', no_answer: 'installed, version not known' })[a.health] || 'installed, version not read'),
            h('td', {}, a.running.length ? a.running.map(r => h('span', { class: 'chip ' + (r.state === 'needs_you' ? 'warn' : ''), title: r.needs_reason || '' }, r.name)) : h('span', { class: 'muted' }, 'none')))))),
        h('p', { class: 'muted small' }, 'Agents that are already running keep their settings. To change them now, use "Run a command in agents" (for example /model opus).'),
        h('h3', {}, 'Permission mode for new agent panes'),
        h('p', { class: 'muted small perm-rule' }, 'How far each program goes without asking you. It applies to panes started from now on, on every station of the fleet; panes that are running keep what they were started with. A station running an older spandeck may ignore it.'),
        h('div', { class: 'perm-all' }, view.agents.map(permRow)),
        h('h3', {}, 'Folders trusted for agents'),
        h('p', { class: 'muted small' }, 'Where you ticked "Trust this folder" when starting a pane: if that program asks about that folder again, spandeck answers yes for you. Each station has its own list. Removing one stops spandeck answering; what the program recorded for itself is its own.'),
        h('div', { class: 'trusted-all' }, [...trustLists].map(([st, list]) => h('div', {},
          h('div', { class: 'muted small' }, `Folders trusted for agents on ${st}`),
          trustedFolders(st, list, () => loadTrust().then(draw)),
          (list.projects || []).length ? h('p', { class: 'muted small' }, 'Projects whose agents start without asking: ',
            list.projects.map(p => h('button', { class: 'mini', title: 'Open this project\'s folder trust', onclick: () => folderTrustDialog(st, p.project) }, p.project))) : null))),
        hooksOn.size ? h('h3', {}, 'Trust spandeck\'s own hooks when an agent asks') : null,
        hooksOn.size ? h('p', { class: 'muted small' }, `${[...hooksOn.values()][0].programs.map(p => p.agent_name).join(', ')} will not run a hook until you have reviewed it, and asks again whenever spandeck's hooks change. On: spandeck chooses "Trust all and continue" on that screen for you, once, and only when every hook the program can be asking about is spandeck's own; otherwise nothing is pressed and the pane says why. Each station has its own switch, kept by that station.`) : null,
        h('div', { class: 'hooks-all' }, [...hooksOn].map(([st, c]) => h('label', { class: 'row hooks-switch' },
          h('input', { type: 'checkbox', checked: c.on !== false, style: { width: 'auto' }, 'aria-label': `Trust spandeck's own hooks on ${st}`,
            onchange: async (e) => { try { await rpc(st, 'hookstrust.set', { on: e.target.checked }); } catch (er) { /* said by rpc */ } await loadHooks(); draw(); } }),
          h('span', {}, `${st}: ${c.on !== false ? 'on' : 'off'}`),
          c.answers === false ? h('span', { class: 'muted small' }, ' · answers nothing here: this station does not keep spandeck\'s hooks in the agent settings') : null))),
        h('div', { class: 'row end' },
          h('button', { onclick: () => { close(); commandDialog(api.station, null); } }, 'Run a command in agents…'),
          h('span', { class: 'grow' }), h('button', { onclick: close }, 'Cancel'), h('button', { class: 'primary', onclick: save }, 'Save for every station')));
    };
    return content();
  }, { wide: true });
}

// ------------------------------------------------------------------- vault
// AgentVault. The passphrase is typed only into the vault's own page, which
// opens in a small pop-up showing the vault's address. Never into this page.

const VAULT_LABEL = { connected: 'connected', not_connected: 'not connected', locked: 'vault locked', unreachable: 'vault unreachable', token_rejected: 'token refused' };

// What the station's last check found wrong, in its own sentence.
const vaultBroken = (v) => (v?.checks || []).find(c => !c.ok);

function vaultChip(s) {
  const v = s.vault;
  if (!v || !v.found) return null;
  const broken = v.health === 'problem' ? vaultBroken(v) : null;
  const cls = v.state === 'connected' && !broken ? 'ok' : v.state === 'not_connected' ? '' : 'warn';
  const label = v.state === 'connected' && broken ? 'needs a look' : VAULT_LABEL[v.state] || v.state;
  return h('span', { class: 'chip vault-chip ' + cls, title: broken?.detail || v.problem || 'AgentVault' }, 'vault: ' + label);
}

// Ask every station to check its vault now. `opened` is this page saying it
// has just opened: a station then checks at most once a minute.
const vaultCheckAll = (opened) => Promise.all(onlineStations().map(st =>
  api.on(st, 'vault.check', opened ? { opened: true } : {}).then(v => ({ st, v })).catch((e) => { if (!opened) toast(`${st}: ${e.message}`, 'error'); return null; })))
  .then(all => all.filter(Boolean));

// When the Control Deck opens: each station checks its vault and repairs what
// it can. A station that found a vault and has no working token is offered
// once, in a bar that blocks nothing. "Not now" is kept by the station.
let vaultAsked = false;
async function vaultAtOpen() {
  if (vaultAsked) return;
  vaultAsked = true;
  const want = (await vaultCheckAll(true)).filter(x => x.v && x.v.prompt);
  if (want.length && !isPhone()) vaultPrompt(want.map(x => x.st), want[0].v.url);
  // Nothing in the deck reads this; a test waits on it to know the question was settled.
  document.body.dataset.vaultAsked = '1';
  refreshSoon();
}

let vaultPromptBox = null;
function vaultPrompt(stations, url) {
  const who = stations.length === 1 ? `${stations[0]} is` : `${stations.slice(0, -1).join(', ')} and ${stations[stations.length - 1]} are`;
  // The bar is part of the page, between the top bar and the view, so it
  // covers nothing: render() puts it there.
  const gone = () => { vaultPromptBox = null; render(); };
  vaultPromptBox = h('div', { class: 'vault-prompt', role: 'region', 'aria-label': 'Connect to your vault', dataset: { stations: stations.join(',') } },
    h('div', { class: 'grow' },
      h('b', {}, 'Your AgentVault is on the network'),
      h('div', { class: 'muted' }, `${who} not connected to the vault at ${url}, so agents there cannot use your keys. You approve once, on the vault's own page.`)),
    h('button', { class: 'primary', onclick: () => { gone(); vaultConnect(stations); } }, stations.length > 1 ? 'Connect all' : 'Connect'),
    h('button', { onclick: () => {
      gone();
      Promise.all(stations.map(st => api.on(st, 'vault.prompt', { dismissed: true }).catch(() => {}))).then(() => refreshSoon());
      toast('You can connect at any time from the Vault screen.');
    } }, 'Not now'));
  render();
}
// ---- one version for the fleet ----
// The Control Deck is the serving station's own copy of it, so a station on
// an older spandeck serves an older deck, and what the fleet keeps the same
// is only kept so by stations that know how. When the stations do not all
// run the same version the deck says so, in a bar that blocks nothing, with
// the update each station already offers in its menu.
const versionOf = (v) => String(v || '').split(/[.+-]/).slice(0, 3).map(n => parseInt(n, 10) || 0);
const olderThan = (a, b) => { const x = versionOf(a), y = versionOf(b); for (let i = 0; i < 3; i += 1) if (x[i] !== y[i]) return x[i] < y[i]; return false; };
let versionBox = null, versionKey = '', versionLater = '', releaseLater = '';
// A newer spandeck than a station runs is published (newrelease.rs): the
// newest release any station of the fleet learned of, as the serving station
// says it. What it says is data: the version is shown, as text, only if it
// is one, and the link is used only if it is a web address (the station
// made it from its own release address, never from an answer).
const RELEASE_LATER = 'spandeck.update.later';
function releaseOut() {
  const u = S.fleet.update || {};
  if (u.state !== 'available' || !/^\d{1,5}(\.\d{1,5}){1,3}(-[0-9A-Za-z.]{1,20})?$/.test(u.version || '')) return null;
  return { version: u.version, url: /^https?:\/\/[^\s"'<>]+$/.test(u.url || '') ? u.url : '', behind: Array.isArray(u.behind) ? u.behind.map(String) : [] };
}
const releaseLink = (out) => out.url ? h('a', { class: 'out', href: out.url, target: '_blank', rel: 'noopener' }, `spandeck ${out.version}`) : `spandeck ${out.version}`;
const releaseLaterIs = (version) => { if (releaseLater === version) return true; try { return localStorage.getItem(RELEASE_LATER) === version; } catch (e) { return false; } };
const updateStations = (can) => Promise.all(can.map(s => api.on(s.station, 'station.update', {}).then((r) => toast(r.current ? `${s.station} already runs the latest release` : r.fleet || r.begun ? r.note : `${s.station} is updating to ${r.release}`)).catch(e => toast(`${s.station}: ${e.message}`, 'error')))).then(() => refreshSoon(1500));
const askUpdate = (can) => confirmBox(`Update ${can.map(s => s.station).join(', ')}?`, `The latest release is downloaded on ${can.length === 1 ? 'that station' : 'each of them'} and checked against its published checksum. Terminals keep running, and a version that does not start properly is undone.`, 'Update', () => updateStations(can), false);
// "Check for a newer spandeck now": every station that answers asks its
// own release host, and the fleet's list is read again when they have.
function checkReleaseNow() {
  const on = S.fleet.stations.filter(s => s.state === 'online' && s.update);
  if (!on.length) return toast('No station here can ask: update spandeck on one of them first.');
  toast('Asking whether a newer spandeck is published…');
  const said = () => { const u = S.fleet.update || {}, out = releaseOut(); toast(out ? `spandeck ${out.version} is available` : u.state === 'latest' ? 'Every station runs the latest release' : `Whether a newer spandeck is published: ${String(u.says || 'not known yet').slice(0, 240)}`); render(); };
  return Promise.all(on.map(s => api.on(s.station, 'update.check', { again: true }).catch(() => null)))
    .then(() => setTimeout(() => api.on(api.station, 'fleet.refresh', {}).catch(() => null).then(() => askFleet()).then(said, said), 3000));
}
function versionBar() {
  const all = S.fleet.stations.filter(s => s.state !== 'not_linked' && s.version && s.version !== '?');
  const newest = all.reduce((n, s) => olderThan(n, s.version) ? s.version : n, '0');
  const behind = all.filter(s => olderThan(s.version, newest));
  const out = releaseOut(), outKey = out ? `+${out.version}|${out.url}|${out.behind.join(',')}` : '';
  const differ = behind.map(s => `${s.station}=${s.version}/${s.state}`).join(' ') + '>' + newest + '@' + api.station + outKey;
  if (behind.length && differ !== versionLater) {
    if (differ === versionKey) return versionBox;
    // The stations that are behind the newest in the fleet, and with a
    // newer release out, every station that is behind that.
    const me = behind.find(s => s.self), can = all.filter(s => s.state === 'online' && (behind.includes(s) || (out && out.behind.includes(s.station)))), names = (list) => list.map(s => `${s.station} (${s.version})`).join(', ');
    const on = all.filter(s => s.version === newest).map(s => s.station).join(', ');
    versionKey = differ;
    versionBox = h('div', { class: 'vault-prompt version-bar', role: 'region', 'aria-label': 'Stations on different versions' },
      h('div', { class: 'grow' },
        h('b', {}, me ? `This deck is ${api.target ? 'working through' : 'served by'} ${api.station}, which runs an older spandeck (${me.version})` : `${behind.length === 1 ? 'A station runs' : 'Some stations run'} an older spandeck`),
        h('div', { class: 'muted' }, `${on} ${on.includes(',') ? 'run' : 'runs'} ${newest}. Older: ${names(behind)}. `,
          me ? `What you see here can differ from the deck ${on.includes(',') ? 'those stations serve' : on + ' serves'}, and newer things may be missing. ` : 'A deck served by an older station can look different, and what the fleet keeps the same may not be kept there. ',
          behind.some(s => s.state !== 'online') ? 'A station that is off or cannot be controlled from here is updated at that machine. ' : '',
          out ? h('span', { class: 'release-out' }, releaseLink(out), ' is available: an update takes every station to it.') : null)),
      can.length ? h('button', { class: 'primary', onclick: () => askUpdate(can) }, can.length === 1 ? `Update ${can[0].station}` : `Update ${can.length} stations`) : null,
      h('button', { onclick: () => { versionLater = differ; render(); } }, 'Not now'));
    return versionBox;
  }
  // Every station runs the same version (or that bar was put away), and a
  // newer release is out: one quiet line. On a phone it is not drawn over
  // an open pane, so a conversation is never moved by it.
  const phone = isPhone();
  if (!out || releaseLaterIs(out.version) || (phone && S.route && S.route.view === 'pane')) { versionBox = null; versionKey = ''; return null; }
  const can = all.filter(s => s.state === 'online' && out.behind.includes(s.station));
  // Who is behind and cannot be updated from here is named: a line that
  // offers a release and no button otherwise reads as a fault (21 Nov 2026).
  const away = out.behind.filter(n => !can.some(s => s.station === n)).slice(0, 4);
  const key = `release${outKey}:${can.map(s => s.station).join(',')}:${away.join(',')}:${phone}`;
  if (key === versionKey) return versionBox;
  versionKey = key;
  versionBox = h('div', { class: 'vault-prompt version-bar update-line', role: 'region', 'aria-label': 'A newer spandeck is available' },
    h('div', { class: 'grow', title: can.length < out.behind.length ? 'A station that is off or cannot be controlled from here is updated at that machine.' : null }, releaseLink(out), ' is available.', away.length ? ` ${away.join(", ")} ${away.length === 1 ? 'is' : 'are'} not on it and cannot be reached now: update ${away.length === 1 ? 'it' : 'them'} when ${away.length === 1 ? 'it is' : 'they are'} back.` : ''),
    can.length ? h('button', { class: 'primary update-all', onclick: () => askUpdate(can) }, phone ? 'Update' : can.length === 1 ? `Update ${can[0].station}` : 'Update all stations') : null,
    h('button', { class: 'update-later', onclick: () => { releaseLater = out.version; try { localStorage.setItem(RELEASE_LATER, out.version); } catch (e) { /* held in this page, then */ } render(); } }, 'Not now'));
  return versionBox;
}

// The bar goes once none of its stations needs it: connected from elsewhere, say.
function vaultPromptSync() {
  if (!vaultPromptBox) return;
  const names = vaultPromptBox.dataset.stations.split(',');
  if (!S.fleet.stations.some(s => names.includes(s.station) && s.vault?.prompt)) vaultPromptBox = null;
}

// The one thing that fixes a failed check. `a` is the station's data: its
// label is inserted as text, and only an http(s) address is ever opened.
function vaultAction(st, a) {
  if (!a) return null;
  const run = a.kind === 'connect' ? () => vaultConnect([st])
    : a.kind === 'open' ? () => { if (/^https?:\/\//.test(a.url || '')) window.open(a.url, 'spandeck-vault', 'popup,width=540,height=700'); }
    : () => vaultCheckAll().then(() => refresh());
  return h('button', { class: a.kind === 'connect' ? 'mini primary' : 'mini', onclick: run }, a.label || 'Fix');
}

// One station's chain from an agent to a key, as it last checked it.
function vaultChecks(s) {
  const v = s.vault || {};
  if (s.state !== 'online' || !(v.checks || []).length) return null;
  return h('div', { class: 'vault-checks', dataset: { station: s.station } },
    h('h3', {}, `Checks on ${s.station}`, v.checked_ms ? h('small', { class: 'muted' }, ` · checked ${ago(v.checked_ms)} ago`) : null),
    h('table', { class: 'table' }, h('tbody', {}, v.checks.map(c => h('tr', { class: c.ok ? '' : 'broken' },
      h('td', {}, h('span', { class: 'chip ' + (c.ok ? 'ok' : 'warn') }, c.ok ? 'ok' : 'fix')),
      h('td', {}, c.name),
      h('td', { class: c.ok ? 'muted' : '' }, c.detail),
      h('td', {}, c.ok ? null : vaultAction(s.station, c.action)))))));
}

// Opened at the moment of the click (or the browser blocks it), pointed at
// the vault's approval page once the station has asked for a token.
function openVaultPopup() {
  return window.open('', 'spandeck-vault', 'popup,width=540,height=700');
}

async function vaultConnect(stations) {
  const popup = openVaultPopup();
  let shown = false;
  for (const st of stations) {
    try {
      const out = await api.on(st, 'vault.connect');
      if (!shown && popup) { popup.location.href = out.approve_url; shown = true; }
    } catch (e) { toast(`${st}: ${e.message}`, 'error'); }
  }
  if (!shown && popup) popup.close();
  if (shown && !popup) toast('Your browser blocked the pop-up. Allow pop-ups for this page and try again.', 'error');
  refreshSoon(300);
}

function vaultPanel() {
  dialog('Vault', (close, body) => {
    const draw = () => {
      const stations = S.fleet.stations;
      const vault = stations.map(s => s.vault).find(v => v && v.found);
      const can = (s) => s.state === 'online' && s.vault?.found;
      const unconnected = stations.filter(s => can(s) && ['not_connected', 'token_rejected'].includes(s.vault.state)).map(s => s.station);
      body.replaceChildren(h('div', {},
        h('p', { class: 'muted' }, vault ? `AgentVault at ${vault.url}${vault.version ? ' · ' + vault.version : ''}. Agents use keys through it and never see them. You approve each station once, on the vault's own page.` : 'No AgentVault was found on your tailnet.'),
        h('table', { class: 'table' },
          h('thead', {}, h('tr', {}, h('th', {}, 'Station'), h('th', {}, 'Vault'), h('th', {}, 'Token rotation'), h('th', {}))),
          h('tbody', {}, stations.map(s => {
            const v = s.vault || {};
            const rotation = h('select', { 'aria-label': 'Token rotation for ' + s.station, disabled: v.state !== 'connected',
              onchange: (e) => rpc(s.station, 'vault.rotation', { every_hours: e.target.value ? Number(e.target.value) : null }).then(() => toast(`Rotation for ${s.station} updated`)) },
              [['', 'Never'], ['1', 'Every hour'], ['6', 'Every 6 hours'], ['24', 'Every day'], ['168', 'Every week'], ['720', 'Every 30 days']].map(([val, label]) => h('option', { value: val, selected: String(v.rotation_hours ?? '') === val }, label)));
            return h('tr', {},
              h('td', {}, h('b', {}, s.station)),
              h('td', {}, s.state !== 'online' ? h('span', { class: 'muted' }, 'station ' + s.state.replace('_', ' ')) : vaultChip(s) || h('span', { class: 'muted' }, 'no vault found'),
                v.pending ? h('small', { class: 'muted block' }, 'Waiting for your approval on the vault…') : null,
                v.problem && v.state !== 'connected' ? h('small', { class: 'warn-text block' }, v.problem) : null,
                v.prompt_dismissed && ['not_connected', 'token_rejected'].includes(v.state) ? h('small', { class: 'muted block' }, 'You are not reminded about this when the Control Deck opens. ',
                  h('button', { class: 'mini', onclick: () => rpc(s.station, 'vault.prompt', { dismissed: false }).then(() => toast(`You will be reminded about ${s.station} again`)) }, 'Remind me again')) : null,
                v.state === 'connected' && v.vault_rotation_hours !== v.rotation_hours && v.vault_rotation_hours !== undefined && (v.vault_rotation_hours ?? null) !== (v.rotation_hours ?? null) ? h('small', { class: 'warn-text block' }, 'The vault has a different rotation setting than this one.') : null),
              h('td', {}, rotation, v.next_rotation_ms && v.rotation_hours ? h('small', { class: 'muted block' }, 'next ' + new Date(v.next_rotation_ms).toLocaleString()) : null),
              h('td', {}, !can(s) ? null
                : v.state === 'connected' ? h('button', { class: 'mini', onclick: () => confirmBox(`Disconnect ${s.station}?`, `The station forgets its token. Revoke spandeck:${s.station} on the vault's Tokens page to end it there too.`, 'Disconnect', () => rpc(s.station, 'vault.disconnect').then((r) => toast(r.next))) }, 'Disconnect')
                : v.state === 'locked' || v.state === 'unreachable' ? h('span', { class: 'muted' }, v.state === 'locked' ? 'Unlock the vault first' : '')
                : h('button', { class: 'primary', onclick: () => vaultConnect([s.station]) }, 'Connect')));
          }))),
        stations.map(vaultChecks),
        h('div', { class: 'row end' },
          h('button', { title: 'Each station checks the vault\'s address, its lock, its own token, the keys and what its panes need, and repairs what it can', onclick: (e) => { e.target.disabled = true; vaultCheckAll().then(() => refresh()).then(draw); } }, 'Check again'),
          h('span', { class: 'grow' }),
          unconnected.length > 1 ? h('button', { class: 'primary', onclick: () => vaultConnect(unconnected) }, 'Connect all stations') : null,
          h('button', { onclick: close }, 'Close'))));
    };
    draw();
    const off = api.onEvent((ev) => { if (ev.kind.startsWith('vault.')) setTimeout(draw, 400); });
    const timer = setInterval(draw, 1500);
    vaultPanel._stop = () => { off(); clearInterval(timer); };
    return h('span');
  }, { wide: true, onClose: () => vaultPanel._stop && vaultPanel._stop() });
}

// -------------------------------------------------------------------- views

// ---- usage ----
// Token counts are what each agent wrote in its own session data. A cost is
// an estimate at list prices and is always called that: a subscription is
// not billed per token.

const tokCell = (n) => h('td', { class: 'num', title: exact(n) + ' tokens' }, fmtTokens(n));
const costCell = (c, tokens) => h('td', { class: 'num', title: c === null || c === undefined ? (tokens ? 'No price for this in the list' : '') : `$${c.toFixed(4)} at list prices` }, c === null || c === undefined ? (tokens ? 'no price' : '—') : fmtCost(c));
const listHost = (src) => { try { return new URL(src).host; } catch (e) { return src; } };

function priceNote(src, fetchedMs, unpriced) {
  const missing = [...new Set(unpriced || [])];
  return h('p', { class: 'muted small price-note' },
    src ? `Cost is an estimate at list prices from ${listHost(src)}, fetched ${new Date(fetchedMs).toLocaleDateString()}. What you actually pay depends on your plan.`
      : 'Cost is not estimated: the price list has not been fetched yet. spandeck fetches it by itself, once a day, when the station can reach the internet.',
    src && missing.length ? ' ' + missing.map(m => `No price for ${m}.`).join(' ') : null);
}

function modelTable(rows) {
  return h('div', { class: 'table-wrap' }, h('table', { class: 'table usage-table' },
    h('thead', {}, h('tr', {}, h('th', {}, 'Model'), h('th', { class: 'num' }, 'In'), h('th', { class: 'num' }, 'Out'), h('th', { class: 'num', title: 'Read from the cache / written to it' }, 'Cache'), h('th', { class: 'num', title: 'Estimate at list prices' }, 'Est. cost'))),
    h('tbody', {}, rows.map(m => h('tr', {},
      h('td', { title: m.provider ? `${m.model} (${m.provider})` : m.model }, m.model), tokCell(m.input_tokens), tokCell(m.output_tokens),
      h('td', { class: 'num', title: `${exact(m.cache_read_tokens)} read, ${exact(m.cache_write_tokens)} written` }, `${fmtTokens(m.cache_read_tokens)} / ${fmtTokens(m.cache_write_tokens)}`),
      costCell(m.cost_estimate, m.total_tokens))))));
}

async function usageDialog(st, p) {
  let pane = p;
  try { pane = await api.on(st, 'pane.get', { pane: p.id }); } catch (e) { /* show what the list had */ }
  const u = pane.usage;
  const fact = (label, value, title) => h('div', { class: 'fact', title }, h('span', { class: 'muted' }, label), h('b', {}, value));
  dialog(`Usage · ${p.name}`, (close) => {
    if (!u || u.total_tokens === null || u.total_tokens === undefined) {
      return h('div', { class: 'usage-dialog' },
        h('p', {}, !u ? 'A shell has no usage.' : u.note === 'not reported by this agent' ? `Not reported by this agent. ${p.agent_name} keeps no usage that spandeck can read.` : 'Nothing reported yet. The numbers appear after the agent\'s first reply.'),
        h('div', { class: 'row end' }, h('button', { onclick: close }, 'Close')));
    }
    const share = u.total_tokens ? Math.round(u.subagent_tokens * 100 / u.total_tokens) : 0;
    return h('div', { class: 'usage-dialog' },
      h('div', { class: 'facts' },
        fact('Model in use now', u.model || 'unknown', u.provider ? `Served by ${u.provider}` : ''),
        fact('Context', u.context_tokens === null ? 'unknown' : u.context_window ? `${fmtTokens(u.context_tokens)} of ${fmtTokens(u.context_window)} (${u.context_pct}%)` : `${fmtTokens(u.context_tokens)} tokens`,
          u.context_tokens === null ? '' : `${exact(u.context_tokens)} tokens in the last model call` + (u.context_window ? `, of a window of ${exact(u.context_window)} (from the ${u.context_window_source})` : '; this model\'s window is not known')),
        fact('Input', fmtTokens(u.input_tokens), exact(u.input_tokens) + ' tokens'),
        fact('Output', fmtTokens(u.output_tokens), exact(u.output_tokens) + ' tokens'),
        fact('Cache read', fmtTokens(u.cache_read_tokens), exact(u.cache_read_tokens) + ' tokens'),
        fact('Cache write', fmtTokens(u.cache_write_tokens), exact(u.cache_write_tokens) + ' tokens'),
        fact('Total', fmtTokens(u.total_tokens), exact(u.total_tokens) + ' tokens'),
        fact('Turns', u.turns === null ? 'not recorded' : String(u.turns), 'Prompts in this conversation'),
        fact('Estimated cost', u.price_source ? (u.cost_estimate === null ? 'no price' : fmtCost(u.cost_estimate)) : 'not yet', u.cost_estimate === null ? '' : `$${u.cost_estimate.toFixed(4)} at list prices`)),
      h('h3', {}, 'By model'),
      modelTable(u.by_model || []),
      h('p', { class: 'muted small' }, u.subagent_tokens ? `Sub-agents used ${fmtTokens(u.subagent_tokens)} of the total (${share}%), counted under the model that ran them.` : 'No sub-agent usage in this conversation.'),
      priceNote(u.price_source, u.price_fetched_ms, u.unpriced_models),
      u.reported_cost_usd !== null && u.reported_cost_usd !== undefined ? h('p', { class: 'muted small' }, `${p.agent_name}'s own figure, when it last wrote one: ${fmtCost(u.reported_cost_usd)}.`) : null,
      h('p', { class: 'muted small' }, `From the ${u.source || 'session data'}` + (u.updated_ms ? ` · last updated ${ago(u.updated_ms)} ago` : '') + '. A new conversation starts from zero.'),
      h('div', { class: 'row end' }, h('button', { onclick: close }, 'Close')));
  });
}

async function fleetUsageDialog() {
  const from = new Date().setHours(0, 0, 0, 0);
  const stations = onlineStations();
  const away = S.fleet.stations.filter(s => s.state !== 'online').map(s => s.station);
  const all = await Promise.all(stations.map(st => api.on(st, 'usage.summary', { today_from_ms: from }).then(sum => ({ st, sum })).catch(e => ({ st, problem: e.message }))));
  const both = (name, row, title) => h('tr', {}, h('td', { title: title || name }, name),
    tokCell(row.today.total_tokens), costCell(row.today.cost_estimate, row.today.total_tokens), tokCell(row.all.total_tokens), costCell(row.all.cost_estimate, row.all.total_tokens));
  const table = (first, rows) => h('div', { class: 'table-wrap' }, h('table', { class: 'table usage-table both' },
    h('thead', {}, h('tr', {}, h('th', {}, first), h('th', { class: 'num' }, 'Today'), h('th', { class: 'num', title: 'Estimate at list prices' }, 'Est. cost'), h('th', { class: 'num' }, 'All time'), h('th', { class: 'num', title: 'Estimate at list prices' }, 'Est. cost'))),
    h('tbody', {}, rows)));
  // The same model on several stations is one line for the fleet.
  const models = new Map();
  const add = (a, b) => (a === null || a === undefined) ? b : (b === null || b === undefined) ? a : a + b;
  for (const { sum } of all) for (const when of sum ? ['today', 'all'] : []) for (const m of sum[when].by_model) {
    const key = `${m.provider || ''}/${m.model}`;
    const row = models.get(key) || { model: m.model, provider: m.provider, today: { total_tokens: 0, cost_estimate: null }, all: { total_tokens: 0, cost_estimate: null } };
    row[when].total_tokens += m.total_tokens;
    row[when].cost_estimate = add(row[when].cost_estimate, m.cost_estimate);
    models.set(key, row);
  }
  const byModel = [...models.values()].sort((a, b) => b.all.total_tokens - a.all.total_tokens);
  const first = all.find(x => x.sum);
  dialog('Usage', (close) => h('div', { class: 'usage-dialog' },
    h('p', { class: 'muted small' }, 'Tokens each agent reported in its own session data, since spandeck began following it. Today is since midnight here.'),
    h('h3', {}, 'By model, whole fleet'),
    byModel.length ? table('Model', byModel.map(m => both(m.model, m, m.provider ? `${m.model} (${m.provider})` : m.model))) : h('p', { class: 'muted' }, 'No agent has reported any usage yet.'),
    all.map(({ st, sum, problem }) => h('div', { class: 'usage-station' },
      h('h3', {}, st),
      problem ? h('p', { class: 'warn-text' }, problem) : [
        table('Project', [both('Everything on ' + st, sum), ...sum.projects.map(pr => both(pr.project || 'No project', pr))]),
        sum.panes.length ? table('Pane', sum.panes.map(pn => both(pn.name || pn.pane, pn, `${pn.name || pn.pane} · ${pn.agent}${pn.project ? ' · ' + pn.project : ''}${pn.live ? '' : ' · closed'}`))) : null])),
    away.length ? h('p', { class: 'muted small' }, `Not counted, because ${away.length === 1 ? 'it is' : 'they are'} not reachable: ${away.join(', ')}.`) : null,
    first ? priceNote(first.sum.price_source, first.sum.price_fetched_ms, all.flatMap(x => x.sum ? x.sum.all.unpriced_models : [])) : null,
    h('div', { class: 'row end' }, h('button', { onclick: close }, 'Close'))), { wide: true });
}

function topbar() {
  const r = S.route;
  const needs = S.fleet.stations.flatMap(s => liveAgents(s.station)).filter(p => p.state === 'needs_you').length;
  const unread = S.notes.filter(n => !n.read).length;
  const pane = r.view === 'pane' ? findPane(r.station, r.pane) : null;
  const lay = r.view === 'layout' ? findLayout(r.layout) : null;
  const project = pane?.project || lay?.project || (r.view === 'watch' ? r.project : null);
  const here = pane ? pane.name : lay ? lay.name : r.view === 'watch' ? 'Watch' : r.view === 'perf' ? 'Performance' : null;
  const own = ownKey();
  const trouble = anyTrouble(S.perf);
  const crumbs = [h('a', { href: '#/fleet' }, 'Fleet')];
  if (r.station) crumbs.push('›', h('a', { href: stationUrl(r.station) }, r.station));
  else if (lay) crumbs.push('›', h('span', {}, 'Layouts'));
  if (project) crumbs.push('›', h('span', {}, project));
  if (here) crumbs.push('›', h('span', { class: 'here' }, here));
  return h('header', { class: 'topbar' },
    h('a', { class: 'brand', href: '#/fleet', title: 'spandeck ' + (api.version || '') }, 'spandeck'),
    h('nav', { class: 'crumbs', 'aria-label': 'Where you are' }, crumbs),
    h('span', { class: 'grow' }),
    linkChip, linkAsOf,
    own ? h('button', { class: 'chip key-chip own-key ' + own.info.level, onclick: () => keyDialog(own.name, own.info, own.more), title: 'This is the device you are using. When its key expires it can no longer open the Control Deck.' }, `This device's Tailscale key ${own.info.text}`) : null,
    needs ? h('span', { class: 'chip needs', title: 'Agents waiting for you' }, `${needs} need${needs === 1 ? 's' : ''} you`) : null,
    h('button', { class: S.approvals.length ? 'attn' : '', onclick: approvalsPanel }, 'Approvals', S.approvals.length ? h('span', { class: 'count' }, S.approvals.length) : null),
    h('button', { onclick: inboxPanel }, 'Inbox', unread ? h('span', { class: 'count' }, unread) : null),
    (S.trust?.requests?.length || S.trust?.outgoing) || !isPhone() ? h('button', { class: S.trust?.requests?.length ? 'attn' : '', onclick: trustPanel, title: 'Stations and browsers allowed to control your fleet' }, 'Access', S.trust?.requests?.length ? h('span', { class: 'count' }, S.trust.requests.length) : null) : null,
    isPhone() ? null : h('button', { onclick: fleetUsageDialog, title: 'Tokens and estimated cost, per station, project, pane and model' }, 'Usage'),
    isPhone() ? null : h('button', { class: 'perf-btn', onclick: () => go('#/perf'), 'aria-label': trouble ? 'Performance: something needs a look' : 'Performance', title: 'Where time goes on every station, the links between them, and Tailscale keys' }, 'Performance', trouble ? h('span', { class: 'perf-dot', 'aria-hidden': 'true' }) : null),
    isPhone() ? null : h('button', { onclick: setupPanel, title: 'MCP servers, skills and model defaults for every agent' }, 'Agent setup'),
    isPhone() ? null : h('button', { class: 'tools-btn', onclick: openTools, title: 'Tools (MCP servers) for your agents: add one once, and each station you choose sets it up' }, 'Tools'),
    isPhone() ? null : h('button', { class: 'roles-btn', onclick: openRoles, title: 'Roles for your agents: what a builder, a critic or a reviewer is asked to be. View and change each prompt' }, 'Roles'),
    isPhone() ? null : h('button', { onclick: () => appearance('deck'), title: 'Themes, colours and fonts for the whole deck' }, 'Appearance'),
    isPhone() || !S.fleet.stations.some(x => x.vault?.found) ? null : h('button', { class: S.fleet.stations.some(x => x.state === 'online' && (['not_connected', 'token_rejected', 'locked'].includes(x.vault?.state) || x.vault?.health === 'problem')) ? 'attn' : '', onclick: vaultPanel, title: 'AgentVault: keys for agents without handing over keys' }, 'Vault'),
    h('button', { class: 'danger', onclick: stopAllMenu, title: 'Interrupt running agent turns' }, 'Stop all'),
    isPhone() ? null : h('button', { onclick: palette, title: 'Ctrl+K' }, '⌘K'),
    // A phone's bar has no room for the rest: they are in a menu.
    isPhone() ? h('button', { class: 'top-more', 'aria-label': 'More', title: 'More', 'aria-haspopup': 'menu', onclick: (e) => { const b = e.currentTarget.getBoundingClientRect(); showMenu(b.right, b.bottom + 2, [
      { label: 'Add a machine…', hint: 'to this fleet', action: () => addMachineDialog() },
      { label: 'Tools', hint: 'for your agents', action: openTools },
      { label: 'Roles', hint: 'what each agent is asked to be', action: openRoles },
      { label: 'Appearance', hint: 'theme and text size', action: () => appearance('deck') },
      { label: 'Usage', hint: 'tokens and cost', action: fleetUsageDialog },
      { label: 'Performance', action: () => go('#/perf') }]); } }, '⋯') : null);
}

function stopRows() {
  // How long ago a stop was issued is what its station says (`age_ms`, by
  // its own clock), not this browser's clock against that station's. A
  // station from before it said so is read the old way.
  // The page is told a stop's age when the stop changes, not at every round:
  // it counts on from when it was told (`got`), by its own clock's passing.
  const age = (s) => (s.age_ms != null ? s.age_ms + (s.got ? Date.now() - s.got : 0) : Date.now() - s.issued_ms);
  const recent = (S.stops || S.fleet.stops || []).filter(s => age(s) < 10 * 60 * 1000 || s.status !== 'confirmed').slice(0, 6);
  const away = S.notesAway || [];
  // A station that is on and not answering: what it last said is still shown.
  const before = away.filter(st => S.busy.has(st) && held.get(st)?.raw.stops);
  const gone = away.filter(st => !before.includes(st));
  if (!recent.length) return null;
  // `saved: false`: the issuing station holds the stop in memory only (its
  // database did not take it). Said for a stop that is still to be delivered:
  // if that station's daemon ends first, the stop is not delivered.
  const unsaved = (s) => s.saved === false && s.status !== 'confirmed';
  return h('div', { class: 'stops' }, h('b', {}, 'Stop requests'), recent.map(s => h('span', { class: 'stop ' + s.status, title: `issued ${ago(Date.now() - age(s))} ago${s.from ? ' at ' + s.from : ''}${unsaved(s) ? `. ${s.from} has not been able to write this stop to its database yet; it keeps trying. If spandeck on ${s.from} restarts before it is written, the stop is not delivered to ${s.station}.` : ''}` },
    `${s.station}: ${s.label}`, s.status === 'confirmed' ? ` (${s.result?.stopped?.length ?? 0} stopped${s.result?.not_reading?.length ? `, ${s.result.not_reading.length} not reading input and not stopped: use Kill` : ''})` : unsaved(s) ? ` — waiting for ${s.station} · not yet saved on ${s.from}` : s.status === 'unreachable' ?` — will be delivered when it is back${s.from && !online(s.from) ? `, once ${s.from} is on too` : ''}` : '')),
    gone.length ? h('span', { class: 'muted small' }, `Stops issued at ${gone.join(', ')} are not shown: not reachable.`) : null,
    before.length ? h('span', { class: 'muted small' }, `Stops issued at ${before.join(', ')} are shown as they were: ${before.length === 1 ? 'it is' : 'they are'} not answering.`) : null);
}

// ---- from one station's Control Deck to another's, with no second approval
// Each station's Control Deck is its own web address, and a browser keeps
// its sign-in per address, so the same phone used to be approved again at
// each station. This browser's token is already accepted by every station of
// the fleet. So the deck it is signed in to (A) opens the other station's
// deck (B) in a new window and hands it the token as a window message:
//
//   1. A opens B's plain address and keeps the window. No secret is in it.
//   2. B's page, if it is not signed in and was opened by another window,
//      tells its opener it is ready. That message holds nothing.
//   3. A answers only the window it opened, only if the message came from
//      B's origin, and tells the browser to deliver the token only to that
//      origin. B's origin is the address B gave in an answer signed with the
//      fleet key. A sends once, and only a browser's own token, never the
//      station's.
//   4. B takes a token only while it waits, only from its opener, asks its
//      own station whether the token is good, and only then keeps it.
//
// The token is never in an address, so it is in no history, no log and no
// address bar. A page that is not the window A opened, or is at another
// origin, is sent nothing. A page that opens B and sends it a token can only
// send one it already holds: B then shows the fleet to a browser whose owner
// that sender could already act as, so it gains nothing.
const HOP_READY = 'spandeck:hop-ready', HOP_TOKEN = 'spandeck:hop-token';
const hops = [];
const ownToken = () => { try { return localStorage.getItem('spandeck.token') || ''; } catch (e) { return ''; } };
// The origin of a station's own Control Deck, or null when this browser
// cannot be handed on to it.
function hopOrigin(s) {
  if (!ownToken().startsWith('dev.') || s.self || s.state !== 'online') return null;
  const local = (a) => /^(127\.|\[::1\]|localhost)/.test(a);
  const addrs = (s.web || []).filter(a => typeof a === 'string' && /^[0-9A-Za-z.:\[\]-]+$/.test(a));
  // A loopback address is another machine's own, unless this page is local too.
  const at = addrs.find(a => !local(a)) || (local(location.host) ? addrs.find(local) : null);
  // A station serves its deck over plain HTTP at this address (Tailscale
  // encrypts the link), whatever address this page was opened at.
  return at ? new URL(`http://${at}`).origin : null;
}
// Called from the click itself, so the browser does not treat it as a popup.
function hopOpen(s) {
  const origin = hopOrigin(s);
  if (origin) hopTo(origin, `${s.station}'s Control Deck`);
}
// The same steps towards an origin this page was told of by its own
// station: another station's deck, or this station's secure deck (HTTPS).
function hopTo(origin, what) {
  const opened = window.open(origin + '/#/fleet', '_blank');
  if (!opened) return toast(`This browser did not let ${what} open in a new window. Open ${origin} yourself and sign in there: the request shows on every station.`, 'error');
  hops.push({ opened, origin, until: Date.now() + 120000 });
}
addEventListener('message', (e) => {
  if (e.data !== HOP_READY) return;
  const at = hops.findIndex(x => x.opened === e.source && x.origin === e.origin && x.until > Date.now());
  if (at < 0) return;
  // Once. A second "ready" from that window gets nothing.
  const [hop] = hops.splice(at, 1);
  const token = ownToken();
  if (token.startsWith('dev.')) hop.opened.postMessage({ kind: HOP_TOKEN, token }, hop.origin);
});

// The other side: this page is not signed in, and another window opened it.
// (A phone's home-screen app, or a link opened from elsewhere, has no
// opener: then the usual request is made, which shows on every station.)
let hopAsked = false;
function hopAsk() {
  if (hopAsked || !window.opener) return;
  hopAsked = true;
  let waiting = true;
  setTimeout(() => { waiting = false; }, 30000);
  addEventListener('message', async (e) => {
    if (!waiting || e.source !== window.opener || !e.data || e.data.kind !== HOP_TOKEN) return;
    waiting = false;
    const token = e.data.token;
    if (typeof token !== 'string' || !/^dev\.[A-Za-z0-9-]{1,64}\.[0-9a-f]{32,128}$/.test(token)) return;
    // Kept only if this station says it is good.
    let good = false;
    try {
      const res = await fetch('/api/rpc', { method: 'POST', headers: { 'content-type': 'application/json', authorization: 'Bearer ' + token }, body: JSON.stringify({ method: 'station.info', params: {} }) });
      good = res.ok;
    } catch (err) { /* not signed in */ }
    if (!good) return;
    try { localStorage.setItem('spandeck.token', token); } catch (err) { return; }
    location.reload();
  });
  // Nothing in this message: any opener may be told that this page waits.
  try { window.opener.postMessage(HOP_READY, '*'); } catch (e) { /* no opener after all */ }
}

const STATION_STATE = { unreachable: 'unreachable', not_linked: 'not in this fleet', update_needed: 'on a version this station cannot control' };

function stationMenu(e, s) {
  e.preventDefault();
  e.stopPropagation();
  const up = s.state === 'online';
  showMenu(e.clientX, e.clientY, [
    { label: 'Open ' + s.station, action: () => go(stationUrl(s.station)) },
    up && { label: 'New pane here…', action: () => newPaneDialog(s.station) },
    up && { label: 'New project here…', action: () => newProjectDialog(s.station) },
    up && { label: 'Check this station', action: () => doctorPanel(s.station) },
    hopOrigin(s) && { label: `Open ${s.station}'s own Control Deck`, action: () => hopOpen(s) },
    up && s.https && { label: s.https.on ? 'Secure access (HTTPS)…' : s.https.state === 'needs_operator' ? 'Turn on secure access (HTTPS)…' : 'Secure access (HTTPS)…', action: () => fo.httpsDialog(s) },
    { label: 'Agent programs and versions…', action: agentsPanel },
    { label: 'Tools for agents…', action: openTools },
    { label: 'Usage: tokens and estimated cost…', action: fleetUsageDialog },
    { label: 'Performance…', action: () => go('#/perf') },
    stationKey(s.station) && { label: `Tailscale key: ${stationKey(s.station).text}…`, action: () => keyDialog(keyName(s.station), stationKey(s.station), stationMore(s.station)) },
    s.state === 'not_linked' && { label: `Add ${s.station} to this fleet…`, action: () => addMachineDialog({ station: s.station, kind: 'join' }) },
    up && (s.caps || []).includes('rename') && { label: 'Rename…', action: () => renameStationDialog(s) },
    up && { label: 'Restart spandeck here (keeps terminals)', action: () => confirmBox(`Restart spandeck on ${s.station}?`, 'Every terminal and agent keeps running. If it does not come back healthy, the restart is undone.', 'Restart', () => rpc(s.station, 'station.restart', {}).then(() => toast(`${s.station} is restarting`))) },
    up && { label: 'Check for a newer spandeck now', action: checkReleaseNow },
    up && { label: 'Update to the latest release…', action: () => confirmBox(`Update spandeck on ${s.station}?`, `It runs ${s.version}. The latest release is downloaded on ${s.station} and checked against its published checksum. Terminals keep running, and a version that does not start properly is undone.`, 'Update', () => rpc(s.station, 'station.update', {}).then((r) => toast(r.current ? `${s.station} already runs the latest release` : r.fleet || r.begun ? r.note : `${s.station} is updating to ${r.release}`))) },
    // A station that is on, and refused for its clock or its Tailscale key,
    // is not helped by waking or starting it.
    awayWhy(s) && { note: awayWhy(s) },
    s.state === 'unreachable' && !awayWhy(s) && { label: 'Wake it (same network only)', action: () => rpc(api.station, 'fleet.wake', { station: s.station }).then((r) => toast(`Sent ${r.sent} wake packets to ${s.station}. Give it a minute.`)) },
    s.state === 'unreachable' && !awayWhy(s) && { label: 'Start spandeck there (over SSH)', action: () => rpc(api.station, 'fleet.start', { station: s.station }).then((r) => toast(`${s.station} is ${r.state}`)) },
    s.state === 'unreachable' && { label: 'Forget this station', danger: true, action: () => confirmBox(`Forget ${s.station}?`, 'It is taken off the list on every station of the fleet (the others follow within seconds, one that is off when it is back). If it comes back it is listed again.', 'Forget', () => rpc(api.station, 'fleet.forget', { station: s.station })) },
    up && { label: 'Stop every agent here', danger: true, action: () => rpc(api.station, 'stop.all', { scope: s.station === api.station ? 'station' : s.station }) },
    up && (s.caps || []).includes('holds') && { label: 'Release all on this station', hint: 'held agents take mail again', action: () => releaseAll(s.station) },
    up && { label: 'Close all panes here…', danger: true, action: () => closeAllDialog(s) },
    // A station that is not in the fleet has nothing to be removed from,
    // and neither has the only station of a fleet of one.
    s.state !== 'not_linked' && S.fleet.stations.some(x => x.station !== s.station && x.state !== 'not_linked')
      && { label: 'Remove this station from the fleet…', danger: true, action: () => removeStationDialog(s) },
    // Over SSH from the station this deck is open on, so not for that one,
    // and not for one that does not answer.
    !s.self && s.state !== 'unreachable' && { label: `Uninstall spandeck on ${s.station}…`, danger: true, action: () => addMachineDialog({ station: s.station, kind: 'uninstall' }) },
  ]);
}

// ---- close every pane on a station; remove a station from the fleet ----
// Both end work that cannot be brought back, so each says what it will do
// with the station's name and its counts, and does nothing until the red
// button is pressed. The button the dialog opens on is Cancel.
const plural = (n, one, many) => `${n} ${n === 1 ? one : (many || one + 's')}`;
function paneCounts(st) {
  const all = panesOf(st), live = all.filter(p => !p.exited);
  return { all: all.length, live: live.length, ended: all.length - live.length,
    agents: live.filter(p => p.agent !== 'shell').length, shells: live.filter(p => p.agent === 'shell').length,
    working: live.filter(p => p.state === 'working' || p.state === 'needs_you').length };
}
function paneCountLine(st) {
  const c = paneCounts(st);
  if (!c.all) return `No pane is on ${st}.`;
  return `${st} has ${plural(c.live, 'running pane')} (${plural(c.agents, 'agent')}, ${plural(c.shells, 'shell')})`
    + `${c.ended ? ` and ${c.ended} ended on its list` : ''}. ${c.working === 0 ? 'None is' : c.working === 1 ? '1 is' : c.working + ' are'} working right now.`;
}
const RESUME_NOTE = 'A conversation can be resumed later only where the agent saved one. A turn that is running now is lost, and so is whatever a shell was doing.';

function closeAllDialog(s) {
  const st = s.station, c = paneCounts(st);
  dialog(`Close all panes on ${st}?`, (close) => h('div', { class: 'consequences' },
    h('p', {}, h('b', {}, paneCountLine(st))),
    h('ul', {},
      h('li', {}, 'Every pane is ended, with everything it started, and taken off the list.'),
      h('li', {}, RESUME_NOTE),
      h('li', {}, 'Projects, layouts, history and the archive are left alone.')),
    h('div', { class: 'row end' },
      h('button', { onclick: close }, 'Cancel'),
      h('button', { class: 'primary danger', disabled: !c.all, onclick: () => { close(); rpc(st, 'station.close_all', {}).then((r) => toast(`Closed ${plural(r.closed, 'pane')} on ${st}`)); } },
        c.all ? `Close ${plural(c.all, 'pane')} on ${st}` : 'Nothing to close'))));
}

function removeStationDialog(s) {
  const st = s.station, me = !!s.self, away = s.state === 'unreachable';
  const others = S.fleet.stations.filter(x => x.station !== st && x.state !== 'not_linked');
  const staying = others.filter(x => !x.self).map(x => x.station);
  const absent = others.filter(x => x.state !== 'online').map(x => x.station);
  const canReach = others.some(x => x.state === 'online');
  const dirs = s.dirs;
  const stop = h('input', { type: 'checkbox', disabled: away });
  const vault = away
    ? `If ${st} holds a vault token, it keeps it. Revoke spandeck:${st} on the vault's Tokens page.`
    : s.vault?.has_token ? `${st} keeps its vault token, and the token keeps working there. To end it, revoke spandeck:${st} on the vault's Tokens page.` : null;
  const steps = [
    away
      ? h('li', {}, h('b', {}, `${st} cannot be reached, so nothing is done on it.`), ` Its panes keep running until someone stops spandeck there.`)
      : h('li', {}, h('b', {}, `Every pane on ${st} is closed.`), ` ${paneCountLine(st)} ${RESUME_NOTE}`),
    me && !canReach
      ? h('li', {}, h('b', {}, `${st} leaves the fleet by itself.`), ` It throws away its copy of the fleet key and stops trusting the other stations. No other station can be reached to be told: they still list ${st} until you remove it at one of them as well.`)
      : h('li', {}, h('b', {}, `${st} is taken out of the fleet for good.`),
        away ? ` The stations that stay get a new fleet key. ${st} keeps the old one, which no station accepts any more: it cannot act on the fleet, and cannot rejoin without a new approval.`
          : ` It stops trusting the other stations, and they stop trusting it: the stations that stay get a new fleet key, so the key ${st} held works nowhere.`,
        h('ul', {},
          h('li', {}, 'Browsers signed in from other devices (a phone, a laptop) must sign in again. This one stays signed in.'),
          absent.length ? h('li', { class: 'warn-text' }, `Not reachable right now: ${absent.join(', ')}. Each keeps the old key until it is seen again and is handed the new one then (for 30 days; after that it rejoins on your approval). Until then it shows as not in this fleet, and ${st} could still act on it.`) : null,
          !staying.length ? h('li', {}, `${st} is the only other station: afterwards ${api.station} is a fleet of one.`) : null)),
    h('li', {}, h('b', {}, `${st} is forgotten.`), ` It goes from this list on every station, with pinned panes and layouts that point at it, its panes in global layouts, and project links to its projects. Project files in your folders are not changed.`),
    vault ? h('li', {}, vault) : null,
    h('li', {}, me ? `This Control Deck then shows ${st} alone. The other stations appear as "not in this fleet".`
      : away ? `If spandeck is still running on ${st}, it appears here again as "not in this fleet", like any machine of yours that never joined.`
        : `spandeck keeps running on ${st} as a fleet of one (unless you tick the box below), and ${st} appears here as "not in this fleet". To put it back, join it again and approve the request.`),
  ];
  const d = dialog(me ? `Remove this station (${st}) from the fleet?` : `Remove ${st} from the fleet?`, (close) => h('div', { class: 'consequences' },
    h('p', {}, me ? `You are using ${st}'s own Control Deck. In this order:` : 'In this order:'),
    h('ol', {}, steps),
    h('label', { class: 'check' + (away ? ' muted' : '') }, stop, h('span', {},
      `Also stop spandeck on ${st} and stop it starting at login`,
      h('small', {}, away ? `Not possible while ${st} cannot be reached.`
        : `The program and its data stay on ${st}'s disk${dirs ? `: ${dirs.program}, ${dirs.state} and ${dirs.config}` : ''}. ${me ? 'This page stops working when it does.' : ''}`))),
    h('div', { class: 'row end' },
      h('button', { onclick: close }, 'Cancel'),
      h('button', { class: 'primary danger', onclick: () => { close(); removeStation(st, stop.checked); } }, `Remove ${st}`))), { wide: true });
  // The dialog opens on Cancel: not on the tick box, and not on the red button.
  d.body.querySelector('.row.end button').focus();
}

function removeStation(st, stop) {
  toast(`Removing ${st}…`);
  rpc(api.station, 'fleet.remove', { station: st, stop }).then((r) => {
    // The fleet's key is new: this browser's sign-in is replaced with it.
    if (r.token) { try { localStorage.setItem('spandeck.token', r.token); } catch (e) { /* signs in again */ } }
    const there = r.there || {};
    const said = [
      r.reached ? `${plural(there.closed || 0, 'pane')} closed on ${st}. It left the fleet and trusts no other station.`
        : `${st} could not be reached (${r.problem || 'no answer'}). Its panes keep running until someone stops spandeck there. It cannot act on the fleet, and cannot rejoin without a new approval.`,
      r.self && (r.not_told || []).length ? `No other station could be told: ${r.not_told.join(', ')}. Remove ${st} at one of them as well.` : null,
      !r.self || r.through ? (r.alone ? `${r.self ? r.through : api.station} is now a fleet of one, with a new key.` : `The fleet has a new key. Took it: ${[r.self ? r.through : api.station, ...(r.rekeyed || [])].join(', ')}.`) : null,
      (r.waiting || []).length ? `Still on the old key: ${r.waiting.join(', ')}. Each is handed the new key when it is next seen. Until then ${st} could still act on it.` : null,
      there.stopping ? `spandeck is stopping on ${st} and will not start at login. Still on its disk: ${there.dirs?.program}, ${there.dirs?.state}, ${there.dirs?.config}.` : null,
      there.service?.problems?.length ? `Stopping it at login did not go cleanly: ${there.service.problems.join('; ')}` : null,
      r.reached ? (there.vault_token ? `${st} still holds its vault token. Revoke spandeck:${st} on the vault's Tokens page.` : null)
        : `If ${st} holds a vault token, revoke spandeck:${st} on the vault's Tokens page.`,
      'Browsers signed in from other devices must sign in again.',
    ].filter(Boolean);
    dialog(`${st} was removed from the fleet`, (close) => h('div', { class: 'consequences' },
      h('ul', {}, said.map(line => h('li', {}, line))),
      h('div', { class: 'row end' }, h('button', { class: 'primary', onclick: close }, 'Done'))));
  }).catch(() => {});
}

function stationTile(s) {
  const agents = liveAgents(s.station).filter(p => p.agent !== 'shell').sort(byState);
  const up = s.state === 'online';
  return h('a', { class: 'tile station ' + s.state, href: stationUrl(s.station), oncontextmenu: (e) => stationMenu(e, s) },
    h('div', { class: 'tile-head' }, h('span', { class: 'dot ' + (up ? (s.needs_you ? 'needs_you' : s.working ? 'working' : 'idle') : 'unreachable') }), h('b', {}, s.station), s.self ? h('span', { class: 'chip' }, 'this station') : null, h('span', { class: 'grow' }), h('span', { class: 'muted tile-version', title: s.update && s.update.says ? `spandeck ${s.version || ''}: ${String(s.update.says).slice(0, 240)}` : null }, [s.version, s.update && s.update.says ? String(s.update.says).slice(0, 48) : ''].filter(Boolean).join(' · '))),
    up ? h('div', { class: 'tile-stats' },
      h('span', {}, h('b', {}, s.working ?? 0), ' working'), h('span', { class: s.needs_you ? 'needs' : '' }, h('b', {}, s.needs_you ?? 0), ' need you'),
      s.load != null ? h('span', { class: 'muted' }, 'load ' + Number(s.load).toFixed(1)) : null, s.keep_awake ? h('span', { class: 'chip' }, 'kept awake') : null,
      linkOld() ? h('span', { class: 'chip warn as-of', title: `This page has not heard from ${api.station}, which it asks everything through, since ${clockOf(api.link().asOf)}. What is shown of ${s.station} is as it was then.` }, `as of ${clockOf(api.link().asOf)}`)
      : S.busy.has(s.station) ? h('span', { class: 'chip warn', title: `${s.station} has not answered what this page last asked of it. What is shown of it here is as of ${ago(S.busy.get(s.station))} ago. It is asked again by itself, one question at a time; everything else on this page is unaffected.` }, `not answering: as of ${ago(S.busy.get(s.station))} ago`) : null,
      s.codex_sandbox_blocked ? h('span', { class: 'chip warn', title: 'Codex cannot run commands on this station: the system blocks its sandbox. On the station, run "spandeck doctor --fix" once. It asks for your password.' }, 'Codex blocked') : null,
      s.service === false ? h('span', { class: 'chip warn', title: 'spandeck does not start at login on this station. After a reboot it stays down until someone starts it. Run "spandeck service --install" there.' }, 'not started at login') : null, vaultChip(s), keyChip(s.station))
      : h('div', { class: 'tile-stats warn-text' }, s.state === 'update_needed' ? 'Needs an update before it can be controlled' : awayWhy(s) ? [awayWhy(s), ' ', s.why === 'clock' ? null : keyChip(s.station)] : s.state === 'unreachable' ? [`Unreachable · last seen ${ago(s.last_seen_ms)} ago. Right-click to wake or start it. `, keyChip(s.station)] : s.state === 'not_linked' ? ['Not in this fleet. ', h('button', { class: 'mini add-station', onclick: (e) => { e.preventDefault(); e.stopPropagation(); addMachineDialog({ station: s.station, kind: 'join' }); } }, `Add ${s.station} to this fleet…`)] : `Unreachable · last seen ${ago(s.last_seen_ms)} ago`),
    h('div', { class: 'tile-agents' }, agents.slice(0, 8).map(p => h('div', { class: 'agent-line' }, dot(p), paneMark(p), h('span', {}, p.name), h('span', { class: 'muted' }, p.tool || p.needs_reason || STATE_LABEL[stateOf(p)]))),
      up && !agents.length ? h('span', { class: 'muted' }, 'No agents running') : null));
}

// ---- add a machine ----
// The last tile of the fleet. The station the deck is open on connects to a
// machine of the owner's over SSH, installs spandeck there and has it ask to
// join; the owner then approves the code, as for any station. What the
// machine prints is text from somewhere else: it is only ever inserted as text.
const JOIN_STEPS = [['connecting', 'Connect over SSH'], ['joining', 'Have it ask to join this fleet']];
const UNINSTALL_STEPS = [['connecting', 'Connect over SSH'], ['stopping', 'Close its panes, stop spandeck and stop it starting at login'], ['removing', 'Delete its programs']];
const ADD_STEPS = [['connecting', 'Connect over SSH'], ['installing', 'Download and install spandeck'], ['starting', 'Start it, now and at login'], ['joining', 'Ask to join this fleet']];
const ADD_STAGE = { confirm: 'Tailscale is waiting for you to confirm this connection', connecting: 'Connecting over SSH…', installing: 'Installing spandeck…', starting: 'Starting spandeck…', joining: 'Asking to join this fleet…', approve: 'Waiting for your approval', joined: 'Approved. Waiting for it to appear…', needs_password: 'It asks for a password', failed: 'It was not added', stopping: 'Stopping spandeck there…', removing: 'Deleting its programs…' };
let addDlg = null;
const addActive = () => (S.adding && S.adding.stage !== 'done' ? S.adding : null);
// The name the owner chose for the new station goes with the approval.
const addAnswer = (a, yes, as) => rpc(api.station, yes ? 'trust.approve' : 'trust.deny', yes && (as === undefined ? a.name_as : as) ? { request: a.request, as: as === undefined ? a.name_as : as } : { request: a.request }).catch(() => { /* said by rpc */ });
// The one thing in what ssh prints that is ever a link: the address
// Tailscale gives to confirm a connection at, and only at its own site.
// The station checked it; the page checks it again.
const TS_CONFIRM = /^https:\/\/login\.tailscale\.com\/[A-Za-z0-9\/_-]{1,120}$/;
function addConfirm(a) {
  const link = typeof a.confirm === 'string' && TS_CONFIRM.test(a.confirm) ? a.confirm : null;
  return h('div', { class: 'add-confirm', role: 'status' },
    h('p', {}, h('b', {}, 'Tailscale is waiting for you to confirm this connection.'), ` ${a.name} is reached over Tailscale SSH, which asks you to sign in again before it lets ${api.station} connect. Nothing is wrong with ${a.name}.`),
    link ? h('p', {}, 'Open ', h('a', { class: 'add-confirm-link', href: link, target: '_blank', rel: 'noopener noreferrer' }, link), ' and confirm. This goes on by itself once you have; it waits a few minutes.')
      : h('p', {}, 'Tailscale printed the address to confirm at in the lines below. This goes on by itself once you have confirmed; it waits a few minutes.'));
}
const addClear = () => api.call('fleet.add_cancel').catch(() => {}).then(() => { S.adding = null; refreshSoon(30); });

// Called when the station's answer about an add arrives.
function addChanged() {
  if (addDlg) return addDlg.redraw();
  const a = S.adding;
  if (a && a.stage === 'done') { toast(a.kind === 'uninstall' ? `spandeck was uninstalled on ${a.name}` : `${a.station || a.name} is in your fleet`); addClear(); }
}

// While a new station asks to join it first has to find a station of the
// fleet: any of them, since they are peers.
const addStageWords = (a) => (a.stage === 'joining' ? 'Asking to join your fleet… (looking for its stations)' : ADD_STAGE[a.stage] || a.stage);
// What had worked, what did not, and what to do: three pieces, each text.
const addProblem = (a, fallback) => [
  a.problem?.worked ? h('p', { class: 'add-worked' }, '✓ ' + a.problem.worked) : null,
  h('p', { class: 'warn-text add-problem' }, a.problem?.text || fallback),
  a.problem?.next ? h('p', { class: 'muted add-next' }, a.problem.next) : null];

function addBody(a) {
  if (a.stage === 'approve') return [
    h('div', {}, a.kind === 'join' ? `It asks to join${(a.with || []).length ? `, with its fleet: ${a.name}${cameWith(a.with)}` : ''}. It shows the code ` : 'spandeck is installed there and asks to join. It shows the code ', h('b', { class: 'code' }, a.code), '.'),
    h('div', { class: 'row end' }, h('button', { class: 'mini', onclick: (e) => { e.stopPropagation(); addAnswer(a, false); } }, 'Refuse'), h('button', { class: 'mini primary', onclick: (e) => { e.stopPropagation(); addAnswer(a, true); } }, 'Approve'))];
  if (a.stage === 'failed') return [a.problem?.worked ? h('div', {}, a.problem.worked) : null, h('div', { class: 'warn-text' }, a.problem?.text || (a.kind === 'uninstall' ? 'It was not uninstalled.' : 'It was not added.'))];
  if (a.stage === 'needs_password') return [h('div', {}, `It asks for a password for ${a.user}. Open this to enter it.`)];
  if (a.stage === 'confirm') return [h('div', {}, 'Tailscale is waiting for you to confirm this connection. Open this for the address.')];
  // A release being sent over SSH is progress, said in the station's own words.
  return [h('div', {}, a.note || addStageWords(a)), h('div', { class: 'muted add-last' }, a.lines?.length ? a.lines[a.lines.length - 1] : '')];
}

function addTile() {
  const a = addActive();
  if (!a) return h('button', { class: 'tile add-tile', onclick: () => addMachineDialog(), title: 'Install spandeck on another of your machines and bring it into this fleet' },
    h('span', { class: 'add-plus', 'aria-hidden': 'true' }, '+'), h('span', {}, 'Add a machine'));
  const mark = a.stage === 'failed' ? 'error' : a.stage === 'approve' || a.stage === 'needs_password' || a.stage === 'confirm' ? 'needs_you' : 'working';
  return h('div', { class: 'tile add-tile busy ' + mark, role: 'button', tabIndex: 0, onclick: () => addMachineDialog(), onkeydown: (e) => { if (e.key === 'Enter') addMachineDialog(); } },
    h('div', { class: 'tile-head' }, h('span', { class: 'dot ' + mark }), h('b', {}, a.name), h('span', { class: 'chip' }, a.kind === 'uninstall' ? 'being uninstalled' : 'being added'), h('span', { class: 'grow' }), h('span', { class: 'muted' }, a.os || '')),
    addBody(a));
}

// The same on a phone: a row under the stations.
function addRow() {
  const a = addActive();
  if (!a) return h('button', { class: 'wide add-row', onclick: () => addMachineDialog() }, '+ Add a machine');
  return h('div', { class: 'phone-station add-row', role: 'button', tabIndex: 0, onclick: () => addMachineDialog() },
    h('div', { class: 'row' }, h('span', { class: 'dot ' + (a.stage === 'failed' ? 'error' : a.running ? 'working' : 'needs_you') }), h('b', {}, a.name), h('span', { class: 'muted' }, a.kind === 'uninstall' ? 'being uninstalled' : 'being added')),
    addBody(a));
}

// `target` names a station and what to do with it: { station, kind: 'join' }
// makes a station that is running ask to join this fleet, and
// { station, kind: 'uninstall' } takes spandeck off it. Both go over SSH from
// the station this deck is open on, like adding a machine, and use the same
// user name, password and progress. With no target it is the picker.
function addMachineDialog(target) {
  if (addDlg) return;
  // A job that is under way is shown, whatever was asked for.
  if (target && S.adding?.running) target = null;
  // `listOpen`: the list of machines is shown whole. Once one is picked it
  // is that one row, with "Change", so the row and the box under it are both
  // in sight above a phone's keyboard.
  let info = null, picked = null, listOpen = true, shown = null, again = !!target;
  const erase = h('input', { type: 'checkbox' });
  const plain = { autocapitalize: 'none', autocorrect: 'off', spellcheck: false, autocomplete: 'off' };
  let lastUser = '';
  try { lastUser = localStorage.getItem('spandeck.addUser') || ''; } catch (e) { /* no storage */ }
  const user = h('input', { ...plain, placeholder: 'the short name of your account there', value: lastUser, 'aria-label': 'User name on that machine' });
  const pass = h('input', { ...plain, type: 'password', placeholder: 'password', 'aria-label': 'Password on that machine' });
  // The name the new station is to have in this fleet. Filled with the
  // machine's Tailscale name when the station would call itself otherwise.
  const nameAs = h('input', { ...plain, class: 'add-name', maxLength: 64, placeholder: 'the name the machine gives itself', 'aria-label': 'Name of the new station in this fleet' });
  const approveBoxes = new Map();
  const log = h('pre', { class: 'add-log', 'aria-label': 'What the machine printed' });
  const box = h('div', { class: 'add-dialog' });
  const setLog = (lines) => { const end = log.scrollHeight - log.scrollTop - log.clientHeight < 30; log.textContent = (lines || []).join('\n'); if (end) log.scrollTop = log.scrollHeight; };
  // `what` is a machine's id, or { station, kind } for a station.
  const start = (what, name, password) => {
    try { localStorage.setItem('spandeck.addUser', user.value.trim()); } catch (e) { /* no storage */ }
    // { again: true }: the machine of the try before is only made to ask
    // again; nothing is installed a second time.
    const station = typeof what === 'object' && !what.again ? what : null;
    const params = what.again ? { again: true, user: user.value.trim() } : station ? { station: station.station, user: user.value.trim() } : { machine: what, user: user.value.trim() };
    if (!station && !what.again && nameAs.value.trim()) params.name = nameAs.value.trim();
    if (station && station.kind === 'uninstall') params.erase = station.erase !== undefined ? !!station.erase : erase.checked;
    // The password goes in this one request and is dropped from the page.
    if (password) params.password = password;
    pass.value = '';
    return api.call(station && station.kind === 'uninstall' ? 'fleet.uninstall' : 'fleet.add', params).then((r) => {
      const a = r.job || r;
      // A station of this fleet was removed from it first, so the fleet's
      // key is new: this browser's sign-in is replaced with it.
      if (r.removed?.token) { try { localStorage.setItem('spandeck.token', r.removed.token); } catch (e) { /* signs in again */ } }
      if (r.removed) toast(`${name} was removed from the fleet. Browsers signed in from other devices must sign in again.`);
      S.adding = a; again = false; target = null; redraw(); update();
    }).catch((e) => toast(e.message, 'error'));
  };
  // What a job was started on, to start it again with a password.
  const whatOf = (a) => (a.can_ask_again ? { again: true } : a.kind === 'join' || a.kind === 'uninstall' ? { station: a.station || a.name, kind: a.kind, erase: a.erase } : a.machine);
  const load = () => api.call('fleet.candidates').then((r) => { info = r; redraw(); }).catch((e) => { info = { enabled: true, tailscale: true, machines: [], error: e.message }; redraw(); });
  const closeBtn = (label = 'Close') => h('button', { onclick: () => d.close() }, label);
  // Said under the user name, before anything is tapped.
  // The station is named with why it is the one: the stations of a fleet
  // are peers, and this one acts only because the page is open on it (the
  // owner, 17 Nov 2026: "It sound like spark2 is some master machine").
  const here = () => `${info.station} (the station this page is open on)`;
  const userHint = (name, does) => `${here()} connects to ${name} over SSH as this user${does}. If ${name} asks for a password you will be asked for it next; it is used for that one connection and not kept. With Tailscale SSH or an SSH key that already lets ${info.station} in, none is asked.`;
  const whoConnects = () => h('p', { class: 'muted add-quiet add-who' }, `Any station of your fleet can add a machine. ${info.station} does the connecting now because this page is open on it: if ${info.station} is down, open the deck on another station and add the machine from there.`);
  const form = (fields, okLabel, submit) => h('form', { onsubmit: (e) => { e.preventDefault(); submit(); } }, fields, h('div', { class: 'row end' }, closeBtn('Cancel'), h('button', { class: 'primary', type: 'submit' }, okLabel)));

  // A station that is running: add it to this fleet, or uninstall it.
  function stationForm() {
    const st = target.station, s = (S.fleet?.stations || []).find(x => x.station === st) || {};
    if (target.kind === 'join') return [
      h('p', {}, `${here()} connects to ${st} over SSH and has spandeck there ask to join this fleet. You do not go to ${st}, and nothing is installed on it. Its code then appears here for you to approve.`),
      h('p', { class: s.merge ? 'muted' : 'warn-text' }, s.merge
        ? `If ${st} is in a fleet with other stations, they come with it: one approval moves its whole fleet into this one, and the request names them. Browsers signed in to ${st} (and to those stations) from other devices sign in again; browsers signed in to this fleet are not affected.`
        : `${st} runs spandeck ${s.version || ''}, from before a join brought a station's fleet with it: it joins alone. If it is in a fleet with other stations, they stay behind in that fleet, and you add each of them the same way. Browsers signed in to ${st} from other devices sign in again.`),
      form(field(`Your user name on ${st}`, user, userHint(st, '')),
        'Connect and ask', () => { if (user.value.trim()) start(target, st); else user.focus(); }),
      whoConnects()];
    const inFleet = s.state !== 'not_linked', dirs = s.dirs;
    const others = (S.fleet?.stations || []).filter(x => x.station !== st && x.state !== 'not_linked');
    const absent = others.filter(x => x.state !== 'online').map(x => x.station);
    return [h('div', { class: 'consequences' },
      h('p', {}, 'In this order:'),
      h('ol', {},
        inFleet ? h('li', {}, h('b', {}, `${st} is removed from the fleet,`), ` exactly as "Remove this station from the fleet" does. Every pane on ${st} is closed. ${paneCountLine(st)} ${RESUME_NOTE}`,
          h('ul', {},
            h('li', {}, `The stations that stay get a new fleet key, so the key ${st} held works nowhere. Browsers signed in from other devices (a phone, a laptop) must sign in again. This one stays signed in.`),
            absent.length ? h('li', { class: 'warn-text' }, `Not reachable right now: ${absent.join(', ')}. Each keeps the old key until it is seen again and is handed the new one then (for 30 days).`) : null,
            h('li', {}, `${st} is forgotten on every station, with pinned panes and layouts that point at it and project links to its projects.`)))
          : h('li', {}, h('b', {}, `Every pane on ${st} is closed.`), ` ${st} is not in this fleet, so its panes cannot be counted from here. ${RESUME_NOTE}`),
        h('li', {}, h('b', {}, `${info.station} connects to ${st} over SSH.`), ` spandeck is stopped there and no longer starts at login, and its programs are deleted${dirs ? ` (${dirs.program})` : ''}.`),
        h('li', {}, h('b', {}, 'Kept, unless you tick the box below:'), ` ${st}'s history, archive and settings${dirs ? ` (${dirs.state} and ${dirs.config})` : ''}, so that adding ${st} back later picks them up.`),
        h('li', {}, h('b', {}, `Your project folders on ${st} are never touched.`)),
        s.vault?.has_token ? h('li', {}, `${st} holds a vault token. Revoke spandeck:${st} on the vault's Tokens page.`) : null,
        h('li', {}, `Afterwards ${st} is listed under Add a machine again.`)),
      h('label', { class: 'check' }, erase, h('span', {}, `Erase everything: also delete ${st}'s history, archive and settings`,
        h('small', {}, 'This cannot be undone. Worktrees that helpers were given, which spandeck keeps with its own data, go with it.'))),
      field(`Your user name on ${st}`, user, userHint(st, '')),
      h('div', { class: 'row end' },
        h('button', { class: 'cancel', onclick: () => d.close() }, 'Cancel'),
        h('button', { class: 'primary danger', onclick: () => { if (user.value.trim()) start(target, st); else user.focus(); } }, `Uninstall spandeck on ${st}`)))];
  }

  function picker() {
    if (!info) return [h('p', { class: 'muted' }, target ? 'Checking…' : 'Looking for your machines…')];
    if (!info.enabled) return [
      h('p', {}, `Adding a machine is turned off on ${here()}.`),
      h('p', { class: 'muted' }, `Turning it on lets ${info.station} connect to another of your machines over SSH, to install spandeck there, to have a station join this fleet, or to uninstall spandeck there, when you ask it to from the Control Deck. Agents cannot do this, and other stations cannot ask ${info.station} to. You can turn it off again from this dialog.`),
      h('div', { class: 'row end' }, closeBtn('Not now'), h('button', { class: 'primary', onclick: () => api.call('fleet.add_enable', { on: true }).then(() => { info = null; redraw(); load(); }).catch((e) => toast(e.message, 'error')) }, `Turn it on for ${info.station}`))];
    if (target && !info.error) return stationForm();
    if (info.error || !info.tailscale) return [h('p', { class: 'warn-text' }, info.error || `Tailscale is not answering on ${here()}, so your machines cannot be listed.`), h('div', { class: 'row end' }, closeBtn())];
    if (!info.machines.length) return [
      h('p', {}, 'No Mac, Linux or Windows machine under your Tailscale login is online that is not already a station.'),
      h('p', { class: 'muted' }, 'Phones and tagged machines are not listed. A machine that is asleep is listed once it wakes. A Windows machine needs its OpenSSH server turned on first.'),
      h('div', { class: 'row end' }, h('button', { onclick: () => { info = null; redraw(); load(); } }, 'Look again'), closeBtn())];
    const m = info.machines.find(x => x.id === picked);
    const about = (x) => [h('b', {}, x.name), h('span', { class: 'muted' }, `${x.os} · ${x.address}`), x.tailscale_ssh ? h('span', { class: 'chip ok', title: 'Tailscale SSH is on there, so no password is needed' }, 'Tailscale SSH') : null];
    return [
      m && !listOpen
        ? h('div', { class: 'list-row add-machine add-picked first' }, about(m), h('button', { class: 'mini add-change', type: 'button', 'aria-label': 'Pick another machine', onclick: () => { listOpen = true; redraw(); } }, 'Change'))
        : [h('p', { class: 'muted' }, 'Your Mac, Linux and Windows machines that are online and not stations yet:'),
          h('div', { class: 'list' }, info.machines.map(x => h('button', { class: 'list-row add-machine' + (x.id === picked ? ' first' : ''), type: 'button', 'aria-pressed': x.id === picked, onclick: () => { picked = x.id; listOpen = false; nameAs.value = x.name_as || ''; nameAs.dataset.forMachine = x.id; redraw(); user.focus(); } }, about(x))))],
      m ? form([field(`Your user name on ${m.name}`, user, userHint(m.name, ' and runs the spandeck install script there')),
        field('Its name in this fleet', nameAs, m.name_as
          ? `The machine calls itself ${m.host || m.name}, which is the name its station would take. ${m.name_as === m.name ? `Tailscale calls it ${m.name}.` : `${m.name_as} is free.`} You give it this name as you approve it; empty keeps its own.`
          : `Empty keeps the name the machine gives itself (${m.host || m.name}).`)],
        'Connect and install', () => { if (user.value.trim()) start(m.id, m.name); else user.focus(); }) : h('div', { class: 'row end' }, closeBtn('Cancel')),
      whoConnects(),
      // The switch for this feature, away from the buttons that act: beside
      // "Connect and install" it read as turning the station off.
      info.by_config ? null : h('p', { class: 'muted add-off' }, `Adding machines is switched on at ${here()}: it may connect to your other machines over SSH when you ask it to here. `,
        h('button', { type: 'button', onclick: () => api.call('fleet.add_enable', { on: false }).then(() => { info = null; redraw(); load(); }).catch((e) => toast(e.message, 'error')) }, `Stop ${info.station} from adding machines`),
        ` That switches off only this feature: ${info.station} and its panes keep running, and this dialog offers to turn it on again.`)];
  }

  function view(a) {
    // The user name stays editable here: a mistyped name is refused exactly
    // like a wrong password, and the owner could retype only the password.
    if (user.dataset.forJob !== a.id) { user.value = a.user; user.dataset.forJob = a.id; }
    const station = a.kind === 'join' || a.kind === 'uninstall', un = a.kind === 'uninstall';
    const another = station ? null : h('button', { type: 'button', onclick: () => { picked = null; listOpen = true; again = true; redraw(); if (!info) load(); } }, 'Pick another machine');
    const passForm = (label) => h('form', { onsubmit: (e) => { e.preventDefault(); if (!user.value.trim()) user.focus(); else if (pass.value) start(whatOf(a), a.name, pass.value); else pass.focus(); } },
      field(`Your user name on ${a.name}`, user, 'The short name of your account there. Change it here if it was wrong.'),
      field(`Password on ${a.name}`, pass, 'Used once, for this connection. It is not saved, logged or shown anywhere.'),
      h('div', { class: 'row end' }, closeBtn('Cancel'), h('button', { class: 'primary', type: 'submit' }, label)),
      another ? h('div', { class: 'row end add-another' }, another) : null);
    if (a.stage === 'needs_password') return [h('p', {}, `${a.name} asks for a password: neither Tailscale SSH nor an SSH key let ${api.station} in as ${a.user}.${un ? ` Nothing has been deleted on ${a.name} yet.` : ''}`), passForm(un ? 'Connect and uninstall' : station ? 'Connect and ask' : 'Connect and install')];
    if (a.stage === 'failed') return [
      addProblem(a, un ? 'It was not uninstalled.' : 'It was not added.'),
      a.problem?.kind === 'confirm' && typeof a.confirm === 'string' && TS_CONFIRM.test(a.confirm) ? h('p', {}, h('a', { class: 'add-confirm-link', href: a.confirm, target: '_blank', rel: 'noopener noreferrer' }, a.confirm)) : null,
      log,
      a.problem?.kind === 'wrong_password' ? passForm('Try again')
        : h('div', { class: 'row end' }, h('button', { onclick: () => { addClear(); d.close(); } }, 'Close'), h('button', { class: 'primary', onclick: () => {
          user.value = a.user;
          // spandeck runs there and only the asking failed: ask again.
          if (a.can_ask_again) return start({ again: true }, a.name);
          if (station && !a.rejoin) { target = whatOf(a); erase.checked = !!a.erase; } else { picked = a.machine; listOpen = !picked; }
          again = true; redraw(); if (!info) load();
        } }, 'Try again'))];
    if (a.stage === 'approve') return [
      h('p', {}, a.kind === 'join' ? `spandeck on ${a.name} asks to join this fleet as ` : `spandeck is installed and running on ${a.name}. It asks to join this fleet as `, h('b', {}, a.station), ' and shows the code ', h('b', { class: 'code' }, a.code), '.'),
      (a.with || []).length ? h('p', {}, h('b', {}, `Its fleet comes with it: ${a.station}${cameWith(a.with)}.`), ' Every station that holds its fleet\'s key joins, whatever names it gives; one that is off joins when it is next seen. Browsers signed in to those stations from other devices sign in again.')
        : a.kind === 'join' && !a.brings_mates ? h('p', { class: 'warn-text' }, `${a.name} runs a version of spandeck that joins alone: any station it shares a fleet with stays behind.`) : null,
      h('p', { class: 'muted' }, 'Approve only if you started this: it will be able to control every station.'),
      addJoinName(a),
      h('div', { class: 'row end' }, h('button', { onclick: () => addAnswer(a, false) }, 'Refuse'), h('button', { class: 'primary', onclick: () => addAnswer(a, true, approveBoxes.get(a.request) ? approveBoxes.get(a.request).value.trim() : undefined) }, 'Approve')), log];
    if (a.stage === 'joined') return [h('p', {}, `Approved. Waiting for ${a.station || a.name} to appear…`), log];
    if (a.stage === 'done' && un) return [
      h('p', {}, 'spandeck was uninstalled on ', h('b', {}, a.name), `. ${a.erase ? 'Its history, archive and settings were deleted too.' : 'Its history, archive and settings are still there, and are picked up if you add it back.'} Your project folders there were not touched.`),
      h('p', { class: 'muted' }, `${a.name} is listed under Add a machine again.`), log,
      h('div', { class: 'row end' }, h('button', { class: 'primary', onclick: () => { addClear(); d.close(); } }, 'Done'))];
    if (a.stage === 'done') return [h('p', {}, h('b', {}, a.station || a.name), ' is in your fleet.'),
      h('div', { class: 'row end' }, h('button', { onclick: () => { addClear(); d.close(); } }, 'Done'), h('button', { class: 'primary', onclick: () => { addClear(); d.close(); go(stationUrl(a.station || a.name)); } }, 'Open it'))];
    const steps = un ? UNINSTALL_STEPS : station ? JOIN_STEPS : ADD_STEPS;
    // Waiting for the owner at Tailscale is still the first step.
    const at = a.stage === 'confirm' ? 0 : steps.findIndex(s => s[0] === a.stage);
    return [
      h('p', {}, un ? `Uninstalling spandeck on ${a.name}, as ${a.user}.` : station ? `Asking ${a.name} to join, as ${a.user}.` : `Adding ${a.name} as ${a.user}.`),
      h('ol', { class: 'add-steps' }, steps.map(([, label], i) => h('li', { class: i < at ? 'done' : i === at ? 'now' : '' }, h('span', { 'aria-hidden': 'true' }, i < at ? '✓' : i === at ? '›' : '·'), label + (i === at ? '…' : '') + (i === at && a.stage === 'joining' ? ' (looking for its stations)' : '')))),
      a.stage === 'confirm' ? addConfirm(a) : null,
      // What the steps do not say: the machine cannot download and is sent the release.
      a.note ? h('p', { class: 'add-note' }, a.note) : null,
      log,
      h('div', { class: 'row end' }, h('button', { class: 'danger', onclick: () => addClear() }, 'Stop'), closeBtn('Hide'))];
  }

  // The request this job waits on, as the station lists it: whether its
  // name is taken, and the box for the name it is let in under.
  function addJoinName(a) {
    const r = ((S.trust || {}).requests || []).find(x => x.id === a.request);
    return r ? joinName(r, approveBoxes, a.name_as) : null;
  }

  function redraw() {
    const a = again ? null : S.adding;
    if (a) setLog(a.lines);
    // Only a change of step redraws: the boxes being typed in are left alone.
    const key = JSON.stringify(a ? [a.id, a.stage, a.problem?.kind, a.code, a.note, a.confirm] : [picked, listOpen, target, info && [info.enabled, info.tailscale, info.error, info.machines.map(x => x.id)]]);
    if (key === shown) return;
    shown = key;
    box.replaceChildren(...(a ? view(a) : picker()).flat().filter(Boolean));
    // With the user name already there, the password's box is the one to
    // type in. (A list of selectors finds whichever comes first on the
    // page, which was the user name: the password step opened on it.)
    const first = (a && user.value.trim() && box.querySelector('input[type=password]')) || box.querySelector(a ? 'input' : 'form input');
    // What cannot be undone opens on Cancel, not on a box or the red button.
    const cancel = !a && target && target.kind === 'uninstall' ? box.querySelector('button.cancel') : null;
    if (cancel) cancel.focus(); else if (first) first.focus();
    // What was drawn may be shorter or longer than what it replaced.
    keepInView(document.activeElement);
  }

  const d = dialog(!target ? 'Add a machine' : target.kind === 'uninstall' ? `Uninstall spandeck on ${target.station}?` : `Add ${target.station} to this fleet`, () => box, { onClose: () => { addDlg = null; pass.value = ''; } });
  addDlg = { redraw, line: (data) => { if (S.adding && data.id === S.adding.id) { S.adding.lines = [...(S.adding.lines || []), data.line]; setLog(S.adding.lines); } } };
  redraw();
  if (!S.adding || target) load();
}

const thumbs = new Map();
// The last lines that have something on them (a screen is mostly blank rows).
function thumbText(lines) {
  let end = lines.length;
  while (end > 0 && !lines[end - 1].trim()) end--;
  return lines.slice(Math.max(0, end - 14), end).join('\n');
}
async function loadThumbs(st, panes) {
  await Promise.all(panes.map(async (p) => {
    try { const t = await api.on(st, 'pane.text', { pane: p.id }); thumbs.set(st + '/' + p.id, t.lines); } catch (e) { /* ignore */ }
  }));
  for (const el of document.querySelectorAll('.thumb[data-key]')) {
    const lines = thumbs.get(el.dataset.key);
    if (lines) el.textContent = thumbText(lines);
  }
}

// A pane's icon, with the name of what it is for a pointer that rests on it.
// Whatever row or tile carries it says the same in its own accessible name.
const paneMark = (p) => h('span', { class: 'ico-wrap', title: iconName(p.icon) }, paneIcon(p.icon));
const paneSays = (p) => `${p.name}, ${iconName(p.icon)}, ${STATE_LABEL[stateOf(p)] || stateOf(p)}`;

function paneTile(st, p) {
  const key = st + '/' + p.id;
  return h('a', { class: 'tile pane-tile ' + stateOf(p), href: paneUrl(st, p.id), 'aria-label': paneSays(p), oncontextmenu: (e) => { e.preventDefault(); paneMenu(e, st, p, 'tile'); } },
    h('div', { class: 'tile-head' }, dot(p), paneMark(p), h('b', {}, p.name), p.agent !== 'shell' ? h('span', { class: 'chip' }, p.agent_name) : null, h('span', { class: 'grow' }),
      h('span', { class: 'muted' }, p.tool ? `${p.tool} · ${ago(p.tool_since_ms)}` : `${STATE_LABEL[stateOf(p)] || stateOf(p)} · ${ago(p.state_since_ms)}`)),
    p.needs_reason ? h('div', { class: 'needs-line' }, p.needs_reason) : null,
    rapidLine(p),
    usageBrief(p.usage) ? h('div', { class: 'muted small tile-usage', title: usageTitle(p.usage) }, [usageBrief(p.usage), p.usage.total_tokens ? fmtTokens(p.usage.total_tokens) + ' tokens' : '', p.usage.cost_estimate !== null && p.usage.cost_estimate !== undefined ? 'est. ' + fmtCost(p.usage.cost_estimate) : ''].filter(Boolean).join(' · ')) : null,
    h('pre', { class: 'thumb', dataset: { key } }, thumbText(thumbs.get(key) || [])));
}

// ---- the list on the left ----
// The same on every station: what is pinned, every station with its projects
// (each with its layouts, Watch and panes), then the global layouts. What is
// selected in it is what the right-hand side shows.

// Which groups are folded away is one of the owner's preferences: the same
// on every station and in every browser.
const folded = new Set(prefLocal('side.folded') || []);
function fold(key) {
  if (!folded.delete(key)) folded.add(key);
  setPref('side.folded', [...folded].slice(0, 256));
  render();
}
const caret = (key, what) => h('button', { class: 'caret', type: 'button', 'aria-expanded': String(!folded.has(key)), 'aria-label': `${folded.has(key) ? 'Show' : 'Hide'} ${what}`,
  onclick: (e) => { e.preventDefault(); e.stopPropagation(); fold(key); } }, folded.has(key) ? '▸' : '▾');

function projMenu(e, st, pr) {
  e.preventDefault();
  e.stopPropagation();
  showMenu(e.clientX, e.clientY, [
    { title: pr.address },
    pr.open ? { label: 'Close project (keeps conversations)', action: () => rpc(st, 'project.close', { name: pr.name }) } : { label: 'Open project', action: () => rpc(st, 'project.open', { name: pr.name }) },
    { label: 'New layout…', action: () => newLayoutDialog(st, pr.name) },
    { label: 'New pane in this project…', action: () => newPaneDialog(st, pr.name) },
    { label: 'Tasks, reviews and sign-off', action: () => tasksPanel(st, pr.name).catch(er => toast(er.message, 'error')) },
    hasCap(st, 'folder_trust') && { label: 'Folder trust for this project…', action: () => folderTrustDialog(st, pr.name).catch(er => toast(er.message, 'error')) },
    '-',
    { label: 'Link with another project…', action: () => linkDialog(st, pr) },
    ...linksOf(pr).map(l => ({ label: `Unlink from ${linkName(st, l)}`, action: () => rpc(st, 'project.unlink', { project: pr.name, with: l.with, ...(l.station && l.station !== st ? { station: l.station } : {}) }).then(() => toast(`${pr.name} and ${l.with} are no longer linked`)) })),
    '-',
    { label: 'Remove from this station', danger: true, action: () => confirmBox('Remove ' + pr.name + '?', 'Its panes are closed. The folder and its .spandeck files are not touched.', 'Remove', () => rpc(st, 'project.remove', { name: pr.name })) },
  ]);
}

function sidebar() {
  const r = S.route, ctx = layCtx;
  const current = (on) => (on ? { class: ' current', 'aria-current': 'page' } : { class: '' });

  // `where` says whose a row is when it is shown outside its own group.
  const paneRow = (st, p, where) => {
    const cur = current(r.view === 'pane' && r.station === st && (r.pane === p.id || r.pane === p.name));
    const can = ctx && !p.exited;
    // What kind it is, as a word at the end of its row: the face it opens in.
    const kind = paneKind(st, p);
    return h('a', { class: 'side-row side-pane' + cur.class + (can && ctx.arrange ? ' draggable' : ''), href: paneUrl(st, p.id),
      'aria-label': `${paneSays(p)}, ${KINDS[kind][0]}`, 'aria-current': cur['aria-current'], dataset: { id: p.id, station: st },
      title: can ? (ctx.arrange ? 'Drag into the layout' : `Drag into ${ctx.lay.name}, or click to open`) : undefined,
      onmousedown: (e) => { if (can) sideDrag(e, st, p); },
      onclick: (e) => { if (ctx?.arrange || Date.now() - dragEnded < 300) e.preventDefault(); },
      oncontextmenu: (e) => { e.preventDefault(); e.stopPropagation(); paneMenu(e, st, p, 'side'); } },
    dot(p), paneMark(p), h('span', { class: 'side-name' }, p.name),
    where ? h('span', { class: 'muted side-where' }, where) : null,
    p.paused ? h('span', { class: 'chip warn' }, 'paused') : null,
    p.held && !p.exited ? h('span', { class: 'chip warn', title: heldWords(p.held) }, 'held') : null,
    ctx && ctx.leafOf(st, p)?.hidden ? h('span', { class: 'chip', title: `Hidden in ${ctx.lay.name}` }, 'hidden')
      : can && ctx.arrange && ctx.has(st, p) ? h('span', { class: 'chip' }, 'in layout') : null,
    kindTag(kind));
  };
  // A layout's views, under it: they are in no station's list of panes.
  const viewRows = (lay) => {
    const views = leaves(lay.cache.tree).filter(isView);
    return views.length ? h('div', { class: 'side-kids side-views' }, views.map(n => sideView(lay, n, 'side-row side-view'))) : null;
  };
  const layRow = (lay, where) => {
    const cur = current(r.view === 'layout' && r.layout === lay.key);
    return h('a', { class: 'side-row side-layout' + cur.class, href: layUrl(lay), 'aria-label': `${lay.name}, layout`, 'aria-current': cur['aria-current'], title: layWhere(lay),
      oncontextmenu: (e) => layMenu(e, lay) },
    h('span', { class: 'ico-wrap' }, layoutIcon()), h('span', { class: 'side-name' }, lay.name), where ? h('span', { class: 'muted side-where' }, where) : null,
    h('button', { class: 'icon-btn', type: 'button', 'aria-label': 'Layout menu', onclick: (e) => layMenu(e, lay) }, '⋮'));
  };
  const watchRow = (st, pr) => {
    const cur = current(r.view === 'watch' && r.station === st && r.project === pr.name);
    return h('a', { class: 'side-row side-watch' + cur.class, href: watchUrl(st, pr.name), 'aria-label': `Watch ${pr.name}`, 'aria-current': cur['aria-current'], title: `Every agent in ${pr.name}, and who is talking to whom` },
      h('span', { class: 'ico-wrap' }, watchIcon()), h('span', { class: 'side-name' }, 'Watch'));
  };

  const project = (st, pr) => {
    const key = `p:${st}/${pr.name}`;
    const mine = panesOf(st).filter(p => p.project === pr.name).sort(byState);
    return h('div', { class: 'side-group side-proj' },
      h('div', { class: 'side-row side-project', title: pr.path, oncontextmenu: (e) => projMenu(e, st, pr) }, caret(key, pr.name), h('b', { class: 'side-name' }, pr.name),
        pr.branch ? h('span', { class: 'muted side-where' }, pr.branch) : null, linkedChip(st, pr, true),
        h('button', { class: 'icon-btn', type: 'button', 'aria-label': 'Project menu', onclick: (e) => projMenu(e, st, pr) }, '⋮')),
      folded.has(key) ? null : h('div', { class: 'side-kids' },
        projectLayouts(st).filter(x => x.project === pr.name).map(x => [layRow(x), viewRows(x)]),
        mine.some(p => !p.exited) ? watchRow(st, pr) : null,
        pr.open ? mine.map(p => paneRow(st, p)) : h('button', { class: 'mini side-open', onclick: () => rpc(st, 'project.open', { name: pr.name }) }, 'Open'),
        pr.problem ? h('div', { class: 'warn-text small' }, pr.problem) : null));
  };
  const station = (s) => {
    const st = s.station, up = s.state === 'online', key = 's:' + st;
    const cur = current(r.view === 'station' && r.station === st);
    const loose = up ? panesOf(st).filter(p => !p.project).sort(byState) : [];
    const projects = up ? (S.projects.get(st) || []) : [];
    return h('div', { class: 'side-group side-station-group', dataset: { station: st } },
      h('a', { class: 'side-row side-station' + cur.class + (up ? '' : ' away'), href: stationUrl(st), 'aria-current': cur['aria-current'],
        title: up ? `Every pane running on ${st}` : `${st} is ${STATION_STATE[s.state] || s.state}`, oncontextmenu: (e) => stationMenu(e, s) },
      up ? caret(key, st) : h('span', { class: 'caret' }),
      h('span', { class: 'dot ' + (up ? (s.needs_you ? 'needs_you' : s.working ? 'working' : 'idle') : 'unreachable') }), h('span', { class: 'ico-wrap' }, stationIcon()),
      h('b', { class: 'side-name' }, st),
      up ? (s.self && S.fleet.stations.length > 1 ? h('span', { class: 'muted side-where' }, 'this one') : null) : h('span', { class: 'warn-text side-where' }, STATION_STATE[s.state] || s.state)),
      !up || folded.has(key) ? null : h('div', { class: 'side-kids' },
        projects.map(pr => project(st, pr)),
        loose.length ? h('div', { class: 'side-group side-proj' }, projects.length ? h('div', { class: 'side-row side-project side-other' }, h('span', { class: 'caret' }), h('b', { class: 'side-name' }, 'Other panes')) : null,
          h('div', { class: projects.length ? 'side-kids' : '' }, loose.map(p => paneRow(st, p)))) : null,
        !projects.length && !loose.length ? h('div', { class: 'muted small side-hint' }, 'Nothing is running here.') : null));
  };

  const pins = pinnedItems(), globals = globalLayouts();
  const here = (e) => e.target.closest?.('.side-station-group')?.dataset.station || r.station || api.station;
  return h('aside', { class: 'sidebar', 'aria-label': 'Stations, projects, layouts and panes',
    oncontextmenu: (e) => { e.preventDefault(); const st = online(here(e)) ? here(e) : api.station; showMenu(e.clientX, e.clientY, [
      { label: 'New global layout…', action: () => newLayoutDialog(null, null) },
      { label: 'New pane…', action: () => newPaneDialog(st) }, { label: 'New project…', action: () => newProjectDialog(st) }, { label: 'Check this station', action: () => doctorPanel(st) }]); } },
  pins.length ? h('div', { class: 'side-group side-pinned' }, h('div', { class: 'side-section' }, h('span', { class: 'ico-wrap' }, pinIcon()), h('b', {}, 'Pinned')),
    pins.map(x => x.p ? paneRow(x.st, x.p, `${x.st}${x.p.project ? ' · ' + x.p.project : ''}`) : x.lay ? layRow(x.lay, layShort(x.lay))
      : h('div', { class: 'side-row side-gone muted', title: 'Pinned, but not here now' }, h('span', { class: 'side-name' }, x.gone), h('button', { class: 'icon-btn', type: 'button', 'aria-label': 'Unpin', title: 'Unpin', onclick: () => togglePin(x.pin) }, '✕')))) : null,
  h('div', { class: 'side-group side-stations' }, h('div', { class: 'side-section' }, h('b', {}, 'Stations')), stationsByName().map(station)),
  h('div', { class: 'side-group side-globals' }, h('div', { class: 'side-section' }, h('b', {}, 'Layouts'), h('span', { class: 'grow' }),
    h('button', { class: 'icon-btn', type: 'button', 'aria-label': 'New global layout', title: 'New global layout: the same on every station', onclick: () => newLayoutDialog(null, null) }, '＋')),
  globals.length ? globals.map(x => [layRow(x), viewRows(x)]) : h('div', { class: 'muted small side-hint' }, 'No global layouts yet. A global layout can hold any pane from any station.')),
  h('div', { class: 'side-actions' }, h('button', { onclick: () => newPaneDialog(online(r.station) ? r.station : api.station) }, '＋ Pane'), h('button', { onclick: () => newProjectDialog(online(r.station) ? r.station : api.station) }, '＋ Project')),
  dotKeyLink());
}
// What kind a pane is, by the face it opens in here: a word (ui.js, KINDS).
const paneKind = (st, p) => (faceOf(st, p) === 'chat' ? 'chat' : 'term');
// The key to the dots, at the foot of the list.
const dotKeyLink = () => h('button', { type: 'button', class: 'dot-key-link', dataset: { keep: 'dot-key' }, onclick: (e) => { e.currentTarget.blur(); dotKeyDialog(); } }, h('span', { class: 'dot', 'aria-hidden': 'true' }), 'What the dots mean');
// One view of a layout as a row: its agents' state for a Watch, what it
// shows, and its kind. It opens the layout at that view.
function sideView(lay, n, cls) {
  const kind = viewKind(n), state = viewState(lay, n), name = viewName(lay, n);
  return h('a', { class: cls, href: layUrl(lay), dataset: { view: n.id },
    'aria-label': `${name}, ${(KINDS[kind] || [kind])[0]}, in the layout ${lay.name}${state !== 'idle' ? ', ' + (STATE_LABEL[state] || state) : ''}${n.hidden ? ', hidden' : ''}`,
    onclick: () => { viewWanted = { key: lay.key, id: n.id }; phoneAt.set(lay.key, ctxFor(lay).refOf(n)); if (location.hash === layUrl(lay)) setTimeout(render, 0); } },
  h('span', { class: 'dot ' + state }), h('span', { class: 'ico-wrap' }, viewIcon(kind)), h('span', { class: 'side-name' }, name),
  n.hidden ? h('span', { class: 'chip', title: `Hidden in ${lay.name}` }, 'hidden') : null, kindTag(kind));
}

// A terminal has been attached again (the page reconnected, or its pane's
// station came back): the one that had the keyboard has it again, without a
// click. (The audit: in a layout the terminal lost the keyboard after a
// reconnect.) Never taken from a dialog, a menu or a box someone is typing in.
function keyboardBack(st, term) {
  if (S.focused !== `${st}/${term.pane.id}` || !term.control) return;
  const a = document.activeElement;
  if (document.querySelector('.dialog') || document.querySelector('.menu')) return;
  if (a && a !== document.body && !term.el.contains(a)) return;
  if (!term.el.isConnected) return;
  term.focus();
}

function getTerm(st, p, where, mode) {
  const key = `${where}:${st}/${p.id}`;
  let t = S.terms.get(key);
  if (!t) {
    const lost = where !== 'phone-fit' ? null : () => { const k = `${st}/${p.id}`; phoneFit.delete(k); phoneAuto.delete(k); phoneBefore.delete(k); phoneMine.delete(k); phoneTaken.add(k); update(); };
    t = new PaneTerm(api, st, p, { mode, typeWhileWatching: where === 'phone', natural: where === 'phone', flow: where === 'phone' || where === 'phone-fit', onLost: lost, face: where === 'leaf' || where === 'zoom' ? (term) => chatToggle(st, term.pane) : null, agentsOf: (pane) => agentRows(st, pane), onMenu: (e, term) => paneMenu(e, st, term.pane, where, term), onUsage: (term) => usageDialog(st, term.pane), onFiles: (term) => (hasCap(st, 'files') ? () => filesFor(st, term.pane) : null), onFocus: (term) => { S.focused = `${st}/${term.pane.id}`; }, onAttached: (term) => keyboardBack(st, term) });
    S.terms.set(key, t);
    t._fresh = true;
    t.setLink(linkPaneWords(api.link()));
  } else t.update(p);
  t._used = true;
  return t;
}

// ---- who works for whom, and who is talking ----

// The agents under a pane: helpers it gave tasks to (and theirs), and each
// agent's own running sub-agents.
function agentRows(st, p, depth = 0, seen = new Set()) {
  if (seen.has(p.id) || depth > 4) return [];
  seen.add(p.id);
  const rows = (p.subagents || []).map(a => ({ depth, name: a.kind, detail: 'its own sub-agent', state: 'working', activity: ago(a.since_ms) }));
  for (const c of panesOf(st).filter(x => x.parent_pane === p.id && !x.exited)) {
    const task = (S.tasks.get(st) || []).find(t => t.id === c.task);
    // Beside this one when a layout is on screen, else on its own.
    rows.push({ depth, name: c.name, detail: [usageBrief(c.usage), task ? `${c.role || 'helper'} · ${task.goal.slice(0, 60)}` : (c.role || 'helper')].filter(Boolean).join(' · '), state: stateOf(c),
      activity: c.tool ? `${c.tool} · ${ago(c.tool_since_ms)}` : (STATE_LABEL[stateOf(c)] || stateOf(c)), open: () => (layCtx && !layCtx.has(st, c) ? addToLayout(layCtx.lay, st, c, 'row') : go(paneUrl(st, c.id))) });
    rows.push(...agentRows(st, c, depth + 1, seen));
  }
  return rows;
}

// An address in a message ("dev", "proj/dev", "p-123@spark2") to the pane it names.
// What comes after an @ is a station, or (between linked projects) a project.
// `among` are the projects on screen: a bare name that is one pane's name
// among them is that pane, which is how a message to a linked project resolves.
function paneAt(st, addr, project, among) {
  if (!addr) return null;
  const raw = String(addr), a = raw.replace(/@[^@]*$/, '');
  const live = panesOf(st).filter(p => !p.exited);
  const named = live.find(p => p.id === a) || live.find(p => p.project && (`${p.project}/${p.name}` === a || `${p.project}/${p.name}` === raw || `${p.name}@${p.project}` === raw))
    || live.find(p => p.name === a && (!project || p.project === project));
  if (named || !among) return named || null;
  const same = live.filter(p => p.name === a && among.has(p.project));
  return same.length === 1 ? same[0] : null;
}

// Lines for a set of panes: a task given, and messages in the last ten minutes.
function linksFor(st, panes) {
  const ids = new Set(panes.map(p => p.id)), out = new Map();
  for (const c of panes) {
    if (c.parent_pane && ids.has(c.parent_pane)) {
      const task = (S.tasks.get(st) || []).find(t => t.id === c.task);
      out.set(`${c.parent_pane}>${c.id}`, { from: c.parent_pane, to: c.id, kind: 'task', label: 'task' + (task ? ` · ${task.goal.slice(0, 28)}` : ''), live: false });
    }
  }
  const now = Date.now(), among = new Set(panes.map(p => p.project).filter(Boolean));
  for (const m of (S.msgs.get(st) || []).slice().reverse()) {
    if (now - m.ts > 600000) continue;
    const from = paneAt(st, m.from ?? m.sender, m.project, among), to = paneAt(st, m.to ?? m.recipient, m.project, among);
    if (!from || !to || from.id === to.id || !ids.has(from.id) || !ids.has(to.id)) continue;
    out.set(`${from.id}>${to.id}:m`, { from: from.id, to: to.id, kind: 'message', label: `${m.kind} · ${ago(m.ts)} ago`, live: now - m.ts < 45000 });
  }
  return [...out.values()];
}

// Draw the lines over `box`, between the elements that show each pane.
function drawLinks(box, links) {
  box.querySelectorAll(':scope > .links, :scope > .link-label').forEach(x => x.remove());
  if (!links.length) return;
  const b = box.getBoundingClientRect();
  const NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('class', 'links');
  svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('viewBox', `0 0 ${b.width} ${b.height}`);
  const labels = [];
  // Two lines between the same pair run side by side, labels apart.
  const pairOf = (l) => [l.from, l.to].sort().join('|');
  const total = new Map(), nth = new Map();
  for (const l of links) total.set(pairOf(l), (total.get(pairOf(l)) || 0) + 1);
  for (const l of links) {
    const fe = box.querySelector(`[data-pane="${l.from}"]`), te = box.querySelector(`[data-pane="${l.to}"]`);
    if (!fe || !te) continue;
    const f = fe.getBoundingClientRect(), t = te.getBoundingClientRect();
    // From the edge of one to the edge of the other, so the line is not hidden behind them.
    const fc = [f.left + f.width / 2 - b.left, f.top + f.height / 2 - b.top], tc = [t.left + t.width / 2 - b.left, t.top + t.height / 2 - b.top];
    const edge = (r, c, o) => { const dx = o[0] - c[0], dy = o[1] - c[1]; const k = Math.min(Math.abs(dx) < 1 ? 9e9 : (r.width / 2) / Math.abs(dx), Math.abs(dy) < 1 ? 9e9 : (r.height / 2) / Math.abs(dy), 1); return [c[0] + dx * k, c[1] + dy * k]; };
    const k = pairOf(l), i = nth.get(k) || 0, n = total.get(k);
    nth.set(k, i + 1);
    const side = (i - (n - 1) / 2) * 16, spread = 0.5 + (i - (n - 1) / 2) * 0.36;
    // Always measured the same way round, so the two lines of a pair do not cross.
    const along = l.from > l.to ? 1 - spread : spread;
    const swap = l.from > l.to, d = swap ? [fc[0] - tc[0], fc[1] - tc[1]] : [tc[0] - fc[0], tc[1] - fc[1]];
    const len = Math.hypot(d[0], d[1]) || 1, nx = -d[1] / len * side, ny = d[0] / len * side;
    const e1 = edge(f, fc, tc), e2 = edge(t, tc, fc);
    const p1 = [e1[0] + nx, e1[1] + ny], p2 = [e2[0] + nx, e2[1] + ny];
    const mid = [p1[0] + (p2[0] - p1[0]) * along, p1[1] + (p2[1] - p1[1]) * along];
    const path = document.createElementNS(NS, 'path');
    path.setAttribute('d', `M ${p1[0]} ${p1[1]} L ${p2[0]} ${p2[1]}`);
    path.setAttribute('class', `link ${l.kind}${l.live ? ' live' : ''}`);
    svg.append(path);
    const dot = document.createElementNS(NS, 'circle');
    dot.setAttribute('cx', p2[0]); dot.setAttribute('cy', p2[1]); dot.setAttribute('r', 4);
    dot.setAttribute('class', `link-end ${l.kind}`);
    svg.append(dot);
    labels.push(h('div', { class: `link-label ${l.kind}${l.live ? ' live' : ''}`, style: { left: mid[0] + 'px', top: mid[1] + 'px' } },
      `${findPaneAny(l.from)?.name || '?'} → ${findPaneAny(l.to)?.name || '?'} · ${l.label}`));
  }
  box.append(svg, ...labels);
}
const findPaneAny = (id) => { for (const list of S.panes.values()) { const p = list.find(x => x.id === id); if (p) return p; } return null; };
let pendingLinks = null; // { selector, links } drawn after the next render

// ---- Watch: a project's agents, and what they say to each other ----
// (The owner, 18 Nov 2026, of the map of cards and lines this replaced: "Hard
// to tell what's really happening between agents".) At the top a row for
// each agent: its state and one sentence of what it is doing. Then who is
// waiting for whom, where that can be told. Then the messages as one
// thread, the newest at the bottom.
const watchOpen = new Set(); // messages whose long text is shown whole
// What each Watch on the page holds, by its key: every message seen while it
// is open (the station's own list is its newest 60, of all its projects) and
// what "Earlier messages" asked for. There is the Watch a project is opened
// as, and since 20 Nov 2026 a Watch in a cell of a layout (viewpanes.js), so
// there can be more than one; one that has left the page is let go.
const watchKept = new Map();
let watchDrawn = new Map(); // key -> where its thread was, for after the draw
const WATCH_FIRST = 60; // what the refresh asks for (`msg.recent`, `deck.snapshot`)
const watchThread = (key) => [...document.querySelectorAll('.watch-thread')].find(t => t.dataset.key === key) || null;

function watchHold(st, key) {
  let k = watchKept.get(key);
  if (!k) { k = { key, byId: new Map(), slots: new Map(), limit: 0, more: null, asking: false, prepend: false, atEnd: true }; watchKept.set(key, k); }
  for (const m of S.msgs.get(st) || []) if (m && m.id) k.byId.set(m.id, m);
  return k;
}
// The station is asked for more of its newest messages (it has no "before
// this one"): of every project, since what the owner sent is in none.
async function watchEarlier(st, k) {
  if (k.asking) return;
  k.asking = true;
  update();
  const limit = k.limit ? k.limit * 2 : WATCH_FIRST * 4;
  try {
    const rows = await api.on(st, 'msg.recent', { limit }, { within: 10000 });
    for (const m of Array.isArray(rows) ? rows : []) if (m && m.id) k.byId.set(m.id, m);
    k.limit = limit;
    k.more = Array.isArray(rows) && rows.length >= limit;
    k.prepend = true;
  } catch (e) { toast(`${st} did not give its earlier messages: ${e.message}`, 'error'); }
  k.asking = false;
  if (watchKept.get(k.key) === k) update();
}

const watchClock = (ts) => {
  const d = new Date(Number(ts) || 0), t = d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  return d.toDateString() === new Date().toDateString() ? t : `${d.toLocaleDateString([], { day: 'numeric', month: 'short' })} ${t}`;
};
// One sentence of what an agent is doing, from what its pane says of itself.
function watchDoes(st, p) {
  const s = stateOf(p), one = (t) => String(t).trim().split('\n')[0].slice(0, 200);
  if (s === 'needs_you') return 'Needs you: ' + (p.needs_reason || 'it is waiting for you');
  if (s === 'error') return 'Stopped on an error' + (p.needs_reason ? ': ' + p.needs_reason : '');
  if (p.paused) return 'Paused';
  const now = s !== 'working' ? '' : p.tool ? `Using ${p.tool} · ${ago(p.tool_since_ms)}` : (p.subagents || []).length ? `Running ${p.subagents.length === 1 ? 'a sub-agent' : p.subagents.length + ' sub-agents'} of its own` : '';
  const task = p.task ? (S.tasks.get(st) || []).find(t => t.id === p.task) : null, boss = p.parent_pane ? panesOf(st).find(x => x.id === p.parent_pane) : null;
  // After a turn that ended by itself the pane holds the agent's last words;
  // during one, or after one that was stopped, it may hold what it was asked.
  const what = task ? `${p.role || 'helper'}${boss ? ' for ' + boss.name : ''}: ${one(task.goal)}` : s === 'done' && p.last_message ?'Last said: ' + one(p.last_message) : p.title && p.title !== p.name ? one(p.title) : '';
  return [now, what].filter(Boolean).join(' · ');
}

// `opts.at`: which Watch this is, when it is not the project's own page (a
// layout's cell). `opts.open(p)`: where a tap on an agent goes, instead of
// to its pane.
function watchView(st, project, opts = {}) {
  const live = panesOf(st).filter(p => !p.exited);
  // Projects linked with this one on the same station: their agents are shown
  // too, marked with their project, since the two message each other.
  const pr = (S.projects.get(st) || []).find(x => x.name === project);
  const linked = [...new Set(linksOf(pr).filter(l => (!l.station || l.station === st) && l.with !== project).map(l => l.with))];
  const among = new Set([project, ...linked]);
  const byName = (name) => live.filter(p => p.project === name && p.agent !== 'shell').sort((a, b) => a.name.localeCompare(b.name));
  const agents = [project, ...linked].flatMap(byName);
  const k = watchHold(st, `${st}/${project}${opts.at ? '#' + opts.at : ''}`);
  const opens = (p) => (opts.open ? { onclick: (e) => { if (e.button || e.metaKey || e.ctrlKey) return; e.preventDefault(); opts.open(p); } } : {});
  // An agent keeps its side and its colour while this Watch is open, whoever
  // starts or ends beside it.
  for (const p of agents) if (!k.slots.has(p.id)) k.slots.set(p.id, k.slots.size);
  const nth = new Map(agents.map(p => [p.id, k.slots.get(p.id)]));
  const label = (p) => (p.project !== project ? `${p.project}/${p.name}` : p.name);
  // Who an address in a message is: an agent on screen, the owner, or the
  // words as they are. A bare name or an id that two panes could be is nobody.
  const who = (addr, m) => {
    const raw = String(addr ?? ''), a = raw.replace(/@[^@]*$/, '');
    if (a === 'you') return { you: true, name: 'you' };
    const p = m.project ? paneAt(st, raw, m.project, among)
      : (c => (c.length === 1 ? c[0] : null))(live.filter(x => x.id === a || x.name === a || (x.project && `${x.project}/${x.name}` === a)));
    if (p && nth.has(p.id)) return { pane: p, name: label(p) };
    const shown = raw.endsWith('@' + st) ? a : raw;
    return { name: (shown.startsWith(project + '/') ? shown.slice(project.length + 1) : shown) || '?' };
  };
  const msgs = [...k.byId.values()].map(m => ({ m, from: who(m.from ?? m.sender, m), to: who(m.to ?? m.recipient, m) }))
    .filter(x => (x.m.project ? among.has(x.m.project) : !!(x.from.pane || x.to.pane)))
    .sort((a, b) => (a.m.ts - b.m.ts) || ((a.m.seq || 0) - (b.m.seq || 0)));
  // Drawn newest first (the owner, 18 Nov 2026): the latest message is at the
  // top, where the eye is, and earlier ones are below it.
  const shown = [...msgs].reverse();
  // More may be held by the station when its list came back full.
  const more = k.more ?? (S.msgs.get(st) || []).length >= WATCH_FIRST;

  // Who waits for whom, said only where it is known: the last word between
  // two agents was A's, A has stopped, and B is at work (or waits for the
  // owner). A result ends a job and waits for nothing.
  const last = new Map();
  for (const x of msgs) if (x.from.pane && x.to.pane && x.from.pane.id !== x.to.pane.id) last.set([x.from.pane.id, x.to.pane.id].sort().join('|'), x);
  const waits = [...last.values()].map(({ m, from, to }) => {
    const a = stateOf(from.pane), b = stateOf(to.pane);
    if (m.kind === 'result' || (a !== 'idle' && a !== 'done') || (b !== 'working' && b !== 'needs_you')) return null;
    return h('div', { class: 'watch-wait' + (b === 'needs_you' ? ' attn' : ''), title: `${from.name}'s last message to ${to.name} was ${ago(m.ts)} ago and has no answer yet; ${from.name} has stopped and ${to.name} ${b === 'working' ? 'is working' : 'needs you'}.` },
      h('b', {}, from.name), ' is waiting for ', h('b', {}, to.name), b === 'needs_you' ? ', which needs you' : '', h('span', { class: 'muted' }, ` · ${ago(m.ts)}`));
  }).filter(Boolean);

  const row = (p) => {
    const s = stateOf(p), u = p.usage, does = watchDoes(st, p);
    const cost = u && u.cost_estimate !== null && u.cost_estimate !== undefined ? 'est. ' + fmtCost(u.cost_estimate) : u && u.total_tokens ? fmtTokens(u.total_tokens) + ' tokens' : '';
    return h('a', { class: `watch-agent ${s} c${nth.get(p.id) % 4}` + (p.project !== project ? ' guest' : ''), href: paneUrl(st, p.id), role: 'listitem', dataset: opts.at ? { agent: p.id } : { pane: p.id }, 'aria-label': `Open ${paneSays(p)}`, ...opens(p), oncontextmenu: (e) => { e.preventDefault(); paneMenu(e, st, p, 'tile'); } },
      h('div', { class: 'watch-agent-head' }, dot(p), h('b', { class: 'watch-agent-name' }, p.name),
        h('span', { class: 'muted small' }, `${p.agent_name || p.agent} · ${STATE_LABEL[s] || s} ${ago(p.state_since_ms)}`),
        p.project !== project ? h('span', { class: 'chip guest', title: `This agent is in ${p.project}, which is linked with ${project}` }, 'from ' + p.project) : null),
      does || cost ? h('div', { class: 'watch-does small' + (s === 'needs_you' || s === 'error' ? ' attn' : ' muted') }, h('span', { class: 'watch-does-text' }, does), cost ? h('span', { class: 'watch-cost', title: usageTitle(u) }, does ? ' · ' + cost : cost) : null) : null);
  };

  // Whether the thread is at its newest message is known from the owner's
  // own scrolling, not measured at a draw: a screen that got shorter (the
  // keyboard, the phone turned) has moved nothing.
  // And a scroll that comes with a change of room (the browser keeping its
  // place in a thread that shrank) is not the owner's.
  const atEnd = (e, sure) => {
    const t = watchThread(k.key);
    if (!t) return;
    if (sure || k.room === watchRoomOf(t)) k.atEnd = t.scrollTop < 48;
    else if (k.atEnd) t.scrollTop = 0;
    k.room = watchRoomOf(t);
  };
  const msg = ({ m, from, to }) => {
    const i = from.pane ? nth.get(from.pane.id) : -1, open = watchOpen.has(m.id);
    const body = h('div', { class: 'watch-body pre' + (open ? '' : ' folded') }, String(m.body ?? ''));
    // Shown after the draw, if the text is longer than the fold (watchAfterRender).
    const fold = h('button', { type: 'button', class: 'mini watch-more', hidden: !open, 'aria-expanded': String(open), onclick: () => {
      const now = !watchOpen.has(m.id);
      if (now) watchOpen.add(m.id); else watchOpen.delete(m.id);
      body.classList.toggle('folded', !now);
      fold.textContent = now ? 'less' : 'more';
      fold.setAttribute('aria-expanded', String(now));
      atEnd(null, true);
    } }, open ? 'less' : 'more');
    return h('div', { class: 'watch-msg ' + (from.you ? 'you right' : i < 0 ? 'left' : `c${i % 4} ${i % 2 ? 'right' : 'left'}`) + (to.you ? ' to-you' : ''), dataset: { msg: m.id } },
      h('div', { class: 'watch-msg-head small' }, h('b', {}, from.name), h('span', { class: 'muted', 'aria-label': 'to' }, '→'), h('b', {}, to.name),
        m.kind && m.kind !== 'note' ? h('span', { class: 'chip' }, m.kind) : null,
        h('span', { class: 'muted watch-time', title: new Date(Number(m.ts) || 0).toLocaleString() }, watchClock(m.ts))),
      h('div', { class: 'watch-bubble' }, body, fold));
  };

  const old = watchThread(k.key);
  watchDrawn.set(k.key, { atEnd: k.atEnd, fromEnd: old ? old.scrollHeight - old.scrollTop : 0, prepend: k.prepend });
  k.prepend = false;
  const where = linked.length ? `${project} or ${linked.join(', ')}` : project;
  const earlier = (words) => h('button', { type: 'button', class: 'watch-earlier', disabled: k.asking, onclick: () => watchEarlier(st, k) }, k.asking ? 'Asking…' : words);
  return h('div', { class: 'watch' },
    h('div', { class: 'watch-side' },
      h('div', { class: 'watch-strip', role: 'list', 'aria-label': `Agents in ${project}` }, agents.length ? agents.map(row) : h('p', { class: 'muted watch-none' }, `No agent is running in ${where}.`)),
      agents.length ? h('div', { class: 'watch-open' }, agents.map(p => h('a', { class: 'btn', href: paneUrl(st, p.id), ...opens(p) }, 'Open ' + p.name))) : null),
    h('div', { class: 'watch-talk' },
      waits.length ? h('div', { class: 'watch-waits', role: 'status' }, waits) : null,
      h('div', { class: 'watch-thread', dataset: { key: k.key }, onscroll: atEnd, role: 'log', 'aria-label': `Messages between the agents in ${project}`, tabindex: '0' },
        msgs.length ? shown.map(msg)
          : h('div', { class: 'muted watch-none' }, h('p', {}, `No messages yet. What the agents in ${where} say to each other, and what you and they say to one another, is shown here.`), more ? earlier('Look further back') : null),
        msgs.length && more ? earlier('Earlier messages') : null,
        msgs.length && !more && k.limit ? h('p', { class: 'muted small watch-first' }, `These are all the messages ${st} holds.`) : null)));
}
// After the draw: "more" where a text is longer than its fold, and the thread
// at its newest message unless the owner has scrolled up to read.
// A thread whose room changed (the keyboard, the phone turned, the bar above
// it wrapping) is put back at its newest message if that is where it was.
const watchRoomOf = (t) => `${t.clientHeight}/${t.scrollHeight}`;
const watchRoom = typeof ResizeObserver === 'function' ? new ResizeObserver((all) => {
  for (const { target: t } of all) {
    const k = watchKept.get(t.dataset.key);
    if (!k) continue;
    if (k.atEnd) t.scrollTop = 0;
    k.room = watchRoomOf(t);
  }
}) : null;
function watchAfterRender() {
  const drawn = watchDrawn;
  watchDrawn = new Map();
  watchRoom?.disconnect();
  for (const key of [...watchKept.keys()]) if (!drawn.has(key)) watchKept.delete(key);
  for (const [key, w] of drawn) {
    const box = watchThread(key);
    if (!box) continue;
    watchRoom?.observe(box);
    for (const b of box.querySelectorAll('.watch-body.folded')) b.nextSibling.hidden = b.scrollHeight <= b.clientHeight + 1;
    // At the newest message it stays there. Reading further down, a message
    // that arrives above must not move what is being read: the distance from
    // the end is kept (Safari does not anchor a scroll by itself). Earlier
    // messages are added below, which moves nothing.
    if (w.atEnd) box.scrollTop = 0;
    else if (!w.prepend && w.fromEnd) box.scrollTop = box.scrollHeight - w.fromEnd;
    watchKept.get(key).room = watchRoomOf(box);
  }
}

// ---- layouts ----
// A layout is a tree of splits whose leaves name panes. A project's layouts
// are saved in its project file, on its station; a global layout is saved in
// the deck document, which every station holds. Either may show any pane:
// one in a project by its name there ({pane, project?, station?}), one in no
// project by its id ({id, station?}). A leaf never names another layout.
//
// A leaf can also be a view, which runs nothing (the owner's request, 20 Nov
// 2026): the file browser, or Watch. It is written as a pane named by id
// is, with an id no pane has ({id: "view.files.k3f9", station?, folder?} and
// {id: "view.watch.x1", station?, watch?}), and what shows it is in
// viewpanes.js. Whether it is hidden is on the leaf itself, not in `hidden`.
//
// A pane of a layout can be hidden (the owner's request, 15 Nov 2026). The
// tree is not changed: the leaf carries `hidden`, and what is drawn leaves it
// out, so its neighbours take its room and showing it puts it back where it
// was, at the sizes the splits had. Hidden panes are saved with the layout as
// a list beside the tree (`hidden`), each named as its leaf is.

const layCache = new Map();    // layout key -> { tree, lines, json, dirty, timer, v }
let arranging = null;          // the key of the layout being arranged
let layCtx = null;             // the layout on screen: how it names, finds and saves its panes
let dragState = null;
let dragEnded = 0;             // when the last drag from the list finished
let autoArrange = false;       // Arrange was turned on by a drag from the list, and ends with it

const isLeaf = (n) => !!n && n.dir === undefined;
const leaves = (n, out = []) => { if (!n) return out; if (isLeaf(n)) out.push(n); else { leaves(n.a, out); leaves(n.b, out); } return out; };
const cleanLeaf = (n) => isView(n) ? cleanView(n) : n.id !== undefined ? { id: n.id, ...(n.station ? { station: n.station } : {}) }
  : { pane: n.pane, ...(n.project ? { project: n.project } : {}), ...(n.station ? { station: n.station } : {}) };
// What a leaf names, for finding it in `hidden` (a view's folder and whether it is hidden are not its name).
const leafSig = (n) => JSON.stringify(isView(n) ? { id: n.id, ...(n.station ? { station: n.station } : {}) } : cleanLeaf(n));
// Is any of this part on screen?
const shows = (n) => !!n && (isLeaf(n) ? !n.hidden : shows(n.a) || shows(n.b));
const fromServer = (n) => !n ? null : isLeaf(n) ? cleanLeaf(n) : { dir: n.dir, ratio: n.ratio / 1000, a: fromServer(n.a), b: fromServer(n.b) };
const toServer = (n) => !n ? null : isLeaf(n) ? cleanLeaf(n)
  : { dir: n.dir, ratio: Math.min(950, Math.max(50, Math.round(n.ratio * 1000))), a: toServer(n.a), b: toServer(n.b) };

// A saved layout's tree, its hidden panes marked; and a layout as it is saved.
function treeFrom(def) {
  const tree = fromServer(def.tree), hid = new Set((Array.isArray(def.hidden) ? def.hidden : []).filter(isLeaf).map(leafSig));
  for (const n of leaves(tree)) if (hid.has(leafSig(n))) n.hidden = true;
  return tree;
}
function layoutBody(name, c) {
  const hidden = leaves(c.tree).filter(n => n.hidden && !isView(n)).map(cleanLeaf);
  return { name, lines: !!c.lines, tree: toServer(c.tree), ...(hidden.length ? { hidden } : {}) };
}

// One layout as the deck holds it. `station` and `project` are empty for a
// global layout. What is on screen is kept while a change is being saved.
function layoutOf(key, station, project, def) {
  const json = JSON.stringify(def);
  let c = layCache.get(key);
  if (!c) { c = { tree: treeFrom(def), lines: !!def.lines, json }; layCache.set(key, c); }
  else if (!c.dirty && c.json !== json) { c.tree = treeFrom(def); c.lines = !!def.lines; c.json = json; }
  return { key, station, project, name: def.name, cache: c };
}
const projectLayouts = (st) => (S.projects.get(st) || []).flatMap(pr => (pr.definition?.layout || []).map(l => layoutOf(`${st}/${pr.name}/${l.name}`, st, pr.name, l)));
const globalLayouts = () => (S.deck.layouts || []).map(l => layoutOf('global:' + l.name, null, null, l));
const allLayouts = () => [...onlineStations().flatMap(projectLayouts), ...globalLayouts()];
const findLayout = (key) => allLayouts().find(x => x.key === key) || null;
const layUrl = (lay) => lay.project ? layoutUrl(lay.station, lay.project, lay.name) : globalUrl(lay.name);
const layShort = (lay) => lay.project ? `${lay.project} on ${lay.station}` : 'every station';
const layWhere = (lay) => lay.project ? `Saved with ${lay.project} on ${lay.station}` : 'Global layout · the same on every station';
const layPin = (lay) => lay.project ? { kind: 'layout', station: lay.station, project: lay.project, name: lay.name } : { kind: 'layout', name: lay.name };
const VALID_NAME = '[A-Za-z0-9._-]+';
// A name nothing in `taken` has: base, base-2, base-3…
const freeName = (base, taken) => { let n = base, i = 1; while (taken.includes(n)) n = `${base}-${i += 1}`; return n; };

function saveLayout(lay) {
  const c = lay.cache;
  c.dirty = true;
  const v = c.v = (c.v || 0) + 1;
  clearTimeout(c.timer);
  c.timer = setTimeout(async () => {
    const layout = layoutBody(lay.name, c);
    try {
      await (lay.project ? api.on(lay.station, 'project.layout.save', { project: lay.project, layout }) : api.call('deck.layout.save', { layout }));
      await refresh();
    } catch (e) { toast(e.message, 'error'); }
    // What was saved is on the page now; from here the saved copy is shown.
    if (c.v === v) { c.dirty = false; c.json = null; update(); }
  }, 250);
}

// How a layout names and finds a pane. A project's layout is kept on one
// station, and a leaf with no station is on that one. A global layout is the
// same on every station, so each of its leaves names its station; one that
// does not (saved through the API, or by an older deck, for which it meant
// "the station this deck is served by") is on no station until it is given
// the one its pane is found on (bindGlobalLeaves). It is never read as the
// serving station: that made one layout show different panes on each station.
const leafStation = (lay, n) => n.station || lay.station || null;
// The stations a leaf with no station could mean: those where its pane is.
const leafCandidates = (n) => onlineStations().filter(st => n.id !== undefined ? panesOf(st).some(p => p.id === n.id)
  : panesOf(st).some(p => !p.exited && (p.project || null) === (n.project || null) && p.name === n.pane)
    || (!!n.project && (S.projects.get(st) || []).some(pr => pr.name === n.project && (pr.definition?.pane || []).some(x => x.name === n.pane))));
// Give each such leaf its station, once, and save: from then on the layout
// names the same panes wherever it is opened. A pane's id is its own on every
// station, so a pane in no project is bound where it is found. A named pane
// could be in a project of the same name on a station that is away, so it is
// bound only when one station has it and every station of the fleet answers.
// Otherwise the leaf waits, and says so where it would be shown.
const bindTried = new Set(); // a save that failed is not tried again by itself
function bindGlobalLeaves() {
  if (!S.loaded) return;
  const allHere = !fleetAway().length && !S.busy.size && onlineStations().every(st => S.panes.has(st) && S.projects.has(st));
  for (const def of S.deck.layouts || []) {
    const lay = findLayout('global:' + def.name);
    if (!lay || lay.cache.dirty || bindTried.has(JSON.stringify(def))) continue;
    let bound = 0;
    for (const n of leaves(lay.cache.tree)) {
      if (n.station) continue;
      const where = leafCandidates(n);
      if (where.length === 1 && (n.id !== undefined || allHere)) { n.station = where[0]; bound += 1; }
    }
    if (bound) { bindTried.add(JSON.stringify(def)); saveLayout(lay); }
  }
}
function leafPane(lay, n) {
  const st = leafStation(lay, n);
  if (n.id !== undefined) return panesOf(st).find(p => p.id === n.id) || null;
  const project = n.project || lay.project || null;
  return panesOf(st).find(p => !p.exited && (p.project || null) === project && p.name === n.pane) || null;
}
// A leaf's name within its layout, short where it can be: "dev",
// "web/logs" (another project), "spark2:web/logs" (another station),
// "#p-01abc" (a pane in no project).
function leafRef(lay, n) {
  const st = lay.station ? (n.station && n.station !== lay.station ? n.station : '') : (n.station || '');
  const at = st ? st + ':' : '';
  if (n.id !== undefined) return `${at}#${n.id}`;
  return at + (n.project && (n.project !== lay.project || st) ? n.project + '/' : '') + n.pane;
}
function refLeaf(ref) {
  const c = ref.indexOf(':'), station = c < 0 ? {} : { station: ref.slice(0, c) };
  const rest = ref.slice(c + 1);
  if (rest[0] === '#') return { id: rest.slice(1), ...station };
  const i = rest.indexOf('/');
  return { pane: rest.slice(i + 1), ...(i < 0 ? {} : { project: rest.slice(0, i) }), ...station };
}
// The leaf that puts pane `p` of station `st` in this layout.
function paneLeaf(lay, st, p) {
  const station = lay.station === st ? {} : { station: st };
  if (!p.project) return { id: p.id, ...station };
  return { pane: p.name, ...(p.project !== lay.project || station.station ? { project: p.project } : {}), ...station };
}
// Where a pane is from, when that is not the layout's own project or station.
function leafGuest(lay, n) {
  const st = leafStation(lay, n);
  const project = n.id !== undefined ? null : (n.project || lay.project || null);
  // In a global layout every pane says its station, whichever station serves
  // the deck; in a fleet of one there is nothing to tell apart.
  const elsewhere = st && st !== lay.station && (lay.station || S.fleet.stations.filter(s => s.state !== 'not_linked').length > 1);
  return [project && project !== lay.project ? 'from ' + project : '', elsewhere ? 'on ' + st : ''].filter(Boolean).join(' ') || null;
}

function ctxFor(lay) {
  const refOf = (n) => leafRef(lay, n);
  const set = (tree) => { lay.cache.tree = tree; saveLayout(lay); };
  const tree = () => lay.cache.tree;
  const label = (n) => isView(n) ? viewLabel(lay, n) : n.pane !== undefined ? n.pane : (leafPane(lay, n)?.name || 'closed pane');
  const ctx = {
    lay, key: lay.key, arrange: arranging === lay.key, refOf, tree, label,
    stationOf: (n) => leafStation(lay, n),
    resolve: (n) => { const p = leafPane(lay, n); return p ? { st: leafStation(lay, n), p } : null; },
    guest: (n) => leafGuest(lay, n),
    emptyText: 'This layout is empty. Press Add pane, or drag a pane here from the list on the left.',
    missing: (n) => {
      // Not a pane at all: Files or Watch, in this cell.
      if (isView(n)) return viewCell(ctx, n, 'leaf');
      const st = leafStation(lay, n), project = n.pane !== undefined ? (n.project || lay.project) : null;
      // Saved without a station, and not yet given one: it is not guessed.
      if (!st) {
        const where = leafCandidates(n), give = (to) => { n.station = to; saveLayout(lay); render(); };
        return h('div', { class: 'empty' }, h('p', {}, `${label(n)}${project ? ' in ' + project : ''} was saved in this layout without its station. `,
          where.length > 1 ? 'More than one station has such a pane: say which it is.' : where.length ? `${where[0]} has such a pane, but a station that is not reachable may too.` : 'No station that answers has such a pane.'),
          where.map(to => h('button', { onclick: () => give(to) }, `It is the one on ${to}`)),
          h('button', { class: 'mini', onclick: () => { ctx.remove(refOf(n)); render(); } }, 'Take out of this layout'));
      }
      if (!online(st)) return h('div', { class: 'empty' }, h('p', {}, `${st} is unreachable, so ${label(n)} cannot be shown.`));
      return h('div', { class: 'empty' }, h('p', {}, n.pane !== undefined ? `${label(n)}${project ? ' in ' + project : ''} is not running.` : 'This pane was closed.'),
        project ? h('button', { onclick: () => rpc(st, 'project.open', { name: project }) }, `Start ${project}'s panes`) : null,
        h('button', { class: 'mini', onclick: () => { ctx.remove(refOf(n)); render(); } }, 'Take out of this layout'),
        ctx.shown().length > 1 ? h('button', { class: 'mini', onclick: () => ctx.hide(n, true) }, 'Hide') : null);
    },
    changed: () => saveLayout(lay),
    move: (ref, target, zone) => set(treeMove(tree(), ref, target, zone, refLeaf, refOf)),
    remove: (ref) => set(treeRemove(tree(), ref, refOf)),
    shape: (kind) => set(treeShape(kind, leaves(tree()).map(x => ({ ...x })))),
    // Any pane in the list can be put in: the name it would have here.
    sideRef: (st, p) => refOf(paneLeaf(lay, st, p)),
    has: (st, p) => !!ctx.leafOf(st, p),
    leafOf: (st, p) => { const ref = refOf(paneLeaf(lay, st, p)); return leaves(tree()).find(n => refOf(n) === ref) || null; },
    // Hiding changes only what is drawn. The pane's program is told nothing;
    // its terminal here is let go like any terminal that leaves the screen,
    // and comes back from the station's copy of the screen when it is shown.
    shown: () => leaves(tree()).filter(n => !n.hidden),
    // A menu or a button holds the leaf it was drawn for. A refresh in
    // between rebuilds the tree, and that leaf is then no part of it: "Show
    // only this" hid every pane, this one too, and Hide did nothing. So the
    // leaf is looked up again, by what it names, when the owner acts.
    live: (n) => { const ref = refOf(n); return leaves(tree()).find(x => refOf(x) === ref) || null; },
    hide: (n, on) => {
      if (!(n = ctx.live(n))) return;
      if (on && !ctx.shown().some(x => x !== n)) return toast('This is the only pane showing. Show another one first.');
      setHidden(n, on);
      saveLayout(lay);
      render();
      if (!on) focusLeafSoon(ctx, n);
    },
    only: (n) => { if (!(n = ctx.live(n))) return; for (const x of leaves(tree())) setHidden(x, x !== n); saveLayout(lay); render(); focusLeafSoon(ctx, n); },
  };
  return ctx;
}
const setHidden = (n, on) => { if (on) n.hidden = true; else delete n.hidden; };
function showAll(lay) {
  for (const n of leaves(lay.cache.tree)) setHidden(n, false);
  saveLayout(lay);
  render();
}
// A pane that was just shown takes the keyboard, so it can be typed into at once.
function focusLeafSoon(ctx, n) {
  setTimeout(() => {
    const r = ctx.resolve(n), t = r && S.terms.get(`leaf:${r.st}/${r.p.id}`);
    if (t && !document.querySelector('.dialog')) t.focus();
  }, 80);
}
// A hidden pane that is gone for good would be a chip that leads nowhere, so
// it leaves the layout: a pane in no project that was closed, and a pane of a
// project that is neither running nor one the project starts. Only when its
// station has answered: nothing is concluded about a station that is away.
function dropDeadHidden(lay, ctx) {
  if (!S.loaded || lay.cache.dirty) return;
  const dead = leaves(lay.cache.tree).filter((n) => {
    const st = ctx.stationOf(n);
    if (!n.hidden || isView(n) || !online(st) || S.busy.has(st) || !S.panes.has(st) || !S.projects.has(st) || ctx.resolve(n)) return false;
    if (n.id !== undefined) return true;
    const pr = S.projects.get(st).find(x => x.name === (n.project || lay.project));
    return !pr || !(pr.definition?.pane || []).some(x => x.name === n.pane);
  });
  if (!dead.length) return;
  for (const n of dead) lay.cache.tree = treeRemove(lay.cache.tree, ctx.refOf(n), ctx.refOf);
  saveLayout(lay);
}
const focusedLeaf = (ctx) => leaves(ctx.tree()).find(n => { const r = ctx.resolve(n); return r && `${r.st}/${r.p.id}` === S.focused; }) || null;

// Put a pane in a layout: beside the pane that has the keyboard, else at the end.
function addToLayout(lay, st, p, dir) {
  if (ctxFor(lay).has(st, p)) return toast(`${p.name} is already in ${lay.name}`);
  addLeaf(lay, paneLeaf(lay, st, p), dir);
}
// Put a leaf in a layout: a pane's, or a view's (Files, Watch).
function addLeaf(lay, leaf, dir) {
  const ctx = ctxFor(lay), all = leaves(lay.cache.tree);
  // Beside the pane with the keyboard, else the last one on screen, so the
  // new pane is seen even when others are hidden.
  const seen = all.filter(n => !n.hidden), focused = focusedLeaf(ctx);
  const beside = (focused && !focused.hidden ? focused : null) || seen[seen.length - 1] || all[all.length - 1];
  lay.cache.tree = beside ? treeInsert(lay.cache.tree, ctx.refOf(beside), dir === 'col' ? 'below' : 'right', leaf, ctx.refOf) : leaf;
  // On a phone, where a layout is one pane at a time, it is the one shown.
  if (isPhone()) phoneAt.set(lay.key, ctx.refOf(leaf));
  saveLayout(lay);
  render();
}

// From a Watch in a layout's cell to one of its agents: the pane in this
// layout if it is there (shown, if it was hidden; on a phone, stepped to),
// else the pane by itself.
function toPaneFrom(lay, st, p) {
  const ctx = ctxFor(lay), leaf = ctx.leafOf(st, p);
  if (!leaf) return go(paneUrl(st, p.id));
  if (isPhone()) { phoneAt.set(lay.key, ctx.refOf(leaf)); return render(); }
  if (leaf.hidden) ctx.hide(leaf, false); else focusLeafSoon(ctx, leaf);
  setTimeout(() => {
    const el = [...document.querySelectorAll('.workspace .pane')].find(x => x.dataset.pane === p.id);
    if (!el) return;
    el.classList.add('view-flash');
    setTimeout(() => el.classList.remove('view-flash'), 1200);
  }, 100);
}
// A view of a layout, gone to from the list on the left.
let viewWanted = null; // { key, id }
function viewsShown() {
  const w = viewWanted;
  viewWanted = null;
  if (!w || S.route.layout !== w.key) return;
  const el = [...document.querySelectorAll('.workspace .view-cell')].find(x => x.dataset.view === w.id);
  if (!el) return;
  el.classList.add('view-flash');
  setTimeout(() => el.classList.remove('view-flash'), 1200);
  el.querySelector('.fx-tree, .watch-thread')?.focus({ preventScroll: true });
}

// Tree edits. `refOf` names a leaf; leaves are compared by that name.
function treeRemove(n, ref, refOf) {
  if (!n) return null;
  if (isLeaf(n)) return refOf(n) === ref ? null : n;
  const a = treeRemove(n.a, ref, refOf), b = treeRemove(n.b, ref, refOf);
  return a && b ? { ...n, a, b } : (a || b);
}
function treeInsert(n, target, zone, leaf, refOf) {
  if (!n) return leaf;
  if (isLeaf(n)) {
    if (refOf(n) !== target) return n;
    const first = zone === 'left' || zone === 'above';
    return { dir: zone === 'left' || zone === 'right' ? 'row' : 'col', ratio: 0.5, a: first ? leaf : n, b: first ? n : leaf };
  }
  return { ...n, a: treeInsert(n.a, target, zone, leaf, refOf), b: treeInsert(n.b, target, zone, leaf, refOf) };
}
function treeMove(tree, ref, target, zone, leafOf, refOf) {
  if (!tree) return leafOf(ref);
  if (ref === target) return tree;
  const all = leaves(tree), src = all.find(x => refOf(x) === ref), dst = all.find(x => refOf(x) === target);
  if (!dst) return src ? tree : { dir: 'row', ratio: 0.5, a: tree, b: leafOf(ref) };
  if (zone === 'swap') {
    const swap = (n) => isLeaf(n) ? (refOf(n) === ref ? { ...dst } : refOf(n) === target ? (src ? { ...src } : leafOf(ref)) : n) : { ...n, a: swap(n.a), b: swap(n.b) };
    return swap(tree);
  }
  return treeInsert(treeRemove(tree, ref, refOf), target, zone, src ? { ...src } : leafOf(ref), refOf);
}
function treeShape(kind, list) {
  const chain = (dir, xs) => xs.length === 1 ? xs[0] : { dir, ratio: 1 / xs.length, a: xs[0], b: chain(dir, xs.slice(1)) };
  if (!list.length) return null;
  if (kind === 'two-over-one' && list.length > 2) return { dir: 'col', ratio: 0.6, a: chain('row', list.slice(0, -1)), b: list[list.length - 1] };
  if (kind === 'one-stack' && list.length > 2) return { dir: 'row', ratio: 0.6, a: list[0], b: chain('col', list.slice(1)) };
  if (kind === 'grid' && list.length > 2) {
    const cols = Math.ceil(Math.sqrt(list.length)), rows = [];
    for (let i = 0; i < list.length; i += cols) rows.push(chain('row', list.slice(i, i + cols)));
    return chain('col', rows);
  }
  return chain('row', list);
}

// Moving a pane in Arrange: a label follows the pointer, the pane under it
// shows where the drop would go, and letting go there moves it. The move is
// made in whatever layout is on screen when the pointer is let go.
function startArrange(e, ref, label) {
  if (e.button) return;
  e.preventDefault();
  const ghost = h('div', { class: 'arr-ghost' }, label);
  document.getElementById('overlays').append(ghost);
  dragState = { ref, over: null };
  document.body.classList.add('dragging');
  const clear = () => document.querySelectorAll('.arr[data-zone]').forEach(x => x.removeAttribute('data-zone'));
  const move = (ev) => {
    ghost.style.left = ev.clientX + 12 + 'px';
    ghost.style.top = ev.clientY + 12 + 'px';
    clear();
    dragState.over = null;
    const over = document.elementFromPoint(ev.clientX, ev.clientY)?.closest('.arr');
    if (!over || over.dataset.ref === ref) return;
    if (over.dataset.empty) { over.dataset.zone = 'swap'; dragState.over = { target: null, zone: 'swap' }; return; }
    const r = over.getBoundingClientRect(), x = (ev.clientX - r.left) / r.width, y = (ev.clientY - r.top) / r.height;
    const zone = x < 0.25 ? 'left' : x > 0.75 ? 'right' : y < 0.25 ? 'above' : y > 0.75 ? 'below' : 'swap';
    over.dataset.zone = zone;
    dragState.over = { target: over.dataset.ref, zone };
  };
  const up = () => {
    removeEventListener('mousemove', move);
    removeEventListener('mouseup', up);
    document.body.classList.remove('dragging');
    ghost.remove();
    clear();
    const d = dragState;
    dragState = null;
    if (d.over && layCtx) layCtx.move(d.ref, d.over.target, d.over.zone);
    if (autoArrange) { autoArrange = false; arranging = null; }
    render();
  };
  addEventListener('mousemove', move);
  addEventListener('mouseup', up);
  move(e);
}

// A pane in the list on the left, pressed while a layout is on screen: once
// the pointer has moved a little it is being dragged into the layout, which
// goes into Arrange so there is somewhere to drop it. A press that does not
// move is still a click.
function sideDrag(e, st, p) {
  if (e.button || !layCtx) return;
  // Without this the browser starts its own drag of the link.
  e.preventDefault();
  const x0 = e.clientX, y0 = e.clientY;
  const off = () => { removeEventListener('mousemove', move); removeEventListener('mouseup', off); };
  const move = (ev) => {
    if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < 6) return;
    off();
    if (!layCtx) return;
    const ref = layCtx.sideRef(st, p);
    if (!layCtx.arrange) { arranging = layCtx.key; autoArrange = true; render(); }
    addEventListener('mouseup', () => { dragEnded = Date.now(); }, { once: true });
    startArrange(ev, ref, p.name);
  };
  addEventListener('mousemove', move);
  addEventListener('mouseup', off);
}

function renderTree(node, ctx) {
  if (!node) {
    return ctx.arrange
      ? h('div', { class: 'empty arr', 'data-empty': '1' }, h('div', { class: 'arr-zone' }), 'Drag a pane here from the list on the left.')
      : h('div', { class: 'empty' }, ctx.emptyText);
  }
  // A hidden pane is not drawn; a split with one side hidden is its other
  // side, at full size, and with both hidden it is nothing.
  if (!shows(node)) return null;
  if (!isLeaf(node) && !(shows(node.a) && shows(node.b))) return renderTree(shows(node.a) ? node.a : node.b, ctx);
  if (isLeaf(node)) {
    const r = ctx.resolve(node);
    const guest = ctx.guest(node);
    // A pane shown as its conversation has no terminal on this page, so it
    // holds no size: the choice is this device's, and is not in the layout.
    const chat = r && faceOf(r.st, r.p) === 'chat' ? conversation(r.st, r.p, 'leaf') : null;
    const t = r && !chat ? getTerm(r.st, r.p, 'leaf', 'control') : null;
    if (t) t.setGuest(guest);
    // The last pane on screen has no Hide: a layout always shows something.
    if (t) t.setHide(ctx.shown().length > 1 ? () => ctx.hide(node, true) : null);
    const body = chat ? chat.frame() : t ? t.el : ctx.missing(node);
    if (!ctx.arrange) return body;
    const ref = ctx.refOf(node), label = ctx.label(node);
    return h('div', { class: 'arr-wrap' }, body,
      h('div', { class: 'arr', 'data-ref': ref, onmousedown: (e) => { if (!e.target.closest('button')) startArrange(e, ref, label); } },
        h('div', { class: 'arr-zone' }),
        h('div', { class: 'arr-card' }, h('b', {}, label), guest ? h('span', { class: 'chip guest' }, guest) : null, h('span', { class: 'muted' }, 'Drag onto another pane'),
          h('button', { class: 'mini', onclick: () => { ctx.remove(ref); render(); } }, 'Take out of this layout'))));
  }
  const a = h('div', { class: 'split-a', style: { flex: `${node.ratio} 1 0` } }, renderTree(node.a, ctx));
  const b = h('div', { class: 'split-b', style: { flex: `${1 - node.ratio} 1 0` } }, renderTree(node.b, ctx));
  const gutter = h('div', { class: 'gutter', role: 'separator', 'aria-orientation': node.dir === 'row' ? 'vertical' : 'horizontal', title: 'Drag to resize' });
  const box = h('div', { class: 'split ' + node.dir }, a, gutter, b);
  // The line between two panes can be dragged at any time.
  gutter.addEventListener('mousedown', (e) => {
    e.preventDefault();
    const rect = box.getBoundingClientRect();
    const move = (ev) => {
      const r = node.dir === 'row' ? (ev.clientX - rect.left) / rect.width : (ev.clientY - rect.top) / rect.height;
      node.ratio = Math.min(0.9, Math.max(0.1, r));
      a.style.flex = `${node.ratio} 1 0`;
      b.style.flex = `${1 - node.ratio} 1 0`;
    };
    const up = () => { removeEventListener('mousemove', move); removeEventListener('mouseup', up); document.body.classList.remove('dragging'); ctx.changed(); };
    document.body.classList.add('dragging');
    addEventListener('mousemove', move);
    addEventListener('mouseup', up);
  });
  return box;
}

// ---- making, renaming and removing layouts; pins ----

const layoutNames = (st, project) => project ? ((S.projects.get(st) || []).find(pr => pr.name === project)?.definition?.layout || []).map(l => l.name) : (S.deck.layouts || []).map(l => l.name);
const layoutSave = (st, project, layout) => project ? rpc(st, 'project.layout.save', { project, layout }) : rpc(api.station, 'deck.layout.save', { layout });
const layoutRemove = (lay) => lay.project ? rpc(lay.station, 'project.layout.remove', { project: lay.project, layout: lay.name }) : rpc(api.station, 'deck.layout.remove', { layout: lay.name });
const nameField = (value, placeholder) => h('input', { value: value || '', placeholder, required: true, pattern: VALID_NAME, title: 'Letters, digits, - _ . only' });

// A new layout in a project (saved with the project), or with no project a
// global one (saved for every station). `first` is a pane to start it with.
function newLayoutDialog(st, project, first) {
  dialog(project ? `New layout in ${project}` : 'New global layout', (close) => {
    const taken = layoutNames(st, project);
    const name = nameField(freeName(project ? 'layout' : 'global', taken));
    const submit = async () => {
      const n = name.value.trim();
      if (taken.includes(n)) return toast(`There is already a layout named ${n} ${project ? 'in ' + project : 'for every station'}. Choose another name.`, 'error');
      const lay = { key: project ? `${st}/${project}/${n}` : 'global:' + n, station: project ? st : null, project: project || null, name: n };
      const mine = first ? [paneLeaf(lay, first.st, first.p)]
        : project ? panesOf(st).filter(p => p.project === project && !p.exited).map(p => ({ pane: p.name })) : [];
      await layoutSave(st, project, { name: n, tree: toServer(treeShape(mine.length === 3 ? 'two-over-one' : 'grid', mine)) });
      close();
      await refresh();
      if (!mine.length || (project && !first)) arranging = lay.key;
      go(layUrl(lay));
    };
    return h('form', { onsubmit: (e) => { e.preventDefault(); submit(); } },
      field('Name', name),
      h('p', { class: 'muted' }, project
        ? `Saved in ${project}'s project file, so it is the same on every device. It starts with the project's running panes; any pane from any project or station can be dragged in from the list.`
        : 'Kept for the whole fleet: it is in the list on every station. Drag any pane from any station or project into it.'),
      h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Cancel'), h('button', { class: 'primary', type: 'submit' }, 'Create')));
  });
}

function renameLayoutDialog(lay) {
  dialog('Rename layout', (close) => {
    const name = nameField(lay.name);
    const submit = async () => {
      const to = name.value.trim();
      if (to === lay.name) return close();
      if (layoutNames(lay.station, lay.project).includes(to)) return toast(`There is already a layout named ${to}. Choose another name.`, 'error');
      const pinned = isPinned(layPin(lay)), next = { ...lay, name: to, key: lay.project ? `${lay.station}/${lay.project}/${to}` : 'global:' + to };
      await layoutSave(lay.station, lay.project, layoutBody(to, lay.cache));
      // The layout's own look moves to its new name.
      if (lookState.doc.layouts[lay.key]) await api.call('look.set', { level: 'layout', key: next.key, choice: lookState.doc.layouts[lay.key] }).catch(() => {});
      await layoutRemove(lay);
      if (pinned) { await api.call('deck.unpin', { pin: layPin(lay) }).catch(() => {}); await api.call('deck.pin', { pin: layPin(next) }).catch(() => {}); }
      close();
      const here = S.route.layout === lay.key;
      await refresh();
      if (here) go(layUrl(next));
    };
    return h('form', { onsubmit: (e) => { e.preventDefault(); submit(); } }, field('Name', name), h('div', { class: 'row end' }, h('button', { class: 'primary', type: 'submit' }, 'Rename')));
  });
}

async function duplicateLayout(lay) {
  const name = freeName(lay.name + '-copy', layoutNames(lay.station, lay.project));
  await layoutSave(lay.station, lay.project, layoutBody(name, lay.cache));
  const copyKey = lay.project ? `${lay.station}/${lay.project}/${name}` : 'global:' + name;
  if (lookState.doc.layouts[lay.key]) await api.call('look.set', { level: 'layout', key: copyKey, choice: lookState.doc.layouts[lay.key] }).catch(() => {});
  await refresh();
  go(layUrl({ ...lay, name }));
}

function deleteLayout(lay) {
  confirmBox(`Delete the layout ${lay.name}?`, `${lay.project ? `It is removed from ${lay.project}'s project file.` : 'It is removed from every station.'} The panes keep running.`, 'Delete', async () => {
    await layoutRemove(lay);
    // Removing a global layout unpins it on the station; a project's is unpinned here.
    if (lay.project && isPinned(layPin(lay))) await api.call('deck.unpin', { pin: layPin(lay) }).catch(() => {});
    layCache.delete(lay.key);
    const here = S.route.layout === lay.key;
    await refresh();
    if (here) go(lay.station ? stationUrl(lay.station) : '#/fleet');
  });
}

function layMenu(e, lay) {
  e.preventDefault();
  e.stopPropagation();
  const pin = layPin(lay);
  showMenu(e.clientX, e.clientY, [
    { title: `${lay.name} · ${layWhere(lay)}` },
    { label: 'Open', action: () => go(layUrl(lay)) },
    { label: 'Arrange', action: () => { arranging = lay.key; go(layUrl(lay)); } },
    { label: 'Rename', action: () => renameLayoutDialog(lay) },
    { label: 'Duplicate', action: () => duplicateLayout(lay) },
    { label: isPinned(pin) ? 'Unpin' : 'Pin', action: () => togglePin(pin) },
    leaves(lay.cache.tree).some(n => n.hidden) && { label: 'Show all panes', hint: `${leaves(lay.cache.tree).filter(n => n.hidden).length} hidden`, action: () => showAll(lay) },
    // Opened on the layout itself, so the change is seen as it is made.
    !isPhone() && { label: 'Appearance…', hint: lookState.doc.layouts[lay.key] ? 'its own' : undefined, action: () => { go(layUrl(lay)); setTimeout(() => appearance('layout', { key: lay.key, name: lay.name }), 0); } },
    '-',
    { label: 'Delete', danger: true, action: () => deleteLayout(lay) }]);
}

// Pins are part of the deck document: the same on every station.
const samePin = (a, b) => ['kind', 'station', 'project', 'id', 'name'].every(k => (a[k] || '') === (b[k] || ''));
const isPinned = (pin) => (S.deck.pins || []).some(x => samePin(x, pin));
const panePin = (st, p) => ({ kind: 'pane', station: st, id: p.id });
function togglePin(pin) {
  return api.call(isPinned(pin) ? 'deck.unpin' : 'deck.pin', { pin }).then((pins) => { S.deck = { ...S.deck, pins }; render(); }).catch((e) => toast(e.message, 'error'));
}
// What is pinned, found: a pane, a layout, or a line saying it is not there.
function pinnedItems() {
  return (S.deck.pins || []).map((pin) => {
    if (pin.kind === 'pane') {
      const p = panesOf(pin.station).find(x => x.id === pin.id);
      return p ? { pin, st: pin.station, p } : { pin, gone: online(pin.station) ? `A pane on ${pin.station} that was closed` : `A pane on ${pin.station} (unreachable)` };
    }
    const lay = findLayout(pin.project ? `${pin.station}/${pin.project}/${pin.name}` : 'global:' + pin.name);
    return lay ? { pin, lay } : { pin, gone: `${pin.name}${pin.project ? ` in ${pin.project} on ${pin.station}` : ''} (not available)` };
  });
}

// Choose a layout for a pane, or make a new global one with it.
function addToLayoutDialog(st, p) {
  dialog(`Add ${p.name} to a layout`, (close) => {
    const lays = allLayouts();
    return h('div', { class: 'list' },
      lays.map(x => h('button', { class: 'list-row', onclick: () => { close(); addToLayout(x, st, p, 'row'); go(layUrl(x)); } }, h('span', { class: 'ico-wrap' }, layoutIcon()), h('b', {}, x.name), h('span', { class: 'muted' }, layShort(x)))),
      h('button', { class: 'list-row', onclick: () => { close(); newLayoutDialog(null, null, { st, p }); } }, '＋ New global layout…'));
  });
}

// Workspaces used to be kept in each browser. Each becomes a global layout,
// once, and the browser's copy is deleted. A workspace of a station that is
// away waits until its panes can be looked up.
let migrating = false;
async function migrateWorkspaces() {
  if (migrating) return;
  const PREFIX = 'spandeck.layout.';
  let keys = [];
  try { keys = Object.keys(localStorage).filter(k => k.startsWith(PREFIX)); } catch (e) { return; }
  if (!keys.length) return;
  migrating = true;
  try {
    const taken = (S.deck.layouts || []).map(l => l.name);
    let made = 0;
    for (const key of keys) {
      const st = key.slice(PREFIX.length);
      const info = S.fleet.stations.find(s => s.station === st);
      if (info && info.state !== 'online') continue;
      // Its panes are not known yet, or are from before: tried again later.
      if (info && (!heard(st) || S.busy.has(st))) continue;
      let old = null;
      try { old = JSON.parse(localStorage.getItem(key)); } catch (e) { /* not ours: dropped below */ }
      for (const tab of info && Array.isArray(old?.tabs) ? old.tabs : []) {
        const convert = (n) => {
          if (!n) return null;
          if (n.pane !== undefined) {
            const p = panesOf(st).find(x => x.id === n.pane);
            return !p ? null : p.project ? { pane: p.name, project: p.project, station: st } : { id: p.id, station: st };
          }
          const a = convert(n.a), b = convert(n.b);
          return a && b ? { dir: n.dir === 'col' ? 'col' : 'row', ratio: Number(n.ratio) || 0.5, a, b } : (a || b);
        };
        const tree = convert(tab.tree);
        if (!tree) continue;
        const name = freeName(String(tab.name || '').trim().replace(/[^A-Za-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 40) || 'workspace', taken);
        await api.call('deck.layout.save', { layout: { name, tree: toServer(tree) } });
        taken.push(name);
        made += 1;
      }
      localStorage.removeItem(key);
    }
    if (made) { toast(`${made} workspace${made === 1 ? '' : 's'} from this browser ${made === 1 ? 'is now a global layout' : 'are now global layouts'}, under Layouts on every station.`); refreshSoon(60); }
  } catch (e) { /* tried again at the next refresh */ }
  migrating = false;
}

// ---- the right-hand side ----

const shell = (...main) => h('div', { class: 'station-view' }, sidebar(), ...main);
const awayText = (st, info) => `${st} is ${STATION_STATE[info.state] || 'unreachable'}. Nothing here says what it is doing right now.`;

function fleetView() {
  const stations = S.fleet.stations.slice().sort((a, b) => (b.needs_you || 0) - (a.needs_you || 0) || (b.self ? 1 : 0) - (a.self ? 1 : 0));
  return shell(h('main', { class: 'station-main' }, h('div', { class: 'fleet' }, stopRows(),
    h('div', { class: 'tiles' }, stations.map(stationTile), addTile()),
    twinsLine(),
    stations.length <= 1 ? h('p', { class: 'muted hint' }, 'Other stations appear here on their own: any machine under your Tailscale login that runs spandeckd.') : null)));
}

// Performance: every station's own numbers, the links between them, and
// what this browser measured. The same page from any station.
function perfView() {
  loadPerf();
  const page = perfPage({ data: S.perf, api, phone: isPhone(), reload: () => loadPerf(true), explain: keyDialog });
  return isPhone() ? page : shell(page);
}

// One station: every running pane as a tile.
function stationView(st) {
  const info = S.fleet.stations.find(s => s.station === st);
  if (info && info.state !== 'online') return shell(h('main', { class: 'station-main' }, h('div', { class: 'fleet' }, h('p', { class: 'warn-text' }, awayText(st, info)))));
  const live = liveAgents(st).sort(byState);
  loadThumbs(st, live);
  return shell(h('main', { class: 'station-main' }, h('div', { class: 'deck' },
    live.length ? h('div', { class: 'tiles' }, live.map(p => paneTile(st, p))) : h('div', { class: 'empty' }, S.loaded && heard(st) ? 'Nothing is running here. Start a pane or a project from the left.' : waitWords(st)))));
}

// A layout: its name and where it is saved, Arrange and Lines, then the panes.
function layoutView(key) {
  const lay = findLayout(key);
  if (!lay) return shell(h('main', { class: 'station-main' }, h('div', { class: 'empty' }, S.loaded ? 'That layout is not here any more: it was deleted, or its station is unreachable.' : 'Loading…')));
  if (arranging && arranging !== lay.key) arranging = null;
  const ctx = layCtx = ctxFor(lay);
  const c = lay.cache;
  dropDeadHidden(lay, ctx);
  const head = h('div', { class: 'lay-head' },
    h('span', { class: 'ico-wrap' }, layoutIcon()), h('b', { class: 'lay-name' }, lay.name), h('span', { class: 'muted lay-saved' }, layWhere(lay)),
    lay.project ? linkedChip(lay.station, (S.projects.get(lay.station) || []).find(pr => pr.name === lay.project)) : null,
    hiddenStrip(ctx),
    h('span', { class: 'grow' }),
    h('button', { class: 'add-pane', title: 'Add a pane to this layout: an agent, a shell, Files or Watch', onclick: () => addPaneDialog(lay) }, '＋ Add pane'),
    h('button', { class: 'toggle' + (ctx.arrange ? ' on' : ''), 'aria-pressed': String(ctx.arrange), title: 'Move panes around by dragging them', onclick: () => { arranging = ctx.arrange ? null : lay.key; render(); } }, ctx.arrange ? 'Done arranging' : 'Arrange'),
    h('button', { class: 'toggle' + (c.lines ? ' on' : ''), 'aria-pressed': String(!!c.lines), title: 'Draw lines between panes whose agents are talking', onclick: () => { c.lines = !c.lines; saveLayout(lay); render(); } }, c.lines ? 'Lines: on' : 'Lines: off'),
    h('button', { class: 'icon-btn', title: 'Layout menu', 'aria-label': 'Layout menu', onclick: (e) => layMenu(e, lay) }, '⋮'));
  const bar = ctx.arrange ? h('div', { class: 'arrange-bar' },
    h('b', {}, 'Arranging'), h('span', {}, 'Drag a pane onto another: an edge puts it beside, the middle swaps. Drag any pane in from the list.'),
    h('span', { class: 'grow' }),
    [['row', 'Side by side'], ['two-over-one', 'Two over one'], ['one-stack', 'One + stack'], ['grid', 'Grid']].map(([k, label]) => h('button', { class: 'mini', onclick: () => { ctx.shape(k); render(); } }, label))) : null;
  // Every pane hidden: reached only when the last one showing was taken out
  // or closed, since the last one showing cannot be hidden.
  const allHidden = c.tree && !shows(c.tree) ? h('div', { class: 'empty all-hidden' + (ctx.arrange ? ' arr' : ''), 'data-empty': ctx.arrange ? '1' : undefined },
    ctx.arrange ? h('div', { class: 'arr-zone' }) : null,
    h('p', {}, 'Every pane in this layout is hidden. They are still running: choose one above, or show them all.'),
    h('button', { class: 'primary', onclick: () => showAll(lay) }, 'Show all')) : null;
  const content = h('div', { class: 'workspace-col' }, bar, h('div', { class: 'workspace' + (ctx.arrange ? ' arranging' : '') }, allHidden || renderTree(c.tree, ctx)));
  if (c.lines && !ctx.arrange) {
    // Messages stay on a station, so the lines are worked out for each.
    const by = new Map();
    for (const r of ctx.shown().map(n => ctx.resolve(n)).filter(Boolean)) by.set(r.st, [...(by.get(r.st) || []), r.p]);
    pendingLinks = { selector: '.workspace', links: [...by].flatMap(([st, panes]) => linksFor(st, panes)).filter(x => x.kind === 'message') };
  }
  // The list on the left is drawn after the layout, so it can say what is in it.
  return shell(h('main', { class: 'station-main' }, head, content));
}

// The hidden panes of the layout on screen, beside its name: one chip each,
// with the pane's icon, name and state, standing out as a pane does when it
// needs you. The chip shows the pane again; the button beside it, or a
// right-click, opens the pane's menu.
function hiddenStrip(ctx) {
  const hid = leaves(ctx.tree()).filter(n => n.hidden);
  if (!hid.length) return null;
  const chip = (n) => {
    const r = ctx.resolve(n), p = r?.p || null, name = ctx.label(n), guest = ctx.guest(n), ref = ctx.refOf(n);
    // A view has no state of its own: a Watch says its agents' (it stands
    // out when one of them needs the owner), and Files says nothing.
    const view = isView(n);
    const state = p ? stateOf(p) : view ? viewState(ctx.lay, n) : 'missing';
    const stateText = p ? (p.exited ? 'ended' : STATE_LABEL[state] || state) : view ? (STATE_LABEL[state] || state) : (!ctx.stationOf(n) ? 'no station' : online(ctx.stationOf(n)) ? 'not running' : 'unreachable');
    const says = (p ? paneSays(p) : view && state === 'idle' ? name : `${name}, ${stateText}`) + (guest ? `, ${guest}` : '') + (p?.needs_reason && state === 'needs_you' ? `: ${p.needs_reason}` : '');
    const menu = (e) => {
      e.preventDefault();
      e.stopPropagation();
      // Opened from the keyboard there is no pointer: the menu goes under the chip.
      const b = e.currentTarget.getBoundingClientRect(), at = e.clientX || e.clientY ? { clientX: e.clientX, clientY: e.clientY } : { clientX: b.left, clientY: b.bottom + 2 };
      if (p) return paneMenu(at, r.st, p, 'chip');
      showMenu(at.clientX, at.clientY, [{ title: `${name} · ${stateText}` },
        { label: 'Show', hint: 'where it was', action: () => ctx.hide(n, false) },
        { label: 'Take out of this layout', action: () => { ctx.remove(ref); render(); } }]);
    };
    return h('span', { class: `hid-chip ${state}`, dataset: { ref }, oncontextmenu: menu },
      h('button', { type: 'button', class: 'hid-show', dataset: { keep: 'hid:' + ref }, 'aria-label': `Show ${says}. Hidden.`, title: `${says}. Hidden: click to show it where it was.`, onclick: () => ctx.hide(n, false) },
        h('span', { class: 'dot ' + state }), p ? paneMark(p) : null, h('span', { class: 'hid-name' }, name),
        state === 'needs_you' || state === 'error' || (!p && !view) ? h('span', { class: 'hid-state' }, stateText) : null),
      h('button', { type: 'button', class: 'icon-btn hid-menu', dataset: { keep: 'hidm:' + ref }, 'aria-label': `Menu for ${name}`, title: 'Pane menu', 'aria-haspopup': 'menu', onclick: menu }, '⋮'));
  };
  return h('div', { class: 'lay-hidden', role: 'group', 'aria-label': `Hidden panes of ${ctx.lay.name}` },
    h('span', { class: 'muted hid-label' }, 'Hidden'), hid.map(chip),
    hid.length > 1 ? h('button', { type: 'button', class: 'mini', dataset: { keep: 'hid-all' }, title: 'Show every hidden pane where it was', onclick: () => showAll(ctx.lay) }, 'Show all') : null);
}

// A project's Watch, with a header that says whose it is.
function watchPage(st, project) {
  const info = S.fleet.stations.find(s => s.station === st);
  const pr = (S.projects.get(st) || []).find(x => x.name === project);
  const head = h('div', { class: 'lay-head' }, h('span', { class: 'ico-wrap' }, watchIcon()), h('b', { class: 'lay-name' }, 'Watch'),
    h('span', { class: 'muted lay-saved' }, `The agents in ${project} on ${st}, and what they say to each other`), linkedChip(st, pr));
  const body = info && info.state !== 'online' ? h('div', { class: 'empty' }, awayText(st, info))
    : !pr ? h('div', { class: 'empty' }, S.loaded && heard(st) ? `There is no project ${project} on ${st}.` : waitWords(st)) : watchView(st, project);
  return shell(h('main', { class: 'station-main' }, head, body));
}

function paneView(st, id) {
  const p = findPane(st, id);
  if (!p) return shell(h('main', { class: 'station-main' }, h('div', { class: 'empty' }, !online(st) && S.loaded ? `${st} is unreachable.` : S.loaded && heard(st) ? 'That pane is not on this station any more.' : waitWords(st))));
  if (faceOf(st, p) === 'chat') return shell(h('main', { class: 'station-main zoomed' }, conversation(st, p, 'zoom').frame()));
  const t = getTerm(st, p, 'zoom', 'control');
  // The terminal takes the keyboard when the view opens, and keeps it across
  // redraws, but never takes it from a dialog or a box someone is typing in.
  setTimeout(() => {
    const a = document.activeElement;
    if (document.querySelector('.dialog')) return;
    if (!a || a === document.body || t.el.contains(a)) t.focus();
  }, 50);
  return shell(h('main', { class: 'station-main zoomed' }, t.el));
}

// ---- phone: status, blockers, approvals, stop controls, and a pane you can
// talk to: a message box (typed or dictated) and the keys a phone lacks ----

const PHONE_KEYS = [['Esc', '\x1b'], ['Tab', '\t'], ['↑', '\x1b[A'], ['↓', '\x1b[B'], ['←', '\x1b[D'], ['→', '\x1b[C'], ['⏎', '\r'], ['^C', '\x03']];
const KEY_NAMES = { '↑': 'Up', '↓': 'Down', '←': 'Left', '→': 'Right', '⏎': 'Enter', '^C': 'Control C' };
const composers = new Map();
const phonePanes = new Map();

// ---- a pane on a phone is read at the phone's text size ----
// (The owner, 16 Nov 2026, of a conversation beside a 120-column terminal
// shrunk to the phone's width: "Two different windows with 2 differ t fonts ,
// should be mobile wide font".) A pane opened on a phone is fitted to the
// phone by itself, as "Fit to screen" does, when that takes its size from
// nobody: no other window has its terminal open (`holders`, which the
// pane's station counts). When one has, the pane keeps its size and is
// drawn here at the phone's text size all the same, scrolled sideways; the
// bar says where it is open and offers Fit. When the phone's view of the
// pane closes, the size the pane had is given back, unless someone else has
// set another since.
const phoneFit = new Set();    // fitted to this phone: asked for, or by itself
const phoneAuto = new Set();   // of those, the ones nobody asked for
const phoneTaken = new Set();  // another window took the size while it was open here
const phoneWhole = new Set();  // watched, and shown whole: shrunk to the phone's width
const phoneBefore = new Map(); // the size to give back: { cols, rows }
const phoneMine = new Map();   // the size this phone gave it, and when: { cols, rows, at }
const phoneSeen = new Set();   // the panes on the page in this redraw
// "Fitted" turned off for a pane is remembered on this device.
const FIT_OFF = 'spandeck.phonefit.off';
let fitOff = {};
try { fitOff = JSON.parse(localStorage.getItem(FIT_OFF) || '{}') || {}; } catch (e) { fitOff = {}; }
const setFitOff = (key, off) => {
  if (off) fitOff[key] = 1; else delete fitOff[key];
  try { localStorage.setItem(FIT_OFF, JSON.stringify(Object.fromEntries(Object.entries(fitOff).slice(-200)))); } catch (e) { /* until the page is closed, then */ }
};
// Whether nobody else has the pane's terminal open, as far as its station
// says: a station from before this does not say, and is then not resized
// unasked. Nor is a pane whose program is at a question of its first screen
// (trust this folder, review these hooks): spandeck reads that screen to
// offer the answer. A pane that has just been started may be about to ask
// one, so it is left for its first seconds, until the station has looked.
const YOUNG_MS = 6000;
const paneBorn = new Map(); // when this page first heard of a pane; 0 for one that was there when its station was first listed
const listed = new Set();
function notePanes() {
  for (const [st, panes] of S.panes) {
    for (const p of panes) if (!paneBorn.has(`${st}/${p.id}`)) paneBorn.set(`${st}/${p.id}`, listed.has(st) ? Date.now() : 0);
    listed.add(st);
  }
  if (paneBorn.size > 2000) for (const k of [...paneBorn.keys()].slice(0, 1000)) paneBorn.delete(k);
}
const phoneFree = (key, p) => !p.exited && !fitOff[key] && !phoneTaken.has(key) && Array.isArray(p.holders) && p.holders.length === 0 && !p.trust?.asks && !p.hooks_trust?.asks
  && Date.now() - (paneBorn.get(key) ?? Date.now()) >= YOUNG_MS;
// What is to be given back is kept through a reload of the page (not
// longer): a pane still at the size this page gave it is given back the size
// it had before that, and not the phone's own.
const FIT_BEFORE = 'spandeck.phonefit.before';
let beforeKept = {}, beforeSaved = '';
try { beforeKept = JSON.parse(sessionStorage.getItem(FIT_BEFORE) || '{}') || {}; } catch (e) { beforeKept = {}; }
function phoneHold(key, p, auto) {
  if (!phoneBefore.has(key)) {
    const kept = beforeKept[key];
    phoneBefore.set(key, kept && kept.mc === p.cols && kept.mr === p.rows && kept.cols > 0 && kept.rows > 0 ? { cols: kept.cols, rows: kept.rows } : { cols: p.cols, rows: p.rows });
  }
  phoneFit.add(key);
  if (auto) phoneAuto.add(key);
}
// Whether the pane is fitted to this phone, fitting it now when that is free.
function phoneFitted(st, p, guest) {
  const key = `${st}/${p.id}`;
  phoneSeen.add(key);
  notePanes();
  // In a layout a pane that has ended is only looked at.
  if (p.exited) return phoneFit.has(key) && guest === undefined;
  if (phoneFit.has(key)) { phoneHold(key, p, false); return true; }
  if (!phoneFree(key, p)) return false;
  phoneHold(key, p, true);
  return true;
}
// The phone's view of a pane has closed, or Fitted was turned off: the pane
// has the size it had again, unless another window has set one since.
function phoneGiveBack(key) {
  const before = phoneBefore.get(key), set = phoneMine.get(key);
  phoneBefore.delete(key);
  phoneMine.delete(key);
  delete beforeKept[key];
  const cut = key.indexOf('/'), st = key.slice(0, cut), id = key.slice(cut + 1);
  const p = findPane(st, id), t = S.terms.get('phone-fit:' + key);
  if (!before || !p || p.exited || !before.cols || !before.rows) return;
  // With its terminal on the page, the terminal knows whether it still has
  // the size; without (its conversation is showing), the list of panes does,
  // a few seconds after the size was set.
  if (t && !t.control) return;
  const now = t && t.chan !== null ? t.size : set;
  if (!now || (!t && (p.cols !== now.cols || p.rows !== now.rows) && Date.now() - now.at > 6000)) return;
  if (now.cols === before.cols && now.rows === before.rows) return;
  api.on(st, 'pane.resize', { pane: id, cols: before.cols, rows: before.rows }).then(() => stationSoon(st, 60)).catch(() => {});
}
// After a redraw: every pane fitted here that is no longer on the page.
function phoneLeft() {
  notePanes();
  for (const key of [...phoneFit]) {
    if (phoneSeen.has(key)) continue;
    phoneGiveBack(key);
    if (phoneAuto.delete(key)) phoneFit.delete(key);
  }
  for (const key of [...phoneTaken]) if (!phoneSeen.has(key)) phoneTaken.delete(key);
  phoneSeen.clear();
  // (What was kept before a reload stays until its pane is fitted again.)
  const keep = Object.fromEntries(Object.entries(beforeKept).slice(-40));
  for (const [key, b] of phoneBefore) {
    const t = S.terms.get('phone-fit:' + key), was = phoneMine.get(key);
    // What its terminal holds it at, for when the terminal is not on the page.
    if (t && t.control && t.chan !== null && t.size.cols && (!was || was.cols !== t.size.cols || was.rows !== t.size.rows)) phoneMine.set(key, { cols: t.size.cols, rows: t.size.rows, at: Date.now() });
    const mine = phoneMine.get(key);
    if (mine) keep[key] = { cols: b.cols, rows: b.rows, mc: mine.cols, mr: mine.rows };
  }
  for (const key of [...phoneMine.keys()]) if (!phoneBefore.has(key)) phoneMine.delete(key);
  beforeKept = keep;
  const text = JSON.stringify(keep);
  if (text !== beforeSaved) { beforeSaved = text; try { sessionStorage.setItem(FIT_BEFORE, text); } catch (e) { /* not kept, then */ } }
}
// (Kept again as the page is left: a page reloaded within a moment of fitting
// a pane, before it was next drawn, kept nothing, and the pane stayed at the
// phone's size. Seen as a test that failed now and then, 17 Nov 2026.)
addEventListener('pagehide', () => { try { phoneLeft(); } catch (e) { /* the page is going */ } });
// Where a pane is open, from who its station says has it: a window that
// reaches it through another station is named for that station.
function holderWords(st, p) {
  const clean = (x) => x.replace(/[^A-Za-z0-9-]/g, '').slice(0, 24);
  const via = (p.holders || []).map(c => S.fleet.stations.find(x => String(c).startsWith(`gw-${clean(x.station)}-`))?.station).find(Boolean);
  return via && via !== st ? `another window, through ${via}` : `another window of ${st}'s deck`;
}
// A pane whose conversation is made from its screen, open as that
// conversation on a phone: what it prints is read at the phone's width only
// if the pane is that wide. The conversation attaches to nothing, so the
// size is asked for by itself, under the same rule, from the width of a
// printed block and the phone's text size.
let cellProbe = null;
function phoneChatFit(st, p, conv) {
  const key = `${st}/${p.id}`;
  phoneSeen.add(key);
  notePanes();
  if (conv.source !== 'screen' || conv.available !== true) return;
  const mine = phoneMine.get(key);
  // Someone else has set another size since: it is theirs.
  if (mine && (p.cols !== mine.cols || p.rows !== mine.rows) && Date.now() - mine.at > 6000 && !S.terms.has('phone-fit:' + key)) {
    phoneMine.delete(key); phoneBefore.delete(key); phoneFit.delete(key); phoneAuto.delete(key); phoneTaken.add(key);
    return;
  }
  if (phoneFit.has(key) || !phoneFree(key, p)) return;
  const px = termSize(pageLook(null)).px;
  cellProbe ||= document.createElement('canvas').getContext('2d');
  cellProbe.font = `${px}px ${getComputedStyle(document.documentElement).getPropertyValue('--mono-font') || 'monospace'}`;
  const cell = cellProbe.measureText('MMMMMMMMMM').width / 10;
  const block = (conv.body.querySelector('.chat-screen .chat-mono pre') || conv.body.querySelector('.chat-screen'));
  const width = (block && block.clientWidth > 60 ? block.clientWidth : innerWidth - 26) - 22;
  const cols = Math.floor(width / cell), rows = Math.max(12, Math.min(60, Math.floor(innerHeight * 0.6 / (px * 1.4))));
  if (!(cell > 0) || cols < 20 || cols > 500) return;
  phoneHold(key, p, true);
  phoneMine.set(key, { cols, rows, at: Date.now() });
  if (p.cols !== cols || p.rows !== rows) api.on(st, 'pane.resize', { pane: p.id, cols, rows }).then(() => stationSoon(st, 60)).catch(() => {});
}

// Kept per pane, so a redraw does not throw away what you are writing.
function composer(st, p) {
  const key = `${st}/${p.id}`;
  let c = composers.get(key);
  if (c) return c;
  const box = h('textarea', { class: 'composer-box', rows: 2, placeholder: p.agent === 'shell' ? 'Type or paste a command' : 'Type, paste or dictate a message', 'aria-label': 'Message', enterkeyhint: 'send' });
  // One deliberate act (typed or dictated): sent now or not at all, and
  // refused by the station when it arrives more than ten seconds late. Not
  // sent, or not known to be: the text stays in the box and the box says so.
  const said = h('p', { class: 'send-said', role: 'alert', hidden: true });
  const sendBtn = h('button', { class: 'primary', type: 'submit' }, 'Send');
  let sending = false;
  const send = async () => {
    // Sent as a paste, so a message of several lines arrives as one.
    const text = box.value.trim();
    if (!text || sending) return;
    sending = true; sendBtn.disabled = true; sendBtn.textContent = 'Sending…'; said.hidden = true;
    try {
      await sendAct(api, st, 'pane.paste', { pane: p.id, text, enter: true });
      box.value = '';
      box.dispatchEvent(new Event('input'));
      stationSoon(st || api.station, 60);
    } catch (e) {
      said.textContent = `${e.message} Your message is still in the box.`;
      said.hidden = false;
    } finally { sending = false; sendBtn.disabled = false; sendBtn.textContent = 'Send'; }
  };
  box.addEventListener('input', () => { if (!sending) said.hidden = true; });
  // A key of the row: stamped like any key, so it is refused when late.
  const key1 = (bytes) => rpc(st, 'pane.input', { pane: p.id, data: bytes, ...(api.stamps ? { t: performance.now() } : {}) });
  // Dictation fills the box and sends nothing (mic.js).
  const talk = micFor({ box, changed: () => box.dispatchEvent(new Event('input')), st: st || api.station });
  const mic = talk.button;
  c = h('div', { class: 'composer' },
    h('div', { class: 'keys', role: 'group', 'aria-label': 'Keys' }, PHONE_KEYS.map(([label, bytes]) => h('button', { type: 'button', class: 'key', 'aria-label': KEY_NAMES[label] || label, onclick: () => key1(bytes) }, label))),
    said, talk.said,
    h('form', { class: 'composer-row', onsubmit: (e) => { e.preventDefault(); send(); } }, box, mic, sendBtn));
  composers.set(key, c);
  // What is being written goes with the owner between Chat and Terminal.
  termBox(st, p, box);
  return c;
}

// ---- phone: a layout, one pane at a time. A small map of the layout at the
// top says where you are; tap a box or swipe to change pane ----

const phoneAt = new Map(); // layout key -> the pane shown, by its name in the layout

// The layout's tree drawn small: each pane is a button.
function layoutMap(node, cells, pick) {
  if (isLeaf(node)) {
    const c = cells.find(x => x.node === node);
    return h('button', { type: 'button', class: `map-box ${c.state}${c.current ? ' current' : ''}${c.hidden ? ' hid' : ''}`, 'aria-label': c.label, 'aria-current': c.current ? 'true' : undefined,
      onclick: (e) => { e.currentTarget.blur(); pick(c.ref); } }, h('span', { class: 'dot ' + c.state }), h('span', { class: 'map-name' }, c.name));
  }
  const a = layoutMap(node.a, cells, pick), b = layoutMap(node.b, cells, pick);
  a.style.flex = `${node.ratio} 1 0`;
  b.style.flex = `${1 - node.ratio} 1 0`;
  return h('div', { class: 'map-split ' + node.dir }, a, b);
}
// How many boxes wide and tall a tree is, so every box stays big enough to tap.
const mapSpan = (n) => {
  if (!n || isLeaf(n)) return [1, 1];
  const [aw, ah] = mapSpan(n.a), [bw, bh] = mapSpan(n.b);
  return n.dir === 'row' ? [aw + bw, Math.max(ah, bh)] : [Math.max(aw, bw), ah + bh];
};

// The Terminal face of a pane on a phone, the same for a pane opened by
// itself and for the pane a layout is showing: Fit to screen, the usage and
// Stop this turn in `v.head`, the terminal in `v.slot`, the keys a phone
// lacks and the message box in `v.foot`. Watching shows the pane at the size
// someone else set, shrunk to fit; fitting resizes the pane to this screen,
// as a computer's deck would. The page is redrawn every few seconds: the
// terminal and the message box stay where they are, so the keyboard does not
// close while you type, and the bar is rebuilt only when what it shows
// changes (rebuilding it on every redraw made the phone put a focus ring on a
// button at each touch). `guest`: in a layout, where the pane is from.
function phoneTerm(v, st, p, guest) {
  const fitKey = `${st}/${p.id}`;
  const fitted = phoneFitted(st, p, guest);
  const t = fitted ? getTerm(st, p, 'phone-fit', 'control') : getTerm(st, p, 'phone', 'watch');
  // Not fitted: at this phone's text size and scrolled, or (asked for) whole.
  const whole = !fitted && phoneWhole.has(fitKey);
  if (!fitted) t.setWhole(whole);
  // Why it is not fitted, beside the button that fits it.
  // (The owner's own choice first: just after "Fitted" is turned off the
  // station still counts this page among the pane's holders for a moment.)
  const why = fitted || p.exited ? '' : fitOff[fitKey] ? 'Not fitted to this phone: you turned that off for this pane.'
    : phoneTaken.has(fitKey) || (p.holders || []).length ? `Open in ${holderWords(st, p)}: Fit takes its size.`
      : !Array.isArray(p.holders) ? `${st} runs an older spandeck, which does not say whether this pane is open elsewhere: Fit resizes it for everyone.` : '';
  if (guest !== undefined) t.setGuest(guest);
  const canStop = !p.exited && (p.state === 'working' || p.state === 'needs_you');
  const pu = p.usage;
  const brief = pu && pu.model ? [pu.model.replace(/^.*\//, ''), contextText(pu) ? 'context ' + contextText(pu) : '', fmtTokens(pu.total_tokens) + ' tokens', pu.cost_estimate !== null && pu.cost_estimate !== undefined ? 'est. ' + fmtCost(pu.cost_estimate) : ''].filter(Boolean).join(' · ') : '';
  // Its program's folder-trust question: the owner's one tap, or the note
  // that spandeck answered it. A strip this narrow has no room for either.
  const asks = !p.exited && p.trust?.asks, answered = trustAnsweredWords(p);
  // And its review of hooks, the same way.
  const hooksAsks = !p.exited && p.hooks_trust?.asks, hooksAnswered = hooksAnsweredWords(p);
  const sig = `${fitKey}|${fitted}|${whole}|${why}|${p.exited}|${canStop}|${brief}|${JSON.stringify(asks || null)}|${answered}|${JSON.stringify(hooksAsks || null)}|${hooksAnswered}`;
  if (v.sig !== sig) {
    v.sig = sig;
    // replaceChildren would print a missing piece as the word "null".
    v.head.replaceChildren(...[
      p.exited ? null : h('button', { class: fitted ? '' : 'primary fit-btn', 'aria-pressed': String(fitted), title: fitted ? 'Stop resizing this pane to the phone: it gets back the size it had' : 'Resize this pane to fit this screen', onclick: (e) => {
        e.currentTarget.blur();
        // Turned off, it stays off for this pane on this device; asked for, it is on again.
        if (fitted) { phoneGiveBack(fitKey); phoneFit.delete(fitKey); phoneAuto.delete(fitKey); setFitOff(fitKey, true); } else { setFitOff(fitKey, false); phoneTaken.delete(fitKey); phoneWhole.delete(fitKey); phoneHold(fitKey, p, false); }
        update();
      } }, fitted ? 'Fitted' : 'Fit to screen'),
      fitted || p.exited ? null : h('button', { class: 'whole-btn', 'aria-pressed': String(whole), title: whole ? 'Show this pane at this phone\'s text size, and scroll it' : 'Show the whole width of this pane, shrunk to fit', onclick: (e) => { e.currentTarget.blur(); whole ? phoneWhole.delete(fitKey) : phoneWhole.add(fitKey); update(); } }, whole ? 'Show at text size' : 'Show whole width'),
      brief ? h('button', { class: 'phone-usage', title: usageTitle(p.usage), 'aria-label': `Usage: ${brief}`, onclick: (e) => { e.currentTarget.blur(); usageDialog(st, p); } }, brief, h('span', { class: 'muted' }, ' ›')) : null,
      h('span', { class: 'grow' }),
      h('button', { class: 'danger big', disabled: !canStop, onclick: (e) => { e.currentTarget.blur(); rpc(st, 'pane.stop', { pane: p.id }).then(() => toast('Stop sent')); } }, 'Stop this turn'),
      why ? h('div', { class: 'fit-note', role: 'status' }, why) : null,
      asks ? trustCard(p, (remember) => rpc(st, 'pane.trust_folder', { pane: p.id, remember }).catch(() => {})) : null,
      answered ? h('div', { class: 'muted small trust-said' }, answered + '. It is in the ship\'s log.') : null,
      hooksAsks ? hooksCard(p, () => rpc(st, 'pane.trust_hooks', { pane: p.id }).catch(() => {})) : null,
      hooksAnswered ? h('div', { class: 'muted small trust-said hooks-said' }, hooksAnswered + '. It is in the ship\'s log.') : null].filter(Boolean));
  }
  if (v.slot.firstChild !== t.el) v.slot.replaceChildren(t.el);
  const foot = p.exited ? (v.ended ||= h('p', { class: 'muted small' }, 'This pane has ended.')) : composer(st, p);
  if (v.foot.firstChild !== foot) v.foot.replaceChildren(foot);
}

// Whether a touch began on something that scrolls sideways itself (a block
// of code, a table, a diff): a drag there is that thing's, not a step.
function scrollsSideways(el, within) {
  for (let n = el; n && n !== within; n = n.parentElement) {
    if (n.classList.contains('xterm')) return false;
    if (n.scrollWidth > n.clientWidth + 1 && /auto|scroll/.test(getComputedStyle(n).overflowX)) return true;
  }
  return false;
}

// Any layout: a project's or a global one, with panes from any station.
// The pane being shown is the pane as it is when opened by itself (the
// owner, 16 Nov 2026: "The panes in the layout should work like the single
// panes"): the conversation's own bar with Chat | Terminal, and under it the
// conversation, or the terminal with its keys and message box. The layout
// adds one line between the two: previous and next, which pane of how many,
// and the map of the layout, which opens from that line and closes again.
function phoneLayoutView(lay) {
  const lays = allLayouts();
  const ctx = ctxFor(lay);
  dropDeadHidden(lay, ctx);
  const cells = leaves(lay.cache.tree).map((n) => {
    const p = ctx.resolve(n)?.p || null, guest = ctx.guest(n), name = ctx.label(n), view = isView(n);
    // A view's name says its kind ("Files · out"); a Watch has its agents' state.
    const state = p ? stateOf(p) : view ? viewState(lay, n) : 'missing';
    return { node: n, ref: ctx.refOf(n), st: ctx.stationOf(n), p, guest, name, state, view, hidden: !!n.hidden, label: `${name}${guest ? ' ' + guest : ''}, ${p ? STATE_LABEL[state] || state : view ? (state === 'idle' ? 'a view' : STATE_LABEL[state] || state) : 'not running'}${n.hidden ? ', hidden' : ''}` };
  });
  // A hidden pane is left out of the order that Previous, Next and a swipe
  // step through. It stays in the map, marked, and a tap there looks at it
  // without showing it again in the layout: Hide and Show do that.
  const order = cells.filter(c => !c.hidden);
  let i = cells.findIndex(c => c.ref === phoneAt.get(lay.key));
  if (i < 0) i = Math.max(0, cells.indexOf(order[0]));
  const cur = cells[i] || null;
  if (cur) cur.current = true;
  const stepTo = (d) => (cur && !cur.hidden ? order[order.indexOf(cur) + d]
    : d > 0 ? order.find(c => cells.indexOf(c) > i) : order.filter(c => cells.indexOf(c) < i).pop()) || null;
  const p = cur?.p || null, st = cur?.st;

  // The page is redrawn every few seconds, and what is on it stays where it
  // is: the line and the map are rebuilt only when what they show changes, so
  // a touch leaves no focus ring behind and an approval keeps the reason
  // being typed into it.
  const vkey = 'layout:' + lay.key;
  let v = phonePanes.get(vkey);
  if (!v) {
    v = { head: h('div', { class: 'row' }), title: h('div', { class: 'lay-title' }), map: h('div', { class: 'lay-map', role: 'group' }),
      showing: h('span', { class: 'lay-showing' }), dots: h('span', { class: 'lay-dots', 'aria-hidden': 'true' }),
      prev: h('button', { type: 'button', class: 'lay-step', onclick: (e) => { e.currentTarget.blur(); v.step(-1); } }, '‹'), next: h('button', { type: 'button', class: 'lay-step', onclick: (e) => { e.currentTarget.blur(); v.step(1); } }, '›'),
      appr: h('div', { class: 'lay-approvals' }), rows: new Map(), slot: h('div', { class: 'keep' }), foot: h('div', { class: 'keep' }), open: false };
    // Hide the pane being looked at, or show it in the layout again.
    v.hide = h('button', { type: 'button', class: 'lay-hide', onclick: (e) => { e.currentTarget.blur(); v.toggle(); } });
    // The one line: which pane of how many. A tap opens the layout's name,
    // where it is from, its map and Hide, over the pane and not above it, so
    // the pane keeps its room (and a fitted terminal its size).
    v.chev = h('span', { class: 'chev', 'aria-hidden': 'true' }, '▾');
    v.where = h('button', { type: 'button', class: 'lay-where', 'aria-expanded': 'false', onclick: (e) => { e.currentTarget.blur(); v.fold(!v.open); } }, v.showing, v.dots, v.chev);
    v.nav = h('div', { class: 'lay-nav' }, v.prev, v.where, v.next);
    // A pane is added by its kind here too (viewpanes.js).
    v.add = h('button', { type: 'button', class: 'lay-add', onclick: (e) => { e.currentTarget.blur(); v.fold(false); addPaneDialog(v.lay); } }, 'Add a pane');
    v.panel = h('div', { class: 'lay-panel', hidden: true }, v.title, v.map, h('div', { class: 'row lay-panel-row' }, h('span', { class: 'muted small grow' }, 'Tap a pane to show it here. A dashed one is hidden in the layout.'), v.add, v.hide));
    v.strip = h('div', { class: 'lay-strip' }, v.nav, v.panel);
    v.fold = (open) => {
      v.open = open;
      v.panel.hidden = !open;
      v.where.setAttribute('aria-expanded', String(open));
      v.where.setAttribute('aria-label', `${v.says || 'Panes'}. ${open ? 'Close' : 'Open'} the map of the layout`);
      v.chev.textContent = open ? '▴' : '▾';
    };
    // With no pane to show there is no bar of a pane: only the way back.
    v.back = h('div', { class: 'row lay-back' }, h('a', { class: 'btn', href: '#/fleet' }, '‹ Back'));
    v.term = h('div', { class: 'lay-term' }, v.head, v.slot, v.foot);
    v.main = h('main', { class: 'phone phone-layout' });
    // A touch outside the open map closes it, and does nothing else: it is
    // not also a press of the Allow or the Send it happened to land on.
    v.main.addEventListener('click', (e) => { if (v.open && !v.strip.contains(e.target)) { e.preventDefault(); e.stopPropagation(); v.fold(false); } }, true);
    // A swipe left or right steps to the next or the previous pane. Heard
    // before the terminal, which scrolls on a drag up or down and never sees
    // a sideways one end. It is a step only when it is plainly sideways (far
    // more across than up or down, so reading a conversation is never one),
    // quick, and not begun on something that is the touch's own: a message
    // box, a card, the open map, or a block that scrolls sideways itself.
    // Selecting text is never a step.
    let sw = null;
    const one = (e) => (e.touches && e.touches.length === 1 ? e.touches[0] : null);
    v.main.addEventListener('touchstart', (e) => {
      const t = one(e), el = e.target;
      const busy = !t || !el.closest || el.closest('.composer, .chat-dock, .approval, .lay-panel, input, textarea, select') || scrollsSideways(el, v.main);
      sw = busy ? null : { x: t.clientX, y: t.clientY, dx: 0, dy: 0, at: Date.now() };
    }, { capture: true, passive: true });
    v.main.addEventListener('touchmove', (e) => { const t = one(e); if (sw && t) { sw.dx = t.clientX - sw.x; sw.dy = t.clientY - sw.y; } else sw = null; }, { capture: true, passive: true });
    v.main.addEventListener('touchend', (e) => {
      const g = sw;
      sw = null;
      if (!g || Math.abs(g.dx) < 60 || Math.abs(g.dx) < 2 * Math.abs(g.dy) || Date.now() - g.at > 900) return;
      const sel = window.getSelection && window.getSelection();
      if (sel && !sel.isCollapsed && sel.anchorNode && v.main.contains(sel.anchorNode)) return;
      if (e.cancelable) e.preventDefault();
      v.step(g.dx < 0 ? 1 : -1);
    }, { capture: true, passive: false });
    v.main.addEventListener('touchcancel', () => { sw = null; }, { capture: true, passive: true });
    phonePanes.clear();
    phonePanes.set(vkey, v);
  }
  v.lay = lay;
  const show = (ref) => { v.fold(false); if (phoneAt.get(lay.key) !== ref) { phoneAt.set(lay.key, ref); render(); } };
  v.step = (d) => { const to = stepTo(d); if (to) show(to.ref); };
  v.toggle = () => { if (cur) { setHidden(cur.node, !cur.hidden); saveLayout(lay); render(); } };

  const titleSig = lays.map(x => x.key).join('|') + '>' + lay.key;
  if (v.titleSig !== titleSig) {
    v.titleSig = titleSig;
    v.title.replaceChildren(lays.length > 1
      ? h('select', { 'aria-label': 'Layout', onchange: (e) => { const x = lays.find(y => y.key === e.target.value); if (x) go(layUrl(x)); } }, lays.map(x => h('option', { value: x.key, selected: x.key === lay.key }, `${x.project ? x.project + (onlineStations().length > 1 ? ' on ' + x.station : '') : 'Global'} · ${x.name}`)))
      : h('span', {}, h('b', {}, lay.name), h('span', { class: 'muted' }, lay.project ? ` · layout in ${lay.project} on ${lay.station}` : ' · global layout')));
  }
  // Another pane of the layout that needs the owner is said on the line too,
  // since the map is closed most of the time.
  const calls = cells.filter(c => c !== cur && (c.state === 'needs_you' || c.state === 'error'));
  const mapSig = JSON.stringify([lay.cache.tree, i, cells.map(c => c.label), lay.name]);
  if (v.mapSig !== mapSig) {
    v.mapSig = mapSig;
    const [w, hh] = mapSpan(lay.cache.tree);
    v.map.setAttribute('aria-label', `Panes in the layout ${lay.name}`);
    v.map.replaceChildren(...(cells.length ? [layoutMap(lay.cache.tree, cells, show)] : []));
    if (v.map.firstChild) Object.assign(v.map.firstChild.style, { minWidth: w * 48 - 4 + 'px', height: Math.max(hh * 48 - 4, 56) + 'px' });
    v.map.hidden = !cells.length;
    v.strip.hidden = !cells.length;
    const count = cur ? (cur.hidden ? 'hidden' : `${order.indexOf(cur) + 1} of ${order.length}`) : '';
    const shown = cur ? `${cur.name}${cur.guest ? ' (' + cur.guest + ')' : ''}` : '';
    v.showing.replaceChildren(...(cur ? [h('span', { class: 'muted lay-in' }, lay.name + ' › '), h('b', {}, shown), h('span', { class: 'lay-count' }, ' · ' + count)] : []));
    v.says = cur ? `Layout ${lay.name}: showing ${shown}, ${count}${calls.length ? `. ${calls.map(c => c.name).join(', ')} ${calls.length === 1 ? 'needs' : 'need'} you` : ''}` : '';
    v.where.classList.toggle('attn', calls.length > 0);
    // The last pane showing cannot be hidden, as on a computer.
    v.hide.hidden = !cur;
    v.hide.disabled = !cur || (!cur.hidden && order.length < 2);
    v.hide.textContent = !cur ? 'Hide' : cur.hidden ? `Show ${cur.name}` : `Hide ${cur.name}`;
    v.hide.setAttribute('aria-label', !cur ? 'Hide' : cur.hidden ? `Show ${cur.name} in this layout again` : `Hide ${cur.name} in this layout. It keeps running.`);
    v.dots.replaceChildren(...order.map((c) => h('i', { class: (c === cur ? 'on ' : '') + c.state })));
    for (const [b, d, word] of [[v.prev, -1, 'Previous'], [v.next, 1, 'Next']]) {
      const to = stepTo(d);
      b.disabled = !to;
      b.setAttribute('aria-label', to ? `${word} pane: ${to.name}` : `${word} pane`);
    }
    v.fold(v.open);
  }

  // The pane itself, by the same code as a pane opened by itself.
  const conv = p ? conversation(st, p, 'phone') : null;
  const chat = !!p && faceOf(st, p) === 'chat';
  if (p && !chat) phoneTerm(v, st, p, cur.guest || '');
  if (p && chat) phoneChatFit(st, p, conv);
  // The panes stepped away from: their conversations are kept, not watched.
  for (const c of cells) if (c.p && c !== cur) chatPark(c.st, c.p);

  // What is waiting for an answer from a pane in this layout stays on top,
  // whichever pane is showing. An approval of the pane on screen is a card
  // of its conversation, and is not said twice.
  const waiting = S.approvals.filter(a => cells.some(c => c.p && c.st === a.station && findPane(a.station, a.pane)?.id === c.p.id && !(chat && c === cur)));
  for (const [id, el] of v.rows) if (!waiting.some(a => a.id === id)) { el.remove(); v.rows.delete(id); }
  for (const a of waiting) if (!v.rows.has(a.id)) { const el = approvalRow(a); v.rows.set(a.id, el); v.appr.append(el); }

  let body;
  // A view has the screen as a pane does: the file browser, or Watch.
  if (cur && cur.view) body = viewCell(ctx, cur.node, 'phone');
  else if (!p) {
    const what = cur ? cur.ref : '';
    if (v.gone?.what !== what) {
      const project = cur && cur.node.pane !== undefined ? (cur.node.project || lay.project) : null;
      v.gone = { what, el: h('div', { class: 'empty lay-gone' }, cur
        ? [h('p', {}, online(st) ? `${cur.name} is not running.` : `${st} is unreachable, so ${cur.name} cannot be shown.`),
          project && online(st) ? h('button', { onclick: () => rpc(st, 'project.open', { name: project }) }, `Start ${project}'s panes`) : null]
        : h('p', {}, 'This layout is empty. Put panes in it with Arrange, on a computer.')) };
    }
    body = v.gone.el;
  } else body = chat ? conv.body : v.term;
  v.main.classList.toggle('phone-chat', chat);
  v.main.classList.toggle('phone-pane', !!p && !chat);
  v.main.classList.toggle('phone-viewleaf', !!(cur && cur.view));
  syncKids(v.main, [conv ? conv.bar : v.back, v.strip, v.appr, body]);
  return v.main;
}

function phonePaneView(st, id) {
  const p = findPane(st, id);
  if (!p) return h('main', { class: 'phone' }, h('p', {}, 'That pane is gone.'), h('a', { href: '#/fleet' }, 'Back'));
  // A pane has two faces. Its conversation (chat.js) never attaches to the
  // pane, so it sets no size; the terminal below is as it always was, under
  // the same bar, which is where Chat and Terminal are chosen. Every pane
  // has both: a shell's conversation, and that of an agent the station
  // cannot read, is made from its screen. A pane on a station from before
  // that is its terminal, and the bar says why.
  const conv = conversation(st, p, 'phone');
  if (faceOf(st, p) === 'chat') { phoneChatFit(st, p, conv); return conv.phoneMain(); }
  const fitKey = `${st}/${p.id}`;
  let v = phonePanes.get(fitKey);
  if (!v) {
    v = { head: h('div', { class: 'row' }), slot: h('div', { class: 'keep' }), foot: h('div', { class: 'keep' }) };
    v.main = h('main', { class: 'phone phone-pane' }, v.head, v.slot, v.foot);
    phonePanes.clear();
    phonePanes.set(fitKey, v);
  }
  phoneTerm(v, st, p);
  // The bar is the conversation's, and a pane left and opened again has a
  // new conversation: its bar takes the old one's place.
  if (v.main.firstChild !== conv.bar) { if (v.bar) v.bar.remove(); v.bar = conv.bar; v.main.prepend(conv.bar); }
  return v.main;
}

// The phone's list: what needs you first, then what is pinned, then the
// sessions. "Recent" is every agent in the fleet, the one that changed last
// first, with a dot on those that said something not yet read; "By station"
// is the list a computer shows on its left, with everything a station
// offers: each station with its projects (their layouts, Watch, panes), the
// global layouts, usage, performance and the fleet's stop.
function phoneList() {
  const all = S.fleet.stations.flatMap(s => liveAgents(s.station).map(p => ({ st: s.station, p })));
  const blockers = all.filter(x => x.p.agent !== 'shell' && (x.p.state === 'needs_you' || x.p.state === 'error'));
  const whereOf = (st, p) => `${st}${p.project ? ' · ' + p.project : ''}`;
  const line = (st, p, where) => h('a', { class: 'phone-row', href: paneUrl(st, p.id), 'aria-label': `${paneSays(p)}, ${KINDS[paneKind(st, p)][0]}` }, dot(p), paneMark(p),
    h('div', { class: 'grow' }, h('b', {}, p.name), where ? h('span', { class: 'muted' }, ' · ' + where) : null,
      h('div', { class: 'muted small' }, p.needs_reason || (p.exited ? 'ended' : p.tool ? `${p.tool} · ${ago(p.tool_since_ms)}` : `${STATE_LABEL[stateOf(p)]} · ${ago(p.state_since_ms)}`)), rapidLine(p)), kindTag(paneKind(st, p)), '›');
  // A layout's views, under it, each with its kind.
  const viewRows = (x) => leaves(x.cache.tree).filter(isView).map(n => sideView(x, n, 'phone-row phone-view-row'));
  // A layout is opened as a map you step through, one pane at a time.
  const layRow = (x, where) => { const n = leaves(x.cache.tree).length, hid = leaves(x.cache.tree).filter(y => y.hidden).length; return h('a', { class: 'phone-row lay-row', href: layUrl(x), 'aria-label': `${x.name}, layout` }, h('span', { class: 'ico-wrap' }, layoutIcon()),
    h('div', { class: 'grow' }, h('b', {}, x.name), where ? h('span', { class: 'muted' }, ' · ' + where) : null, h('div', { class: 'muted small' }, `${n} pane${n === 1 ? '' : 's'}, one at a time${hid ? ` · ${hid} hidden` : ''}`)), '›'); };
  const watchRow = (st, pr) => h('a', { class: 'phone-row watch-row', href: watchUrl(st, pr.name), 'aria-label': `Watch ${pr.name}` }, h('span', { class: 'ico-wrap' }, watchIcon()),
    h('div', { class: 'grow' }, h('b', {}, 'Watch'), h('div', { class: 'muted small' }, 'Every agent here, and who is talking')), '›');
  const pins = pinnedItems();
  const globals = globalLayouts();
  const station = (s) => {
    const st = s.station, up = s.state === 'online';
    const projects = up ? (S.projects.get(st) || []) : [];
    const loose = up ? panesOf(st).filter(p => !p.project && !p.exited).sort(byState) : [];
    return h('div', { class: 'phone-station' },
      h('div', { class: 'row' }, h('span', { class: 'dot ' + (up ? 'idle' : 'unreachable') }), h('b', {}, st), h('span', { class: 'muted' }, up ? `${s.working ?? 0} working · ${s.needs_you ?? 0} need you` : s.why === 'clock' ? `clock differs by ${clockWords(s.clock_diff_ms || 0)}` : STATION_STATE[s.state] || s.state.replace('_', ' ')), keyChip(st), h('span', { class: 'grow' }),
        up ? h('button', { onclick: () => newPaneDialog(st) }, 'New') : null,
        s.state === 'unreachable' && !awayWhy(s) ? h('button', { onclick: () => rpc(api.station, 'fleet.wake', { station: st }).then((r) => toast(`Sent ${r.sent} wake packets. Give it a minute.`)) }, 'Wake') : null,
        s.state === 'unreachable' && !awayWhy(s) ? h('button', { onclick: () => rpc(api.station, 'fleet.start', { station: st }).then((r) => toast(`${st} is ${r.state}`)) }, 'Start') : null,
        up ? h('button', { class: 'danger', onclick: () => confirmBox(`Stop every agent on ${st}?`, 'Running turns are interrupted. Processes stay.', 'Stop', () => rpc(api.station, 'stop.all', { scope: s.self ? 'station' : st })) }, 'Stop') : null,
        // Everything else a station offers, with closing its panes and removing it.
        h('button', { class: 'station-more', 'aria-label': `More for ${st}`, title: `More for ${st}`, onclick: (e) => stationMenu(e, s) }, '⋯')),
      projects.map(pr => {
        const mine = panesOf(st).filter(p => p.project === pr.name && !p.exited).sort(byState);
        return h('div', { class: 'phone-project' },
          h('div', { class: 'row phone-project-head' }, h('b', {}, pr.name), pr.branch ? h('span', { class: 'muted' }, pr.branch) : null, linkedChip(st, pr)),
          projectLayouts(st).filter(x => x.project === pr.name).map(x => [layRow(x), viewRows(x)]),
          mine.length ? watchRow(st, pr) : null,
          mine.map(p => line(st, p)),
          !mine.length ? h('div', { class: 'row' }, h('span', { class: 'muted small grow' }, pr.open ? 'Nothing is running.' : 'Closed.'), pr.open ? null : h('button', { onclick: () => rpc(st, 'project.open', { name: pr.name }) }, 'Open')) : null);
      }),
      loose.length ? h('div', { class: 'phone-project' }, projects.length ? h('div', { class: 'row phone-project-head' }, h('b', {}, 'Other panes')) : null, loose.map(p => line(st, p))) : null);
  };
  // An agent's row is its session: its title, and a dot when it is unread.
  const row = (st, p) => (p.agent === 'shell' ? line(st, p, whereOf(st, p)) : sessionRow(st, p));
  const byStation = homeMode() === 'stations';
  return h('main', { class: 'phone phone-home' },
    h('section', { class: 'phone-needs' }, h('h2', {}, 'Needs you'), S.approvals.map(a => approvalRow(a)), blockers.map(x => row(x.st, x.p)),
      S.approvals.length || blockers.length ? null : h('p', { class: 'muted' }, 'Nothing is waiting for you.')),
    pins.length ? h('section', { class: 'phone-pinned' }, h('h2', {}, 'Pinned'), pins.map(x => x.p ? row(x.st, x.p) : x.lay ? layRow(x.lay, layShort(x.lay)) : h('div', { class: 'phone-row muted' }, x.gone))) : null,
    homeSwitch(),
    byStation ? null : h('section', { class: 'phone-recents' }, h('h2', {}, 'Recents'), recentRows()),
    byStation ? h('section', {}, h('h2', {}, 'Stations'), stationsByName().map(station), addRow(), twinsLine()) : null,
    byStation && globals.length ? h('section', { class: 'phone-layouts' }, h('h2', {}, 'Layouts'), globals.map(x => [layRow(x, 'every station'), viewRows(x)])) : null,
    byStation ? dotKeyLink() : null,
    stopRows(),
    byStation ? h('section', {}, h('button', { class: 'wide', onclick: fleetUsageDialog }, 'Usage: tokens and estimated cost'),
      h('a', { class: 'btn wide perf-btn', href: '#/perf' }, 'Performance', anyTrouble(S.perf) ? h('span', { class: 'perf-dot', 'aria-hidden': 'true' }) : null)) : null,
    byStation ? h('section', {}, h('button', { class: 'danger big wide', onclick: () => confirmBox('Stop the whole fleet?', 'Every running agent turn on every station is interrupted.', 'Stop everything', () => rpc(api.station, 'stop.all', { scope: 'fleet' })) }, 'Stop every agent in the fleet')) : null,
    h('div', { class: 'new-session-bar' }, h('button', { type: 'button', class: 'primary big wide new-session', dataset: { keep: 'new-session' }, onclick: (e) => { e.currentTarget.blur(); newSession(); } }, '+ New session')));
}

function phoneView() {
  const r = S.route;
  if (r.view === 'layout') {
    const lay = findLayout(r.layout);
    if (lay) return phoneLayoutView(lay);
    return h('main', { class: 'phone' }, h('p', {}, S.loaded ? 'That layout is not here any more.' : 'Loading…'), h('a', { class: 'btn', href: '#/fleet' }, '‹ Back'));
  }
  if (r.view === 'watch') return h('main', { class: 'phone phone-watch' },
    h('div', { class: 'row' }, h('a', { class: 'btn', href: '#/fleet' }, '‹ Back'), h('b', {}, `Watch · ${r.project}`), h('span', { class: 'muted' }, 'on ' + r.station)),
    watchView(r.station, r.project));
  if (r.view === 'pane') return phonePaneView(r.station, r.pane);
  if (r.view === 'perf') return perfView();
  return phoneList();
}

// A browser on another device asks a station to let it in, shows the code the
// owner must see on the other side, and waits for the answer.
async function signIn() {
  const post = (path, body) => fetch(path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }).then(r => r.json());
  const show = (...kids) => root.replaceChildren(h('main', { class: 'login' }, h('h1', {}, 'spandeck'), ...kids));
  const again = () => h('button', { onclick: () => { S.signIn = false; signIn(); } }, 'Ask again');
  S.signIn = true;
  const ua = navigator.userAgent;
  const name = (/iPhone/.test(ua) ? 'iPhone' : /iPad/.test(ua) ? 'iPad' : /Android/.test(ua) ? 'Android phone' : /Mac/.test(ua) ? 'Mac' : /Windows/.test(ua) ? 'Windows PC' : /Linux/.test(ua) ? 'Linux computer' : 'A device')
    + ' (' + (/Edg\//.test(ua) ? 'Edge' : /Chrome\//.test(ua) ? 'Chrome' : /Firefox\//.test(ua) ? 'Firefox' : /Safari\//.test(ua) ? 'Safari' : 'browser') + ')';
  let asked;
  try { asked = (await post('/api/device/request', { name })).result; } catch (e) { asked = null; }
  if (!asked) return show(h('p', { class: 'warn-text' }, 'This station did not accept the request. Check this device is on your Tailscale account.'), again());
  show(h('p', {}, 'Approve this browser on a station. It shows the code'),
    h('p', { class: 'big-code' }, asked.code),
    h('p', {}, `In a Control Deck that is already signed in, open Access and approve "${name}". Or on ${asked.station}, run:`),
    h('p', { class: 'pre' }, `spandeck approve ${asked.code}`),
    h('p', { class: 'muted' }, 'Approve only if the codes match. This page continues by itself.'));
  for (;;) {
    await new Promise(r => setTimeout(r, 2000));
    let got;
    try { got = (await post('/api/device/collect', { id: asked.id, collect: asked.collect })).result; } catch (e) { continue; }
    if (!got || got.status === 'waiting') continue;
    if (got.status === 'approved' && got.token) {
      localStorage.setItem('spandeck.token', got.token);
      location.reload();
      return;
    }
    return show(h('p', { class: 'warn-text' }, got.status === 'denied' ? 'The request was refused.' : 'The request ran out of time.'), again());
  }
}

// ------------------------------------------------- the link to this station
//
// What the screen says when the station serving this page stops answering
// (api.js: the heartbeat, and `linkLevel`). The audit found nothing at all
// on screen at 1 s, 5 s, 30 s and 5 minutes of a half-open link; the owner's
// rule: "This has to be well designed and seamless to the user. 100% uptime
// from every client, control deck, session etc". So:
//   under 0.3 s   nothing (an upgrade's reconnect is not worth a flash)
//   to 3 s        "reconnecting…"
//   3 to 10 s     amber, with the time; panes say typing is off; what is
//                 shown is marked "as of 12:01:05"
//   from 10 s     a bar: "alpha has not answered for 12 s. Your agents keep
//                 running there.", and a button for each other station's deck
//   from 60 s     "alpha last answered 1 min 12 s ago (12:01:05)"
// An open connection that does not answer ("no answer for 4 s") and one that
// is being made again ("reconnecting… 12 s") are told apart; both are counted
// from the last thing heard.
// The three pieces stay on the page and are filled in here, four times a
// second while anything is wrong, so that they are right while a menu or a
// dialog is open and the rest of the page is not being drawn again.
const spanWords = (ms) => { const s = Math.max(0, Math.round(ms / 1000)); return s < 60 ? `${s} s` : s < 3600 ? `${Math.floor(s / 60)} min${s % 60 ? ` ${s % 60} s` : ''}` : `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`; };
const clockOf = (at) => new Date(at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
const linkChip = h('span', { class: 'chip link-chip', role: 'status', hidden: true });
const linkAsOf = h('span', { class: 'chip warn as-of link-as-of', hidden: true });
const linkBar = h('div', { class: 'link-bar', role: 'alert', hidden: true });
// Carrying on through another station, the secure deck and the saved copy.
const fo = initFailover({
  api, update, render, hopOpen, hopTo,
  fleet: () => S.fleet,
  full: () => true,
  refresh: () => refreshSoon(0),
  linkTick: () => linkTick(),
  // Something written and not sent: in a message box on the page, or kept
  // for a view that is off it.
  hasDraft: () => draftCount() > 0 || [...document.querySelectorAll('textarea:not(.xterm-helper-textarea):not(.copy-buffer)')].some(t => t.value.trim()),
});
root.failover = fo;
let linkShown = 0, linkDrawn = 0, linkBarKey = '';
// What is on the page is from before a gap, and is marked so.
const linkOld = () => api.link().level >= 2;
function linkWords(l) {
  // An open connection that does not answer is not being made again: it
  // is said as what it is, from the moment it is known.
  if (l.level <= 1 && l.state !== 'silent') return 'reconnecting…';
  // Short: a phone's bar has room for this and the "as of" beside it.
  return l.state === 'silent' ? `no answer for ${spanWords(l.wall)}` : `reconnecting… ${spanWords(l.wall)}`;
}
function linkTick() {
  const l = api.link(), me = api.station || 'the station';
  // Refused for now, for a reason that is not this browser's.
  const waiting = !api.connected && !api.authError && api.waitWhy;
  const starting = !api.connected && !api.authError && !api.everConnected && !waiting;
  linkChip.hidden = !(l.level || waiting || starting);
  linkChip.className = 'chip link-chip ' + (l.level >= 2 || waiting ? 'warn' : 'bad');
  const chip = waiting ? 'waiting for Tailscale on the station' : starting ? 'connecting…' : linkWords(l);
  if (linkChip.textContent !== chip) linkChip.textContent = chip;
  linkChip.title = waiting ? api.waitWhy : l.level ? `This page has heard nothing from ${me} since ${clockOf(l.asOf)}. It keeps trying by itself.` : '';
  linkAsOf.hidden = l.level < 2;
  if (l.level >= 2) {
    const text = `as of ${clockOf(l.asOf)}`;
    if (linkAsOf.textContent !== text) linkAsOf.textContent = text;
    linkAsOf.title = `Everything on this page (who needs you, approvals, the inbox, every station's agents) is as it was at ${clockOf(l.asOf)}, when ${me} last answered.`;
  }
  linkBar.hidden = l.level < 3;
  // A page that can carry on through another station offers the ones that
  // answer, and takes the fastest after a few seconds (failover.js).
  if (l.level < 3) fo.calm(linkBar);
  if (l.level >= 3 && fo.drawBar(linkBar, l, spanWords)) { /* drawn there */ } else if (l.level >= 3) {
    const said = l.level >= 4 ? `${me} last answered ${spanWords(l.wall)} ago (${clockOf(l.asOf)}).` : `${me} has not answered for ${spanWords(l.wall)}.`;
    // The other stations' own decks, by the hand-over (`hopOpen`): the
    // buttons are made again only when the list changes, so that one being
    // clicked is not replaced under the finger.
    const others = S.fleet.stations.filter(s => hopOrigin(s));
    const key = others.map(s => s.station).join(' ');
    if (key !== linkBarKey || !linkBar.querySelector('.link-said') || linkBar.querySelector('.fo-list')) {
      linkBarKey = key;
      linkBar.replaceChildren(h('div', { class: 'grow' }, h('b', { class: 'link-said' }), ' Your agents keep running there. ',
        h('span', { class: 'muted' }, 'This page keeps trying by itself. What it shows is from before.')),
        ...others.map(s => h('button', { onclick: () => hopOpen(s) }, `Open ${s.station}'s deck`)));
    }
    const b = linkBar.querySelector('.link-said');
    if (b.textContent !== said) b.textContent = said;
  }
  // The panes: typing is off, and they say so.
  if (l.level !== linkShown) {
    linkShown = l.level;
    for (const t of S.terms.values()) t.setLink(linkPaneWords(l));
  }
  // The marks on the tiles are part of the page's drawing: drawn again when
  // the level changes, as soon as nothing forbids it.
  if (l.level !== linkDrawn && !document.querySelector('.menu') && !document.body.classList.contains('dragging')) { linkDrawn = l.level; updateSoon(); }
}
const linkPaneWords = (l) => (l.level >= 2 ? `Not connected to ${api.station}: typing is off. What is shown is as of ${clockOf(l.asOf)}.` : null);
setInterval(linkTick, 250);
// The link is back after a gap the owner was told of.
function linkBack(ms) {
  // A pane that said "typing is off" at a key says so no longer.
  for (const t of S.terms.values()) t.setLink(null);
  linkTick();
  // (A page that moved to another station said so itself.)
  if (ms >= 10000 && !api.target) toast(`${api.station} is back after ${spanWords(ms)}.`);
  refreshSoon(0);
}

// ------------------------------------------------------------------- render

let drawnAt = null;
function render() {
  // The saved copy of the deck, opened while no station answers.
  if (fo.none()) { root.replaceChildren(fo.noneScreen()); return; }
  if (!api.connected && api.authError) {
    if (S.signIn) return;
    const needsSignIn = /^sign in/.test(api.authError);
    // The deck that opened this page may be about to sign it in.
    if (needsSignIn) hopAsk();
    root.replaceChildren(h('main', { class: 'login' }, h('h1', {}, 'spandeck'),
      needsSignIn ? h('p', {}, 'This browser is not signed in to your fleet yet.') : h('p', {}, api.authError),
      needsSignIn ? h('button', { class: 'primary', onclick: signIn }, 'Sign in this browser') : null,
      h('p', { class: 'muted' }, needsSignIn
        ? 'You approve it once, on a station or in a Control Deck that is already signed in. On the station itself you can instead run "spandeck cc" and open the address it prints.'
        : 'On this computer, run "spandeck cc" and open the address it prints. From another device, open this station by its Tailscale name while signed in to the same Tailscale account.')));
    return;
  }
  closeMenu();
  pendingLinks = null;
  layCtx = null;
  S.route = parseRoute();
  for (const t of S.terms.values()) t._used = false;
  chatBeforeRender();
  viewsBeforeRender();
  const r = S.route;
  // The page is drawn again for every change, every few seconds at least:
  // whatever was scrolled stays where it was. (On a phone the list jumped
  // back to the top while the owner was scrolling down to "Add a machine".)
  // The list on the left keeps its place across views; the rest only while
  // the same view is shown.
  const sideTop = document.querySelector('.sidebar')?.scrollTop || 0;
  const sameView = drawnAt === location.hash;
  drawnAt = location.hash;
  const scrollKey = (e) => e.tagName + '.' + (e.classList[0] || '');
  const scrolled = sameView ? [...root.querySelectorAll('*')].filter(e => e.scrollTop > 0 && !e.closest('.xterm, .sidebar')).map(e => [scrollKey(e), e.scrollTop]) : [];
  const pageTop = sameView ? (document.scrollingElement?.scrollTop || 0) : 0;
  let main;
  if (isPhone()) main = phoneView();
  else if (r.view === 'pane') main = paneView(r.station, r.pane);
  else if (r.view === 'layout') main = layoutView(r.layout);
  else if (r.view === 'watch') main = watchPage(r.station, r.project);
  else if (r.view === 'station') main = stationView(r.station);
  else if (r.view === 'perf') main = perfView();
  else main = fleetView();
  if (!layCtx) arranging = null;
  // A view that is already on the page stays attached: replacing it would
  // take the keyboard away from whatever is being typed into.
  const kids = [linkBar, fo.viaBar(), fo.shellBar(), fo.secureBar(), signInBar(), versionBar(), vaultPromptBox, main].filter(Boolean);
  if (root.children.length === kids.length + 1 && kids.every((k, i) => root.children[i + 1] === k)) root.replaceChild(topbar(), root.children[0]);
  else root.replaceChildren(topbar(), ...kids);
  const side = document.querySelector('.sidebar');
  if (side && sideTop) side.scrollTop = sideTop;
  for (const [key, top] of scrolled) {
    const e = [...root.querySelectorAll(key.endsWith('.') ? key.slice(0, -1) : key)].find(x => !x.closest('.xterm, .sidebar'));
    if (e) e.scrollTop = top;
  }
  if (pageTop && document.scrollingElement) document.scrollingElement.scrollTop = pageTop;
  phoneLeft();
  for (const [key, t] of S.terms) if (!t._used) { t.dispose(); S.terms.delete(key); }
  // A conversation that left the page stops being watched; one still on it
  // is where it was scrolled to.
  chatAfterRender();
  watchAfterRender();
  viewsAfterRender();
  viewsShown();
  filesSync();
  // Each terminal has its look before it is first drawn.
  applyLook();
  for (const t of S.terms.values()) {
    if (t._fresh) { t._fresh = false; t.mount(); }
    else t.layout();
  }
  const lay = r.view === 'layout' ? findLayout(r.layout) : null;
  const what = r.view === 'pane' ? (findPane(r.station, r.pane)?.name || 'pane') : lay ? lay.name : r.view === 'watch' ? `Watch ${r.project}` : r.view === 'perf' ? 'Performance' : r.station || '';
  document.title = (what ? what + ' · ' : '') + 'spandeck';
  // Lines are drawn once the panes have their places on screen.
  const links = pendingLinks;
  pendingLinks = null;
  if (links) requestAnimationFrame(() => { const box = document.querySelector(links.selector); if (box) drawLinks(box, links.links); });
}

// Data changed: redraw everything except live terminals, which are reused.
let draws = 0;
function update() {
  // Do not redraw under an open dialog's feet, while a menu is open, or in
  // the middle of dragging a pane or the line between two.
  if (document.querySelector('.menu') || document.body.classList.contains('dragging')) return false;
  // Nor while a name is being typed in a layout's file browser, or a file dragged.
  if (filesBusy()) return false;
  const active = document.activeElement;
  const typing = active && active.closest && active.closest('.dialog');
  const focusedTerm = [...S.terms.values()].find(t => t.el.contains(active));
  // A hidden pane's chip is drawn again with its new state: the one that had
  // the keyboard has it afterwards.
  const keep = active?.dataset?.keep;
  vaultPromptSync();
  render();
  if (focusedTerm && !typing) focusedTerm.focus();
  if (keep && !typing) [...document.querySelectorAll('[data-keep]')].find(x => x.dataset.keep === keep)?.focus();
  // How many times the page was redrawn for new data. Nothing in the deck
  // reads it; a test waits on it instead of sleeping through a redraw.
  root.dataset.draws = String(++draws);
  return true;
}

// New data arrived, which nobody on this page asked for just now: the page
// is drawn with it soon. `update` is for what the owner did here (a tap on
// this page, a face chosen), and draws at once, as it always did.
//   - However many answers and events arrive, one drawing a frame.
//   - Two a second, kept up (DRAW_RATE): what the page already allowed
//     itself for an agent's numbers (`pane.usage`, above). Up to DRAW_BURST
//     that come close together are each drawn as they come, the first at
//     once, as before: a page that opens hears from every station within
//     a moment, and one change is shown as fast as it was. So in any t
//     seconds there are at most 4 + 2t; what comes faster waits for its
//     turn, half a second at most. Drawing the page makes every row of it
//     again, and a phone that does that at every event does nothing else.
//   - Data that follows the owner's own tap, choice or Enter within
//     ACTED_MS is their answer: it is drawn at the next frame, whatever
//     was drawn before it.
//   - Not while a finger is on the page or it is being scrolled (a touch
//     that moves, the wheel, and the scrolling that goes on after a finger
//     lifts): a list made again under a finger loses the finger's scroll,
//     and a row made again under it loses the tap. It is drawn SCROLL_REST
//     after the last movement; and after SCROLL_CAP whatever happens, so
//     that a finger that rests on the glass, or a lift the browser never
//     told the page of, does not leave the page as it was.
// What is to be drawn is whatever the page holds when it is drawn: nothing
// is queued, so the last state is the one shown. What does not go through
// here, and waits for none of this: what a terminal shows and its echo
// (term.js, on the pane's own channel), a conversation's thread, cards and
// message box (chat.js, the same), a view opened (`render`, on the
// address), dialogs, menus and toasts.
const DRAW_RATE = 2, DRAW_BURST = 4, FRAME_MS = 16, ACTED_MS = 2000, SCROLL_REST = 200, SCROLL_CAP = 5000;
let drawWanted = 0, drawTimer = null, drawFrame = false, lastDraw = -1000, actedAt = -ACTED_MS;
let drawsLeft = DRAW_BURST, leftAt = 0;
let finger = false, fingerAt = 0, movedAt = -SCROLL_REST;
const scrolling = (now = performance.now()) => (finger && now - fingerAt < SCROLL_CAP) || now - movedAt < SCROLL_REST;
// How many drawings may be made now: DRAW_RATE more each second, DRAW_BURST at most.
const drawsNow = (now) => { drawsLeft = Math.min(DRAW_BURST, drawsLeft + (now - leftAt) * DRAW_RATE / 1000); leftAt = now; return drawsLeft; };
const drew = () => { drawsLeft = Math.max(0, drawsLeft - 1); lastDraw = performance.now(); };
function updateSoon() {
  const now = performance.now();
  // Nothing is waiting, nothing is in the way and none was drawn in this
  // frame: now, as before, so that what follows the data finds the page
  // drawn with it.
  if (!drawWanted && !scrolling(now) && drawsNow(now) >= 1 && now - lastDraw >= FRAME_MS && update() !== false) return drew();
  if (!drawWanted) drawWanted = now;
  drawPlan();
}
function drawPlan() {
  if (drawFrame || !drawWanted) return;
  clearTimeout(drawTimer);
  const now = performance.now(), waited = now - drawWanted, left = drawsNow(now);
  let wait = now - actedAt < ACTED_MS || left >= 1 ? 0 : Math.ceil((1 - left) * 1000 / DRAW_RATE);
  if (scrolling(now) && waited < SCROLL_CAP) wait = Math.max(wait, Math.min(SCROLL_REST, SCROLL_CAP - waited));
  if (wait > 0) { drawTimer = setTimeout(drawPlan, wait); return; }
  // (A page that is not in front is given no frame: the timer draws it.)
  drawFrame = true;
  let done = false;
  const run = () => {
    if (done) return;
    done = true;
    drawFrame = false;
    const at = performance.now();
    if (scrolling(at) && at - drawWanted < SCROLL_CAP) return drawPlan();
    // A menu is open, or a pane is being dragged: when it is over.
    if (update() === false) { drawTimer = setTimeout(drawPlan, 300); return; }
    drawWanted = 0;
    drew();
  };
  requestAnimationFrame(run);
  setTimeout(run, 100);
}
{
  const opt = { capture: true, passive: true };
  const acted = () => { actedAt = performance.now(); };
  for (const kind of ['click', 'submit', 'change']) addEventListener(kind, acted, opt);
  addEventListener('keydown', (e) => { if (e.key === 'Enter') acted(); }, opt);
  addEventListener('touchstart', () => { finger = true; fingerAt = performance.now(); }, opt);
  addEventListener('touchmove', () => { fingerAt = movedAt = performance.now(); }, opt);
  const lifted = (e) => { finger = (e.touches?.length || 0) > 0; if (drawWanted) drawPlan(); };
  addEventListener('touchend', lifted, opt);
  addEventListener('touchcancel', lifted, opt);
  addEventListener('wheel', () => { movedAt = performance.now(); }, opt);
  // What goes on scrolling after the finger lifts. (A scroll the page made
  // itself, with nothing of the owner's just before it, is not one.)
  document.addEventListener('scroll', () => { const now = performance.now(); if (finger || now - movedAt < SCROLL_REST) movedAt = now; }, opt);
}

addEventListener('hashchange', render);
addEventListener('resize', () => { clearTimeout(S._rs); S._rs = setTimeout(render, 150); });
addEventListener('keydown', (e) => {
  const mod = e.ctrlKey || e.metaKey;
  if (mod && e.key.toLowerCase() === 'k') { e.preventDefault(); palette(); return; }
  // With a layout on screen: put a pane beside the focused one, or take the
  // focused one out.
  if (mod && e.shiftKey && layCtx) {
    const k = e.key.toLowerCase(), ctx = layCtx;
    if (k === 'e' || k === 'o') { e.preventDefault(); pickPane((st, p) => addToLayout(ctx.lay, st, p, k === 'e' ? 'row' : 'col'), ctx); }
    const n = k === 'w' ? focusedLeaf(ctx) : null;
    if (n) { e.preventDefault(); ctx.remove(ctx.refOf(n)); render(); }
  }
  // Esc zooms out, unless a terminal has the keyboard (agents need Esc).
  if (e.key === 'Escape' && !document.querySelector('.backdrop,.menu') && !document.activeElement?.closest('.pane')) {
    const r = S.route;
    if (r.view !== 'fleet') go(r.view !== 'station' && r.station ? stationUrl(r.station) : '#/fleet');
  }
}, false);

// A phone's keyboard covers the bottom of the page without making the page
// smaller, and the browser may slide what is in sight up the page to show
// the box being typed in (iOS does). So the app is made to be exactly what
// is in sight: as tall as the visual viewport, and placed where it is. Its
// last row (a conversation's message box) is then directly above the
// keyboard and its first (a pane's bar) at the top, wherever the browser
// slid to; the page itself never scrolls (app.css: the body is fixed).
// While the keyboard is up the bars that are not the pane's step aside.
// (What the viewport is, is read each time: `window.visualViewport`.)
if (window.visualViewport) {
  // The tallest the screen has been at each width: a browser that makes
  // the window itself shorter with the keyboard up (and not only what is
  // in sight) still has its keyboard known.
  const tallest = new Map();
  const fit = () => {
    const vv = window.visualViewport, app = document.getElementById('app');
    const wide = Math.round(vv.width), was = tallest.get(wide) || 0;
    if (Math.max(vv.height, window.innerHeight) > was) tallest.set(wide, Math.max(vv.height, window.innerHeight));
    // (Zoomed in with two fingers, what is in sight is a part of the page
    // and no keyboard: the page stays as it is, to be looked over.)
    const zoomed = (vv.scale || 1) > 1.01;
    const top = zoomed ? 0 : Math.max(0, Math.round(vv.offsetTop || 0));
    app.style.height = zoomed ? '' : vv.height + 'px';
    app.style.top = top ? top + 'px' : '';
    const keyboard = !zoomed && vv.height < Math.max(window.innerHeight, tallest.get(wide) || 0) * 0.78;
    app.classList.toggle('kbd', keyboard);
    app.classList.toggle('kbd-term', keyboard && !!document.activeElement?.closest?.('.xterm'));
    // Dialogs, sheets and toasts are in what is in sight too (app.css, the
    // backdrop): where it begins, how tall it is, and what is under it.
    const page = document.documentElement;
    for (const [name, px] of [['--vv-top', top], ['--vv-height', vv.height], ['--vv-bottom', Math.max(0, window.innerHeight - top - vv.height)]]) {
      if (zoomed) page.style.removeProperty(name); else page.style.setProperty(name, px + 'px');
    }
    page.classList.toggle('kbd', keyboard);
    if (window.scrollY || document.documentElement.scrollTop) window.scrollTo(0, 0);
    // The box being typed in, in a dialog that just got shorter.
    keepInView(document.activeElement);
    // A conversation's log keeps its place in its new room, now.
    chatResized();
  };
  window.visualViewport.addEventListener('resize', fit);
  window.visualViewport.addEventListener('scroll', fit);
  window.addEventListener('resize', fit);
  window.addEventListener('scroll', fit, { passive: true });
  // (And again once the keyboard has finished moving: iOS has told of the
  // viewport's new size before it was final, and not again after.)
  const soon = () => { for (const ms of [50, 350, 800]) setTimeout(fit, ms); };
  document.addEventListener('focusin', soon);
  document.addEventListener('focusout', soon);
  fit();
}

// The conversation view is given what it needs of the page: nothing of it
// reaches a station but through the calls the page already makes.
// The microphone of every message box: where the secure deck is, and which
// stations turn a recording into text.
initMic({
  api, hasCap, hopTo,
  stationsWith: (cap) => S.fleet.stations.filter(s => s.state === 'online' && (s.caps || []).includes(cap)).map(s => s.station),
  secureDeck: () => { const me = S.fleet.stations.find(s => s.self), t = me?.https; return t && t.on && t.name ? { origin: `https://${t.name}:${t.port}`, what: `${me.station}'s secure deck` } : null; },
});
initViews({
  api, isPhone, hasCap, online, panesOf, go, paneUrl, watchUrl, render, watchView, addLeaf, addToLayout, newPaneDialog, refresh, findPane, paneMark, layUrl, layWhere,
  stations: () => stationsByName(),
  projects: (st) => S.projects.get(st) || [],
  loaded: (st) => S.loaded && heard(st),
  toPane: toPaneFrom,
  has: (lay, st, p) => ctxFor(lay).has(st, p),
});
initFiles({
  api, isPhone, findPane,
  stations: () => S.fleet.stations,
  // Move agent here: the pane's program is started again in `cwd`.
  movePane: (st, p, cwd) => api.on(st, 'pane.restart', { pane: p.id, cwd }).then((r) => { reattach(st, p.id); stationSoon(st, 60); return r; }),
});
// A pane's header chips (chips.js): what they read of this page, and the
// dialogs their warnings open.
initChips({ api, hasCap, isPhone, vault: (st) => S.fleet.stations.find(s => s.station === st)?.vault || null, openVault: vaultPanel, openAgents: agentsPanel, openInbox: inboxPanel });
initChat({
  files: (st, p) => (hasCap(st, 'files') ? () => filesFor(st, p) : null),
  api, hasCap, isPhone, update, go, paneUrl, rpc, findPane, paneMenu, usage: usageDialog, online, onlineStations, panesOf, freeName, lastSessionNote, pref, setPref,
  AGENTS: AGENT_CHOICES,
  approvals: () => S.approvals,
  stations: () => stationsByName(),
  projects: (st) => S.projects.get(st) || [],
  panes: () => S.fleet.stations.flatMap(s => liveAgents(s.station).map(p => ({ st: s.station, p }))),
  paneMark: (p) => paneMark(p),
  agentMark: (kind) => h('span', { class: 'ico-wrap' }, paneIcon(kind)),
});

// The page is drawn again when its connection changes: the terminal that
// had the keyboard has it afterwards (see `keyboardBack`).
api.onStatus(() => {
  const active = document.activeElement;
  const had = [...S.terms.values()].find(t => t.el.contains(active));
  linkTick();
  render();
  if (had && had.el.isConnected && !document.querySelector('.dialog') && !had.el.contains(document.activeElement)) had.focus();
});
fo.shell();
fo.boot().then((ok) => {
  if (!ok) return render();
  if (!location.hash || location.hash === '#') history.replaceState(null, '', '#/fleet');
  render();
  refresh();
});

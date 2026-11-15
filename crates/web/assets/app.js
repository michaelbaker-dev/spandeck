// The Control Deck. The list on the left is the same on every station; what
// is selected in it is shown on the right, and each selection has a URL:
//   #/fleet                      every station
//   #/s/<station>                one station: every running pane as a tile
//   #/s/<station>/p/<pane>       one pane, full size
//   #/s/<station>/l/<project>/<layout>   a project's layout
//   #/s/<station>/w/<project>    Watch: a project's agents and who is talking
//   #/l/<layout>                 a global layout (kept by the whole fleet)
// On a phone the same URLs show a small view: status, blockers, approvals,
// stop, one pane to talk to, and a layout stepped through a pane at a time.

import { Api } from './api.js';
import { PaneTerm } from './term.js';
import { h, showMenu, closeMenu, toast, dialog, confirmBox, field, ago, dot, stateOf, stateRank, STATE_LABEL, copyText, fmtTokens, fmtCost, exact, usageBrief, usageTitle, contextText } from './ui.js';
import { paneIcon, iconName, layoutIcon, watchIcon, stationIcon, pinIcon } from './icons.js';
import { lookState, setDoc, pageLook, paneLook, applyPage, applyRemembered, rememberPage, loadFonts, fontListeners } from './look.js';
import { lookEditor } from './lookedit.js';

// The page opens in the look this browser last had, before anything is asked.
applyRemembered();

const api = new Api();
const root = document.getElementById('app');
const isPhone = () => matchMedia('(max-width: 760px)').matches;

const S = {
  route: { view: 'fleet' },
  fleet: { stations: [], stops: [] },
  panes: new Map(),      // station -> [pane]
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
  if (parts[0] === 's' && parts[1]) {
    const station = parts[1];
    if (parts[2] === 'p' && parts[3]) return { view: 'pane', station, pane: parts[3] };
    if (parts[2] === 'l' && parts[3] && parts[4]) return { view: 'layout', station, layout: `${station}/${parts[3]}/${parts[4]}` };
    if (parts[2] === 'w' && parts[3]) return { view: 'watch', station, project: parts[3] };
    return { view: 'station', station };
  }
  return { view: 'fleet' };
}
const go = (hash) => { if (location.hash !== hash) location.hash = hash; else render(); };
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

async function refreshStation(st) {
  try {
    const [panes, projects, tasks, msgs] = await Promise.all([api.on(st, 'pane.list'), api.on(st, 'project.list'),
      api.on(st, 'task.list', { active: true, limit: 100 }).catch(() => []), api.on(st, 'msg.recent', { limit: 60 }).catch(() => [])]);
    S.panes.set(st, panes);
    S.projects.set(st, projects);
    S.tasks.set(st, tasks);
    S.msgs.set(st, msgs);
  } catch (e) { /* unreachable: the fleet view says so */ }
}

async function refresh() {
  if (!api.connected) return;
  try { S.fleet = await api.call('fleet.view'); } catch (e) { return; }
  const stations = S.fleet.stations.filter(s => s.state === 'online').map(s => s.station);
  await Promise.all(stations.map(refreshStation));
  const lists = await Promise.all(stations.map(st => api.on(st, 'approval.list').catch(() => [])));
  S.approvals = lists.flat();
  await loadNotes(stations);
  try { S.trust = await api.call('trust.status'); } catch (e) { /* keep */ }
  try { S.adding = await api.call('fleet.add_status'); addChanged(); } catch (e) { /* keep */ }
  // Always from the station the deck was opened on: every station has the same copy.
  try { S.deck = await api.call('deck.get'); } catch (e) { /* keep */ }
  await loadLook();
  await loadPrefs();
  S.loaded = true;
  update();
  vaultAtOpen();
  migrateWorkspaces();
  bindGlobalLeaves();
}

// The inbox is the whole fleet's: each station keeps the notices it raised,
// and every station that answers is asked for them. A station that does not
// answer is named as not counted (S.notesAway), never passed over in silence.
// A notice every station raised for the same thing (a request to be let in)
// is one entry that names them all.
const fleetAway = () => S.fleet.stations.filter(s => s.state !== 'online' && s.state !== 'not_linked').map(s => s.station);
const announced = new Set();
let notesSeeded = false;
async function loadNotes(stations) {
  const failed = [];
  const lists = await Promise.all(stations.map(st => api.on(st, 'notify.list', { limit: 60 })
    .then(list => list.map(n => ({ ...n, station: st })))
    // What it last said is kept on screen; it is named as not counted.
    .catch(() => { failed.push(st); return S.notes.flatMap(n => n.at.filter(a => a.station === st).map(a => ({ ...n, ...a }))); })));
  const merged = new Map();
  for (const n of lists.flat().sort((a, b) => b.ts - a.ts)) {
    const key = n.pane ? `${n.station}/${n.id}` : `${n.kind}\n${n.title}\n${n.body}`;
    const had = merged.get(key);
    if (had) { if (!had.at.some(a => a.station === n.station)) had.at.push({ station: n.station, id: n.id }); had.read = had.read && n.read; }
    else merged.set(key, { ...n, at: [{ station: n.station, id: n.id }] });
  }
  S.notes = [...merged.values()].slice(0, 120);
  S.notesAway = [...new Set([...fleetAway(), ...failed])].sort();
  // A notice from a station that does not tell the others (one from before
  // it could) is announced when it first shows in the list.
  for (const n of S.notes) {
    const fresh = n.at.filter(a => !announced.has(`${a.station}/${a.id}`));
    for (const a of n.at) announced.add(`${a.station}/${a.id}`);
    if (notesSeeded && !n.read && fresh.length === n.at.length) notifyBrowser(n);
  }
  notesSeeded = true;
}
// Mark a notice read at every station that holds it.
const readNote = (n) => Promise.all(n.at.map(a => api.on(a.station, 'notify.read', { id: a.id }).catch(() => {})));

// The look document: themes, and what the deck, a layout or a pane uses. Like
// the deck document it is asked of the station the deck was opened on.
async function loadLook() {
  try { setDoc(await api.call('look.get')); } catch (e) { return; }
  // What is being tried in the editor is not what to open with next time.
  if (!lookState.preview) rememberPage(pageLook(null));
  // The fonts the owner uploaded, from this station. Not waited for: a
  // terminal shows in the next font of its list until one arrives.
  loadFonts((id) => api.file('/api/font/' + id));
}
// A font arrived after terminals were drawn with another in its place.
fontListeners.add(() => { for (const t of S.terms.values()) t.remeasure(); });

// Show every look again: the page's (the deck's, or the layout's on screen)
// and each terminal's. Called on every redraw and by the Appearance editor
// as a value changes.
function applyLook() {
  const layout = S.route.view === 'layout' ? S.route.layout : null;
  const page = pageLook(layout);
  applyPage(page);
  for (const t of S.terms.values()) {
    const { look, own } = paneLook(layout, `${t.station}/${t.pane.id}`);
    t.setLook(look, own ? page : null);
  }
}
// The editor for the deck, a layout ({key, name}) or a pane ({key, name}).
function appearance(level, scope = {}) {
  lookEditor({ api, apply: applyLook, reload: async () => { await loadLook(); } },
    { level, ...scope, layoutKey: S.route.view === 'layout' ? S.route.layout : null });
}
const paneLookKey = (st, p) => `${st}/${p.id}`;

let refreshTimer = null;
function refreshSoon(ms = 120) { clearTimeout(refreshTimer); refreshTimer = setTimeout(refresh, ms); }
setInterval(() => { if (document.visibilityState === 'visible') refresh(); }, 2500);

api.onEvent((ev) => {
  if (ev.kind === 'pane.resized') {
    for (const t of S.terms.values()) if (t.station === api.station && t.pane.id === ev.pane) t.resizedBy(ev.data.controller, ev.data.cols, ev.data.rows);
    return;
  }
  // A line from a machine being added: shown as it arrives, nothing re-read.
  if (ev.kind === 'fleet.add.output') { if (addDlg) addDlg.line(ev.data); return; }
  // Raised here, or at another station of the fleet, which told this one.
  if (ev.kind === 'notify.new') { announced.add(`${ev.data.station}/${ev.data.id}`); notifyBrowser(ev.data); }
  if (ev.kind === 'connection.restored') { for (const t of S.terms.values()) t.attach(); }
  if (/^(pane|agent|approval|project|fleet|stop|notify|task|recipe|msg|station|connection|events|vault|trust|deck|look|prefs)\./.test(ev.kind)) refreshSoon();
});

const lastSaid = new Map(); // what was announced -> when, so one thing every station raised is said once
function notifyBrowser(n) {
  const same = `${n.kind}\n${n.title}\n${n.body}`, now = Date.now();
  for (const [k, at] of lastSaid) if (now - at > 15000) lastSaid.delete(k);
  if (lastSaid.has(same)) return;
  lastSaid.set(same, now);
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

const rpc = (st, method, params) => api.on(st, method, params).then((r) => { refreshSoon(60); return r; }).catch((e) => { toast(e.message, 'error'); throw e; });

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
    return h('form', { onsubmit: (e) => { e.preventDefault(); const text = box.value; close(); term.pasteText(text).then((ok) => ok && term.focus()); } },
      h('p', { class: 'muted' }, 'This browser does not let a page read the clipboard here. Press Cmd-V or Ctrl-V in the box, then Paste. Nothing runs until you press Enter in the pane.'),
      box, h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Cancel'), h('button', { class: 'primary', type: 'submit' }, 'Paste')));
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
function paneMenu(e, st, p, where, term) {
  const s = stateOf(p), agent = p.agent !== 'shell', alive = !p.exited;
  const act = (m, extra) => () => rpc(st, m, { pane: p.id, ...extra }).then((r) => {
    // A closed pane's own look goes with it. Its station sees to that; one
    // from before themes does not know how, so it is also asked for here.
    if (m === 'pane.close' && lookState.doc.panes[paneLookKey(st, p)]) api.call('look.set', { level: 'pane', key: paneLookKey(st, p), choice: null }).catch(() => {});
    // A restarted pane is a new program behind the same name: show it.
    if (m === 'pane.restart') for (const t of S.terms.values()) if (t.station === st && t.pane.id === p.id) { if (t.chan !== null) api.detach(t.chan); t.chan = null; t.attach(); }
    return r;
  });
  const ctx = isPhone() ? null : layCtx, inLay = ctx && ctx.has(st, p), pin = panePin(st, p);
  // In the layout on screen a pane can be hidden: it keeps running and keeps
  // its place, and stays in sight as a chip beside the layout's name.
  const leaf = inLay ? ctx.leafOf(st, p) : null, seen = ctx ? ctx.shown().length : 0;
  showMenu(e.clientX, e.clientY, [
    { title: `${p.name} · ${p.address}` },
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
    agent && { label: p.watch ? 'Stop notifying when it finishes' : 'Notify me when it finishes', action: act('pane.set', { watch: !p.watch }) },
    { label: p.muted ? 'Unmute notifications' : 'Mute notifications', action: act('pane.set', { muted: !p.muted }) },
    '-',
    agent && { label: 'Usage', action: () => usageDialog(st, p) },
    { label: 'Show changes (diff)', action: () => diffPanel(st, p) },
    agent && { label: 'Standing orders for this pane', action: () => ordersPanel(st, p) },
    agent && p.project && alive && { label: 'Start a review loop on its work', action: () => rpc(st, 'recipe.review', { pane: p.id }).then(() => toast('Review loop started')) },
    agent && alive && { label: 'Run a command in it…', action: () => commandDialog(st, p) },
    { label: 'Send it a message', action: () => messageDialog(st, p) },
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

// ------------------------------------------------------------------ dialogs

const AGENT_CHOICES = [['claude', 'Claude Code'], ['codex', 'Codex'], ['opencode', 'OpenCode'], ['antigravity', 'Antigravity'], ['shell', 'Shell']];

function newPaneDialog(st, project, then) {
  dialog('New pane on ' + st, (close) => {
    const agent = h('select', {}, AGENT_CHOICES.map(([v, label]) => h('option', { value: v }, label)));
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
    let canContinue = true, asked = 0, askTimer = null;
    const ask = async () => {
      const n = ++asked;
      const isAgent = agent.value !== 'shell';
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
      const params = { agent: agent.value, name: name.value.trim() || undefined, project: proj.value || undefined, cwd: cwd.value || undefined, resume_last: resumeLast || undefined };
      const pane = await rpc(st, 'pane.spawn', params);
      close();
      if (resumeLast && !pane.resumed) toast('No saved conversation to continue here: a new one was started');
      then ? then(pane.id) : go(paneUrl(st, pane.id));
    };
    return h('form', { onsubmit: (e) => { e.preventDefault(); submit(); } },
      field('Run', agent), field('Name', name, 'A name is used once in a project.'), field('Project', proj), field('Folder', cwd), lastRow,
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
async function loadPrefs() {
  let doc;
  try { doc = await api.call('prefs.get'); } catch (e) { S.prefs = null; return; }
  if (prefSaving) return; // a change of this page's is on its way: not undone by an older answer
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
// The stations of this fleet other than the one the deck is open on.
const fleetMates = () => (S.fleet?.stations || []).filter(s => !s.self && s.state !== 'not_linked').map(s => s.station);

// Stations and browsers asking to be let in, at any station of the fleet.
// Approving one gives it control of every station, so the code shown here
// must match the one it shows.
function trustPanel() {
  dialog('Stations and browsers', (close) => {
    const t = S.trust || {};
    const answer = (r, yes) => rpc(api.station, yes ? 'trust.approve' : 'trust.deny', { request: r.id }).then(close);
    return h('div', {},
      h('h3', {}, 'Asking to be let in'),
      (t.requests || []).length ? h('div', { class: 'list' }, t.requests.map(r => h('div', { class: 'note unread' },
        h('div', {}, h('b', {}, r.kind === 'station' ? `Station ${r.name}${cameWith(r.with)}` : r.name), h('span', { class: 'muted' }, ` · from ${r.from}${r.at && r.at !== t.station ? ` · asked at ${r.at}` : ''}`)),
        h('div', {}, 'It shows the code ', h('b', { class: 'code' }, r.code), '. Approve only if you started this and the codes match: it will be able to control every station.',
          (r.with || []).length ? ` Its whole fleet joins this one: every station that holds its key, whatever names it gives. Browsers signed in to those stations sign in again; browsers signed in to this fleet are not affected.` : '',
          r.at && r.at !== t.station ? ` Your answer is passed to ${r.at}, which decides.` : ''),
        h('div', { class: 'row end' }, h('button', { class: 'mini', onclick: () => answer(r, false) }, 'Refuse'), h('button', { class: 'mini primary', onclick: () => answer(r, true) }, 'Approve')))))
        : h('p', { class: 'muted' }, 'Nothing is waiting.'),
      t.outgoing ? h('p', { class: t.outgoing.problem ? 'warn-text' : '' }, t.outgoing.problem
        ? `Joining ${t.outgoing.station}'s fleet did not finish: ${t.outgoing.problem}.`
        : `Waiting for an approval at ${t.outgoing.station}, or at any station of its fleet, of the request that shows the code ${t.outgoing.code}. ${(t.outgoing.with || []).length ? `This station${cameWith(t.outgoing.with)} then move into one fleet with ${t.outgoing.station}.` : `This station then joins ${t.outgoing.station}'s fleet.`}`) : null,
      h('h3', {}, 'Browsers signed in here'),
      (t.devices || []).length ? h('div', { class: 'list' }, t.devices.map(d => h('div', { class: 'note' },
        h('div', { class: 'row' }, h('b', {}, d.name), h('span', { class: 'muted' }, ` · from ${d.from} · ${ago(d.approved_ms)} ago`), h('span', { class: 'grow' }),
          h('button', { class: 'mini danger', onclick: () => rpc(api.station, 'device.revoke', { device: d.id }).then(close) }, 'Sign out')))))
        : h('p', { class: 'muted' }, 'None. A browser on another device asks to sign in the first time it opens the Control Deck.'),
      h('p', { class: 'muted' }, `Fleet ${t.fleet || ''}. To add a station, run "spandeck fleet --join ${api.station}" on it, then approve it here or at any station of this fleet. If it is in a fleet of its own, every station of that fleet joins with it: there is one fleet afterwards.`));
  });
}

// A join is a merge: the stations of this fleet all move into the other
// one, after one approval on that side.
function joinFleet(station) {
  rpc(api.station, 'fleet.join', { station }).then((r) => {
    const w = r.with || [], n = w.length + 1;
    dialog(w.length ? `Merging this fleet into ${station}'s` : `Joining ${station}'s fleet`, () => h('div', {},
      h('p', {}, `${api.station} asked to join. It shows the code `, h('b', { class: 'code' }, r.code), '.'),
      h('p', {}, `At ${station}, or at any station of its fleet, approve the request with that code: under Access in the Control Deck, or with "spandeck approve ${r.code}". One approval is all it takes.`),
      h('p', {}, w.length
        ? `${api.station}${cameWith(w)} then move into one fleet with ${station}. A station that is off at that moment is moved when it is next seen (for 30 days; after that it joins on your approval).`
        : `${api.station} then joins ${station}'s fleet.`),
      h('p', { class: 'muted' }, `Browsers signed in to ${n === 1 ? 'this station' : `these ${n} stations`} from other devices sign in again. Browsers signed in to ${station}'s fleet are not affected.`)));
  });
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
  dialog('Inbox', (close) => h('div', {},
    h('div', { class: 'row end' }, h('button', { class: 'mini', onclick: () => readAll().then(close) }, 'Mark all read'),
      'Notification' in window && Notification.permission === 'default' ? h('button', { class: 'mini', onclick: () => Notification.requestPermission() }, 'Enable browser notifications') : null),
    away.length ? h('p', { class: 'muted small' }, `Not counted, because ${away.length === 1 ? 'it is' : 'they are'} not reachable: ${away.join(', ')}.`) : null,
    h('h3', {}, 'Messages to you'),
    msgs.length ? h('div', { class: 'list' }, msgs.map(m => h('div', { class: 'note' + (m.read ? '' : ' unread') },
      h('div', {}, h('b', {}, m.from), h('span', { class: 'muted' }, ` · ${m.station} · ${m.kind} · ${ago(m.ts)} ago`)), h('div', { class: 'pre' }, m.body)))) : h('p', { class: 'muted' }, 'No messages.'),
    h('h3', {}, 'Notifications'),
    S.notes.length ? h('div', { class: 'list' }, S.notes.map(n => h('button', { class: 'list-row note' + (n.read ? '' : ' unread'), onclick: () => { readNote(n).then(() => refreshSoon(0)); if (n.url) { close(); location.hash = n.url.replace(/^\/#/, '#'); } } },
      h('span', { class: 'dot ' + (n.kind === 'approval' ? 'needs_you' : n.kind) }), h('b', {}, n.title), h('span', { class: 'muted' }, n.body), h('span', { class: 'grow' }),
      h('span', { class: 'muted' }, n.at.map(a => a.station).join(', '), ' · ', n.pushed === 2 ? 'held (you were here) · ' : n.pushed === 1 ? 'pushed · ' : '', ago(n.ts))))) : h('p', { class: 'muted' }, 'No notifications.')), { wide: true });
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
      h('tbody', {}, tasks.map(t => h('tr', {}, h('td', {}, h('span', { class: 'chip' }, t.state.replaceAll('_', ' '))), h('td', {}, t.role || ''), h('td', {}, t.goal), h('td', { class: 'muted' }, t.outputs?.summary || t.resume?.reason || ''))))) : h('p', { class: 'muted' }, 'No tasks yet.')), { wide: true });
}

function palette() {
  const actions = [
    { label: 'Go to the fleet', run: () => go('#/fleet') },
    ...S.fleet.stations.map(s => ({ label: `Go to station ${s.station}`, run: () => go(stationUrl(s.station)) })),
    ...S.fleet.stations.flatMap(s => liveAgents(s.station).map(p => ({ label: `Open pane ${p.name} on ${s.station}`, run: () => go(paneUrl(s.station, p.id)) }))),
    ...allLayouts().map(x => ({ label: `Open layout ${x.name} (${layShort(x)})`, run: () => go(layUrl(x)) })),
    { label: 'New global layout…', run: () => newLayoutDialog(null, null) },
    { label: 'New pane…', run: () => newPaneDialog(S.route.station || api.station) },
    { label: 'New project…', run: () => newProjectDialog(S.route.station || api.station) },
    { label: 'Approval requests', run: approvalsPanel },
    { label: 'Inbox', run: inboxPanel },
    { label: 'Usage: tokens and estimated cost', run: fleetUsageDialog },
    { label: 'Appearance: themes, colours and fonts', run: () => appearance('deck') },
    { label: 'Stop every agent on this station', run: () => rpc(api.station, 'stop.all', { scope: 'station' }) },
    { label: 'Check this station (doctor)', run: doctorPanel },
    { label: 'Agent setup: MCP servers, skills, models', run: setupPanel },
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
        // Commands like /login are finished by hand: bring the terminal up.
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
// version, and installing or updating them. Everything a program or a station
// said (an error line, a path, a command) is put in as text.
async function agentsPanel() {
  const stations = S.fleet.stations.filter(s => s.state === 'online').map(s => s.station);
  const load = () => Promise.all(stations.map(st => api.on(st, 'agents.status').then(rows => ({ st, rows })).catch(e => ({ st, rows: [], problem: e.message }))));
  let all = await load();
  let open = true;
  const picked = new Map();   // "station agent" -> the way to install the owner picked here
  dialog('Agent programs', (close) => {
    const body = h('div', {});
    // A station that does not say (an older spandeck) only knows there or not.
    const healthOf = (r) => r.health || (r.installed ? 'ok' : 'missing');
    const update = (st, agent) => rpc(st, 'agents.update', agent ? { agent } : {}).then((r) => {
      toast(r.started?.length ? `${st}: updating ${r.started.map(x => x.name).join(', ')}` : `${st}: nothing to update`);
      setTimeout(settle, 1500);
    });
    const wayOf = (st, r) => {
      const ways = r.methods || [];
      return ways.find(m => m.id === picked.get(st + ' ' + r.agent) && m.available) || ways.find(m => m.id === r.method) || ways.find(m => m.available);
    };
    const install = (st, r) => {
      const way = wayOf(st, r), again = healthOf(r) !== 'missing';
      if (!way) return;
      dialog(`${again ? 'Reinstall' : 'Install'} ${r.name} on ${st}?`, (shut) => h('div', {},
        h('p', {}, `This runs on ${st}, and is remembered as the way ${r.name} is installed there:`),
        h('div', { class: 'note mono' }, way.command),
        h('p', { class: 'muted' }, 'Whether it worked is judged by the program answering afterwards. Agents that are running are not restarted.'),
        h('div', { class: 'row end' }, h('button', { onclick: shut }, 'Cancel'),
          h('button', { class: 'primary', onclick: () => { shut(); rpc(st, 'agents.install', { agent: r.agent, method: way.id }).then(() => { toast(`${st}: installing ${r.name}`); setTimeout(settle, 800); }); } }, again ? 'Reinstall' : 'Install'))));
    };
    const status = (r) => {
      const health = healthOf(r);
      if (health === 'broken') return h('span', { class: 'chip bad' }, 'Installed but not working');
      if (health === 'no_answer') return h('span', { class: 'warn-text', title: 'It was asked its version and did not answer in time. It was left running, not stopped. This is not known to be a fault.' }, 'installed · it did not answer when asked its version');
      if (health === 'missing') return h('span', { class: 'muted' }, 'not installed');
      return h('span', {}, r.version || 'installed, version not read');
    };
    const row = (st, r) => {
      const health = healthOf(r), ways = r.methods || [], way = wayOf(st, r), busy = !!(r.updating || r.installing);
      return h('div', { class: 'note agent-program', 'data-agent': r.agent, 'data-health': health },
        h('div', { class: 'row', style: { flexWrap: 'wrap' } },
          paneIcon(r.agent), h('b', {}, r.name), status(r),
          r.panes ? h('span', { class: 'muted' }, `· ${r.panes} running`) : null,
          r.installing ? h('span', { class: 'chip warn' }, 'installing…') : r.updating ? h('span', { class: 'chip warn' }, 'updating…') : null,
          r.last_update && !busy ? h('span', { class: 'muted', title: r.last_update.message || '' }, r.last_update.ok ? `· updated ${ago(r.last_update.at_ms)} ago` : '· last update failed') : null,
          r.last_install && !busy ? h('span', { class: r.last_install.ok ? 'muted' : 'warn-text', title: r.last_install.message || '' }, r.last_install.ok ? `· installed ${ago(r.last_install.at_ms)} ago` : '· the last install did not work') : null,
          h('span', { class: 'grow' }),
          ways.length > 1 ? h('select', { 'aria-label': `How ${r.name} is installed on ${st}`, title: way ? way.command : '', disabled: busy, style: { width: 'auto', flex: 'none' },
            onchange: (e) => { picked.set(st + ' ' + r.agent, e.target.value); draw(); } },
            ways.map(m => h('option', { value: m.id, selected: way && m.id === way.id, disabled: !m.available }, m.available ? m.label : `${m.label} (not on this station)`))) : null,
          way ? h('button', { class: health === 'broken' ? 'mini primary' : 'mini', disabled: busy, title: way.command, onclick: () => install(st, r) }, health === 'missing' ? 'Install' : 'Reinstall') : null,
          (health === 'ok' || health === 'no_answer') && r.can_update !== false ? h('button', { class: 'mini', disabled: busy, onclick: () => update(st, r.agent) }, 'Update') : null),
        health === 'broken' ? h('div', { class: 'agent-problem' },
          h('div', { class: 'muted' }, 'Found at ', h('span', { class: 'mono' }, r.path || ''), '. When started it said:'),
          h('div', { class: 'mono warn-text' }, r.problem || 'nothing'),
          way ? h('div', { class: 'muted' }, 'Reinstall runs ', h('span', { class: 'mono' }, way.command)) : h('div', { class: 'muted' }, 'spandeck knows no install command for it.')) : null);
    };
    const draw = () => body.replaceChildren(
      h('p', { class: 'muted' }, 'An update restarts that program\'s agents and they go back to their conversations. An agent in the middle of a turn is restarted when it finishes, not interrupted. A program that is not installed, or is installed but does not start, is installed the first time you start a pane for it, or here.'),
      h('div', { class: 'row end' }, h('button', { class: 'primary', onclick: () => Promise.all(stations.map(st => update(st))) }, 'Update everything on every station')),
      ...all.map(({ st, rows, problem }) => h('div', {},
        h('div', { class: 'row' }, h('h3', {}, st), h('span', { class: 'grow' }), rows.some(r => ['ok', 'no_answer'].includes(healthOf(r)) && r.can_update !== false) ? h('button', { class: 'mini', onclick: () => update(st) }, 'Update all here') : null),
        problem ? h('p', { class: 'warn-text' }, problem) : null,
        h('div', { class: 'list' }, rows.map(r => row(st, r))))),
      h('div', { class: 'row end' }, h('button', { onclick: redraw }, 'Check again'), h('button', { onclick: close }, 'Close')));
    const redraw = async () => { all = await load(); if (open) draw(); };
    // Look again until nothing is being installed or updated.
    const settle = async () => { await redraw(); if (open && all.some(x => x.rows.some(r => r.installing || r.updating))) setTimeout(settle, 2000); };
    draw();
    return body;
  }, { wide: true, onClose: () => { open = false; } });
}

async function setupPanel() {
  let view;
  try { view = await api.call('setup.get'); } catch (e) { return toast(e.message, 'error'); }
  const setup = view.setup;
  setup.mcp_servers ||= {}; setup.skills ||= {}; setup.defaults ||= {};
  dialog('Agent setup', (close, body) => {
    const draw = () => body.replaceChildren(content());
    const save = async () => {
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
let versionBox = null, versionKey = '', versionLater = '';
function versionBar() {
  const all = S.fleet.stations.filter(s => s.state !== 'not_linked' && s.version && s.version !== '?');
  const newest = all.reduce((n, s) => olderThan(n, s.version) ? s.version : n, '0');
  const behind = all.filter(s => olderThan(s.version, newest));
  const key = behind.map(s => `${s.station}=${s.version}/${s.state}`).join(' ') + '>' + newest + '@' + api.station;
  if (!behind.length || key === versionLater) { versionBox = null; versionKey = ''; return null; }
  if (key === versionKey) return versionBox;
  const me = behind.find(s => s.self), can = behind.filter(s => s.state === 'online'), names = (list) => list.map(s => `${s.station} (${s.version})`).join(', ');
  const on = all.filter(s => s.version === newest).map(s => s.station).join(', ');
  const update = () => Promise.all(can.map(s => api.on(s.station, 'station.update', {}).then((r) => toast(r.current ? `${s.station} already runs the latest release` : `${s.station} is updating to ${r.release}`)).catch(e => toast(`${s.station}: ${e.message}`, 'error')))).then(() => refreshSoon(1500));
  versionKey = key;
  versionBox = h('div', { class: 'vault-prompt version-bar', role: 'region', 'aria-label': 'Stations on different versions' },
    h('div', { class: 'grow' },
      h('b', {}, me ? `This deck is served by ${api.station}, which runs an older spandeck (${me.version})` : `${behind.length === 1 ? 'A station runs' : 'Some stations run'} an older spandeck`),
      h('div', { class: 'muted' }, `${on} ${on.includes(',') ? 'run' : 'runs'} ${newest}. Older: ${names(behind)}. `,
        me ? `What you see here can differ from the deck ${on.includes(',') ? 'those stations serve' : on + ' serves'}, and newer things may be missing. ` : 'A deck served by an older station can look different, and what the fleet keeps the same may not be kept there. ',
        behind.some(s => s.state !== 'online') ? 'A station that is off or cannot be controlled from here is updated at that machine.' : '')),
    can.length ? h('button', { class: 'primary', onclick: () => confirmBox(`Update ${can.map(s => s.station).join(', ')}?`, `The latest release is downloaded on ${can.length === 1 ? 'that station' : 'each of them'} and checked against its published checksum. Terminals keep running, and a version that does not start properly is undone.`, 'Update', update, false) }, can.length === 1 ? `Update ${can[0].station}` : `Update ${can.length} stations`) : null,
    h('button', { onclick: () => { versionLater = key; render(); } }, 'Not now'));
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
  const here = pane ? pane.name : lay ? lay.name : r.view === 'watch' ? 'Watch' : null;
  const crumbs = [h('a', { href: '#/fleet' }, 'Fleet')];
  if (r.station) crumbs.push('›', h('a', { href: stationUrl(r.station) }, r.station));
  else if (lay) crumbs.push('›', h('span', {}, 'Layouts'));
  if (project) crumbs.push('›', h('span', {}, project));
  if (here) crumbs.push('›', h('span', { class: 'here' }, here));
  return h('header', { class: 'topbar' },
    h('a', { class: 'brand', href: '#/fleet', title: 'spandeck ' + (api.version || '') }, 'spandeck'),
    h('nav', { class: 'crumbs', 'aria-label': 'Where you are' }, crumbs),
    h('span', { class: 'grow' }),
    !api.connected ? h('span', { class: 'chip bad' }, api.authError || 'reconnecting…') : null,
    needs ? h('span', { class: 'chip needs', title: 'Agents waiting for you' }, `${needs} need${needs === 1 ? 's' : ''} you`) : null,
    h('button', { class: S.approvals.length ? 'attn' : '', onclick: approvalsPanel }, 'Approvals', S.approvals.length ? h('span', { class: 'count' }, S.approvals.length) : null),
    h('button', { onclick: inboxPanel }, 'Inbox', unread ? h('span', { class: 'count' }, unread) : null),
    (S.trust?.requests?.length || S.trust?.outgoing) || !isPhone() ? h('button', { class: S.trust?.requests?.length ? 'attn' : '', onclick: trustPanel, title: 'Stations and browsers allowed to control your fleet' }, 'Access', S.trust?.requests?.length ? h('span', { class: 'count' }, S.trust.requests.length) : null) : null,
    isPhone() ? null : h('button', { onclick: fleetUsageDialog, title: 'Tokens and estimated cost, per station, project, pane and model' }, 'Usage'),
    isPhone() ? null : h('button', { onclick: setupPanel, title: 'MCP servers, skills and model defaults for every agent' }, 'Agent setup'),
    isPhone() ? null : h('button', { onclick: () => appearance('deck'), title: 'Themes, colours and fonts for the whole deck' }, 'Appearance'),
    isPhone() || !S.fleet.stations.some(x => x.vault?.found) ? null : h('button', { class: S.fleet.stations.some(x => x.state === 'online' && (['not_connected', 'token_rejected', 'locked'].includes(x.vault?.state) || x.vault?.health === 'problem')) ? 'attn' : '', onclick: vaultPanel, title: 'AgentVault: keys for agents without handing over keys' }, 'Vault'),
    h('button', { class: 'danger', onclick: stopAllMenu, title: 'Interrupt running agent turns' }, 'Stop all'),
    isPhone() ? null : h('button', { onclick: palette, title: 'Ctrl+K' }, '⌘K'));
}

function stopRows() {
  const recent = (S.fleet.stops || []).filter(s => Date.now() - s.issued_ms < 10 * 60 * 1000 || s.status !== 'confirmed').slice(0, 6);
  if (!recent.length) return null;
  return h('div', { class: 'stops' }, h('b', {}, 'Stop requests'), recent.map(s => h('span', { class: 'stop ' + s.status, title: `issued ${ago(s.issued_ms)} ago` },
    `${s.station}: ${s.label}`, s.status === 'confirmed' ? ` (${s.result?.stopped?.length ?? 0} stopped)` : s.status === 'unreachable' ? ' — will be delivered when it is back' : '')));
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
    { label: 'Agent programs and versions…', action: agentsPanel },
    { label: 'Usage: tokens and estimated cost…', action: fleetUsageDialog },
    s.state === 'not_linked' && { label: fleetMates().length ? `Merge this fleet into ${s.station}'s…` : `Join ${s.station}'s fleet…`, action: () => joinFleet(s.station) },
    up && { label: 'Restart spandeck here (keeps terminals)', action: () => confirmBox(`Restart spandeck on ${s.station}?`, 'Every terminal and agent keeps running. If it does not come back healthy, the restart is undone.', 'Restart', () => rpc(s.station, 'station.restart', {}).then(() => toast(`${s.station} is restarting`))) },
    up && { label: 'Update to the latest release…', action: () => confirmBox(`Update spandeck on ${s.station}?`, `It runs ${s.version}. The latest release is downloaded on ${s.station} and checked against its published checksum. Terminals keep running, and a version that does not start properly is undone.`, 'Update', () => rpc(s.station, 'station.update', {}).then((r) => toast(r.current ? `${s.station} already runs the latest release` : `${s.station} is updating to ${r.release}`))) },
    s.state === 'unreachable' && { label: 'Wake it (same network only)', action: () => rpc(api.station, 'fleet.wake', { station: s.station }).then((r) => toast(`Sent ${r.sent} wake packets to ${s.station}. Give it a minute.`)) },
    s.state === 'unreachable' && { label: 'Start spandeck there (over SSH)', action: () => rpc(api.station, 'fleet.start', { station: s.station }).then((r) => toast(`${s.station} is ${r.state}`)) },
    s.state === 'unreachable' && { label: 'Forget this station', danger: true, action: () => confirmBox(`Forget ${s.station}?`, 'It is taken off the list on every station of the fleet (the others follow within seconds, one that is off when it is back). If it comes back it is listed again.', 'Forget', () => rpc(api.station, 'fleet.forget', { station: s.station })) },
    up && { label: 'Stop every agent here', danger: true, action: () => rpc(api.station, 'stop.all', { scope: s.station === api.station ? 'station' : s.station }) },
    up && { label: 'Close all panes here…', danger: true, action: () => closeAllDialog(s) },
    // A station that is not in the fleet has nothing to be removed from,
    // and neither has the only station of a fleet of one.
    s.state !== 'not_linked' && S.fleet.stations.some(x => x.station !== s.station && x.state !== 'not_linked')
      && { label: 'Remove this station from the fleet…', danger: true, action: () => removeStationDialog(s) },
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
    h('div', { class: 'tile-head' }, h('span', { class: 'dot ' + (up ? (s.needs_you ? 'needs_you' : s.working ? 'working' : 'idle') : 'unreachable') }), h('b', {}, s.station), s.self ? h('span', { class: 'chip' }, 'this station') : null, h('span', { class: 'grow' }), h('span', { class: 'muted' }, s.version || '')),
    up ? h('div', { class: 'tile-stats' },
      h('span', {}, h('b', {}, s.working ?? 0), ' working'), h('span', { class: s.needs_you ? 'needs' : '' }, h('b', {}, s.needs_you ?? 0), ' need you'),
      s.load != null ? h('span', { class: 'muted' }, 'load ' + Number(s.load).toFixed(1)) : null, s.keep_awake ? h('span', { class: 'chip' }, 'kept awake') : null,
      s.codex_sandbox_blocked ? h('span', { class: 'chip warn', title: 'Codex cannot run commands on this station: the system blocks its sandbox. On the station, run "spandeck doctor --fix" once. It asks for your password.' }, 'Codex blocked') : null,
      s.service === false ? h('span', { class: 'chip warn', title: 'spandeck does not start at login on this station. After a reboot it stays down until someone starts it. Run "spandeck service --install" there.' }, 'not started at login') : null, vaultChip(s))
      : h('div', { class: 'tile-stats warn-text' }, s.state === 'update_needed' ? 'Needs an update before it can be controlled' : s.state === 'unreachable' ? `Unreachable · last seen ${ago(s.last_seen_ms)} ago. Right-click to wake or start it.` : s.state === 'not_linked' ? `Not in this fleet. To make one fleet of both: on ${s.station}, run "spandeck fleet --join ${api.station}", then approve it under Access at any station of this fleet. Every station of ${s.station}'s fleet comes with it.` : `Unreachable · last seen ${ago(s.last_seen_ms)} ago`),
    h('div', { class: 'tile-agents' }, agents.slice(0, 8).map(p => h('div', { class: 'agent-line' }, dot(p), paneMark(p), h('span', {}, p.name), h('span', { class: 'muted' }, p.tool || p.needs_reason || STATE_LABEL[stateOf(p)]))),
      up && !agents.length ? h('span', { class: 'muted' }, 'No agents running') : null));
}

// ---- add a machine ----
// The last tile of the fleet. The station the deck is open on connects to a
// machine of the owner's over SSH, installs spandeck there and has it ask to
// join; the owner then approves the code, as for any station. What the
// machine prints is text from somewhere else: it is only ever inserted as text.
const ADD_STEPS = [['connecting', 'Connect over SSH'], ['installing', 'Download and install spandeck'], ['starting', 'Start it, now and at login'], ['joining', 'Ask to join this fleet']];
const ADD_STAGE = { connecting: 'Connecting over SSH…', installing: 'Installing spandeck…', starting: 'Starting spandeck…', joining: 'Asking to join this fleet…', approve: 'Waiting for your approval', joined: 'Approved. Waiting for it to appear…', needs_password: 'It asks for a password', failed: 'It was not added' };
let addDlg = null;
const addActive = () => (S.adding && S.adding.stage !== 'done' ? S.adding : null);
const addAnswer = (a, yes) => rpc(api.station, yes ? 'trust.approve' : 'trust.deny', { request: a.request });
const addClear = () => api.call('fleet.add_cancel').catch(() => {}).then(() => { S.adding = null; refreshSoon(30); });

// Called when the station's answer about an add arrives.
function addChanged() {
  if (addDlg) return addDlg.redraw();
  const a = S.adding;
  if (a && a.stage === 'done') { toast(`${a.station || a.name} is in your fleet`); addClear(); }
}

function addBody(a) {
  if (a.stage === 'approve') return [
    h('div', {}, 'spandeck is installed there and asks to join. It shows the code ', h('b', { class: 'code' }, a.code), '.'),
    h('div', { class: 'row end' }, h('button', { class: 'mini', onclick: (e) => { e.stopPropagation(); addAnswer(a, false); } }, 'Refuse'), h('button', { class: 'mini primary', onclick: (e) => { e.stopPropagation(); addAnswer(a, true); } }, 'Approve'))];
  if (a.stage === 'failed') return [h('div', { class: 'warn-text' }, a.problem?.text || 'It was not added.')];
  if (a.stage === 'needs_password') return [h('div', {}, `It asks for a password for ${a.user}. Open this to enter it.`)];
  return [h('div', {}, ADD_STAGE[a.stage] || a.stage), h('div', { class: 'muted add-last' }, a.lines?.length ? a.lines[a.lines.length - 1] : '')];
}

function addTile() {
  const a = addActive();
  if (!a) return h('button', { class: 'tile add-tile', onclick: () => addMachineDialog(), title: 'Install spandeck on another of your machines and bring it into this fleet' },
    h('span', { class: 'add-plus', 'aria-hidden': 'true' }, '+'), h('span', {}, 'Add a machine'));
  const mark = a.stage === 'failed' ? 'error' : a.stage === 'approve' || a.stage === 'needs_password' ? 'needs_you' : 'working';
  return h('div', { class: 'tile add-tile busy ' + mark, role: 'button', tabIndex: 0, onclick: () => addMachineDialog(), onkeydown: (e) => { if (e.key === 'Enter') addMachineDialog(); } },
    h('div', { class: 'tile-head' }, h('span', { class: 'dot ' + mark }), h('b', {}, a.name), h('span', { class: 'chip' }, 'being added'), h('span', { class: 'grow' }), h('span', { class: 'muted' }, a.os || '')),
    addBody(a));
}

// The same on a phone: a row under the stations.
function addRow() {
  const a = addActive();
  if (!a) return h('button', { class: 'wide add-row', onclick: () => addMachineDialog() }, '+ Add a machine');
  return h('div', { class: 'phone-station add-row', role: 'button', tabIndex: 0, onclick: () => addMachineDialog() },
    h('div', { class: 'row' }, h('span', { class: 'dot ' + (a.stage === 'failed' ? 'error' : a.running ? 'working' : 'needs_you') }), h('b', {}, a.name), h('span', { class: 'muted' }, 'being added')),
    addBody(a));
}

function addMachineDialog() {
  if (addDlg) return;
  let info = null, picked = null, shown = null, again = false;
  const plain = { autocapitalize: 'none', autocorrect: 'off', spellcheck: false, autocomplete: 'off' };
  let lastUser = '';
  try { lastUser = localStorage.getItem('spandeck.addUser') || ''; } catch (e) { /* no storage */ }
  const user = h('input', { ...plain, placeholder: 'the short name of your account there', value: lastUser, 'aria-label': 'User name on that machine' });
  const pass = h('input', { ...plain, type: 'password', placeholder: 'password', 'aria-label': 'Password on that machine' });
  const log = h('pre', { class: 'add-log', 'aria-label': 'What the machine printed' });
  const box = h('div', { class: 'add-dialog' });
  const setLog = (lines) => { const end = log.scrollHeight - log.scrollTop - log.clientHeight < 30; log.textContent = (lines || []).join('\n'); if (end) log.scrollTop = log.scrollHeight; };
  const start = (machine, name, password) => {
    try { localStorage.setItem('spandeck.addUser', user.value.trim()); } catch (e) { /* no storage */ }
    const params = { machine, user: user.value.trim() };
    // The password goes in this one request and is dropped from the page.
    if (password) params.password = password;
    pass.value = '';
    return api.call('fleet.add', params).then((a) => { S.adding = a; again = false; redraw(); update(); }).catch((e) => toast(e.message, 'error'));
  };
  const load = () => api.call('fleet.candidates').then((r) => { info = r; redraw(); }).catch((e) => { info = { enabled: true, tailscale: true, machines: [], error: e.message }; redraw(); });
  const closeBtn = (label = 'Close') => h('button', { onclick: () => d.close() }, label);
  const form = (fields, okLabel, submit) => h('form', { onsubmit: (e) => { e.preventDefault(); submit(); } }, fields, h('div', { class: 'row end' }, closeBtn('Cancel'), h('button', { class: 'primary', type: 'submit' }, okLabel)));

  function picker() {
    if (!info) return [h('p', { class: 'muted' }, 'Looking for your machines…')];
    if (!info.enabled) return [
      h('p', {}, `Adding a machine is turned off on ${info.station}.`),
      h('p', { class: 'muted' }, `Turning it on lets ${info.station} connect to another of your machines over SSH and install spandeck there, when you ask it to from this tile. Agents cannot do this, and other stations cannot ask ${info.station} to. You can turn it off again from this dialog.`),
      h('div', { class: 'row end' }, closeBtn('Not now'), h('button', { class: 'primary', onclick: () => api.call('fleet.add_enable', { on: true }).then(() => { info = null; redraw(); load(); }).catch((e) => toast(e.message, 'error')) }, `Turn it on for ${info.station}`))];
    if (info.error || !info.tailscale) return [h('p', { class: 'warn-text' }, info.error || `Tailscale is not answering on ${info.station}, so your machines cannot be listed.`), h('div', { class: 'row end' }, closeBtn())];
    if (!info.machines.length) return [
      h('p', {}, 'No Mac, Linux or Windows machine under your Tailscale login is online that is not already a station.'),
      h('p', { class: 'muted' }, 'Phones and tagged machines are not listed. A machine that is asleep is listed once it wakes. A Windows machine needs its OpenSSH server turned on first.'),
      h('div', { class: 'row end' }, h('button', { onclick: () => { info = null; redraw(); load(); } }, 'Look again'), closeBtn())];
    const m = info.machines.find(x => x.id === picked);
    return [
      h('p', { class: 'muted' }, 'Your Mac, Linux and Windows machines that are online and not stations yet:'),
      h('div', { class: 'list' }, info.machines.map(x => h('button', { class: 'list-row add-machine' + (x.id === picked ? ' first' : ''), type: 'button', 'aria-pressed': x.id === picked, onclick: () => { picked = x.id; redraw(); user.focus(); } },
        h('b', {}, x.name), h('span', { class: 'muted' }, `${x.os} · ${x.address}`), x.tailscale_ssh ? h('span', { class: 'chip ok', title: 'Tailscale SSH is on there, so no password is needed' }, 'Tailscale SSH') : null))),
      m ? form(field(`Your user name on ${m.name}`, user, `${info.station} connects to ${m.name} over SSH as this user and runs the spandeck install script there. If Tailscale SSH or an SSH key already lets it in, no password is asked.`),
        'Connect and install', () => { if (user.value.trim()) start(m.id, m.name); else user.focus(); }) : h('div', { class: 'row end' }, closeBtn('Cancel')),
      info.by_config ? null : h('p', { class: 'muted' }, h('button', { type: 'button', onclick: () => api.call('fleet.add_enable', { on: false }).then(() => { info = null; redraw(); load(); }).catch((e) => toast(e.message, 'error')) }, `Turn adding machines off on ${info.station}`))];
  }

  function view(a) {
    // The user name stays editable here: a mistyped name is refused exactly
    // like a wrong password, and the owner could retype only the password.
    if (user.dataset.forJob !== a.id) { user.value = a.user; user.dataset.forJob = a.id; }
    const another = h('button', { type: 'button', onclick: () => { picked = null; again = true; redraw(); if (!info) load(); } }, 'Pick another machine');
    const passForm = (label) => h('form', { onsubmit: (e) => { e.preventDefault(); if (!user.value.trim()) user.focus(); else if (pass.value) start(a.machine, a.name, pass.value); else pass.focus(); } },
      field(`Your user name on ${a.name}`, user, 'The short name of your account there. Change it here if it was wrong.'),
      field(`Password on ${a.name}`, pass, 'Used once, for this connection. It is not saved, logged or shown anywhere.'),
      h('div', { class: 'row end' }, another, closeBtn('Cancel'), h('button', { class: 'primary', type: 'submit' }, label)));
    if (a.stage === 'needs_password') return [h('p', {}, `${a.name} asks for a password: neither Tailscale SSH nor an SSH key let ${api.station} in as ${a.user}.`), passForm('Connect and install')];
    if (a.stage === 'failed') return [
      h('p', { class: 'warn-text' }, a.problem?.text || 'It was not added.'), log,
      a.problem?.kind === 'wrong_password' ? passForm('Try again')
        : h('div', { class: 'row end' }, h('button', { onclick: () => { addClear(); d.close(); } }, 'Close'), h('button', { class: 'primary', onclick: () => { picked = a.machine; user.value = a.user; again = true; redraw(); if (!info) load(); } }, 'Try again'))];
    if (a.stage === 'approve') return [
      h('p', {}, `spandeck is installed and running on ${a.name}. It asks to join this fleet as `, h('b', {}, a.station), ' and shows the code ', h('b', { class: 'code' }, a.code), '.'),
      h('p', { class: 'muted' }, 'Approve only if you started this: it will be able to control every station.'),
      h('div', { class: 'row end' }, h('button', { onclick: () => addAnswer(a, false) }, 'Refuse'), h('button', { class: 'primary', onclick: () => addAnswer(a, true) }, 'Approve')), log];
    if (a.stage === 'joined') return [h('p', {}, `Approved. Waiting for ${a.station || a.name} to appear…`), log];
    if (a.stage === 'done') return [h('p', {}, h('b', {}, a.station || a.name), ' is in your fleet.'),
      h('div', { class: 'row end' }, h('button', { onclick: () => { addClear(); d.close(); } }, 'Done'), h('button', { class: 'primary', onclick: () => { addClear(); d.close(); go(stationUrl(a.station || a.name)); } }, 'Open it'))];
    const at = ADD_STEPS.findIndex(s => s[0] === a.stage);
    return [
      h('p', {}, `Adding ${a.name} as ${a.user}.`),
      h('ol', { class: 'add-steps' }, ADD_STEPS.map(([, label], i) => h('li', { class: i < at ? 'done' : i === at ? 'now' : '' }, h('span', { 'aria-hidden': 'true' }, i < at ? '✓' : i === at ? '›' : '·'), label + (i === at ? '…' : '')))),
      log,
      h('div', { class: 'row end' }, h('button', { class: 'danger', onclick: () => addClear() }, 'Stop'), closeBtn('Hide'))];
  }

  function redraw() {
    const a = again ? null : S.adding;
    if (a) setLog(a.lines);
    // Only a change of step redraws: the boxes being typed in are left alone.
    const key = JSON.stringify(a ? [a.id, a.stage, a.problem?.kind, a.code] : [picked, info && [info.enabled, info.tailscale, info.error, info.machines.map(x => x.id)]]);
    if (key === shown) return;
    shown = key;
    box.replaceChildren(...(a ? view(a) : picker()).flat().filter(Boolean));
    const first = box.querySelector(a ? (user.value.trim() ? 'input[type=password], input' : 'input') : '.list-row.first ~ * input, form input');
    if (first) first.focus();
  }

  const d = dialog('Add a machine', () => box, { onClose: () => { addDlg = null; pass.value = ''; } });
  addDlg = { redraw, line: (data) => { if (S.adding && data.id === S.adding.id) { S.adding.lines = [...(S.adding.lines || []), data.line]; setLog(S.adding.lines); } } };
  redraw();
  if (!S.adding) load();
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
    return h('a', { class: 'side-row side-pane' + cur.class + (can && ctx.arrange ? ' draggable' : ''), href: paneUrl(st, p.id),
      'aria-label': paneSays(p), 'aria-current': cur['aria-current'], dataset: { id: p.id, station: st },
      title: can ? (ctx.arrange ? 'Drag into the layout' : `Drag into ${ctx.lay.name}, or click to open`) : undefined,
      onmousedown: (e) => { if (can) sideDrag(e, st, p); },
      onclick: (e) => { if (ctx?.arrange || Date.now() - dragEnded < 300) e.preventDefault(); },
      oncontextmenu: (e) => { e.preventDefault(); e.stopPropagation(); paneMenu(e, st, p, 'side'); } },
    dot(p), paneMark(p), h('span', { class: 'side-name' }, p.name),
    where ? h('span', { class: 'muted side-where' }, where) : null,
    p.paused ? h('span', { class: 'chip warn' }, 'paused') : null,
    ctx && ctx.leafOf(st, p)?.hidden ? h('span', { class: 'chip', title: `Hidden in ${ctx.lay.name}` }, 'hidden')
      : can && ctx.arrange && ctx.has(st, p) ? h('span', { class: 'chip' }, 'in layout') : null);
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
        projectLayouts(st).filter(x => x.project === pr.name).map(x => layRow(x)),
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
  globals.length ? globals.map(x => layRow(x)) : h('div', { class: 'muted small side-hint' }, 'No global layouts yet. A global layout can hold any pane from any station.')),
  h('div', { class: 'side-actions' }, h('button', { onclick: () => newPaneDialog(online(r.station) ? r.station : api.station) }, '＋ Pane'), h('button', { onclick: () => newProjectDialog(online(r.station) ? r.station : api.station) }, '＋ Project')));
}

function getTerm(st, p, where, mode) {
  const key = `${where}:${st}/${p.id}`;
  let t = S.terms.get(key);
  if (!t) {
    t = new PaneTerm(api, st, p, { mode, typeWhileWatching: where === 'phone', agentsOf: (pane) => agentRows(st, pane), onMenu: (e, term) => paneMenu(e, st, term.pane, where, term), onUsage: (term) => usageDialog(st, term.pane), onFocus: (term) => { S.focused = `${st}/${term.pane.id}`; } });
    S.terms.set(key, t);
    t._fresh = true;
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

// Watch: every agent in a project as a small live card, laid out by who works
// for whom, with the lines, and the messages between them at the side.
function watchView(st, project) {
  const live = panesOf(st).filter(p => !p.exited);
  const mine = live.filter(p => p.project === project);
  // Projects linked with this one on the same station: their agents are shown
  // too, marked with their project, so the lines between the two are drawn.
  const pr = (S.projects.get(st) || []).find(x => x.name === project);
  const linked = [...new Set(linksOf(pr).filter(l => (!l.station || l.station === st) && l.with !== project).map(l => l.with))];
  const theirs = live.filter(p => linked.includes(p.project));
  const all = [...mine, ...theirs];
  const rowsOf = (set) => {
    const depth = (p, n = 0) => { const up = set.find(x => x.id === p.parent_pane); return up && n < 6 ? depth(up, n + 1) : n; };
    const rows = [];
    for (const p of set) (rows[depth(p)] ||= []).push(p);
    return rows.filter(Boolean).map(r => r.sort((a, b) => a.name.localeCompare(b.name)));
  };
  loadThumbs(st, all);
  pendingLinks = { selector: '.watch-map', links: linksFor(st, all) };
  const msgs = (S.msgs.get(st) || []).filter(m => m.project === project || linked.includes(m.project)).slice(0, 14);
  const card = (p) => h('div', { class: 'watch-card' + (p.project !== project ? ' guest' : ''), dataset: { pane: p.id } },
    p.project !== project ? h('span', { class: 'chip guest watch-from', title: `This agent is in ${p.project}, which is linked with ${project}` }, 'from ' + p.project) : null, paneTile(st, p));
  return h('div', { class: 'watch' },
    h('div', { class: 'watch-map' }, all.length
      ? [rowsOf(mine).map(r => h('div', { class: 'watch-row' }, r.map(card))),
        linked.map(name => rowsOf(theirs.filter(p => p.project === name)).map(r => h('div', { class: 'watch-row' }, r.map(card))))]
      : h('div', { class: 'empty' }, `Nothing is running in ${project}.`),
      h('div', { class: 'watch-key' }, h('span', {}, h('i', { class: 'key task' }), 'task given'), h('span', {}, h('i', { class: 'key message live' }), 'message, moving now'), h('span', {}, h('i', { class: 'key message' }), 'message in the last 10 minutes'))),
    h('aside', { class: 'watch-feed' }, h('h3', {}, 'Messages between agents'),
      msgs.length ? msgs.map(m => h('div', { class: 'note' }, h('div', {}, h('b', {}, String(m.from ?? m.sender).replace(/@.*$/, '')), ' → ', h('b', {}, String(m.to ?? m.recipient).replace(/@.*$/, '')), h('span', { class: 'muted' }, ` · ${m.kind} · ${ago(m.ts)} ago`)), h('div', { class: 'pre' }, String(m.body || '').slice(0, 280))))
        : h('p', { class: 'muted' }, linked.length ? `No messages yet. When agents in ${project} or ${linked.join(', ')} message each other, they appear here and as lines on the left.` : 'No messages yet. When agents in this project message each other, they appear here and as lines on the left.')));
}

// ---- layouts ----
// A layout is a tree of splits whose leaves name panes. A project's layouts
// are saved in its project file, on its station; a global layout is saved in
// the deck document, which every station holds. Either may show any pane:
// one in a project by its name there ({pane, project?, station?}), one in no
// project by its id ({id, station?}). A leaf never names another layout.
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
const cleanLeaf = (n) => n.id !== undefined ? { id: n.id, ...(n.station ? { station: n.station } : {}) }
  : { pane: n.pane, ...(n.project ? { project: n.project } : {}), ...(n.station ? { station: n.station } : {}) };
const leafSig = (n) => JSON.stringify(cleanLeaf(n));
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
  const hidden = leaves(c.tree).filter(n => n.hidden).map(cleanLeaf);
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
function bindGlobalLeaves() {
  if (!S.loaded) return;
  const allHere = !fleetAway().length && onlineStations().every(st => S.panes.has(st) && S.projects.has(st));
  for (const def of S.deck.layouts || []) {
    const lay = findLayout('global:' + def.name);
    if (!lay || lay.cache.dirty) continue;
    let bound = 0;
    for (const n of leaves(lay.cache.tree)) {
      if (n.station) continue;
      const where = leafCandidates(n);
      if (where.length === 1 && (n.id !== undefined || allHere)) { n.station = where[0]; bound += 1; }
    }
    if (bound) saveLayout(lay);
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
  const label = (n) => n.pane !== undefined ? n.pane : (leafPane(lay, n)?.name || 'closed pane');
  const ctx = {
    lay, key: lay.key, arrange: arranging === lay.key, refOf, tree, label,
    stationOf: (n) => leafStation(lay, n),
    resolve: (n) => { const p = leafPane(lay, n); return p ? { st: leafStation(lay, n), p } : null; },
    guest: (n) => leafGuest(lay, n),
    emptyText: 'This layout is empty. Press Arrange, or drag a pane here from the list on the left.',
    missing: (n) => {
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
    hide: (n, on) => {
      if (on && !ctx.shown().some(x => x !== n)) return toast('This is the only pane showing. Show another one first.');
      setHidden(n, on);
      saveLayout(lay);
      render();
      if (!on) focusLeafSoon(ctx, n);
    },
    only: (n) => { for (const x of leaves(tree())) setHidden(x, x !== n); saveLayout(lay); render(); focusLeafSoon(ctx, n); },
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
    if (!n.hidden || !online(st) || !S.panes.has(st) || !S.projects.has(st) || ctx.resolve(n)) return false;
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
  const ctx = ctxFor(lay), leaf = paneLeaf(lay, st, p), all = leaves(lay.cache.tree);
  if (ctx.has(st, p)) return toast(`${p.name} is already in ${lay.name}`);
  // Beside the pane with the keyboard, else the last one on screen, so the
  // new pane is seen even when others are hidden.
  const seen = all.filter(n => !n.hidden), focused = focusedLeaf(ctx);
  const beside = (focused && !focused.hidden ? focused : null) || seen[seen.length - 1] || all[all.length - 1];
  lay.cache.tree = beside ? treeInsert(lay.cache.tree, ctx.refOf(beside), dir === 'col' ? 'below' : 'right', leaf, ctx.refOf) : leaf;
  saveLayout(lay);
  render();
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
    const t = r ? getTerm(r.st, r.p, 'leaf', 'control') : null;
    if (t) t.setGuest(guest);
    // The last pane on screen has no Hide: a layout always shows something.
    if (t) t.setHide(ctx.shown().length > 1 ? () => ctx.hide(node, true) : null);
    const body = t ? t.el : ctx.missing(node);
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
    stations.length <= 1 ? h('p', { class: 'muted hint' }, 'Other stations appear here on their own: any machine under your Tailscale login that runs spandeckd.') : null)));
}

// One station: every running pane as a tile.
function stationView(st) {
  const info = S.fleet.stations.find(s => s.station === st);
  if (info && info.state !== 'online') return shell(h('main', { class: 'station-main' }, h('div', { class: 'fleet' }, h('p', { class: 'warn-text' }, awayText(st, info)))));
  const live = liveAgents(st).sort(byState);
  loadThumbs(st, live);
  return shell(h('main', { class: 'station-main' }, h('div', { class: 'deck' },
    live.length ? h('div', { class: 'tiles' }, live.map(p => paneTile(st, p))) : h('div', { class: 'empty' }, S.loaded ? 'Nothing is running here. Start a pane or a project from the left.' : 'Loading…'))));
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
    const state = p ? stateOf(p) : 'missing';
    const stateText = p ? (p.exited ? 'ended' : STATE_LABEL[state] || state) : (!ctx.stationOf(n) ? 'no station' : online(ctx.stationOf(n)) ? 'not running' : 'unreachable');
    const says = (p ? paneSays(p) : `${name}, ${stateText}`) + (guest ? `, ${guest}` : '') + (p?.needs_reason && state === 'needs_you' ? `: ${p.needs_reason}` : '');
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
        state === 'needs_you' || state === 'error' || !p ? h('span', { class: 'hid-state' }, stateText) : null),
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
    h('span', { class: 'muted lay-saved' }, `Every agent in ${project} on ${st}, and who is talking to whom`), linkedChip(st, pr));
  const body = info && info.state !== 'online' ? h('div', { class: 'empty' }, awayText(st, info))
    : !pr ? h('div', { class: 'empty' }, S.loaded ? `There is no project ${project} on ${st}.` : 'Loading…') : watchView(st, project);
  return shell(h('main', { class: 'station-main' }, head, body));
}

function paneView(st, id) {
  const p = findPane(st, id);
  if (!p) return shell(h('main', { class: 'station-main' }, h('div', { class: 'empty' }, !S.loaded ? 'Loading…' : online(st) ? 'That pane is not on this station any more.' : `${st} is unreachable.`)));
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
const phoneFit = new Set();
const phonePanes = new Map();

// Kept per pane, so a redraw does not throw away what you are writing.
function composer(st, p) {
  const key = `${st}/${p.id}`;
  let c = composers.get(key);
  if (c) return c;
  const box = h('textarea', { class: 'composer-box', rows: 2, placeholder: p.agent === 'shell' ? 'Type or paste a command' : 'Type, paste or dictate a message', 'aria-label': 'Message', enterkeyhint: 'send' });
  const send = async () => {
    // Sent as a paste, so a message of several lines arrives as one.
    const text = box.value.trim();
    if (!text) return;
    await rpc(st, 'pane.paste', { pane: p.id, text, enter: true });
    box.value = '';
  };
  const key1 = (bytes) => rpc(st, 'pane.input', { pane: p.id, data: bytes });
  const Speech = window.SpeechRecognition || window.webkitSpeechRecognition;
  let listening = null;
  const mic = Speech ? h('button', { type: 'button', class: 'mic', title: 'Dictate', 'aria-label': 'Dictate', 'aria-pressed': 'false' }, '🎤') : null;
  if (mic) mic.onclick = () => {
    if (listening) { listening.stop(); return; }
    const rec = new Speech();
    const before = box.value ? box.value.replace(/\s*$/, ' ') : '';
    rec.lang = navigator.language || 'en-US';
    rec.interimResults = true;
    rec.continuous = true;
    rec.onresult = (e) => { box.value = before + Array.from(e.results, r => r[0].transcript).join(''); };
    rec.onerror = (e) => { if (e.error !== 'aborted' && e.error !== 'no-speech') toast(e.error === 'not-allowed' ? 'Dictation needs microphone permission, and the HTTPS address of this station.' : 'Dictation stopped: ' + e.error, 'error'); };
    rec.onend = () => { listening = null; mic.classList.remove('on'); mic.setAttribute('aria-pressed', 'false'); };
    try { rec.start(); listening = rec; mic.classList.add('on'); mic.setAttribute('aria-pressed', 'true'); }
    catch (e) { toast('Dictation is not available here. Use the microphone on the keyboard.', 'error'); }
  };
  c = h('div', { class: 'composer' },
    h('div', { class: 'keys', role: 'group', 'aria-label': 'Keys' }, PHONE_KEYS.map(([label, bytes]) => h('button', { type: 'button', class: 'key', 'aria-label': KEY_NAMES[label] || label, onclick: () => key1(bytes) }, label))),
    h('form', { class: 'composer-row', onsubmit: (e) => { e.preventDefault(); send(); } }, box, mic, h('button', { class: 'primary', type: 'submit' }, 'Send')));
  composers.set(key, c);
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

// Any layout: a project's or a global one, with panes from any station.
function phoneLayoutView(lay) {
  const lays = allLayouts();
  const ctx = ctxFor(lay);
  dropDeadHidden(lay, ctx);
  const cells = leaves(lay.cache.tree).map((n) => {
    const p = ctx.resolve(n)?.p || null, guest = ctx.guest(n), name = ctx.label(n);
    const state = p ? stateOf(p) : 'missing';
    return { node: n, ref: ctx.refOf(n), st: ctx.stationOf(n), p, guest, name, state, hidden: !!n.hidden, label: `${name}${guest ? ' ' + guest : ''}, ${p ? STATE_LABEL[state] || state : 'not running'}${n.hidden ? ', hidden' : ''}` };
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

  // As in the pane view: the page is redrawn every few seconds, and what is
  // on it stays where it is. The terminal and the message box are never put
  // back, so the keyboard does not close while you type; the map and the bars
  // are rebuilt only when what they show changes, so a touch leaves no focus
  // ring behind and an approval keeps the reason being typed into it.
  const vkey = 'layout:' + lay.key;
  let v = phonePanes.get(vkey);
  if (!v) {
    v = { head: h('div', { class: 'row' }), title: h('div', { class: 'lay-title' }), map: h('div', { class: 'lay-map', role: 'group' }),
      showing: h('span', { class: 'lay-showing', role: 'status' }), dots: h('span', { class: 'lay-dots', 'aria-hidden': 'true' }),
      prev: h('button', { type: 'button', onclick: (e) => { e.currentTarget.blur(); v.step(-1); } }, '‹'), next: h('button', { type: 'button', onclick: (e) => { e.currentTarget.blur(); v.step(1); } }, '›'),
      appr: h('div', { class: 'lay-approvals' }), rows: new Map(), slot: h('div', { class: 'keep' }), foot: h('div', { class: 'keep' }) };
    // Hide the pane being looked at, or show it in the layout again.
    v.hide = h('button', { type: 'button', class: 'lay-hide', onclick: (e) => { e.currentTarget.blur(); v.toggle(); } });
    v.nav = h('div', { class: 'lay-nav' }, v.prev, h('div', { class: 'lay-where' }, v.showing, v.dots), v.hide, v.next);
    v.main = h('main', { class: 'phone phone-pane phone-layout' }, v.head, v.title, v.map, v.nav, v.appr, v.slot, v.foot);
    // A swipe left or right anywhere but the message box steps to the next or
    // the previous pane. Heard before the terminal, which scrolls on a drag
    // up or down and never sees a sideways one end.
    let sw = null;
    const one = (e) => (e.touches && e.touches.length === 1 ? e.touches[0] : null);
    v.main.addEventListener('touchstart', (e) => {
      const t = one(e), el = e.target;
      const busy = !t || !el.closest || el.closest('.composer, .approval, input, textarea, select') || (el.closest('.lay-map') && v.map.scrollWidth > v.map.clientWidth + 1);
      sw = busy ? null : { x: t.clientX, y: t.clientY, dx: 0, dy: 0, at: Date.now() };
    }, { capture: true, passive: true });
    v.main.addEventListener('touchmove', (e) => { const t = one(e); if (sw && t) { sw.dx = t.clientX - sw.x; sw.dy = t.clientY - sw.y; } else sw = null; }, { capture: true, passive: true });
    v.main.addEventListener('touchend', (e) => {
      const g = sw;
      sw = null;
      if (!g || Math.abs(g.dx) < 60 || Math.abs(g.dx) < 2 * Math.abs(g.dy) || Date.now() - g.at > 900) return;
      if (e.cancelable) e.preventDefault();
      v.step(g.dx < 0 ? 1 : -1);
    }, { capture: true, passive: false });
    v.main.addEventListener('touchcancel', () => { sw = null; }, { capture: true, passive: true });
    phonePanes.clear();
    phonePanes.set(vkey, v);
  }
  const show = (ref) => { if (phoneAt.get(lay.key) !== ref) { phoneAt.set(lay.key, ref); render(); } };
  v.step = (d) => { const to = stepTo(d); if (to) show(to.ref); };
  v.toggle = () => { if (cur) { setHidden(cur.node, !cur.hidden); saveLayout(lay); render(); } };

  const live = !!p && !p.exited;
  const fitKey = p ? `${st}/${p.id}` : '';
  const fitted = live && phoneFit.has(fitKey);
  const canStop = live && (p.state === 'working' || p.state === 'needs_you');
  const headSig = `${st}/${p?.id}|${live}|${fitted}|${canStop}`;
  if (v.headSig !== headSig) {
    v.headSig = headSig;
    v.head.replaceChildren(...[h('a', { class: 'btn', href: '#/fleet' }, '‹ Back'),
      live ? h('button', { 'aria-pressed': String(fitted), title: fitted ? 'Stop resizing this pane to the phone' : 'Resize this pane to fit this screen', onclick: (e) => { e.currentTarget.blur(); fitted ? phoneFit.delete(fitKey) : phoneFit.add(fitKey); update(); } }, fitted ? 'Fitted' : 'Fit to screen') : null,
      h('span', { class: 'grow' }),
      h('button', { class: 'danger big', disabled: !canStop, onclick: (e) => { e.currentTarget.blur(); rpc(st, 'pane.stop', { pane: p.id }).then(() => toast('Stop sent')); } }, 'Stop this turn')].filter(Boolean));
  }
  const titleSig = lays.map(x => x.key).join('|') + '>' + lay.key;
  if (v.titleSig !== titleSig) {
    v.titleSig = titleSig;
    v.title.replaceChildren(lays.length > 1
      ? h('select', { 'aria-label': 'Layout', onchange: (e) => { const x = lays.find(y => y.key === e.target.value); if (x) go(layUrl(x)); } }, lays.map(x => h('option', { value: x.key, selected: x.key === lay.key }, `${x.project ? x.project + (onlineStations().length > 1 ? ' on ' + x.station : '') : 'Global'} · ${x.name}`)))
      : h('span', {}, h('b', {}, lay.name), h('span', { class: 'muted' }, lay.project ? ` · layout in ${lay.project} on ${lay.station}` : ' · global layout')));
  }
  const mapSig = JSON.stringify([lay.cache.tree, i, cells.map(c => c.label)]);
  if (v.mapSig !== mapSig) {
    v.mapSig = mapSig;
    const [w, hh] = mapSpan(lay.cache.tree);
    v.map.setAttribute('aria-label', `Panes in the layout ${lay.name}`);
    v.map.replaceChildren(...(cells.length ? [layoutMap(lay.cache.tree, cells, show)] : []));
    if (v.map.firstChild) Object.assign(v.map.firstChild.style, { minWidth: w * 48 - 4 + 'px', height: Math.max(hh * 48 - 4, 56) + 'px' });
    v.map.hidden = !cells.length;
    v.nav.hidden = !cells.length;
    v.showing.textContent = cur ? `Showing ${cur.name}${cur.guest ? ' (' + cur.guest + ')' : ''} · ${cur.hidden ? 'hidden' : `${order.indexOf(cur) + 1} of ${order.length}`}` : '';
    // The last pane showing cannot be hidden, as on a computer.
    v.hide.hidden = !cur;
    v.hide.disabled = !cur || (!cur.hidden && order.length < 2);
    v.hide.textContent = cur?.hidden ? 'Show' : 'Hide';
    v.hide.setAttribute('aria-label', !cur ? 'Hide' : cur.hidden ? `Show ${cur.name} in this layout again` : `Hide ${cur.name} in this layout. It keeps running.`);
    v.dots.replaceChildren(...order.map((c) => h('i', { class: c === cur ? 'on' : '' })));
    for (const [b, d, word] of [[v.prev, -1, 'Previous'], [v.next, 1, 'Next']]) {
      const to = stepTo(d);
      b.disabled = !to;
      b.setAttribute('aria-label', to ? `${word} pane: ${to.name}` : `${word} pane`);
    }
  }
  // What is waiting for an answer from a pane in this layout stays on top,
  // whichever pane is showing.
  const waiting = S.approvals.filter(a => cells.some(c => c.p && c.st === a.station && findPane(a.station, a.pane)?.id === c.p.id));
  for (const [id, el] of v.rows) if (!waiting.some(a => a.id === id)) { el.remove(); v.rows.delete(id); }
  for (const a of waiting) if (!v.rows.has(a.id)) { const el = approvalRow(a); v.rows.set(a.id, el); v.appr.append(el); }

  let body, foot = null;
  if (p) {
    const t = fitted ? getTerm(st, p, 'phone-fit', 'control') : getTerm(st, p, 'phone', 'watch');
    t.setGuest(cur.guest);
    body = t.el;
    foot = live ? composer(st, p) : null;
  } else {
    const what = cur ? cur.ref : '';
    if (v.gone?.what !== what) {
      const project = cur && cur.node.pane !== undefined ? (cur.node.project || lay.project) : null;
      v.gone = { what, el: h('div', { class: 'empty lay-gone' }, cur
        ? [h('p', {}, online(st) ? `${cur.name} is not running.` : `${st} is unreachable, so ${cur.name} cannot be shown.`),
          project && online(st) ? h('button', { onclick: () => rpc(st, 'project.open', { name: project }) }, `Start ${project}'s panes`) : null]
        : h('p', {}, 'This layout is empty. Put panes in it with Arrange, on a computer.')) };
    }
    body = v.gone.el;
  }
  if (v.slot.firstChild !== body) v.slot.replaceChildren(body);
  if (v.foot.firstChild !== foot) v.foot.replaceChildren(...(foot ? [foot] : []));
  return v.main;
}

function phonePaneView(st, id) {
  const p = findPane(st, id);
  if (!p) return h('main', { class: 'phone' }, h('p', {}, 'That pane is gone.'), h('a', { href: '#/fleet' }, 'Back'));
  // Watching shows the pane at the size someone else set, shrunk to fit.
  // Fitting resizes the pane to this screen, as a computer's deck would.
  const fitKey = `${st}/${p.id}`;
  const fitted = phoneFit.has(fitKey);
  const t = fitted ? getTerm(st, p, 'phone-fit', 'control') : getTerm(st, p, 'phone', 'watch');
  // The page is redrawn every few seconds. The terminal and the message
  // box stay where they are, so the keyboard does not close while you type.
  let v = phonePanes.get(fitKey);
  if (!v) {
    v = { head: h('div', { class: 'row' }), slot: h('div', { class: 'keep' }), foot: h('div', { class: 'keep' }) };
    v.main = h('main', { class: 'phone phone-pane' }, v.head, v.slot, v.foot);
    phonePanes.clear();
    phonePanes.set(fitKey, v);
  }
  // The bar is rebuilt only when what it shows changes. Rebuilding it on
  // every redraw made the phone put a focus ring on a button at each touch.
  const canStop = !p.exited && (p.state === 'working' || p.state === 'needs_you');
  const pu = p.usage;
  const brief = pu && pu.model ? [pu.model.replace(/^.*\//, ''), contextText(pu) ? 'context ' + contextText(pu) : '', fmtTokens(pu.total_tokens) + ' tokens', pu.cost_estimate !== null && pu.cost_estimate !== undefined ? 'est. ' + fmtCost(pu.cost_estimate) : ''].filter(Boolean).join(' · ') : '';
  const sig = `${fitted}|${p.exited}|${canStop}|${brief}`;
  if (v.sig !== sig) {
    v.sig = sig;
    // replaceChildren would print a missing piece as the word "null".
    v.head.replaceChildren(...[h('a', { class: 'btn', href: '#/fleet' }, '‹ Back'),
      p.exited ? null : h('button', { 'aria-pressed': String(fitted), title: fitted ? 'Stop resizing this pane to the phone' : 'Resize this pane to fit this screen', onclick: (e) => { e.currentTarget.blur(); fitted ? phoneFit.delete(fitKey) : phoneFit.add(fitKey); update(); } }, fitted ? 'Fitted' : 'Fit to screen'),
      brief ? h('button', { class: 'phone-usage', title: usageTitle(p.usage), 'aria-label': `Usage: ${brief}`, onclick: (e) => { e.currentTarget.blur(); usageDialog(st, p); } }, brief, h('span', { class: 'muted' }, ' ›')) : null,
      h('span', { class: 'grow' }),
      h('button', { class: 'danger big', disabled: !canStop, onclick: (e) => { e.currentTarget.blur(); rpc(st, 'pane.stop', { pane: p.id }).then(() => toast('Stop sent')); } }, 'Stop this turn')].filter(Boolean));
  }
  if (v.slot.firstChild !== t.el) v.slot.replaceChildren(t.el);
  const foot = p.exited ? (v.ended ||= h('p', { class: 'muted small' }, 'This pane has ended.')) : composer(st, p);
  if (v.foot.firstChild !== foot) v.foot.replaceChildren(foot);
  return v.main;
}

// The phone's list: what needs you first, then the same list as a computer
// shows on its left: Pinned, each station with its projects (their layouts,
// Watch, panes), and the global layouts.
function phoneList() {
  const all = S.fleet.stations.flatMap(s => liveAgents(s.station).map(p => ({ st: s.station, p })));
  const blockers = all.filter(x => x.p.agent !== 'shell' && (x.p.state === 'needs_you' || x.p.state === 'error'));
  const whereOf = (st, p) => `${st}${p.project ? ' · ' + p.project : ''}`;
  const line = (st, p, where) => h('a', { class: 'phone-row', href: paneUrl(st, p.id), 'aria-label': paneSays(p) }, dot(p), paneMark(p),
    h('div', { class: 'grow' }, h('b', {}, p.name), where ? h('span', { class: 'muted' }, ' · ' + where) : null,
      h('div', { class: 'muted small' }, p.needs_reason || (p.exited ? 'ended' : p.tool ? `${p.tool} · ${ago(p.tool_since_ms)}` : `${STATE_LABEL[stateOf(p)]} · ${ago(p.state_since_ms)}`))), '›');
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
      h('div', { class: 'row' }, h('span', { class: 'dot ' + (up ? 'idle' : 'unreachable') }), h('b', {}, st), h('span', { class: 'muted' }, up ? `${s.working ?? 0} working · ${s.needs_you ?? 0} need you` : STATION_STATE[s.state] || s.state.replace('_', ' ')), h('span', { class: 'grow' }),
        up ? h('button', { onclick: () => newPaneDialog(st) }, 'New') : null,
        s.state === 'unreachable' ? h('button', { onclick: () => rpc(api.station, 'fleet.wake', { station: st }).then((r) => toast(`Sent ${r.sent} wake packets. Give it a minute.`)) }, 'Wake') : null,
        s.state === 'unreachable' ? h('button', { onclick: () => rpc(api.station, 'fleet.start', { station: st }).then((r) => toast(`${st} is ${r.state}`)) }, 'Start') : null,
        up ? h('button', { class: 'danger', onclick: () => confirmBox(`Stop every agent on ${st}?`, 'Running turns are interrupted. Processes stay.', 'Stop', () => rpc(api.station, 'stop.all', { scope: s.self ? 'station' : st })) }, 'Stop') : null,
        // Everything else a station offers, with closing its panes and removing it.
        h('button', { class: 'station-more', 'aria-label': `More for ${st}`, title: `More for ${st}`, onclick: (e) => stationMenu(e, s) }, '⋯')),
      projects.map(pr => {
        const mine = panesOf(st).filter(p => p.project === pr.name && !p.exited).sort(byState);
        return h('div', { class: 'phone-project' },
          h('div', { class: 'row phone-project-head' }, h('b', {}, pr.name), pr.branch ? h('span', { class: 'muted' }, pr.branch) : null, linkedChip(st, pr)),
          projectLayouts(st).filter(x => x.project === pr.name).map(x => layRow(x)),
          mine.length ? watchRow(st, pr) : null,
          mine.map(p => line(st, p)),
          !mine.length ? h('div', { class: 'row' }, h('span', { class: 'muted small grow' }, pr.open ? 'Nothing is running.' : 'Closed.'), pr.open ? null : h('button', { onclick: () => rpc(st, 'project.open', { name: pr.name }) }, 'Open')) : null);
      }),
      loose.length ? h('div', { class: 'phone-project' }, projects.length ? h('div', { class: 'row phone-project-head' }, h('b', {}, 'Other panes')) : null, loose.map(p => line(st, p))) : null);
  };
  return h('main', { class: 'phone' },
    h('section', {}, h('h2', {}, 'Approvals'), S.approvals.length ? S.approvals.map(a => approvalRow(a)) : h('p', { class: 'muted' }, 'Nothing is waiting for approval.')),
    h('section', {}, h('h2', {}, 'Needs you'), blockers.length ? blockers.map(x => line(x.st, x.p, whereOf(x.st, x.p))) : h('p', { class: 'muted' }, 'No agent is blocked.')),
    pins.length ? h('section', { class: 'phone-pinned' }, h('h2', {}, 'Pinned'), pins.map(x => x.p ? line(x.st, x.p, whereOf(x.st, x.p)) : x.lay ? layRow(x.lay, layShort(x.lay)) : h('div', { class: 'phone-row muted' }, x.gone))) : null,
    h('section', {}, h('h2', {}, 'Stations'), stationsByName().map(station), addRow()),
    globals.length ? h('section', { class: 'phone-layouts' }, h('h2', {}, 'Layouts'), globals.map(x => layRow(x, 'every station'))) : null,
    stopRows(),
    h('section', {}, h('button', { class: 'wide', onclick: fleetUsageDialog }, 'Usage: tokens and estimated cost')),
    h('section', {}, h('button', { class: 'danger big wide', onclick: () => confirmBox('Stop the whole fleet?', 'Every running agent turn on every station is interrupted.', 'Stop everything', () => rpc(api.station, 'stop.all', { scope: 'fleet' })) }, 'Stop every agent in the fleet')));
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

// ------------------------------------------------------------------- render

function render() {
  if (!api.connected && api.authError) {
    if (S.signIn) return;
    const needsSignIn = /^sign in/.test(api.authError);
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
  const r = S.route;
  // The list on the left is drawn again each time: keep where it was scrolled to.
  const sideTop = document.querySelector('.sidebar')?.scrollTop || 0;
  let main;
  if (isPhone()) main = phoneView();
  else if (r.view === 'pane') main = paneView(r.station, r.pane);
  else if (r.view === 'layout') main = layoutView(r.layout);
  else if (r.view === 'watch') main = watchPage(r.station, r.project);
  else if (r.view === 'station') main = stationView(r.station);
  else main = fleetView();
  if (!layCtx) arranging = null;
  // A view that is already on the page stays attached: replacing it would
  // take the keyboard away from whatever is being typed into.
  const kids = [versionBar(), vaultPromptBox, main].filter(Boolean);
  if (root.children.length === kids.length + 1 && kids.every((k, i) => root.children[i + 1] === k)) root.replaceChild(topbar(), root.children[0]);
  else root.replaceChildren(topbar(), ...kids);
  const side = document.querySelector('.sidebar');
  if (side && sideTop) side.scrollTop = sideTop;
  for (const [key, t] of S.terms) if (!t._used) { t.dispose(); S.terms.delete(key); }
  // Each terminal has its look before it is first drawn.
  applyLook();
  for (const t of S.terms.values()) {
    if (t._fresh) { t._fresh = false; t.mount(); }
    else t.layout();
  }
  const lay = r.view === 'layout' ? findLayout(r.layout) : null;
  const what = r.view === 'pane' ? (findPane(r.station, r.pane)?.name || 'pane') : lay ? lay.name : r.view === 'watch' ? `Watch ${r.project}` : r.station || '';
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
  if (document.querySelector('.menu') || document.body.classList.contains('dragging')) return;
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
// smaller. Size the app to what is actually visible, and while the keyboard
// is up give the terminal the room: the bars above it step aside.
if (window.visualViewport) {
  const vv = window.visualViewport;
  const fit = () => {
    const app = document.getElementById('app');
    app.style.height = vv.height + 'px';
    const keyboard = vv.height < window.innerHeight * 0.78;
    app.classList.toggle('kbd', keyboard);
    app.classList.toggle('kbd-term', keyboard && !!document.activeElement?.closest?.('.xterm'));
    if (keyboard) window.scrollTo(0, 0);
  };
  vv.addEventListener('resize', fit);
  vv.addEventListener('scroll', fit);
  document.addEventListener('focusin', () => setTimeout(fit, 50));
  document.addEventListener('focusout', () => setTimeout(fit, 50));
  fit();
}

api.onStatus(() => render());
if ('serviceWorker' in navigator) navigator.serviceWorker.register('sw.js').catch(() => {});
api.connect().then((ok) => {
  if (!ok) return render();
  if (!location.hash || location.hash === '#') history.replaceState(null, '', '#/fleet');
  render();
  refresh();
});

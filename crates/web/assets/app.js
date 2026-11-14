// The Control Deck. Three zoom levels, each with a URL:
//   #/fleet                      every station
//   #/s/<station>                one station: the deck of panes, and split workspaces
//   #/s/<station>/p/<pane>       one pane, full size
// On a phone the same URLs show a small view: status, blockers, approvals, stop.

import { Api } from './api.js';
import { PaneTerm } from './term.js';
import { h, showMenu, closeMenu, toast, dialog, confirmBox, field, ago, dot, stateOf, stateRank, STATE_LABEL } from './ui.js';

const api = new Api();
const root = document.getElementById('app');
const isPhone = () => matchMedia('(max-width: 760px)').matches;

const S = {
  route: { view: 'fleet' },
  fleet: { stations: [], stops: [] },
  panes: new Map(),      // station -> [pane]
  projects: new Map(),   // station -> [project]
  approvals: [],         // across the fleet
  notes: [],             // notification inbox (this station)
  terms: new Map(),      // key -> PaneTerm
  focused: null,
};

// ------------------------------------------------------------------ routing

function parseRoute() {
  const parts = location.hash.replace(/^#\/?/, '').split('/').map(decodeURIComponent);
  if (parts[0] === 's' && parts[1]) {
    if (parts[2] === 'p' && parts[3]) return { view: 'pane', station: parts[1], pane: parts[3] };
    return { view: 'station', station: parts[1] };
  }
  return { view: 'fleet' };
}
const go = (hash) => { if (location.hash !== hash) location.hash = hash; else render(); };
const stationUrl = (s) => `#/s/${encodeURIComponent(s)}`;
const paneUrl = (s, p) => `#/s/${encodeURIComponent(s)}/p/${encodeURIComponent(p)}`;

// --------------------------------------------------------------------- data

const panesOf = (st) => S.panes.get(st) || [];
const liveAgents = (st) => panesOf(st).filter(p => !p.exited);
const findPane = (st, id) => panesOf(st).find(p => p.id === id || p.name === id);
const online = (st) => S.fleet.stations.find(s => s.station === st)?.state === 'online';

async function refreshStation(st) {
  try {
    const [panes, projects] = await Promise.all([api.on(st, 'pane.list'), api.on(st, 'project.list')]);
    S.panes.set(st, panes);
    S.projects.set(st, projects);
  } catch (e) { /* unreachable: the fleet view says so */ }
}

async function refresh() {
  if (!api.connected) return;
  try { S.fleet = await api.call('fleet.view'); } catch (e) { return; }
  const stations = S.fleet.stations.filter(s => s.state === 'online').map(s => s.station);
  await Promise.all(stations.map(refreshStation));
  const lists = await Promise.all(stations.map(st => api.on(st, 'approval.list').catch(() => [])));
  S.approvals = lists.flat();
  try { S.notes = await api.call('notify.list', { limit: 60 }); } catch (e) { /* keep */ }
  try { S.trust = await api.call('trust.status'); } catch (e) { /* keep */ }
  update();
}

let refreshTimer = null;
function refreshSoon(ms = 120) { clearTimeout(refreshTimer); refreshTimer = setTimeout(refresh, ms); }
setInterval(() => { if (document.visibilityState === 'visible') refresh(); }, 2500);

api.onEvent((ev) => {
  if (ev.kind === 'pane.resized') {
    for (const t of S.terms.values()) if (t.station === api.station && t.pane.id === ev.pane) t.resizedBy(ev.data.controller, ev.data.cols, ev.data.rows);
    return;
  }
  if (ev.kind === 'notify.new') notifyBrowser(ev.data);
  if (ev.kind === 'connection.restored') { for (const t of S.terms.values()) t.attach(); }
  if (/^(pane|agent|approval|project|fleet|stop|notify|task|recipe|msg|station|connection|events|vault|trust)\./.test(ev.kind)) refreshSoon();
});

function notifyBrowser(n) {
  toast(n.title + (n.body ? ' — ' + n.body : ''), n.kind === 'error' ? 'error' : 'info');
  if (document.visibilityState !== 'visible' && 'Notification' in window && Notification.permission === 'granted') {
    const note = new Notification(n.title, { body: n.body || '', tag: n.id });
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

// --------------------------------------------------------------- layouts
// Per station: tabs, each a tree of splits. Leaves name a pane.

function loadLayout(st) {
  try { return JSON.parse(localStorage.getItem('spandeck.layout.' + st)) || { tabs: [], active: -1 }; }
  catch (e) { return { tabs: [], active: -1 }; }
}
function saveLayout(st, l) { localStorage.setItem('spandeck.layout.' + st, JSON.stringify(l)); }
const leaves = (n, out = []) => { if (!n) return out; if (n.pane) out.push(n); else { leaves(n.a, out); leaves(n.b, out); } return out; };

function openInWorkspace(st, paneId, dir) {
  const l = loadLayout(st);
  let tab = l.tabs[l.active];
  const existing = l.tabs.findIndex(t => leaves(t.tree).some(x => x.pane === paneId));
  if (existing >= 0 && !dir) { l.active = existing; saveLayout(st, l); return go(stationUrl(st)); }
  if (!tab) {
    const p = findPane(st, paneId);
    tab = { name: p?.project || p?.name || 'workspace', tree: { pane: paneId } };
    l.tabs.push(tab);
    l.active = l.tabs.length - 1;
  } else if (!tab.tree) {
    tab.tree = { pane: paneId };
  } else {
    const target = leaves(tab.tree).find(x => x.pane === S.focused) || leaves(tab.tree).pop();
    const old = { ...target };
    for (const k of Object.keys(target)) delete target[k];
    Object.assign(target, { dir: dir || 'row', ratio: 0.5, a: old, b: { pane: paneId } });
  }
  saveLayout(st, l);
  go(stationUrl(st));
}

function closeLeaf(st, paneId) {
  const l = loadLayout(st);
  const tab = l.tabs[l.active];
  if (!tab) return;
  const prune = (n) => {
    if (!n) return null;
    if (n.pane) return n.pane === paneId ? null : n;
    const a = prune(n.a), b = prune(n.b);
    if (a && b) return { ...n, a, b };
    return a || b;
  };
  tab.tree = prune(tab.tree);
  if (!tab.tree) { l.tabs.splice(l.active, 1); l.active = l.tabs.length ? Math.max(0, l.active - 1) : -1; }
  saveLayout(st, l);
  render();
}

// ------------------------------------------------------------- pane menus

function paneMenu(e, st, p, where) {
  const s = stateOf(p), agent = p.agent !== 'shell', alive = !p.exited;
  const act = (m, extra) => () => rpc(st, m, { pane: p.id, ...extra });
  showMenu(e.clientX, e.clientY, [
    { title: `${p.name} · ${p.address}` },
    where !== 'zoom' && { label: 'Open full size', action: () => go(paneUrl(st, p.id)) },
    where !== 'leaf' && { label: 'Open in workspace', action: () => openInWorkspace(st, p.id) },
    { label: 'Split right', hint: 'Ctrl+Shift+E', action: () => pickPane(st, (id) => openInWorkspace(st, id, 'row'), p.id) },
    { label: 'Split down', hint: 'Ctrl+Shift+O', action: () => pickPane(st, (id) => openInWorkspace(st, id, 'col'), p.id) },
    where === 'leaf' && { label: 'Close this view', hint: 'Ctrl+Shift+W', action: () => closeLeaf(st, p.id) },
    '-',
    agent && alive && { label: 'Stop this turn', action: act('pane.stop'), disabled: !(s === 'working' || s === 'needs_you') },
    agent && alive && { label: p.paused ? 'Unpause (deliver work again)' : 'Pause (no new work)', action: act('pane.pause', { paused: !p.paused }) },
    agent && { label: p.watch ? 'Stop notifying when it finishes' : 'Notify me when it finishes', action: act('pane.set', { watch: !p.watch }) },
    { label: p.muted ? 'Unmute notifications' : 'Mute notifications', action: act('pane.set', { muted: !p.muted }) },
    '-',
    { label: 'Show changes (diff)', action: () => diffPanel(st, p) },
    agent && { label: 'Standing orders for this pane', action: () => ordersPanel(st, p) },
    agent && p.project && alive && { label: 'Start a review loop on its work', action: () => rpc(st, 'recipe.review', { pane: p.id }).then(() => toast('Review loop started')) },
    agent && alive && { label: 'Run a command in it…', action: () => commandDialog(st, p) },
    { label: 'Send it a message', action: () => messageDialog(st, p) },
    { label: 'Rename', action: () => renameDialog(st, p) },
    { label: 'Copy address', action: () => navigator.clipboard?.writeText(p.address).then(() => toast('Copied ' + p.address)) },
    '-',
    alive && { label: 'Kill the process', danger: true, action: () => confirmBox('Kill ' + p.name + '?', 'The process ends now. An agent can be resumed later by its session.', 'Kill', act('pane.kill')) },
    { label: alive ? 'Kill and remove' : 'Remove from the list', danger: true, action: () => confirmBox('Remove ' + p.name + '?', 'The pane is closed and removed from this station.', 'Remove', act('pane.close')) },
  ]);
}

function pickPane(st, then, exclude) {
  const options = liveAgents(st).filter(p => p.id !== exclude);
  dialog('Choose a pane', (close) => h('div', { class: 'list' },
    options.map(p => h('button', { class: 'list-row', onclick: () => { close(); then(p.id); } }, dot(p), h('b', {}, p.name), h('span', { class: 'muted' }, p.project || ''))),
    h('button', { class: 'list-row', onclick: () => { close(); newPaneDialog(st, null, then); } }, '＋ New pane…')));
}

// ------------------------------------------------------------------ dialogs

function newPaneDialog(st, project, then) {
  dialog('New pane on ' + st, (close) => {
    const agent = h('select', {}, h('option', { value: 'claude' }, 'Claude Code'), h('option', { value: 'codex' }, 'Codex'), h('option', { value: 'shell' }, 'Shell'));
    const name = h('input', { placeholder: 'optional' });
    const proj = h('select', {}, h('option', { value: '' }, '(no project)'), (S.projects.get(st) || []).map(p => h('option', { value: p.name, selected: p.name === project }, p.name)));
    const cwd = h('input', { placeholder: 'default: the project folder, or your home folder' });
    const submit = async () => {
      const params = { agent: agent.value, name: name.value || undefined, project: proj.value || undefined, cwd: cwd.value || undefined };
      const pane = await rpc(st, 'pane.spawn', params);
      close();
      then ? then(pane.id) : go(paneUrl(st, pane.id));
    };
    return h('form', { onsubmit: (e) => { e.preventDefault(); submit(); } },
      field('Run', agent), field('Name', name), field('Project', proj), field('Folder', cwd),
      h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Cancel'), h('button', { class: 'primary', type: 'submit' }, 'Start')));
  });
}

function newProjectDialog(st) {
  dialog('New project on ' + st, (close) => {
    const path = h('input', { placeholder: '/path/to/the/repo', required: true });
    const name = h('input', { placeholder: 'default: the folder name' });
    const rows = h('div', { class: 'slots' });
    const addRow = (n = '', a = 'claude', r = '') => rows.append(h('div', { class: 'slot' },
      h('input', { placeholder: 'pane name', value: n, 'aria-label': 'Pane name' }),
      h('select', { 'aria-label': 'Agent' }, ['claude', 'codex', 'shell'].map(x => h('option', { value: x, selected: x === a }, x))),
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
      field('Folder on ' + st, path, 'A folder that already has .spandeck/project.toml is opened as it is.'),
      field('Name', name),
      h('div', { class: 'field' }, h('span', {}, 'Panes to start'), rows, h('button', { type: 'button', class: 'mini', onclick: () => addRow() }, '＋ Add a pane')),
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

// Stations and browsers asking to be let in. Approving one gives it control
// of every station, so the code shown here must match the one it shows.
function trustPanel() {
  dialog('Stations and browsers', (close) => {
    const t = S.trust || {};
    const answer = (r, yes) => rpc(api.station, yes ? 'trust.approve' : 'trust.deny', { request: r.id }).then(close);
    return h('div', {},
      h('h3', {}, 'Asking to be let in'),
      (t.requests || []).length ? h('div', { class: 'list' }, t.requests.map(r => h('div', { class: 'note unread' },
        h('div', {}, h('b', {}, r.kind === 'station' ? `Station ${r.name}` : r.name), h('span', { class: 'muted' }, ` · from ${r.from}`)),
        h('div', {}, 'It shows the code ', h('b', { class: 'code' }, r.code), '. Approve only if you started this and the codes match: it will be able to control every station.'),
        h('div', { class: 'row end' }, h('button', { class: 'mini', onclick: () => answer(r, false) }, 'Refuse'), h('button', { class: 'mini primary', onclick: () => answer(r, true) }, 'Approve')))))
        : h('p', { class: 'muted' }, 'Nothing is waiting.'),
      t.outgoing ? h('p', { class: t.outgoing.problem ? 'warn-text' : '' }, t.outgoing.problem
        ? `Joining ${t.outgoing.station}'s fleet did not finish: ${t.outgoing.problem}.`
        : `Waiting for ${t.outgoing.station} to approve this station. It should show the code ${t.outgoing.code}.`) : null,
      h('h3', {}, 'Browsers signed in here'),
      (t.devices || []).length ? h('div', { class: 'list' }, t.devices.map(d => h('div', { class: 'note' },
        h('div', { class: 'row' }, h('b', {}, d.name), h('span', { class: 'muted' }, ` · from ${d.from} · ${ago(d.approved_ms)} ago`), h('span', { class: 'grow' }),
          h('button', { class: 'mini danger', onclick: () => rpc(api.station, 'device.revoke', { device: d.id }).then(close) }, 'Sign out')))))
        : h('p', { class: 'muted' }, 'None. A browser on another device asks to sign in the first time it opens the Control Deck.'),
      h('p', { class: 'muted' }, `Fleet ${t.fleet || ''}. To add a station, run "spandeck fleet --join ${api.station}" on it, then approve it here.`));
  });
}

function joinFleet(station) {
  rpc(api.station, 'fleet.join', { station }).then((r) => dialog(`Joining ${station}'s fleet`, () => h('div', {},
    h('p', {}, 'This station asked to join. It shows the code ', h('b', { class: 'code' }, r.code), '.'),
    h('p', {}, `On ${station}, open its Control Deck and approve the request with that code, or run "spandeck approve ${r.code}" there.`),
    h('p', { class: 'muted' }, 'This station then leaves its own fleet. Browsers signed in to it sign in again.'))));
}

function approvalsPanel() {
  dialog('Approval requests', (close) => S.approvals.length
    ? h('div', { class: 'list' }, S.approvals.map(a => approvalRow(a, close)))
    : h('p', {}, 'Nothing is waiting for your approval.'));
}

function inboxPanel() {
  api.call('msg.inbox', { all: true, peek: true }).then((msgs) => {
    dialog('Inbox', () => h('div', {},
      h('div', { class: 'row end' }, h('button', { class: 'mini', onclick: () => { api.call('notify.read', {}); api.call('msg.inbox', {}); refreshSoon(); } }, 'Mark all read'),
        'Notification' in window && Notification.permission === 'default' ? h('button', { class: 'mini', onclick: () => Notification.requestPermission() }, 'Enable browser notifications') : null),
      h('h3', {}, 'Messages to you'),
      msgs.length ? h('div', { class: 'list' }, msgs.slice().reverse().map(m => h('div', { class: 'note' + (m.read ? '' : ' unread') },
        h('div', {}, h('b', {}, m.from), h('span', { class: 'muted' }, ` · ${m.kind} · ${ago(m.ts)} ago`)), h('div', { class: 'pre' }, m.body)))) : h('p', { class: 'muted' }, 'No messages.'),
      h('h3', {}, 'Notifications'),
      S.notes.length ? h('div', { class: 'list' }, S.notes.map(n => h('button', { class: 'list-row note' + (n.read ? '' : ' unread'), onclick: () => { api.call('notify.read', { id: n.id }); if (n.url) location.hash = n.url.replace(/^\/#/, '#'); } },
        h('span', { class: 'dot ' + (n.kind === 'approval' ? 'needs_you' : n.kind) }), h('b', {}, n.title), h('span', { class: 'muted' }, n.body), h('span', { class: 'grow' }),
        h('span', { class: 'muted' }, n.pushed === 2 ? 'held (you were here) · ' : n.pushed === 1 ? 'pushed · ' : '', ago(n.ts))))) : h('p', { class: 'muted' }, 'No notifications.')), { wide: true });
  }).catch(e => toast(e.message, 'error'));
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
    { label: 'New pane…', run: () => newPaneDialog(S.route.station || api.station) },
    { label: 'New project…', run: () => newProjectDialog(S.route.station || api.station) },
    { label: 'Approval requests', run: approvalsPanel },
    { label: 'Inbox', run: inboxPanel },
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

async function doctorPanel() {
  const st = S.route.station || api.station;
  const d = await api.on(st, 'station.doctor');
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
async function setupPanel() {
  let view;
  try { view = await api.call('setup.get'); } catch (e) { return toast(e.message, 'error'); }
  const setup = view.setup;
  setup.mcp_servers ||= {}; setup.skills ||= {}; setup.defaults ||= {};
  dialog('Agent setup', (close, body) => {
    const draw = () => body.replaceChildren(content());
    const save = async () => {
      const out = await rpc(api.station, 'setup.set', { setup, fleet: true });
      const bad = out.stations.filter(x => !x.ok);
      toast(bad.length ? `Saved. Not yet on: ${bad.map(x => x.station).join(', ')} (they catch up when they are back)` : `Saved on ${out.stations.length} station${out.stations.length === 1 ? '' : 's'}`);
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
            h('td', { class: 'muted' }, a.installed_version || 'not installed'),
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

function vaultChip(s) {
  const v = s.vault;
  if (!v || !v.found) return null;
  const cls = v.state === 'connected' ? 'ok' : v.state === 'not_connected' ? '' : 'warn';
  return h('span', { class: 'chip vault-chip ' + cls, title: v.problem || 'AgentVault' }, 'vault: ' + (VAULT_LABEL[v.state] || v.state));
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
                v.state === 'connected' && v.vault_rotation_hours !== v.rotation_hours && v.vault_rotation_hours !== undefined && (v.vault_rotation_hours ?? null) !== (v.rotation_hours ?? null) ? h('small', { class: 'warn-text block' }, 'The vault has a different rotation setting than this one.') : null),
              h('td', {}, rotation, v.next_rotation_ms && v.rotation_hours ? h('small', { class: 'muted block' }, 'next ' + new Date(v.next_rotation_ms).toLocaleString()) : null),
              h('td', {}, !can(s) ? null
                : v.state === 'connected' ? h('button', { class: 'mini', onclick: () => confirmBox(`Disconnect ${s.station}?`, `The station forgets its token. Revoke spandeck:${s.station} on the vault's Tokens page to end it there too.`, 'Disconnect', () => rpc(s.station, 'vault.disconnect').then((r) => toast(r.next))) }, 'Disconnect')
                : v.state === 'locked' || v.state === 'unreachable' ? h('span', { class: 'muted' }, v.state === 'locked' ? 'Unlock the vault first' : '')
                : h('button', { class: 'primary', onclick: () => vaultConnect([s.station]) }, 'Connect')));
          }))),
        h('div', { class: 'row end' },
          h('button', { onclick: () => rpc(api.station, 'vault.refresh').then(() => refresh()) }, 'Check now'),
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

function topbar() {
  const r = S.route;
  const needs = S.fleet.stations.flatMap(s => liveAgents(s.station)).filter(p => p.state === 'needs_you').length;
  const unread = S.notes.filter(n => !n.read).length;
  const pane = r.view === 'pane' ? findPane(r.station, r.pane) : null;
  const crumbs = [h('a', { href: '#/fleet' }, 'Fleet')];
  if (r.station) crumbs.push('›', h('a', { href: stationUrl(r.station) }, r.station));
  if (pane?.project) crumbs.push('›', h('span', {}, pane.project));
  if (pane) crumbs.push('›', h('span', { class: 'here' }, pane.name));
  return h('header', { class: 'topbar' },
    h('a', { class: 'brand', href: '#/fleet', title: 'spandeck ' + (api.version || '') }, 'spandeck'),
    h('nav', { class: 'crumbs', 'aria-label': 'Where you are' }, crumbs),
    h('span', { class: 'grow' }),
    !api.connected ? h('span', { class: 'chip bad' }, api.authError || 'reconnecting…') : null,
    needs ? h('span', { class: 'chip needs', title: 'Agents waiting for you' }, `${needs} need${needs === 1 ? 's' : ''} you`) : null,
    h('button', { class: S.approvals.length ? 'attn' : '', onclick: approvalsPanel }, 'Approvals', S.approvals.length ? h('span', { class: 'count' }, S.approvals.length) : null),
    h('button', { onclick: inboxPanel }, 'Inbox', unread ? h('span', { class: 'count' }, unread) : null),
    (S.trust?.requests?.length || S.trust?.outgoing) || !isPhone() ? h('button', { class: S.trust?.requests?.length ? 'attn' : '', onclick: trustPanel, title: 'Stations and browsers allowed to control your fleet' }, 'Access', S.trust?.requests?.length ? h('span', { class: 'count' }, S.trust.requests.length) : null) : null,
    isPhone() ? null : h('button', { onclick: setupPanel, title: 'MCP servers, skills and model defaults for every agent' }, 'Agent setup'),
    isPhone() || !S.fleet.stations.some(x => x.vault?.found) ? null : h('button', { class: S.fleet.stations.some(x => x.state === 'online' && ['not_connected', 'token_rejected', 'locked'].includes(x.vault?.state)) ? 'attn' : '', onclick: vaultPanel, title: 'AgentVault: keys for agents without handing over keys' }, 'Vault'),
    h('button', { class: 'danger', onclick: stopAllMenu, title: 'Interrupt running agent turns' }, 'Stop all'),
    isPhone() ? null : h('button', { onclick: palette, title: 'Ctrl+K' }, '⌘K'));
}

function stopRows() {
  const recent = (S.fleet.stops || []).filter(s => Date.now() - s.issued_ms < 10 * 60 * 1000 || s.status !== 'confirmed').slice(0, 6);
  if (!recent.length) return null;
  return h('div', { class: 'stops' }, h('b', {}, 'Stop requests'), recent.map(s => h('span', { class: 'stop ' + s.status, title: `issued ${ago(s.issued_ms)} ago` },
    `${s.station}: ${s.label}`, s.status === 'confirmed' ? ` (${s.result?.stopped?.length ?? 0} stopped)` : s.status === 'unreachable' ? ' — will be delivered when it is back' : '')));
}

function stationTile(s) {
  const agents = liveAgents(s.station).filter(p => p.agent !== 'shell').sort((a, b) => stateRank(a) - stateRank(b));
  const up = s.state === 'online';
  return h('a', {
    class: 'tile station ' + s.state, href: stationUrl(s.station),
    oncontextmenu: (e) => { e.preventDefault(); showMenu(e.clientX, e.clientY, [
      { label: 'Open ' + s.station, action: () => go(stationUrl(s.station)) },
      up && { label: 'New pane here…', action: () => newPaneDialog(s.station) },
      up && { label: 'New project here…', action: () => newProjectDialog(s.station) },
      s.state === 'not_linked' && { label: `Join ${s.station}'s fleet…`, action: () => joinFleet(s.station) },
      up && { label: 'Restart spandeck here (keeps terminals)', action: () => confirmBox(`Restart spandeck on ${s.station}?`, 'Every terminal and agent keeps running. If it does not come back healthy, the restart is undone.', 'Restart', () => rpc(s.station, 'station.restart', {}).then(() => toast(`${s.station} is restarting`))) },
      up && { label: 'Update to the latest release…', action: () => confirmBox(`Update spandeck on ${s.station}?`, `It runs ${s.version}. The latest release is downloaded on ${s.station} and checked against its published checksum. Terminals keep running, and a version that does not start properly is undone.`, 'Update', () => rpc(s.station, 'station.update', {}).then((r) => toast(r.current ? `${s.station} already runs the latest release` : `${s.station} is updating to ${r.release}`))) },
      s.state === 'unreachable' && { label: 'Wake it (same network only)', action: () => rpc(api.station, 'fleet.wake', { station: s.station }).then((r) => toast(`Sent ${r.sent} wake packets to ${s.station}. Give it a minute.`)) },
      s.state === 'unreachable' && { label: 'Start spandeck there (over SSH)', action: () => rpc(api.station, 'fleet.start', { station: s.station }).then((r) => toast(`${s.station} is ${r.state}`)) },
      s.state === 'unreachable' && { label: 'Forget this station', danger: true, action: () => confirmBox(`Forget ${s.station}?`, 'It is removed from this list. If it comes back it is listed again.', 'Forget', () => rpc(api.station, 'fleet.forget', { station: s.station })) },
      up && { label: 'Stop every agent here', danger: true, action: () => rpc(api.station, 'stop.all', { scope: s.station === api.station ? 'station' : s.station }) },
    ]); },
  },
    h('div', { class: 'tile-head' }, h('span', { class: 'dot ' + (up ? (s.needs_you ? 'needs_you' : s.working ? 'working' : 'idle') : 'unreachable') }), h('b', {}, s.station), s.self ? h('span', { class: 'chip' }, 'this station') : null, h('span', { class: 'grow' }), h('span', { class: 'muted' }, s.version || '')),
    up ? h('div', { class: 'tile-stats' },
      h('span', {}, h('b', {}, s.working ?? 0), ' working'), h('span', { class: s.needs_you ? 'needs' : '' }, h('b', {}, s.needs_you ?? 0), ' need you'),
      s.load != null ? h('span', { class: 'muted' }, 'load ' + Number(s.load).toFixed(1)) : null, s.keep_awake ? h('span', { class: 'chip' }, 'kept awake') : null,
      s.service === false ? h('span', { class: 'chip warn', title: 'spandeck does not start at login on this station. After a reboot it stays down until someone starts it. Run "spandeck service --install" there.' }, 'not started at login') : null, vaultChip(s))
      : h('div', { class: 'tile-stats warn-text' }, s.state === 'update_needed' ? 'Needs an update before it can be controlled' : s.state === 'unreachable' ? `Unreachable · last seen ${ago(s.last_seen_ms)} ago. Right-click to wake or start it.` : s.state === 'not_linked' ? `Not in this fleet. On ${s.station}, run "spandeck fleet --join ${api.station}", then approve it under Access.` : `Unreachable · last seen ${ago(s.last_seen_ms)} ago`),
    h('div', { class: 'tile-agents' }, agents.slice(0, 8).map(p => h('div', { class: 'agent-line' }, dot(p), h('span', {}, p.name), h('span', { class: 'muted' }, p.tool || p.needs_reason || STATE_LABEL[stateOf(p)]))),
      up && !agents.length ? h('span', { class: 'muted' }, 'No agents running') : null));
}

function fleetView() {
  const stations = S.fleet.stations.slice().sort((a, b) => (b.needs_you || 0) - (a.needs_you || 0) || (b.self ? 1 : 0) - (a.self ? 1 : 0));
  return h('main', { class: 'fleet' }, stopRows(),
    h('div', { class: 'tiles' }, stations.map(stationTile)),
    stations.length <= 1 ? h('p', { class: 'muted hint' }, 'Other stations appear here on their own: any machine under your Tailscale login that runs spandeckd.') : null);
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

function paneTile(st, p) {
  const key = st + '/' + p.id;
  return h('a', { class: 'tile pane-tile ' + stateOf(p), href: paneUrl(st, p.id), oncontextmenu: (e) => { e.preventDefault(); paneMenu(e, st, p, 'tile'); } },
    h('div', { class: 'tile-head' }, dot(p), h('b', {}, p.name), p.agent !== 'shell' ? h('span', { class: 'chip' }, p.agent_name) : null, h('span', { class: 'grow' }),
      h('span', { class: 'muted' }, p.tool ? `${p.tool} · ${ago(p.tool_since_ms)}` : `${STATE_LABEL[stateOf(p)] || stateOf(p)} · ${ago(p.state_since_ms)}`)),
    p.needs_reason ? h('div', { class: 'needs-line' }, p.needs_reason) : null,
    h('pre', { class: 'thumb', dataset: { key } }, thumbText(thumbs.get(key) || [])));
}

function sidebar(st) {
  const projects = S.projects.get(st) || [];
  const panes = panesOf(st);
  const row = (p) => h('a', { class: 'side-pane' + (S.route.pane === p.id ? ' current' : ''), href: paneUrl(st, p.id), oncontextmenu: (e) => { e.preventDefault(); paneMenu(e, st, p, 'side'); } }, dot(p), h('span', {}, p.name), p.paused ? h('span', { class: 'chip warn' }, 'paused') : null);
  const projMenu = (e, pr) => { e.preventDefault(); showMenu(e.clientX, e.clientY, [
    { title: pr.address },
    pr.open ? { label: 'Close project (keeps conversations)', action: () => rpc(st, 'project.close', { name: pr.name }) } : { label: 'Open project', action: () => rpc(st, 'project.open', { name: pr.name }) },
    { label: 'New pane in this project…', action: () => newPaneDialog(st, pr.name) },
    { label: 'Tasks, reviews and sign-off', action: () => tasksPanel(st, pr.name).catch(er => toast(er.message, 'error')) },
    '-',
    { label: 'Remove from this station', danger: true, action: () => confirmBox('Remove ' + pr.name + '?', 'Its panes are closed. The folder and its .spandeck files are not touched.', 'Remove', () => rpc(st, 'project.remove', { name: pr.name })) },
  ]); };
  return h('aside', { class: 'sidebar', oncontextmenu: (e) => { if (e.target.closest('.side-pane,.side-project')) return; e.preventDefault(); showMenu(e.clientX, e.clientY, [
    { label: 'New pane…', action: () => newPaneDialog(st) }, { label: 'New project…', action: () => newProjectDialog(st) }, { label: 'Check this station', action: doctorPanel }]); } },
    projects.map(pr => h('div', { class: 'side-group' },
      h('div', { class: 'side-project', oncontextmenu: (e) => projMenu(e, pr), title: pr.path }, h('b', {}, pr.name), pr.branch ? h('span', { class: 'muted' }, pr.branch) : null, h('span', { class: 'grow' }),
        h('button', { class: 'icon-btn', 'aria-label': 'Project menu', onclick: (e) => projMenu(e, pr) }, '⋮')),
      pr.open ? panes.filter(p => p.project === pr.name).sort((a, b) => stateRank(a) - stateRank(b)).map(row) : h('button', { class: 'mini', onclick: () => rpc(st, 'project.open', { name: pr.name }) }, 'Open'),
      pr.problem ? h('div', { class: 'warn-text small' }, pr.problem) : null)),
    h('div', { class: 'side-group' }, h('div', { class: 'side-project' }, h('b', {}, 'Other panes')), panes.filter(p => !p.project).sort((a, b) => stateRank(a) - stateRank(b)).map(row)),
    h('div', { class: 'side-actions' }, h('button', { onclick: () => newPaneDialog(st) }, '＋ Pane'), h('button', { onclick: () => newProjectDialog(st) }, '＋ Project')));
}

function getTerm(st, p, where, mode) {
  const key = `${where}:${st}/${p.id}`;
  let t = S.terms.get(key);
  if (!t) {
    t = new PaneTerm(api, st, p, { mode, typeWhileWatching: where === 'phone', onMenu: (e, term) => paneMenu(e, st, term.pane, where), onFocus: (term) => { S.focused = term.pane.id; } });
    S.terms.set(key, t);
    t._fresh = true;
  } else t.update(p);
  t._used = true;
  return t;
}

function renderTree(st, node, tab) {
  if (!node) return h('div', { class: 'empty' }, 'Empty workspace. Click a pane on the left to place it here.');
  if (node.pane) {
    const p = findPane(st, node.pane);
    if (!p) return h('div', { class: 'empty' }, 'This pane was closed. ', h('button', { class: 'mini', onclick: () => closeLeaf(st, node.pane) }, 'Remove the view'));
    return getTerm(st, p, 'leaf', 'control').el;
  }
  const a = h('div', { class: 'split-a', style: { flex: `${node.ratio} 1 0` } }, renderTree(st, node.a, tab));
  const b = h('div', { class: 'split-b', style: { flex: `${1 - node.ratio} 1 0` } }, renderTree(st, node.b, tab));
  const gutter = h('div', { class: 'gutter', role: 'separator', title: 'Drag to resize' });
  const box = h('div', { class: 'split ' + node.dir }, a, gutter, b);
  gutter.addEventListener('mousedown', (e) => {
    e.preventDefault();
    const rect = box.getBoundingClientRect();
    const move = (ev) => {
      const r = node.dir === 'row' ? (ev.clientX - rect.left) / rect.width : (ev.clientY - rect.top) / rect.height;
      node.ratio = Math.min(0.9, Math.max(0.1, r));
      a.style.flex = `${node.ratio} 1 0`;
      b.style.flex = `${1 - node.ratio} 1 0`;
    };
    const up = () => { removeEventListener('mousemove', move); removeEventListener('mouseup', up); document.body.classList.remove('dragging'); const l = loadLayout(st); l.tabs[l.active] = tab; saveLayout(st, l); };
    document.body.classList.add('dragging');
    addEventListener('mousemove', move);
    addEventListener('mouseup', up);
  });
  return box;
}

function stationView(st) {
  const info = S.fleet.stations.find(s => s.station === st);
  if (info && info.state !== 'online') return h('main', { class: 'fleet' }, h('p', { class: 'warn-text' }, `${st} is ${info.state === 'update_needed' ? 'on a version this station cannot control' : 'unreachable'}. Nothing here says what it is doing right now.`));
  const l = loadLayout(st);
  if (l.active >= l.tabs.length) l.active = l.tabs.length - 1;
  const tab = l.tabs[l.active];
  const tabs = h('div', { class: 'tabs', role: 'tablist' },
    h('button', { class: 'tab' + (l.active < 0 ? ' current' : ''), role: 'tab', onclick: () => { l.active = -1; saveLayout(st, l); render(); } }, 'Deck'),
    l.tabs.map((t, i) => h('button', { class: 'tab' + (i === l.active ? ' current' : ''), role: 'tab',
      onclick: () => { l.active = i; saveLayout(st, l); render(); },
      oncontextmenu: (e) => { e.preventDefault(); showMenu(e.clientX, e.clientY, [
        { label: 'Rename', action: () => dialog('Rename workspace', (close) => { const n = h('input', { value: t.name }); return h('form', { onsubmit: (ev) => { ev.preventDefault(); t.name = n.value; saveLayout(st, l); close(); render(); } }, field('Name', n), h('div', { class: 'row end' }, h('button', { class: 'primary', type: 'submit' }, 'Rename'))); }) },
        { label: 'Close workspace', danger: true, action: () => { l.tabs.splice(i, 1); l.active = Math.min(l.active, l.tabs.length - 1); saveLayout(st, l); render(); } }]); } }, t.name)),
    h('button', { class: 'tab add', title: 'New workspace', onclick: () => { l.tabs.push({ name: 'workspace ' + (l.tabs.length + 1), tree: null }); l.active = l.tabs.length - 1; saveLayout(st, l); render(); } }, '＋'));
  let content;
  if (!tab) {
    const live = liveAgents(st).sort((a, b) => stateRank(a) - stateRank(b));
    content = h('div', { class: 'deck' }, live.length ? h('div', { class: 'tiles' }, live.map(p => paneTile(st, p))) : h('div', { class: 'empty' }, 'Nothing is running here. Start a pane or a project from the left.'));
    loadThumbs(st, live);
  } else {
    content = h('div', { class: 'workspace' }, renderTree(st, tab.tree, tab));
  }
  return h('div', { class: 'station-view' }, sidebar(st), h('main', { class: 'station-main' }, tabs, content));
}

function paneView(st, id) {
  const p = findPane(st, id);
  if (!p) return h('div', { class: 'station-view' }, sidebar(st), h('main', { class: 'station-main' }, h('div', { class: 'empty' }, online(st) === false ? `${st} is unreachable.` : 'That pane is not on this station any more.')));
  const t = getTerm(st, p, 'zoom', 'control');
  setTimeout(() => t.focus(), 50);
  return h('div', { class: 'station-view' }, sidebar(st), h('main', { class: 'station-main zoomed' }, t.el));
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

function phoneView() {
  const r = S.route;
  if (r.view === 'pane') {
    const p = findPane(r.station, r.pane);
    if (!p) return h('main', { class: 'phone' }, h('p', {}, 'That pane is gone.'), h('a', { href: '#/fleet' }, 'Back'));
    // Watching shows the pane at the size someone else set, shrunk to fit.
    // Fitting resizes the pane to this screen, as a computer's deck would.
    const fitKey = `${r.station}/${p.id}`;
    const fitted = phoneFit.has(fitKey);
    const t = fitted ? getTerm(r.station, p, 'phone-fit', 'control') : getTerm(r.station, p, 'phone', 'watch');
    // The page is redrawn every few seconds. The terminal and the message
    // box stay where they are, so the keyboard does not close while you type.
    let v = phonePanes.get(fitKey);
    if (!v) {
      v = { head: h('div', { class: 'row' }), slot: h('div', { class: 'keep' }), foot: h('div', { class: 'keep' }) };
      v.main = h('main', { class: 'phone phone-pane' }, v.head, v.slot, v.foot);
      phonePanes.clear();
      phonePanes.set(fitKey, v);
    }
    v.head.replaceChildren(h('a', { class: 'btn', href: '#/fleet' }, '‹ Back'),
      p.exited ? '' : h('button', { 'aria-pressed': String(fitted), title: fitted ? 'Stop resizing this pane to the phone' : 'Resize this pane to fit this screen', onclick: () => { fitted ? phoneFit.delete(fitKey) : phoneFit.add(fitKey); update(); } }, fitted ? 'Fitted' : 'Fit to screen'),
      h('span', { class: 'grow' }),
      h('button', { class: 'danger big', disabled: p.exited || !(p.state === 'working' || p.state === 'needs_you'), onclick: () => rpc(r.station, 'pane.stop', { pane: p.id }).then(() => toast('Stop sent')) }, 'Stop this turn'));
    if (v.slot.firstChild !== t.el) v.slot.replaceChildren(t.el);
    const foot = p.exited ? (v.ended ||= h('p', { class: 'muted small' }, 'This pane has ended.')) : composer(r.station, p);
    if (v.foot.firstChild !== foot) v.foot.replaceChildren(foot);
    return v.main;
  }
  const all = S.fleet.stations.flatMap(s => liveAgents(s.station).map(p => ({ st: s.station, p })));
  const blockers = all.filter(x => x.p.agent !== 'shell' && (x.p.state === 'needs_you' || x.p.state === 'error'));
  const line = ({ st, p }) => h('a', { class: 'phone-row', href: paneUrl(st, p.id) }, dot(p), h('div', { class: 'grow' }, h('b', {}, p.name), h('span', { class: 'muted' }, ` · ${st}${p.project ? ' · ' + p.project : ''}`),
    h('div', { class: 'muted small' }, p.needs_reason || (p.tool ? `${p.tool} · ${ago(p.tool_since_ms)}` : `${STATE_LABEL[stateOf(p)]} · ${ago(p.state_since_ms)}`))), '›');
  return h('main', { class: 'phone' },
    h('section', {}, h('h2', {}, 'Approvals'), S.approvals.length ? S.approvals.map(a => approvalRow(a)) : h('p', { class: 'muted' }, 'Nothing is waiting for approval.')),
    h('section', {}, h('h2', {}, 'Needs you'), blockers.length ? blockers.map(line) : h('p', { class: 'muted' }, 'No agent is blocked.')),
    h('section', {}, h('h2', {}, 'Stations'),
      S.fleet.stations.map(s => h('div', { class: 'phone-station' },
        h('div', { class: 'row' }, h('span', { class: 'dot ' + (s.state === 'online' ? 'idle' : 'unreachable') }), h('b', {}, s.station), h('span', { class: 'muted' }, s.state === 'online' ? `${s.working ?? 0} working · ${s.needs_you ?? 0} need you` : s.state.replace('_', ' ')), h('span', { class: 'grow' }),
          s.state === 'online' ? h('button', { onclick: () => newPaneDialog(s.station) }, 'New') : null,
          s.state === 'unreachable' ? h('button', { onclick: () => rpc(api.station, 'fleet.wake', { station: s.station }).then((r) => toast(`Sent ${r.sent} wake packets. Give it a minute.`)) }, 'Wake') : null,
          s.state === 'unreachable' ? h('button', { onclick: () => rpc(api.station, 'fleet.start', { station: s.station }).then((r) => toast(`${s.station} is ${r.state}`)) }, 'Start') : null,
          s.state === 'online' ? h('button', { class: 'danger', onclick: () => confirmBox(`Stop every agent on ${s.station}?`, 'Running turns are interrupted. Processes stay.', 'Stop', () => rpc(api.station, 'stop.all', { scope: s.self ? 'station' : s.station })) }, 'Stop') : null),
        all.filter(x => x.st === s.station).sort((a, b) => stateRank(a.p) - stateRank(b.p)).map(line)))),
    stopRows(),
    h('section', {}, h('button', { class: 'danger big wide', onclick: () => confirmBox('Stop the whole fleet?', 'Every running agent turn on every station is interrupted.', 'Stop everything', () => rpc(api.station, 'stop.all', { scope: 'fleet' })) }, 'Stop every agent in the fleet')));
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
  S.route = parseRoute();
  for (const t of S.terms.values()) t._used = false;
  const r = S.route;
  let main;
  if (isPhone()) main = phoneView();
  else if (r.view === 'pane') main = paneView(r.station, r.pane);
  else if (r.view === 'station') main = stationView(r.station);
  else main = fleetView();
  // A view that is already on the page stays attached: replacing it would
  // take the keyboard away from whatever is being typed into.
  if (root.children.length === 2 && root.children[1] === main) root.replaceChild(topbar(), root.children[0]);
  else root.replaceChildren(topbar(), main);
  for (const [key, t] of S.terms) {
    if (!t._used) { t.dispose(); S.terms.delete(key); }
    else if (t._fresh) { t._fresh = false; t.mount(); }
    else t.layout();
  }
  document.title = (r.pane ? (findPane(r.station, r.pane)?.name || 'pane') + ' · ' : r.station ? r.station + ' · ' : '') + 'spandeck';
}

// Data changed: redraw everything except live terminals, which are reused.
function update() {
  // Do not redraw under an open dialog's feet or while a menu is open.
  if (document.querySelector('.menu')) return;
  const active = document.activeElement;
  const typing = active && active.closest && active.closest('.dialog');
  const focusedTerm = [...S.terms.values()].find(t => t.el.contains(active));
  render();
  if (focusedTerm && !typing) focusedTerm.focus();
}

addEventListener('hashchange', render);
addEventListener('resize', () => { clearTimeout(S._rs); S._rs = setTimeout(render, 150); });
addEventListener('keydown', (e) => {
  const mod = e.ctrlKey || e.metaKey;
  if (mod && e.key.toLowerCase() === 'k') { e.preventDefault(); palette(); return; }
  const st = S.route.station;
  if (mod && e.shiftKey && st) {
    const k = e.key.toLowerCase();
    if (k === 'e' || k === 'o') { e.preventDefault(); pickPane(st, (id) => openInWorkspace(st, id, k === 'e' ? 'row' : 'col'), S.focused); }
    if (k === 'w' && S.focused) { e.preventDefault(); closeLeaf(st, S.focused); }
  }
  // Esc zooms out, unless a terminal has the keyboard (agents need Esc).
  if (e.key === 'Escape' && !document.querySelector('.backdrop,.menu') && !document.activeElement?.closest('.pane')) {
    if (S.route.view === 'pane') go(stationUrl(S.route.station));
    else if (S.route.view === 'station') go('#/fleet');
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

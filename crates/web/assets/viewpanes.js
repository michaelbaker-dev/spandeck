// A layout holds more than programs (DECISIONS.md, 20 Nov 2026, the owner's
// request): two kinds of leaf show a view and run nothing.
//   Files  the file browser (files.js), at a folder of a station
//   Watch  a project's agents and what they say to each other (app.js)
//
// A view is a leaf of the layout itself, saved wherever the layout is saved
// (the project's file, or the deck document), so it is the same from every
// station and in every browser. It is not a pane of any station: it has no
// process and no token, nothing an agent can address or message, and it
// starts and stops nothing. Taking it out of a layout removes nothing on
// disk.
//
// The saved form is a pane named by id, with an id no pane has:
//   { id: "view.files.k3f9", station?, folder?, hidden? }
//   { id: "view.watch.x1",   station?, watch?,  hidden? }
// A station and a deck from before views read that as a pane they do not
// have, show "This pane was closed", and keep `id` and `station` when they
// write the layout again; the kind is in the id for that reason. What they
// drop has a meaning when it is missing: no `folder` is the project's
// folder (or the station's home), no `watch` is the layout's own project,
// no `hidden` is showing.
//
// Rules kept here:
//   - A folder's name, a project's name and a station's name are data: put
//     on the page as text (`h()`), never as HTML.
//   - Nothing here asks a station anything the page did not ask already: a
//     Files cell is the panel's own listing, and Watch is drawn from what
//     the page holds.
//   - Where a Files cell was browsed to is this browser's own. The folder
//     it opens at is the layout's, and is changed only when the owner says.

import { h, showMenu, dialog, toast, field, kindTag, KINDS, DOT_KEY, stateOf, dot } from './ui.js';
import { fileIcon, watchIcon, hideIcon } from './icons.js';
import { filesCell, filesCellsKeep } from './files.js';

// A station that keeps a view whole says so (project.rs, VIEW_CAP).
export const CAP = 'view_panes';
let env = null;
export function initViews(e) { env = e; }

// ------------------------------------------------------------- the leaf

const VIEW_ID = /^view\.([A-Za-z0-9]+)\.[A-Za-z0-9]+$/;
export const isView = (n) => !!n && typeof n.id === 'string' && n.id.startsWith('view.');
// 'files', 'watch', a kind from a later version, or 'other' for an id that names none.
export const viewKind = (n) => (VIEW_ID.exec(n.id) || [])[1] || 'other';
const text = (v) => (typeof v === 'string' && v ? v : null);
// A view as it is saved: what this version knows of it, and nothing else.
export const cleanView = (n) => ({ id: n.id, ...(text(n.station) ? { station: n.station } : {}), ...(text(n.folder) ? { folder: n.folder } : {}), ...(text(n.watch) ? { watch: n.watch } : {}), ...(n.hidden ? { hidden: true } : {}) });
const token = () => Array.from(crypto.getRandomValues(new Uint8Array(3)), b => b.toString(36).padStart(2, '0')).join('');
export const newView = (kind, more) => cleanView({ id: `view.${kind}.${token()}`, ...more });

const stationOf = (lay, n) => n.station || lay.station || null;
const projectOf = (lay, n) => n.watch || lay.project || null;
const project = (st, name) => env.projects(st).find(p => p.name === name) || null;
// The folder a Files view opens at. None: the station's home folder.
const folderOf = (lay, n) => text(n.folder) || (lay.project && stationOf(lay, n) === lay.station ? project(lay.station, lay.project)?.path : null) || null;
const lastPart = (path) => String(path).replace(/[\\/]+$/, '').split(/[\\/]/).pop() || String(path);
const samePath = (a, b) => !!a && !!b && a.replace(/(.)[\\/]$/, '$1') === b.replace(/(.)[\\/]$/, '$1');
const agentsOf = (st, name) => env.panesOf(st).filter(p => !p.exited && p.project === name && p.agent !== 'shell');
const word = (kind) => (KINDS[kind] || [kind])[0];

// What a view shows, in a word or two: a folder's own name, a project's.
export function viewName(lay, n) {
  const kind = viewKind(n);
  if (kind === 'files') { const f = folderOf(lay, n); return f ? lastPart(f) : 'home folder'; }
  if (kind === 'watch') return projectOf(lay, n) || 'no project';
  return 'a newer kind';
}
// Its name where panes are named: the kind first ("Files · out").
export const viewLabel = (lay, n) => `${word(viewKind(n))} · ${viewName(lay, n)}`;
// A dot is a state. A view has none of its own: a Watch has its agents'
// (the one that most needs the owner), and Files is idle.
export function viewState(lay, n) {
  if (viewKind(n) !== 'watch') return 'idle';
  const st = stationOf(lay, n), name = projectOf(lay, n);
  const states = st && name ? agentsOf(st, name).map(stateOf) : [];
  return ['needs_you', 'error', 'working'].find(s => states.includes(s)) || 'idle';
}
export const viewIcon = (kind) => (kind === 'watch' ? watchIcon() : fileIcon('folder'));

// ------------------------------------- where a Files cell was, in this browser

const PLACE = 'spandeck.view.at.';
function recall(key, st) {
  try { const v = JSON.parse(localStorage.getItem(PLACE + key) || 'null'); return v && v.station === st && text(v.path) ? v.path : null; } catch (e) { return null; }
}
function remember(key, st, path) { try { localStorage.setItem(PLACE + key, JSON.stringify({ station: st, path })); } catch (e) { /* not kept: it opens at its folder */ } }
function forget(key) { try { localStorage.removeItem(PLACE + key); } catch (e) { /* nothing was kept */ } }

// ----------------------------------------------------------------- the cells

// A Files cell stays what it is while the page is drawn again around it
// (the listing, what is selected, a name being typed): one per layout and
// leaf, let go when it has left the page.
const cells = new Map(); // "layout|leaf" -> { key, c, wrap, headEl, wanted, tried, st, base, ctx, n }
let seen = new Set(), kept = [];
export function viewsBeforeRender() {
  seen = new Set();
  kept = [];
  for (const x of cells.values()) if (x.wrap.isConnected) kept.push({ x, top: x.c.view.tree.scrollTop, had: x.wrap.contains(document.activeElement) ? document.activeElement : null });
}
export function viewsAfterRender() {
  for (const key of [...cells.keys()]) if (!seen.has(key)) cells.delete(key);
  filesCellsKeep(seen);
  // A cell is put back where it was: scrolled as it was, and with the
  // keyboard if it had it and nothing took it meanwhile.
  for (const k of kept) {
    if (!k.x.wrap.isConnected) continue;
    k.x.c.view.tree.scrollTop = k.top;
    const a = document.activeElement;
    if (k.had && k.had.isConnected && (!a || a === document.body) && !document.querySelector('.dialog, .menu')) k.had.focus({ preventScroll: true });
  }
}

// What every view's head ends with: Hide, and its menu.
function headEnd(x, where, menu) {
  const { ctx, n } = x, name = viewLabel(ctx.lay, n);
  const open = (e) => { const b = e.currentTarget.getBoundingClientRect(); menu(b.left, b.bottom + 2); };
  return [
    h('span', { class: 'grow' }),
    where === 'leaf' && ctx.shown().length > 1 ? h('button', { type: 'button', class: 'icon-btn strip-hide', dataset: { keep: 'view-hide:' + n.id }, title: 'Hide this pane. It stays as a chip beside the layout\'s name.', 'aria-label': `Hide ${name}`, onclick: () => ctx.hide(n, true) }, hideIcon()) : null,
    h('button', { type: 'button', class: 'icon-btn view-menu', dataset: { keep: 'view-menu:' + n.id }, title: 'Pane menu', 'aria-label': `Menu for ${name}`, 'aria-haspopup': 'menu', onclick: open }, '⋮'),
  ];
}
// What a view's menu ends with, as a pane's does.
function menuEnd(ctx, n) {
  const live = ctx.live(n), seenNow = ctx.shown().length;
  if (!live) return [];
  return ['-',
    !live.hidden && { label: 'Hide', hint: seenNow < 2 ? 'the only pane showing' : 'it stays as a chip', disabled: seenNow < 2, action: () => ctx.hide(live, true) },
    live.hidden && { label: 'Show', hint: 'where it was', action: () => ctx.hide(live, false) },
    (live.hidden || seenNow > 1) && { label: 'Show only this', hint: 'hides the others', action: () => ctx.only(live) },
    { label: 'Take out of this layout', hint: 'nothing is deleted or stopped', action: () => { ctx.remove(ctx.refOf(live)); env.render(); } }];
}
const guestChip = (ctx, n) => { const g = ctx.guest(n); return g ? h('span', { class: 'chip guest', title: `This view is ${g}` }, g) : null; };
// Change a view in its layout, and save it.
function change(ctx, n, set) {
  const live = ctx.live(n);
  if (!live) return;
  set(live);
  ctx.changed();
  env.render();
}

// The cell of a view: `where` is 'leaf' (a layout on a computer) or 'phone'
// (the layout stepped through, where the view has the screen).
export function viewCell(ctx, n, where) {
  const kind = viewKind(n), st = stationOf(ctx.lay, n);
  if (kind !== 'files' && kind !== 'watch') return plainCell(ctx, n, where, kind, h('div', { class: 'empty' },
    h('p', {}, `This pane is of a kind (${kind}) that this version of spandeck does not show. It is kept in the layout as it is: a station on a newer version shows it.`)));
  // Saved without its station (through the API): it is not guessed.
  if (!st) return plainCell(ctx, n, where, kind, h('div', { class: 'empty' },
    h('p', {}, `${viewLabel(ctx.lay, n)} was saved in this layout without its station. Say which station it shows.`),
    env.stations().filter(s => s.state === 'online').map(s => h('button', { type: 'button', onclick: () => change(ctx, n, (live) => { live.station = s.station; }) }, `It is ${s.station}`))));
  return kind === 'files' ? filesView(ctx, n, where, st) : watchCell(ctx, n, where, st);
}
function plainCell(ctx, n, where, kind, body) {
  const x = { ctx, n };
  return h('section', { class: 'view-cell view-' + (KINDS[kind] ? kind : 'other'), dataset: { view: n.id }, 'aria-label': viewLabel(ctx.lay, n) },
    h('div', { class: 'view-head' }, kindTag(kind), h('b', { class: 'view-name' }, h('bdi', {}, viewName(ctx.lay, n))), guestChip(ctx, n),
      headEnd(x, where, (px, py) => showMenu(px, py, [{ title: viewLabel(ctx.lay, n) }, ...menuEnd(ctx, n).slice(1)]))), body);
}

// ---- Files ----

function filesView(ctx, n, where, st) {
  const lay = ctx.lay, key = `${lay.key}|${n.id}`;
  seen.add(key);
  let x = cells.get(key);
  if (!x) {
    x = { key, headEl: h('div', { class: 'view-head' }), wanted: null, tried: 0 };
    // Where it was browsed to is kept in this browser; at its own folder, nothing is.
    x.c = filesCell(key, { onGo: (path) => {
      const base = x.base || x.c.view.home?.home;
      if (samePath(path, base)) forget(key); else remember(key, x.st, path);
      drawFilesHead(x);
    } });
    x.wrap = h('section', { class: 'view-cell view-files', dataset: { view: n.id } }, x.headEl, x.c.el);
    cells.set(key, x);
  }
  Object.assign(x, { ctx, n, st, where, base: folderOf(lay, n) });
  x.wrap.setAttribute('aria-label', viewLabel(lay, n) + ' on ' + st);
  // Shown at the place it was in this browser, else at its folder. A station
  // that did not answer is asked again when it does; a place that is not
  // there any more is forgotten.
  const view = x.c.view, want = `${st}\n${x.base || ''}`;
  const can = () => env.online(st) && env.hasCap(st, 'files');
  const again = !!view.error && !view.root && !view.loading && can() && Date.now() - x.tried > 3000;
  if (x.wanted !== want || again) {
    x.wanted = want;
    x.tried = Date.now();
    const at = recall(key, st);
    view.show(st, at || x.base).then(() => { if (at && view.error && x.wanted === want && can()) { forget(key); view.show(st, x.base); } });
  }
  drawFilesHead(x);
  return x.wrap;
}
function drawFilesHead(x) {
  const { ctx, n, st, base, where } = x, view = x.c.view, name = viewName(ctx.lay, n);
  const guest = ctx.guest(n), away = !!view.root && !samePath(view.root, base || view.home?.home);
  const sig = JSON.stringify([n.id, name, base, st, guest, away, where, ctx.shown().length > 1, !!n.hidden]);
  if (sig === x.headSig) return;
  x.headSig = sig;
  const menu = (px, py) => showMenu(px, py, [
    { title: `Files · ${base || 'home folder'} on ${st}` },
    { label: 'Go to its folder', hint: base ? lastPart(base) : 'home', disabled: !view.home || !away, action: () => view.go(base || view.home.home) },
    { label: 'Open here from now on', hint: 'on every device', disabled: !away, action: () => { const to = view.root; forget(x.key); change(x.ctx, x.n, (live) => { live.folder = to; }); } },
    { label: 'Station and folder…', action: () => filesDialog(x.ctx, x.n) },
    ...menuEnd(x.ctx, x.n)]);
  x.headEl.replaceChildren(...[
    kindTag('files'),
    h('b', { class: 'view-name', title: `Opens at ${base || 'the home folder'} on ${st}` }, h('bdi', {}, name)),
    guest ? h('span', { class: 'chip guest', title: `This view is ${guest}` }, guest) : null,
    away ? h('button', { type: 'button', class: 'mini view-back', title: `Back to ${base || 'the home folder'}`, onclick: () => view.go(base || view.home.home) }, h('span', {}, 'Back to ', h('bdi', {}, base ? lastPart(base) : 'home'))) : null,
    ...headEnd(x, where, menu)].filter(Boolean));
}

// A station's folder, typed: asked of the station before it is taken.
async function folderThere(st, path) {
  const got = await env.api.on(st, 'files.list', { path }, { within: 8000 });
  return got.path || path;
}
const canFiles = (s) => s.state === 'online' && (s.caps || []).includes('files');
const stationPick = (stations, chosen, label) => h('select', { 'aria-label': label }, stations.map(s => h('option', { value: s.station, selected: s.station === chosen }, s.station)));
// The fields of a Files view, for a new one and for one being changed.
function filesFields(lay, st0, folder0) {
  const stations = env.stations().filter(canFiles);
  const station = stationPick(stations, stations.some(s => s.station === st0) ? st0 : stations[0]?.station, 'Station');
  const folder = h('input', { class: 'mono', value: folder0 || '', spellcheck: false, autocapitalize: 'off', autocomplete: 'off' });
  const hint = () => (lay.project && station.value === lay.station ? `Empty: ${lay.project}'s folder.` : 'Empty: the home folder on that station.');
  const small = h('small', {}, hint());
  station.addEventListener('change', () => { small.textContent = hint(); });
  const err = h('p', { class: 'fx-move-err', role: 'alert' });
  return {
    none: !stations.length, err,
    rows: [field('Station', station), h('label', { class: 'field' }, h('span', {}, 'Folder'), folder, small), err],
    // { station, folder } as they go on the leaf, or null with the reason shown.
    async value() {
      err.textContent = '';
      const st = station.value, typed = folder.value.trim();
      let path = null;
      if (typed) { try { path = await folderThere(st, typed); } catch (e) { err.textContent = `${st} did not show that folder: ${e.message || 'it did not answer'}`; return null; } }
      return { station: st === lay.station ? undefined : st, folder: path || undefined };
    },
  };
}
function filesDialog(ctx, n) {
  const lay = ctx.lay;
  dialog('Files: station and folder', (close) => {
    const f = filesFields(lay, stationOf(lay, n), text(n.folder));
    const submit = async () => {
      const v = await f.value();
      if (!v) return;
      forget(`${lay.key}|${n.id}`);
      change(ctx, n, (live) => { delete live.station; delete live.folder; Object.assign(live, cleanView({ ...live, ...v })); });
      close();
    };
    return h('form', { onsubmit: (e) => { e.preventDefault(); submit(); } }, f.rows,
      h('p', { class: 'muted small' }, 'This is where the pane opens, on every device. Browsing in it changes nothing here.'),
      h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Cancel'), h('button', { class: 'primary', type: 'submit' }, 'Save')));
  });
}

// ---- Watch ----

function watchCell(ctx, n, where, st) {
  const lay = ctx.lay, name = projectOf(lay, n), x = { ctx, n };
  const agents = name ? agentsOf(st, name) : [], needs = agents.filter(p => stateOf(p) === 'needs_you' || stateOf(p) === 'error').length;
  const menu = (px, py) => showMenu(px, py, [
    { title: `Watch · ${name || 'no project'} on ${st}` },
    name && { label: 'Open Watch full size', action: () => env.go(env.watchUrl(st, name)) },
    { label: 'Station and project…', action: () => watchDialog(ctx, n) },
    ...menuEnd(ctx, n)]);
  const body = !name ? h('div', { class: 'empty' }, h('p', {}, 'This Watch was saved without its project.'), h('button', { type: 'button', onclick: () => watchDialog(ctx, n) }, 'Choose a project'))
    : !env.online(st) ? h('div', { class: 'empty' }, h('p', {}, `${st} is unreachable, so its agents cannot be shown.`))
      : !project(st, name) ? h('div', { class: 'empty' }, h('p', {}, env.loaded(st) ? `There is no project ${name} on ${st}.` : 'Loading…'))
        : env.watchView(st, name, { at: n.id, open: (p) => env.toPane(lay, st, p) });
  return h('section', { class: 'view-cell view-watch', dataset: { view: n.id }, 'aria-label': `${viewLabel(lay, n)} on ${st}` },
    h('div', { class: 'view-head' }, kindTag('watch'), h('b', { class: 'view-name', title: `The agents in ${name || '?'} on ${st}` }, h('bdi', {}, name || 'no project')), guestChip(ctx, n),
      name ? h('span', { class: 'muted view-count' }, `${agents.length} agent${agents.length === 1 ? '' : 's'}`) : null,
      needs ? h('span', { class: 'chip needs', role: 'status' }, `${needs} need${needs === 1 ? 's' : ''} you`) : null,
      headEnd(x, where, menu)),
    body);
}
function watchFields(lay, st0, name0) {
  const stations = env.stations().filter(s => s.state === 'online');
  const station = stationPick(stations, stations.some(s => s.station === st0) ? st0 : stations[0]?.station, 'Station');
  const proj = h('select', { 'aria-label': 'Project' });
  const fill = () => { const list = env.projects(station.value); proj.replaceChildren(...list.map(p => h('option', { value: p.name, selected: p.name === name0 }, p.name))); };
  station.addEventListener('change', fill);
  fill();
  return {
    rows: [field('Station', station), field('Project', proj, 'Its agents, and the agents of the projects linked with it.')],
    value() {
      if (!proj.value) { toast(`${station.value || 'That station'} has no project to watch.`, 'error'); return null; }
      return { station: station.value === lay.station ? undefined : station.value, watch: station.value === lay.station && proj.value === lay.project ? undefined : proj.value };
    },
  };
}
function watchDialog(ctx, n) {
  const lay = ctx.lay;
  dialog('Watch: station and project', (close) => {
    const f = watchFields(lay, stationOf(lay, n), projectOf(lay, n));
    const submit = () => {
      const v = f.value();
      if (!v) return;
      change(ctx, n, (live) => { delete live.station; delete live.watch; Object.assign(live, cleanView({ ...live, ...v })); });
      close();
    };
    return h('form', { onsubmit: (e) => { e.preventDefault(); submit(); } }, f.rows,
      h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Cancel'), h('button', { class: 'primary', type: 'submit' }, 'Save')));
  });
}

// ------------------------------------------------- adding a pane, by its kind

// First what kind it is, then what that kind needs. An agent and a shell
// are what they were: a pane that is running, or the New pane dialog.
export function addPaneDialog(lay) {
  const where = lay.project ? `${lay.project} on ${lay.station}` : 'A global layout: the same on every station';
  // A view is kept whole only by a station that says so. A project's layout
  // is on one station; a global one is on all of them.
  const fleet = env.stations().filter(s => s.state !== 'not_linked');
  const behind = (lay.project ? fleet.filter(s => s.station === lay.station) : fleet).filter(s => !(s.caps || []).includes(CAP)).map(s => s.station);
  const noViews = lay.project && behind.length ? `${lay.station} runs an older spandeck, which cannot keep this in a layout. Update it first.` : '';
  let kind = 'agent';
  dialog(`Add a pane to ${lay.name}`, (close, body) => {
    const show = (el) => { body.replaceChildren(el); (el.querySelector('input:checked') || el.querySelector('input, select, button'))?.focus(); };
    const put = (leaf) => { close(); env.addLeaf(lay, leaf, 'row'); env.go(env.layUrl(lay)); };
    const foot = (back, next) => h('div', { class: 'row end' }, back ? h('button', { type: 'button', class: 'add-back', onclick: () => show(first()) }, '‹ Back') : null, h('span', { class: 'grow' }),
      h('button', { type: 'button', onclick: close }, 'Cancel'), next);

    const first = () => {
      const card = (value, tags, title, says, off) => h('label', { class: 'kind-card' + (off ? ' off' : '') },
        h('input', { type: 'radio', name: 'pane-kind', value, checked: kind === value && !off, disabled: !!off, onchange: () => { kind = value; } }),
        h('span', { class: 'kind-card-tags' }, tags.map(kindTag)),
        h('span', { class: 'kind-card-text' }, h('b', {}, title), h('small', { class: 'muted' }, off || says)));
      if (noViews && (kind === 'files' || kind === 'watch')) kind = 'agent';
      return h('form', { class: 'add-pane', onsubmit: (e) => { e.preventDefault(); show(details[kind]()); } },
        h('p', { class: 'muted' }, `${where} · the pane goes beside the one you are in`),
        h('fieldset', { class: 'kind-group' }, h('legend', {}, 'Runs a program'),
          card('agent', ['chat', 'term'], 'An agent', 'Claude Code, Codex and the others. Shown as Chat or as its terminal, one tap apart.'),
          card('shell', ['term'], 'A shell', lay.project ? 'A terminal in the project\'s folder.' : 'A terminal on a station.')),
        h('fieldset', { class: 'kind-group' }, h('legend', {}, 'Shows something, and runs nothing'),
          card('files', ['files'], 'Files', `The file browser, opened at a folder ${lay.project ? 'of this project' : 'of a station'}. Drag in and out as in the panel.`, noViews),
          card('watch', ['watch'], 'Watch', 'Every agent of a project: what it is doing, who needs you, who is talking to whom.', noViews)),
        !lay.project && behind.length ? h('p', { class: 'muted small add-behind' }, `${behind.join(', ')} ${behind.length === 1 ? 'runs' : 'run'} an older spandeck: a deck opened there shows Files and Watch as a closed pane, and a change to the global layouts made there puts one back at its first folder or project.`) : null,
        foot(false, h('button', { class: 'primary', type: 'submit' }, 'Next')));
    };

    // A pane that runs a program: one that is running, or a new one.
    const program = (shell) => () => {
      const running = env.stations().filter(s => s.state === 'online').flatMap(s => env.panesOf(s.station).filter(p => !p.exited && (p.agent === 'shell') === shell && !env.has(lay, s.station, p)).map(p => ({ st: s.station, p })));
      const stations = env.stations().filter(s => s.state === 'online');
      const st0 = lay.station || env.api.station;
      const station = stationPick(stations, stations.some(s => s.station === st0) ? st0 : stations[0]?.station, 'Station to start it on');
      const start = () => {
        const st = station.value;
        close();
        env.newPaneDialog(st, st === lay.station ? lay.project : undefined, async (id) => {
          await env.refresh();
          const p = env.findPane(st, id);
          if (p) env.addToLayout(lay, st, p, 'row');
          env.go(env.layUrl(lay));
        }, { agent: shell ? 'shell' : undefined });
      };
      return h('div', { class: 'add-pane' },
        h('h3', {}, shell ? 'A shell that is running' : 'An agent that is running'),
        running.length ? h('div', { class: 'list' }, running.map(({ st, p }) => h('button', { type: 'button', class: 'list-row', onclick: () => { close(); env.addToLayout(lay, st, p, 'row'); env.go(env.layUrl(lay)); } },
          dot(p), env.paneMark(p), h('b', {}, p.name), h('span', { class: 'muted' }, `${st}${p.project ? ' · ' + p.project : ''}`))))
          : h('p', { class: 'muted' }, shell ? 'Every running shell is already in this layout.' : 'Every running agent is already in this layout.'),
        h('h3', {}, 'Or a new one'),
        h('div', { class: 'row add-new' }, stations.length > 1 || !lay.station ? station : null, h('button', { type: 'button', class: 'add-start', onclick: start }, shell ? 'Start a new shell…' : 'Start a new agent…')),
        foot(true, null));
    };
    const details = {
      agent: program(false),
      shell: program(true),
      files: () => {
        const f = filesFields(lay, lay.station || env.api.station, '');
        const add = h('button', { class: 'primary', type: 'submit', disabled: f.none }, 'Add');
        const submit = async () => { add.disabled = true; const v = await f.value(); add.disabled = false; if (v) put(newView('files', v)); };
        return h('form', { class: 'add-pane', onsubmit: (e) => { e.preventDefault(); submit(); } },
          f.none ? h('p', { class: 'warn-text' }, 'No station that answers has the file explorer.') : f.rows,
          h('p', { class: 'muted small' }, 'Everything the Files panel does works in the pane. It opens here on every device; where you browse to in it is kept by this browser.'),
          foot(true, add));
      },
      watch: () => {
        const f = watchFields(lay, lay.station || env.api.station, lay.project);
        const submit = () => { const v = f.value(); if (v) put(newView('watch', v)); };
        return h('form', { class: 'add-pane', onsubmit: (e) => { e.preventDefault(); submit(); } }, f.rows,
          h('p', { class: 'muted small' }, 'A tap on an agent goes to its pane: in this layout if it is here.'),
          foot(true, h('button', { class: 'primary', type: 'submit' }, 'Add')));
      },
    };
    return first();
  });
}

// ---------------------------------------------------------- the key to the dots

export function dotKeyDialog() {
  dialog('What the dots mean', (close) => h('div', { class: 'dot-key' },
    h('p', { class: 'muted' }, 'A dot\'s colour is the state of a pane or a station. It never says what kind of pane it is.'),
    h('ul', { class: 'dot-key-list' }, DOT_KEY.map(([cls, looks, says]) => h('li', {}, h('span', { class: 'dot ' + cls, 'aria-hidden': 'true' }), h('b', {}, looks), h('span', {}, says)))),
    h('p', { class: 'muted' }, 'The kind is the word at the end of a row: ', Object.keys(KINDS).map(k => [kindTag(k), ' ']), '.'),
    h('div', { class: 'row end' }, h('button', { class: 'primary', type: 'button', onclick: close }, 'Done'))));
}

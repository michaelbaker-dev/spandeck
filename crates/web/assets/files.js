// The file explorer: a panel at the right of the window (a full-screen sheet
// on a phone) that shows the files of a station (DECISIONS.md, "The files of
// a station", "The panel").
//
// The rules it keeps:
//   - Browsing never moves a pane. Only "Move … here" does, after asking.
//   - Between stations files are copied, never moved.
//   - A file's name and what it holds are data: put on the page as text,
//     never as HTML. A preview is text in a box, an image the station
//     itself says is a PNG, JPEG, GIF or WebP, or a film or a sound the
//     station itself says is one, in the browser's own player.
//   - What a pane is working in is not deleted, renamed or moved: the
//     station refuses it, and the page names the panes and offers no way
//     round it.
//   - A station that is slow or does not answer is said in its own listing;
//     the other listing and the rest of the deck go on.
// Everything is asked through the calls the page already makes (`api.on`);
// nothing here reaches a station another way, except a download, which is
// a link the station gave a ticket for.

import { h, showMenu, closeMenu, toast, dialog, confirmBox, copyText } from './ui.js';
import { fileIcon, fileKind } from './icons.js';

let env = null;
let panel = null;
export function initFiles(e) { env = e; }
// Open the panel at a pane's folder, on that pane's station; with no pane,
// at the station's home folder.
export function openFiles(station, pane) {
  if (!panel) panel = new Panel();
  panel.open(station || env.api.station, pane || null);
}
// The page was drawn again: the pane the panel was opened for may have
// another folder or name, and the fleet other stations.
export function filesSync() { if (panel && (panel.shown || panel.cells.size)) panel.sync(); }

// A listing in a cell of a layout (viewpanes.js; DECISIONS.md, "A layout
// holds more than programs"): the same listing the panel shows, with the
// page's one clipboard, its uploads and its jobs, so a file copied in the
// panel is pasted in a cell and the other way round. `key` names the cell;
// asked for again, the same cell comes back as it was. `opts.onGo(path)`
// hears of each folder shown. The cell is `{ el, view }`: `el` goes on the
// page, `view.show(station, path)` says what it lists.
export function filesCell(key, opts = {}) {
  if (!panel) panel = new Panel();
  return panel.cell(key, opts);
}
// The cells still on the page are `keep`; the others are let go.
export function filesCellsKeep(keep) {
  if (panel) for (const key of [...panel.cells.keys()]) if (!keep.has(key)) panel.dropCell(key);
}
// A name is being typed in a cell, or something is being dragged: the page
// is not drawn again under it.
export function filesBusy() {
  return !!panel && (!!panel.drag || [...panel.cells.values()].some(c => c.el.isConnected && (c.view.draft || c.view.renaming || c.view.pathEdit)));
}

// A file's preview, for the rest of the page (Chat shows the files an
// agent sent with it): the same dialog the panel's listing opens. `known`
// is what is known of the file already (`size`, `mtime_ms`, `kind`).
export function previewFile(station, path, name, known = {}) {
  if (!panel) panel = new Panel();
  showPreview(panel, station || env.api.station, String(path), String(name || path), known || {});
}
// A link to one file of a station, as the panel asks for one: the page's
// station gives it, for that station's file or another's.
export function fileTicket(station, what) {
  if (!panel) panel = new Panel();
  return panel.ticket(station || env.api.station, what);
}
export function downloadFile(station, path, name) {
  if (!panel) panel = new Panel();
  return panel.downloadFile(station || env.api.station, String(path), String(name || path));
}
// What a preview shows a file as, by the end of its name. (What it is is
// decided by the station, from its first bytes: a name is only a guess at
// which element to ask with.)
export const previewKind = (name) => (INLINE.test(name) ? 'image' : VIDEO.test(name) ? 'video' : AUDIO.test(name) ? 'audio' : 'other');

const KIB = 1024, MIB = 1024 * 1024;
const PREVIEW_MAX = 256 * KIB;
const RELIST_MS = 4000, JOBS_MS = 1000, TICKET_MS = 45000;
const ROWS_STEP = 300;
const INLINE = /\.(png|jpe?g|gif|webp)$/i;
const VIDEO = /\.(mp4|m4v|mov|webm)$/i, AUDIO = /\.(mp3|m4a|wav|ogg)$/i;
const NOT_PLAYED = 'This cannot be played here. Download it to open it.';

const store = {
  get(key, otherwise) { try { const v = localStorage.getItem('spandeck.files.' + key); return v === null ? otherwise : v; } catch (e) { return otherwise; } },
  set(key, value) { try { localStorage.setItem('spandeck.files.' + key, String(value)); } catch (e) { /* not kept: the panel works without */ } },
};
const sleep = (ms) => new Promise(r => setTimeout(r, ms));
const hex = (n) => Array.from(crypto.getRandomValues(new Uint8Array(n)), b => b.toString(16).padStart(2, '0')).join('');
const plural = (n, one, many = one + 's') => `${n} ${n === 1 ? one : many}`;
const items = (n) => plural(n, 'item');
// What went wrong, in the station's own sentence. Never an object's text.
const say = (e) => (e && typeof e.message === 'string' && e.message) || 'it did not work';
const ico = (name, cls = '') => h('span', { class: 'fx-ico ' + cls }, fileIcon(name));
// (replaceChildren would print a missing piece as the word "null".)
const fill = (el, ...kids) => el.replaceChildren(...kids.flat().filter(k => k !== null && k !== undefined && k !== false));
// How deep a row is in the tree (a custom property is not set as a style's own field is).
const deep = (el, depth) => { el.style.setProperty('--d', String(depth)); return el; };
// A path's last part and the folder it is in, by its station's separator.
const baseOf = (path, sep) => { const cut = path.length > 1 && path.endsWith(sep) ? path.slice(0, -1) : path; return cut.slice(cut.lastIndexOf(sep) + 1) || path; };
const trimSep = (path, sep) => (path.length > sep.length && path.endsWith(sep) ? path.slice(0, -sep.length) : path);
const parentOf = (path, sep) => { const cut = trimSep(path, sep), at = cut.lastIndexOf(sep); return at < 0 ? '' : trimSep(cut.slice(0, at + sep.length), sep); };

export function fmtSize(n) {
  n = Number(n) || 0;
  if (n < KIB) return n + ' B';
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / KIB, u = 0;
  while (v >= 1024 && u < units.length - 1) { v /= 1024; u++; }
  return (v >= 100 ? Math.round(v) : v.toFixed(1).replace(/\.0$/, '')) + ' ' + units[u];
}
function fmtWhen(ms) {
  if (!ms) return '';
  const d = new Date(Number(ms)), now = new Date();
  return d.getFullYear() === now.getFullYear()
    ? d.toLocaleString([], { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' })
    : d.toLocaleDateString([], { day: 'numeric', month: 'short', year: 'numeric' });
}
function fmtAge(ms) {
  if (!ms) return '';
  const s = Math.max(0, (Date.now() - Number(ms)) / 1000);
  if (s < 90) return 'just now';
  if (s < 3600) return Math.round(s / 60) + ' min ago';
  if (s < 86400) return Math.round(s / 3600) + ' h ago';
  if (s < 86400 * 60) return Math.round(s / 86400) + ' d ago';
  return fmtWhen(ms);
}
async function toB64(blob) {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let bin = '';
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}
function fromB64(text) {
  const bin = atob(text || ''), out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// What only looks (a listing, a job's progress): at most two questions on
// their way to one station at a time, each given ten seconds. A station that
// does not answer so holds up nothing but its own listing.
// A text file as text, an image as an image, a film or a sound in the
// browser's own player, anything else as what is known of it. Nothing of
// the file is ever put on the page as HTML: a picture, a film and a sound
// are bytes the station sent as that type because their first bytes say
// so, asked for by a link it gave a ticket for.
function showPreview(P, station, path, name, e) {
  let playing = null;
  dialog(name, (close, body) => {
    const facts = h('div', { class: 'fx-facts muted' }, [e.kind === 'link' ? 'link' : 'file', typeof e.size === 'number' ? fmtSize(e.size) : null, e.mtime_ms ? 'changed ' + fmtWhen(e.mtime_ms) : null, `on ${station}`].filter(Boolean).join(' · '));
    const where = h('div', { class: 'fx-where mono muted' }, h('bdi', {}, path));
    const box = h('div', { class: 'fx-preview' }, h('p', { class: 'muted' }, 'Reading…'));
    const foot = h('div', { class: 'row end' },
      h('button', { type: 'button', onclick: () => copyText(path, 'Copied the path') }, 'Copy path'),
      h('button', { type: 'button', class: 'primary fx-preview-get', onclick: () => P.downloadFile(station, path, name) }, 'Download'));
    const plain = (text) => { playing = null; box.replaceChildren(h('p', { class: 'muted fx-preview-note' }, text)); };
    const ticket = () => P.ticket(station, { path, inline: true });
    // A film or a sound. The player asks for part after part, and each
    // use keeps its link good; one that ran out while it was paused for
    // long is asked for again, once, and the film goes on from where it
    // was. What still does not play is said plainly.
    const play = async (video) => {
      let t;
      try { t = await ticket(); } catch (err) { return plain(say(err)); }
      const el = video
        ? h('video', { class: 'fx-media fx-video', controls: true, playsInline: true, preload: 'metadata', 'aria-label': `Video: ${name}` })
        : h('audio', { class: 'fx-media fx-audio', controls: true, preload: 'metadata', 'aria-label': `Sound: ${name}` });
      if (video) el.setAttribute('playsinline', '');
      let at = 0, again = false;
      el.addEventListener('timeupdate', () => { if (el.currentTime > 0) at = el.currentTime; });
      el.addEventListener('loadedmetadata', () => { again = false; });
      el.addEventListener('error', async () => {
        if (again) return plain(NOT_PLAYED);
        again = true;
        const was = at;
        let fresh;
        try { fresh = await ticket(); } catch (err) { return plain(NOT_PLAYED); }
        if (playing !== el) return;
        el.addEventListener('loadedmetadata', () => { if (was > 0) { try { el.currentTime = was; } catch (err) { /* from the start */ } } }, { once: true });
        el.src = fresh.url;
        el.load();
      });
      el.src = t.url;
      playing = el;
      box.replaceChildren(el);
    };
    (async () => {
      const kind = previewKind(name);
      if (kind === 'image') {
        let t;
        try { t = await ticket(); } catch (err) { return plain(say(err)); }
        const img = h('img', { class: 'fx-img', alt: `Picture: ${name}`, src: t.url });
        img.addEventListener('error', () => plain('This picture cannot be shown here. Download it to see it.'));
        return box.replaceChildren(img);
      }
      if (kind === 'video' || kind === 'audio') return play(kind === 'video');
      let got;
      try { got = await look(station, 'files.read', { path, offset: 0, len: PREVIEW_MAX }); } catch (err) { return plain(say(err)); }
      const bytes = fromB64(got.data_b64);
      const text = new TextDecoder('utf-8').decode(bytes);
      const odd = (text.match(/�/g) || []).length;
      if (bytes.includes(0) || odd > Math.max(2, text.length / 100)) return plain('This file is not text: there is nothing to show of it here. Download it to open it.');
      if (!bytes.length) return plain('This file is empty.');
      fill(box,
        h('pre', { class: 'fx-text mono', tabIndex: 0, 'aria-label': `What ${name} holds` }, text),
        got.eof ? null : h('p', { class: 'muted fx-preview-note' }, `Cut off: the first ${fmtSize(bytes.length)} of ${fmtSize(got.size)}. Download it for the rest.`));
    })();
    return h('div', { class: 'fx-preview-wrap' }, facts, where, box, foot);
    // (A player taken off the page would play on: it is stopped.)
  }, { wide: true, onClose: () => { if (playing) { try { playing.pause(); playing.removeAttribute('src'); playing.load(); } catch (err) { /* gone */ } playing = null; } } });
}

const lanes = new Map();
function look(station, method, params = {}, within = 10000) {
  return new Promise((resolve, reject) => {
    let lane = lanes.get(station);
    if (!lane) lanes.set(station, lane = { busy: 0, waiting: [] });
    const run = () => {
      lane.busy++;
      env.api.on(station, method, params, { within }).then(resolve, reject).finally(() => { lane.busy--; const next = lane.waiting.shift(); if (next) next(); });
    };
    if (lane.busy < 2) run(); else lane.waiting.push(run);
  });
}
// A change. It is not given up from this side: the station answers it once.
const change = (station, method, params) => env.api.on(station, method, params);
const own = (station) => !station || station === env.api.station;
const stations = () => env.stations().slice().sort((a, b) => a.station.localeCompare(b.station));
const canFiles = (s) => s && s.state === 'online' && (s.caps || []).includes('files');
const NEEDS = (st) => `${st} does not have the file explorer yet: it needs updating to this version of spandeck.`;

// Replace / Keep both / Skip. `all`: the tick "for all of this upload".
function askClash(title, text, all) {
  return new Promise((resolve) => {
    let answered = false;
    const tick = h('input', { type: 'checkbox', class: 'fx-all', style: { width: 'auto' } });
    const pick = (choice, close) => () => { answered = true; close(); resolve({ choice, all: !!tick.checked }); };
    dialog(title, (close) => h('div', { class: 'fx-clash' },
      h('p', {}, text),
      all ? h('label', { class: 'row' }, tick, 'Do the same for every other name that is taken') : null,
      h('div', { class: 'row end' },
        h('button', { type: 'button', onclick: pick('skip', close) }, 'Skip'),
        h('button', { type: 'button', onclick: pick('keep_both', close) }, 'Keep both'),
        h('button', { type: 'button', class: 'primary danger', onclick: pick('replace', close) }, 'Replace'))),
    { onClose: () => { if (!answered) resolve({ choice: 'skip', all: false }); } });
  });
}

// "builder and critic (project tally), and sh (ended, not closed)": the
// panes a station says are in a folder.
function paneNames(panes) {
  const and = (list, last) => (list.length < 2 ? list.join('') : list.slice(0, -1).join(', ') + last + list[list.length - 1]);
  const groups = new Map();
  for (const p of panes) {
    const key = p.project || '';
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(String(p.name) + (p.ended ? ' (ended, not closed)' : ''));
  }
  return and([...groups].map(([project, names]) => and(names, ' and ') + (project ? ` (project ${project})` : '')), ', and ');
}
// Nothing in use is deleted, renamed or moved: the station refused these
// (`in_use`), and names the panes. There is no way to do it anyway from
// here: the panes are closed first. `verb`: delete, rename or move.
// `done`: what did go ahead in the same request, said first.
function inUseNotice(verb, refused, sep, done) {
  const one = refused.length === 1 ? baseOf(refused[0].path, sep) : null;
  dialog(one ? `${one} is in use` : `${refused.length} folders are in use`, (close) => h('div', { class: 'fx-inuse' },
    done ? h('p', { class: 'fx-inuse-done' }, done) : null,
    refused.slice(0, 20).map(f => {
      const panes = f.panes || [], many = panes.length !== 1;
      return h('p', { class: 'fx-inuse-item' },
        panes.length ? `${paneNames(panes)} ${many ? 'are' : panes[0].ended ? 'is still open' : 'is working'} in ` : 'A pane is working in ',
        h('bdi', { class: 'mono' }, baseOf(f.path, sep)), ' or a folder inside it. ',
        `Close ${many ? 'those panes' : 'that pane'}, then ${verb} it.`);
    }),
    refused.length > 20 ? h('p', { class: 'muted' }, `and ${refused.length - 20} more`) : null,
    h('p', { class: 'muted fx-inuse-why' }, `Nothing a pane is working in is ${verb === 'delete' ? 'deleted' : verb === 'rename' ? 'renamed' : 'moved'}.`),
    h('div', { class: 'row end' }, h('button', { type: 'button', class: 'primary fx-inuse-ok', onclick: close }, 'OK'))));
}

// ------------------------------------------------------------- one listing

let listings = 0;
class Listing {
  // `opts.fixed`: its station is not chosen in its own bar (a cell of a
  // layout says its station). `opts.onGo(path)`: told of each folder shown.
  constructor(owner, opts = {}) {
    this.panel = owner;
    this.opts = opts;
    this.uid = ++listings;
    this.station = null;
    this.home = null;         // files.home of the station
    this.root = null;         // the folder shown
    this.dirs = new Map();    // path -> {entries, total, truncated, parent} | {error}
    this.expanded = new Set();
    this.sel = new Set();
    this.anchor = null;
    this.cursor = null;
    this.limit = ROWS_STEP;
    this.error = null;        // the listing as a whole could not be shown
    this.loading = false;
    this.busy = false;        // a round of re-listing is on its way
    this.draft = null;        // {kind: 'dir' | 'file', error} a new name being typed
    this.renaming = null;     // {path, error}
    this.pathEdit = null;     // {value, error} the path being typed
    this.selecting = false;   // tick boxes (a phone)
    this.seq = 0;
    this.flat = [];
    this.build();
  }

  build() {
    this.barEl = h('div', { class: 'fx-bar' });
    this.pathEl = h('div', { class: 'fx-path' });
    this.toolsEl = h('div', { class: 'fx-tools', role: 'toolbar', 'aria-label': 'File actions' });
    this.colsEl = h('div', { class: 'fx-cols', 'aria-hidden': 'true' }, h('span', { class: 'fx-c-name' }, 'Name'), h('span', { class: 'fx-size' }, 'Size'), h('span', { class: 'fx-when' }, 'Modified'));
    this.tree = h('div', { class: 'fx-tree', role: 'tree', tabIndex: 0, 'aria-multiselectable': 'true', 'aria-label': 'Files' });
    this.actEl = h('div', { class: 'fx-actbar', role: 'toolbar', 'aria-label': 'What to do with the files ticked', hidden: true });
    this.el = h('section', { class: 'fx-view' }, this.barEl, this.pathEl, this.toolsEl, this.colsEl, this.tree, this.actEl);
    this.el.listing = this;
    const t = this.tree;
    t.addEventListener('click', (e) => this.onClick(e));
    t.addEventListener('dblclick', (e) => this.onDouble(e));
    t.addEventListener('contextmenu', (e) => this.onContext(e));
    t.addEventListener('keydown', (e) => this.onKey(e));
    t.addEventListener('scroll', () => { if (this.flat.length > this.limit && t.scrollTop + t.clientHeight > t.scrollHeight - 400) { this.limit += ROWS_STEP; this.drawTree(); } });
    t.addEventListener('mouseover', (e) => this.onHover(e));
    t.addEventListener('dragstart', (e) => this.onDragStart(e));
    t.addEventListener('dragend', () => this.panel.dragOver(null));
    this.el.addEventListener('focusin', () => this.panel.activate(this));
    this.el.addEventListener('mousedown', () => this.panel.activate(this), true);
    for (const type of ['dragenter', 'dragover']) this.el.addEventListener(type, (e) => this.onDragOver(e));
    this.el.addEventListener('dragleave', (e) => { if (!this.el.contains(e.relatedTarget)) this.panel.dragOver(null); });
    this.el.addEventListener('drop', (e) => this.onDrop(e));
  }

  // ---- paths, as the station writes them ----
  get sep() { return this.home?.sep || '/'; }
  join(dir, name) { return dir.endsWith(this.sep) ? dir + name : dir + this.sep + name; }
  // The folder's own name: its last part, or the root itself.
  nameOf(path) { const cut = path.endsWith(this.sep) ? path.slice(0, -1) : path; return cut.slice(cut.lastIndexOf(this.sep) + 1) || path; }
  dirOf(path) {
    const root = this.rootOf(path), rest = path.slice(root.length).split(this.sep).filter(Boolean);
    rest.pop();
    return root + rest.join(this.sep);
  }
  rootOf(path) { return (this.home?.roots || ['/']).filter(r => path.toLowerCase().startsWith(r.toLowerCase())).sort((a, b) => b.length - a.length)[0] || ''; }
  // [[label, path]…] from the disk's root down to `path`.
  segments(path) {
    const root = this.rootOf(path), out = root ? [[root, root]] : [];
    let at = root;
    for (const part of path.slice(root.length).split(this.sep).filter(Boolean)) { at = at ? this.join(at, part) : part; out.push([part, at]); }
    return out;
  }
  under(path, dir) { return path === dir || path.startsWith(dir.endsWith(this.sep) ? dir : dir + this.sep); }
  isDir(e) { return e.kind === 'dir' || (e.kind === 'link' && e.target === 'dir'); }

  // ---- what is shown ----
  // Show another station, at `path` or its home folder. `pane`: a pane of
  // that station whose folder is wanted.
  async show(station, path = null, pane = null) {
    const mine = ++this.seq;
    this.station = station;
    this.home = null; this.root = null; this.dirs.clear(); this.expanded.clear(); this.sel.clear(); this.cursor = this.anchor = null;
    this.error = null; this.draft = this.renaming = this.pathEdit = null; this.selecting = false;
    const s = env.stations().find(x => x.station === station);
    if (!canFiles(s)) {
      this.error = !s ? `${station} is not in the fleet.` : s.state !== 'online' ? `${station} is not answering just now.` : NEEDS(station);
      return this.draw();
    }
    this.loading = true;
    this.draw();
    try {
      const home = await look(station, 'files.home', pane ? { pane } : {});
      if (mine !== this.seq) return;
      this.home = home;
      await this.go(path || home.cwd || home.home, { mine });
    } catch (e) {
      if (mine !== this.seq) return;
      this.loading = false;
      this.error = say(e);
      this.draw();
    }
  }

  // Make `path` the folder shown. `typed`: it was typed into the path
  // bar, so a path that cannot be shown is said there and nothing changes.
  async go(path, { typed = false, mine = ++this.seq } = {}) {
    this.loading = true;
    if (!typed) this.draw();
    let got;
    try { got = await look(this.station, 'files.list', { path }); } catch (e) {
      if (mine !== this.seq) return false;
      this.loading = false;
      if (typed && this.pathEdit) { this.pathEdit.error = say(e); this.draw(); return false; }
      if (this.root) { toast(say(e), 'error'); this.draw(); return false; }
      this.error = say(e);
      this.draw();
      return false;
    }
    if (mine !== this.seq) return false;
    this.loading = false;
    this.error = null;
    this.pathEdit = null;
    this.root = got.path;
    for (const p of [...this.dirs.keys()]) if (!this.under(p, got.path)) this.dirs.delete(p);
    for (const p of [...this.expanded]) if (!this.under(p, got.path) || p === got.path) this.expanded.delete(p);
    this.dirs.set(got.path, got);
    this.sel.clear(); this.cursor = this.anchor = null; this.draft = this.renaming = null;
    this.limit = ROWS_STEP;
    this.tree.scrollTop = 0;
    this.draw();
    this.panel.sync();
    if (this.opts.onGo) this.opts.onGo(got.path);
    return true;
  }

  // List one folder again. Not there any more: it is closed.
  async relist(path) {
    try { this.dirs.set(path, await look(this.station, 'files.list', { path })); return true; } catch (e) {
      if (path === this.root) { if (!this.dirs.has(path)) this.error = say(e); this.stale = say(e); return false; }
      this.dirs.set(path, { error: say(e), entries: [] });
      return false;
    }
  }

  // The folder shown and every folder open in it, one after another: what
  // is selected, open and scrolled to stays.
  async refresh() {
    if (!this.root || this.busy || !this.station) return;
    this.busy = true;
    const mine = this.seq;
    try {
      this.stale = null;
      for (const path of [this.root, ...[...this.expanded].sort()]) {
        if (mine !== this.seq) return;
        if (path !== this.root && !this.visible(path)) continue;
        await this.relist(path);
      }
      if (mine !== this.seq) return;
      // What is gone is no longer selected.
      const there = new Set(this.rows().filter(r => r.path).map(r => r.path));
      for (const p of [...this.sel]) if (!there.has(p)) this.sel.delete(p);
      this.drawTree();
      this.drawTools();
    } finally { this.busy = false; }
  }
  // An open folder is asked again only while every folder above it is open.
  visible(path) {
    for (let d = this.dirOf(path); d && d !== this.root && this.under(d, this.root); d = this.dirOf(d)) if (!this.expanded.has(d)) return false;
    return true;
  }

  async toggle(row) {
    if (!row || !row.isDir || row.odd) return;
    if (this.expanded.has(row.path)) { this.expanded.delete(row.path); return this.drawTree(); }
    this.expanded.add(row.path);
    this.drawTree();
    await this.relist(row.path);
    this.drawTree();
  }

  // The rows of the tree, top to bottom.
  rows() {
    const out = [], hidden = this.panel.showHidden;
    const walk = (dir, depth) => {
      const d = this.dirs.get(dir);
      if (!d) return;
      let shown = 0;
      for (const e of d.entries || []) {
        if (e.hidden && !hidden) continue;
        shown++;
        const path = this.join(dir, e.name), isDir = this.isDir(e), open = isDir && this.expanded.has(path) && !e.odd_name;
        out.push({ e, path, dir, depth, isDir, open, odd: !!e.odd_name });
        if (!open) continue;
        const sub = this.dirs.get(path);
        if (!sub) out.push({ note: 'Reading…', depth: depth + 1 });
        else if (sub.error) out.push({ note: sub.error, depth: depth + 1, bad: true });
        else if (!walk(path, depth + 1)) out.push({ note: (sub.entries || []).length ? 'Only hidden files here' : 'Empty', depth: depth + 1 });
      }
      if (d.truncated) out.push({ note: `… ${Math.max(0, (d.total || 0) - (d.entries || []).length)} more not shown: this folder has ${d.total}`, depth });
      return shown;
    };
    if (this.root) walk(this.root, 0);
    return out;
  }
  rowOf(path) { return this.flat.find(r => r.path === path); }
  selected() { return this.flat.filter(r => r.path && !r.odd && this.sel.has(r.path)); }

  // ---- drawing ----
  draw() { this.drawBar(); this.drawPath(); this.drawTools(); this.drawTree(); }

  drawBar() {
    const all = stations(), pane = this.panel.paneFor(this), parent = this.dirs.get(this.root)?.parent;
    const sig = JSON.stringify([all.map(s => [s.station, s.state, canFiles(s)]), this.station, !!parent, pane?.cwd, pane?.name, this.root, this.panel.showHidden, !!this.home]);
    if (sig === this.barSig) return;
    this.barSig = sig;
    const pick = this.opts.fixed ? h('span', { class: 'grow' }) : h('select', { class: 'fx-station', 'aria-label': 'Station', title: 'Whose files these are', onchange: (e) => this.show(e.target.value) },
      all.map(s => h('option', { value: s.station, selected: s.station === this.station, disabled: !canFiles(s) && s.station !== this.station },
        s.station + (s.state !== 'online' ? ' (not answering)' : canFiles(s) ? '' : ' (needs updating)'))));
    if (!this.opts.fixed && this.station && !all.some(s => s.station === this.station)) pick.append(h('option', { value: this.station, selected: true }, this.station));
    const btn = (name, label, run, off) => h('button', { type: 'button', class: 'fx-nav fx-nav-' + name, title: label, 'aria-label': label, disabled: !!off, onclick: run }, ico(name));
    fill(this.barEl, pick,
      btn('up', 'Up: the folder this one is in', () => this.go(parent), !parent),
      btn('home', 'Home folder', () => this.go(this.home.home), !this.home),
      pane && pane.cwd ? btn('pane', `${pane.name}'s folder: ${pane.cwd}`, () => this.go(pane.cwd), !this.home) : null,
      btn('refresh', 'Read this folder again', () => this.root ? this.refresh() : this.show(this.station), false),
      h('button', { type: 'button', class: 'fx-nav fx-hidden' + (this.panel.showHidden ? ' on' : ''), title: 'Show hidden files', 'aria-label': 'Show hidden files', 'aria-pressed': String(this.panel.showHidden),
        onclick: () => this.panel.setHidden(!this.panel.showHidden) }, ico('hidden')));
  }

  drawPath() {
    if (this.pathEdit) {
      if (this.pathEl.querySelector('input')) { this.pathEl.querySelector('.fx-path-err').textContent = this.pathEdit.error || ''; return; }
      const box = h('input', { class: 'fx-path-box mono', 'aria-label': 'Folder to show', value: this.pathEdit.value, spellcheck: false, autocapitalize: 'off', autocomplete: 'off' });
      const done = () => { this.pathEdit = null; this.drawPath(); this.tree.focus({ preventScroll: true }); };
      box.addEventListener('keydown', (e) => {
        e.stopPropagation();
        if (e.key === 'Escape') { e.preventDefault(); done(); }
        if (e.key === 'Enter') { e.preventDefault(); const to = box.value.trim(); if (to) { this.pathEdit.error = null; this.go(to, { typed: true }).then(ok => { if (ok) this.tree.focus({ preventScroll: true }); }); } }
      });
      this.pathEl.replaceChildren(box, h('button', { type: 'button', class: 'mini', onclick: done }, 'Cancel'), h('div', { class: 'fx-path-err', role: 'alert' }, this.pathEdit.error || ''));
      box.focus();
      box.select();
      return;
    }
    const segs = this.root ? this.segments(this.root) : [];
    const edit = () => { if (!this.home) return; this.pathEdit = { value: this.root || this.home.home, error: null }; this.drawPath(); };
    const crumbs = h('div', { class: 'fx-crumbs', role: 'navigation', 'aria-label': 'The folder shown', onclick: (e) => { if (e.target === crumbs) edit(); } },
      segs.map(([label, path], i) => [
        i > 1 || (i === 1 && !segs[0][0].endsWith(this.sep)) ? h('span', { class: 'fx-sep', 'aria-hidden': 'true' }, this.sep) : null,
        h('button', { type: 'button', class: 'fx-seg' + (i === segs.length - 1 ? ' here' : ''), title: path, 'aria-current': i === segs.length - 1 ? 'location' : undefined, onclick: () => this.go(path) }, h('bdi', {}, label)),
      ]));
    this.pathEl.replaceChildren(crumbs, h('button', { type: 'button', class: 'fx-nav fx-edit', title: 'Type a folder to go to', 'aria-label': 'Type a folder to go to', disabled: !this.home, onclick: edit }, ico('edit')));
    crumbs.scrollLeft = crumbs.scrollWidth;
  }

  drawTools() {
    const n = this.selected().length, clip = this.panel.clip, phone = env.isPhone(), ok = !!this.root;
    const sig = JSON.stringify([n, !!clip, phone, ok, this.selecting, this.panel.tapOpens()]);
    if (sig === this.toolsSig) return;
    this.toolsSig = sig;
    const b = (cls, label, run, off, title) => h('button', { type: 'button', class: 'mini fx-' + cls, disabled: !!off, title, onclick: run }, label);
    const under = (e) => { const r = e.currentTarget.getBoundingClientRect(); return [r.left, r.bottom + 2]; };
    const more = (e) => this.menu(...under(e));
    if (this.panel.tapOpens()) {
      // No right-click where a finger does the pointing: the buttons stay,
      // and ticks are how several things are chosen where a tap opens.
      this.toolsEl.replaceChildren(
        b('upload', 'Upload', () => this.panel.pickFiles(this, this.root), !ok, 'Send files from this device into this folder'),
        b('mkdir', 'New folder', () => this.startDraft('dir'), !ok),
        b('mkfile', 'New file', () => this.startDraft('file'), !ok),
        b('paste', 'Paste', () => this.panel.paste(this, this.root), !ok || !clip, clip ? `Put the ${items(clip.paths.length)} ${clip.op === 'cut' ? 'cut' : 'copied'} from ${clip.station} into this folder` : 'Nothing was copied or cut'),
        h('span', { class: 'grow' }), b('select' + (this.selecting ? ' on' : ''), this.selecting ? 'Done' : 'Select', () => this.setSelecting(!this.selecting), !ok));
      this.actEl.hidden = !this.selecting;
      this.actEl.replaceChildren(h('span', { class: 'fx-count', 'aria-live': 'polite' }, n ? `${n} ticked` : 'Tick what you want'),
        b('copy', 'Copy', () => this.copy('copy'), !n),
        b('cut', 'Cut', () => this.copy('cut'), !n),
        b('rename', 'Rename', () => this.startRename(), n !== 1),
        b('delete', 'Delete', () => this.trash(), !n, 'To the trash of this station: it can be undone'),
        b('download', 'Download', () => this.download(), !n, 'Save on this device. Several things, or a folder, come as one zip'),
        b('more', 'More', more, !ok));
    } else {
      // At a computer everything is in the right-click menu and on the
      // keys. Two buttons are left: New, and the same menu for a mouse
      // that has no right button.
      const fresh = h('button', { type: 'button', class: 'mini fx-new', disabled: !ok, 'aria-haspopup': 'menu', title: 'A new folder or a new file in this folder',
        onclick: (e) => showMenu(...under(e), [{ label: 'Folder', action: () => this.startDraft('dir') }, { label: 'File', action: () => this.startDraft('file') }]) }, 'New');
      const all = h('button', { type: 'button', class: 'mini fx-more', disabled: !ok, 'aria-haspopup': 'menu', 'aria-label': 'More: what can be done with what is selected, or with this folder',
        title: n ? `What can be done with the ${items(n)} selected (the same as a right-click)` : 'Upload, Paste and the rest for this folder (the same as a right-click)', onclick: more }, '⋯');
      this.toolsEl.replaceChildren(fresh, all);
      this.actEl.hidden = true;
    }
  }

  drawTree() {
    const phone = this.panel.tapOpens();
    const st = this.panel, clip = st.clip && st.clip.op === 'cut' && st.clip.station === this.station ? new Set(st.clip.paths) : null;
    if (this.error || !this.root) {
      this.flat = [];
      this.treeSig = null;
      this.tree.replaceChildren(this.error
        ? h('div', { class: 'fx-fail', role: 'alert' }, h('p', {}, this.error), h('button', { type: 'button', class: 'fx-retry', onclick: () => this.show(this.station, this.root) }, 'Retry'))
        : h('div', { class: 'fx-note' }, `Reading ${this.station}…`));
      return;
    }
    const flat = this.rows();
    this.flat = flat;
    const shown = flat.slice(0, this.limit);
    const sig = JSON.stringify([phone, this.selecting, this.limit, this.cursor, this.stale, this.draft && this.draft.kind, this.renaming && this.renaming.path, this.station,
      shown.map(r => r.note ? [r.note, r.depth] : [r.path, r.e.kind, r.e.target, r.e.size, r.e.mtime_ms, r.depth, r.open, this.sel.has(r.path), !!clip && clip.has(r.path)])]);
    if (sig === this.treeSig) return;
    this.treeSig = sig;
    const kids = [];
    if (this.stale) kids.push(h('div', { class: 'fx-stale', role: 'status' }, `${this.station} did not answer: ${this.stale} `, h('button', { type: 'button', class: 'mini fx-retry', onclick: () => this.refresh() }, 'Retry')));
    if (this.draft) kids.push(this.nameRow(this.draft, this.draft.kind === 'dir' ? 'folder' : 'binary', this.draft.kind === 'dir' ? 'New folder' : 'New file', '', (name) => this.commitDraft(name), () => { this.draft = null; this.drawTree(); }));
    shown.forEach((r, i) => kids.push(r.note ? deep(h('div', { class: 'fx-row fx-note-row' + (r.bad ? ' bad' : '') }, r.note), r.depth) : this.row(r, i, phone, clip)));
    if (flat.length > shown.length) kids.push(h('button', { type: 'button', class: 'fx-more-rows', onclick: () => { this.limit += ROWS_STEP; this.drawTree(); } }, `Show more (${flat.length - shown.length} below)`));
    if (!flat.length && !this.draft) kids.push(h('div', { class: 'fx-note' }, (this.dirs.get(this.root)?.entries || []).length ? 'Only hidden files here. The eye above shows them.' : phone ? 'This folder is empty. Use Upload to put files here.' : 'This folder is empty. Drop files here, or right-click for Upload.'));
    const top = this.tree.scrollTop;
    this.tree.replaceChildren(...kids);
    this.tree.scrollTop = top;
    if (this.cursor) this.tree.setAttribute('aria-activedescendant', this.idOf(this.cursor)); else this.tree.removeAttribute('aria-activedescendant');
    const box = this.tree.querySelector('.fx-name-box');
    if (box && document.activeElement !== box) { box.focus(); const dot = box.value.lastIndexOf('.'); box.setSelectionRange(0, dot > 0 ? dot : box.value.length); }
  }
  idOf(path) { const i = this.flat.findIndex(r => r.path === path); return `fx-${this.uid}-${i}`; }

  row(r, i, phone, cut) {
    const e = r.e, sel = this.sel.has(r.path), kind = fileKind(e, r.open);
    if (this.renaming && this.renaming.path === r.path) {
      return this.nameRow(this.renaming, kind, 'New name', e.name, (name) => this.commitRename(r, name), () => { this.renaming = null; this.drawTree(); }, r.depth);
    }
    const facts = r.isDir ? fmtAge(e.mtime_ms) : [fmtSize(e.size), fmtAge(e.mtime_ms)].filter(Boolean).join(' · ');
    const says = r.odd ? 'This name is not UTF-8: it cannot be opened, renamed or deleted from here. It still goes with its folder.' : e.kind === 'link' ? `A link to ${e.target === 'missing' ? 'something that is not there' : e.target === 'dir' ? 'a folder' : 'a file'}` : '';
    return deep(h('div', {
      class: 'fx-row' + (sel ? ' sel' : '') + (this.cursor === r.path ? ' cursor' : '') + (r.odd ? ' odd' : '') + (cut && cut.has(r.path) ? ' cut' : '') + (e.hidden ? ' dot-file' : ''),
      id: `fx-${this.uid}-${i}`, role: 'treeitem', 'aria-level': r.depth + 1, 'aria-selected': String(sel), 'aria-expanded': r.isDir && !r.odd ? String(r.open) : undefined,
      'aria-disabled': r.odd ? 'true' : undefined, draggable: !r.odd && !phone ? 'true' : undefined, dataset: { i: String(i) }, title: says || undefined,
    },
    this.selecting ? h('input', { type: 'checkbox', class: 'fx-tick', checked: sel, disabled: r.odd, tabIndex: -1, 'aria-label': `Tick ${e.name}` }) : null,
    r.isDir && !r.odd ? h('button', { type: 'button', class: 'fx-tri' + (r.open ? ' open' : ''), tabIndex: -1, 'aria-label': (r.open ? 'Close ' : 'Open in place: ') + e.name }, fileIcon('tri')) : h('span', { class: 'fx-tri none' }),
    ico(kind, 'k-' + kind),
    h('span', { class: 'fx-main' },
      // The name is text, whatever it says, and its direction is its own.
      h('bdi', { class: 'fx-name' }, e.name),
      r.odd ? h('span', { class: 'fx-sub' }, 'not UTF-8: cannot be opened here') : phone ? h('span', { class: 'fx-sub' }, facts) : null),
    phone ? null : h('span', { class: 'fx-size' }, r.isDir || r.odd ? '' : fmtSize(e.size)),
    phone ? null : h('span', { class: 'fx-when', title: e.mtime_ms ? new Date(e.mtime_ms).toLocaleString() : '' }, fmtWhen(e.mtime_ms)),
    phone && !r.odd && !this.selecting ? h('button', { type: 'button', class: 'fx-dots', tabIndex: -1, 'aria-label': `What to do with ${e.name}`, 'aria-haspopup': 'menu' }, '⋯') : null), r.depth);
  }

  // A row whose name is being typed: a new folder, a new file, a rename.
  nameRow(state, kind, label, value, commit, cancel, depth = 0) {
    let over = false;
    const box = h('input', { class: 'fx-name-box', 'aria-label': label, placeholder: label, value, spellcheck: false, autocapitalize: 'off', autocomplete: 'off' });
    const end = (keep) => {
      if (over) return;
      const name = box.value.trim();
      if (!keep || !name || name === value) { over = true; return cancel(); }
      over = true;
      commit(name).then((err) => { if (err) { over = false; state.error = err; note.textContent = err; box.focus(); } });
    };
    const note = h('span', { class: 'fx-name-err', role: 'alert' }, state.error || '');
    box.addEventListener('keydown', (e) => { e.stopPropagation(); if (e.key === 'Enter') { e.preventDefault(); end(true); } if (e.key === 'Escape') { e.preventDefault(); end(false); this.tree.focus({ preventScroll: true }); } });
    box.addEventListener('blur', () => setTimeout(() => { if (!over && box.isConnected && document.activeElement !== box) end(true); }, 120));
    for (const type of ['click', 'dblclick', 'mousedown']) box.addEventListener(type, (e) => e.stopPropagation());
    return deep(h('div', { class: 'fx-row fx-naming' }, h('span', { class: 'fx-tri none' }), ico(kind, 'k-' + kind), h('span', { class: 'fx-main' }, box, note)), depth);
  }

  // ---- choosing ----
  hit(e) { const el = e.target.closest?.('.fx-row[data-i]'); return el ? this.flat[Number(el.dataset.i)] : null; }
  select(paths, cursor = null) {
    this.sel = new Set(paths);
    if (cursor !== null) this.cursor = this.anchor = cursor;
    this.drawTree(); this.drawTools();
    this.panel.sync();
  }
  setSelecting(on) { this.selecting = on; if (!on) this.sel.clear(); this.toolsSig = null; this.drawTools(); this.drawTree(); }
  range(to) {
    const rows = this.flat.filter(r => r.path && !r.odd), a = rows.findIndex(r => r.path === (this.anchor || to)), b = rows.findIndex(r => r.path === to);
    if (a < 0 || b < 0) return [to];
    return rows.slice(Math.min(a, b), Math.max(a, b) + 1).map(r => r.path);
  }

  onClick(e) {
    const r = this.hit(e);
    if (!r) { if (e.target === this.tree && !this.selecting) this.select([], null); return; }
    if (e.target.closest('.fx-tri')) { e.stopPropagation(); return this.toggle(r); }
    if (r.odd) return;
    if (e.target.closest('.fx-dots')) { const b = e.target.closest('.fx-dots').getBoundingClientRect(); this.select([r.path], r.path); return this.menu(b.right, b.bottom + 2); }
    if (this.selecting) { const next = new Set(this.sel); if (next.has(r.path)) next.delete(r.path); else next.add(r.path); return this.select(next, r.path); }
    if (this.panel.tapOpens()) return this.open(r);
    if (e.shiftKey) { this.sel = new Set(this.range(r.path)); this.cursor = r.path; this.drawTree(); this.drawTools(); return; }
    if (e.ctrlKey || e.metaKey) { const next = new Set(this.sel); if (next.has(r.path)) next.delete(r.path); else next.add(r.path); return this.select(next, r.path); }
    this.select([r.path], r.path);
  }
  onDouble(e) {
    const r = this.hit(e);
    if (!r || r.odd || this.panel.tapOpens() || e.target.closest('.fx-tri')) return;
    this.open(r);
  }
  // A folder becomes the folder shown; a file is previewed.
  open(r) { if (r.isDir) this.go(r.path); else this.preview(r); }

  onContext(e) {
    e.preventDefault();
    const r = this.hit(e);
    if (r && r.odd) return;
    // As a desktop's file manager does: a right-click on a row that is not
    // selected selects it alone; inside what is selected, the selection
    // stays; on empty space, nothing is selected and the menu is the
    // folder's.
    if (r && !this.sel.has(r.path)) this.select([r.path], r.path);
    if (!r && this.sel.size && !this.selecting) this.select([], null);
    this.menu(e.clientX, e.clientY);
  }

  // Everything that can be done here: with what is selected, or, with
  // nothing selected, with the folder shown.
  menu(x, y) {
    if (!this.root) return;
    const picked = this.selected(), n = picked.length, one = n === 1 ? picked[0] : null, clip = this.panel.clip;
    showMenu(x, y, [
      { title: n ? (one ? one.e.name : items(n)) : this.nameOf(this.root) },
      one && { label: one.isDir ? 'Open' : 'Preview', hint: 'Enter', action: () => this.open(one) },
      one && one.isDir && { label: this.expanded.has(one.path) ? 'Close in place' : 'Open in place', action: () => this.toggle(one) },
      n && '-',
      n && { label: 'Copy', hint: 'Ctrl+C', action: () => this.copy('copy') },
      n && { label: 'Cut', hint: 'Ctrl+X', action: () => this.copy('cut') },
      clip && { label: one && one.isDir ? `Paste into ${one.e.name}` : 'Paste', hint: `${items(clip.paths.length)} from ${clip.station}`, action: () => this.panel.paste(this, one && one.isDir ? one.path : this.root) },
      one && { label: 'Rename', hint: 'F2', action: () => this.startRename() },
      n && { label: 'Download', hint: n > 1 || (one && one.isDir) ? 'as one zip' : undefined, action: () => this.download() },
      n && { label: n > 1 ? 'Copy paths' : 'Copy path', action: () => copyText(picked.map(p => p.path).join('\n'), n > 1 ? 'Copied the paths' : 'Copied the path') },
      n && { label: 'Delete', hint: 'to the trash', danger: true, action: () => this.trash() },
      (n || clip) && '-',
      { label: 'New folder', action: () => this.startDraft('dir') },
      { label: 'New file', action: () => this.startDraft('file') },
      { label: 'Upload…', action: () => this.panel.pickFiles(this, one && one.isDir ? one.path : this.root) },
      !n && { label: 'Copy this folder\'s path', action: () => copyText(this.root, 'Copied the path') },
      { label: 'Read again', action: () => this.refresh() },
    ]);
  }

  onKey(e) {
    if (e.target !== this.tree) return;
    const mod = e.ctrlKey || e.metaKey, key = e.key, rows = this.flat.filter(r => r.path && !r.odd);
    const at = rows.findIndex(r => r.path === this.cursor), cur = rows[at];
    const move = (to) => {
      const r = rows[Math.max(0, Math.min(rows.length - 1, to))];
      if (!r) return;
      if (e.shiftKey) { this.sel = new Set(this.range(r.path)); this.cursor = r.path; } else { this.sel = new Set([r.path]); this.cursor = this.anchor = r.path; }
      this.drawTree(); this.drawTools();
      document.getElementById(this.idOf(r.path))?.scrollIntoView({ block: 'nearest' });
    };
    const done = () => { e.preventDefault(); e.stopPropagation(); };
    if (key === 'ArrowDown') { done(); move(at + 1); }
    else if (key === 'ArrowUp') { done(); move(at < 0 ? 0 : at - 1); }
    else if (key === 'Home') { done(); move(0); }
    else if (key === 'End') { done(); move(rows.length - 1); }
    else if (key === 'ArrowRight' && cur?.isDir) { done(); if (!cur.open) this.toggle(cur); else move(at + 1); }
    else if (key === 'ArrowLeft' && cur) { done(); if (cur.open) this.toggle(cur); else if (cur.dir !== this.root) { this.sel = new Set([cur.dir]); this.cursor = this.anchor = cur.dir; this.drawTree(); this.drawTools(); } }
    else if (key === 'Enter' && cur) { done(); this.open(cur); }
    else if (key === 'F2' && this.selected().length === 1) { done(); this.startRename(); }
    else if ((key === 'Delete' || (key === 'Backspace' && e.metaKey)) && this.selected().length) { done(); this.trash(); }
    else if (mod && key.toLowerCase() === 'a') { done(); this.select(rows.map(r => r.path)); }
    else if (mod && key.toLowerCase() === 'c' && this.selected().length) { done(); this.copy('copy'); }
    else if (mod && key.toLowerCase() === 'x' && this.selected().length) { done(); this.copy('cut'); }
    // Ctrl+V: the browser says `paste` next, with whatever files the
    // device's clipboard holds (the panel hears it). A browser that says
    // nothing for a tree gets the page's own clipboard a moment later.
    else if (mod && key.toLowerCase() === 'v') { clearTimeout(this.panel.pasteSoon); this.panel.pasteSoon = setTimeout(() => this.panel.paste(this, this.root), 150); }
  }

  // ---- what is done ----
  copy(op) {
    const picked = this.selected();
    if (!picked.length) return;
    this.panel.setClip({ station: this.station, paths: picked.map(r => r.path), op, sep: this.sep });
  }

  startDraft(kind) {
    if (!this.root) return;
    this.renaming = null;
    this.draft = { kind, error: null };
    this.tree.scrollTop = 0;
    this.drawTree();
  }
  async commitDraft(name) {
    const kind = this.draft?.kind;
    let r;
    try { r = await change(this.station, kind === 'dir' ? 'files.mkdir' : 'files.create', { dir: this.root, name }); } catch (e) { return say(e); }
    if (r.ok === false) return r.message || 'That name is taken.';
    this.draft = null;
    await this.relist(this.root);
    this.select([r.path], r.path);
    this.tree.focus({ preventScroll: true });
    return null;
  }

  startRename() {
    const one = this.selected();
    if (one.length !== 1) return;
    this.draft = null;
    this.renaming = { path: one[0].path, error: null };
    this.drawTree();
  }
  async commitRename(row, name) {
    let r;
    try { r = await change(this.station, 'files.rename', { path: row.path, name }); } catch (e) { return say(e); }
    if (r.ok === false && r.code === 'in_use') {
      this.renaming = null;
      this.drawTree();
      inUseNotice('rename', [{ path: row.path, panes: r.panes }], this.sep);
      return null;
    }
    if (r.ok === false) return r.message || 'That name is taken.';
    this.renaming = null;
    if (this.expanded.delete(row.path)) this.dirs.delete(row.path);
    await this.relist(row.dir);
    this.select([r.path], r.path);
    this.tree.focus({ preventScroll: true });
    return null;
  }

  // To the station's trash, with Undo. What is on another disk is not
  // moved there: it is asked about, and deleted for good only on a yes.
  async trash() {
    const picked = this.selected(), station = this.station;
    if (!picked.length) return;
    let r;
    try { r = await change(station, 'files.trash', { paths: picked.map(p => p.path) }); } catch (e) { return toast(say(e), 'error'); }
    const gone = (r.trashed || []).length, failed = r.failed || [], sep = this.sep;
    const far = failed.filter(f => f.code === 'other_disk'), used = failed.filter(f => f.code === 'in_use'), rest = failed.filter(f => f.code !== 'other_disk' && f.code !== 'in_use');
    this.sel.clear();
    this.panel.changed(station);
    if (gone) {
      toast(`${items(gone)} moved to the trash on ${station}`, 'info', { label: 'Undo', run: async () => {
        try {
          const back = await change(station, 'files.restore', { trash: r.trash });
          const kept = (back.failed || []).length;
          toast(kept ? `${items((back.restored || []).length)} put back; ${kept} not: ${back.failed[0].message}` : `${items((back.restored || []).length)} put back`, kept ? 'error' : 'info');
        } catch (e) { toast(say(e), 'error'); }
        this.panel.changed(station);
      } });
    }
    if (rest.length) toast(`${plural(rest.length, 'thing')} not deleted: ${rest[0].message}`, 'error');
    // What a pane is working in stays, and is never asked about again as
    // "delete anyway": the panes are named, to be closed first.
    if (used.length) inUseNotice('delete', used, sep, gone ? `${items(gone)} moved to the trash. Not deleted:` : null);
    if (far.length) {
      confirmBox('Delete for good?', `${far.length === 1 ? this.nameOf(far[0].path) + ' is' : items(far.length) + ' are'} on another disk than ${station}'s trash, and cannot be moved there. Delete for good? This cannot be undone.`, 'Delete for good', async () => {
        try {
          const d = await change(station, 'files.delete', { paths: far.map(f => f.path) });
          const held = (d.failed || []).filter(f => f.code === 'in_use'), not = (d.failed || []).filter(f => f.code !== 'in_use'), n = (d.deleted || []).length;
          if (held.length) inUseNotice('delete', held, sep, n ? `${items(n)} deleted for good. Not deleted:` : null);
          if (not.length || !held.length) toast(not.length ? `${plural(not.length, 'thing')} not deleted: ${not[0].message}` : `${items(n)} deleted for good`, not.length ? 'error' : 'info');
        } catch (e) { toast(say(e), 'error'); }
        this.panel.changed(station);
      });
    }
  }

  // One file: a link the station gives a ticket for. Several, or a
  // folder: the station builds one zip, and then the same.
  download() {
    const picked = this.selected();
    if (!picked.length) return;
    if (picked.length === 1 && !picked[0].isDir) return this.panel.downloadFile(this.station, picked[0].path, picked[0].e.name);
    this.panel.downloadZip(this.station, picked.map(p => p.path));
  }

  preview(r) { showPreview(this.panel, this.station, r.path, r.e.name, r.e); }

  // ---- dragging ----
  onHover(e) {
    const r = this.hit(e);
    clearTimeout(this.restTimer);
    if (!r || r.odd || r.isDir || this.panel.tapOpens()) return;
    // A file's link has to be in hand before a drag begins (a browser lets
    // nothing be waited for then): asked for when the pointer rests on it.
    this.restTimer = setTimeout(() => this.panel.ticketSoon(this.station, r.path), 350);
  }
  onDragStart(e) {
    const r = this.hit(e);
    if (!r || r.odd || this.renaming) { e.preventDefault(); return; }
    if (!this.sel.has(r.path)) this.select([r.path], r.path);
    const picked = this.selected();
    this.panel.drag = { from: this, station: this.station, paths: picked.map(p => p.path), sep: this.sep };
    try {
      e.dataTransfer.effectAllowed = 'copyMove';
      e.dataTransfer.setData('application/x-spandeck-files', String(picked.length));
      // One file dragged out to the desktop (Chrome and Edge take this).
      const link = picked.length === 1 && !picked[0].isDir ? this.panel.ticketHeld(this.station, picked[0].path) : null;
      if (link) e.dataTransfer.setData('DownloadURL', `application/octet-stream:${picked[0].e.name.replace(/[:\r\n]/g, '_')}:${new URL(link, location.href).href}`);
    } catch (err) { /* a browser that will not carry it still drags inside the panel */ }
  }
  // Where a drop at this event would go: a folder's row, or the folder shown.
  target(e) {
    const r = this.hit(e);
    if (r && r.isDir && !r.odd) return { dir: r.path, row: e.target.closest('.fx-row') };
    return { dir: this.root, row: null };
  }
  onDragOver(e) {
    if (!this.root) return;
    const drag = this.panel.drag, files = !drag && [...(e.dataTransfer?.types || [])].includes('Files');
    if (!drag && !files) return;
    e.preventDefault();
    e.stopPropagation();
    const t = this.target(e);
    let says;
    if (drag) {
      const across = drag.station !== this.station, op = across || e.altKey ? 'copy' : 'move';
      try { e.dataTransfer.dropEffect = op; } catch (err) { /* the badge says it */ }
      says = `${op === 'copy' ? 'Copy' : 'Move'} ${drag.paths.length}${across ? ' to ' + this.station : ''}`;
    } else {
      try { e.dataTransfer.dropEffect = 'copy'; } catch (err) { /* as above */ }
      says = `Upload to ${this.nameOf(t.dir)} on ${this.station}`;
    }
    this.panel.dragOver({ view: this, row: t.row, says, x: e.clientX, y: e.clientY });
  }
  onDrop(e) {
    const drag = this.panel.drag, t = this.target(e);
    this.panel.dragOver(null);
    if (!this.root) return;
    if (drag) {
      e.preventDefault(); e.stopPropagation();
      this.panel.drag = null;
      const across = drag.station !== this.station;
      return this.panel.transfer(across || e.altKey ? 'copy' : 'move', drag, this, t.dir);
    }
    if (![...(e.dataTransfer?.types || [])].includes('Files') && !(e.dataTransfer?.files || []).length) return;
    e.preventDefault(); e.stopPropagation();
    this.panel.uploadDropped(e.dataTransfer, this, t.dir);
  }
}

// ---------------------------------------------------------------- the panel

class Panel {
  constructor() {
    this.shown = false;
    this.pane = null;         // {station, id}: the pane it was opened for
    this.views = [];
    this.cells = new Map();   // key -> {key, view, el, footEl}: listings in a layout's cells
    this.active = null;
    this.clip = null;         // {station, paths, op}: the page's own clipboard
    this.rows = [];           // uploads, zips and links, newest last
    this.jobs = new Map();    // "station/id" -> {station, id, job, mine, to}
    this.queue = [];          // uploads waiting their turn
    this.tickets = new Map(); // "station\npath" -> {url, at}
    this.drag = null;
    this.showHidden = store.get('hidden', '0') === '1';
    this.width = Number(store.get('w', 440)) || 440;
    this.build();
  }

  build() {
    this.headEl = h('div', { class: 'fx-head' });
    this.viewsEl = h('div', { class: 'fx-views' });
    this.footEl = h('div', { class: 'fx-foot' });
    // (Out of sight, not `hidden`: an iPhone opens its chooser only for a box that is laid out.)
    this.input = h('input', { type: 'file', multiple: true, class: 'fx-file-input', 'aria-hidden': 'true', tabIndex: -1 });
    this.input.addEventListener('change', () => {
      const files = [...this.input.files], to = this.inputFor;
      this.input.value = '';
      if (files.length && to) this.upload(files.map(file => ({ file, parts: [] })), to.view.station, to.dir, to.view);
    });
    this.grip = h('div', { class: 'fx-grip', role: 'separator', 'aria-orientation': 'vertical', 'aria-label': 'The width of the file panel', tabIndex: 0, title: 'Drag to make the panel wider or narrower' });
    this.el = h('aside', { id: 'files-panel', class: 'fx', role: 'complementary', 'aria-label': 'Files' }, this.grip, this.headEl, this.viewsEl, this.footEl, this.input);
    // The element leads back to the panel (tests read it; the deck does not).
    this.el.panel = this;
    this.badge = h('div', { class: 'fx-badge', hidden: true, 'aria-hidden': 'true' });
    this.grip.addEventListener('pointerdown', (e) => this.resize(e));
    this.grip.addEventListener('keydown', (e) => { if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') { e.preventDefault(); this.setWidth(this.width + (e.key === 'ArrowLeft' ? 24 : -24), true); } });
    this.el.addEventListener('keydown', (e) => {
      // Escape closes the panel when the panel has the keyboard (a box
      // being typed in, a menu and a dialog take it first).
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      e.stopPropagation();
      const sel = this.views.find(v => v.selecting);
      if (sel) return sel.setSelecting(false);
      this.close();
    });
    // What the device's clipboard holds: files are uploaded; anything
    // else, and the page's own clipboard is pasted.
    this.el.addEventListener('paste', (e) => this.onPaste(e, this.mine()));
    // A file dropped on the panel beside a listing goes to the listing
    // that has the keyboard, and never opens in place of the deck.
    this.besideDrop(this.el, () => this.mine());
    document.addEventListener('visibilitychange', () => { if (!document.hidden) this.relist(); });
  }
  onPaste(e, view) {
    if (e.target.closest('input, textarea')) return;
    if (!view || !view.root) return;
    clearTimeout(this.pasteSoon);
    const files = [...(e.clipboardData?.files || [])];
    e.preventDefault();
    if (files.length) this.upload(files.map(file => ({ file, parts: [] })), view.station, view.root, view);
    else this.paste(view, view.root);
  }
  besideDrop(el, viewOf) {
    for (const type of ['dragenter', 'dragover']) el.addEventListener(type, (e) => { if ([...(e.dataTransfer?.types || [])].includes('Files')) { e.preventDefault(); try { e.dataTransfer.dropEffect = 'copy'; } catch (err) { /* nothing to say */ } } });
    el.addEventListener('drop', (e) => {
      const view = viewOf();
      this.dragOver(null);
      if (!(e.dataTransfer?.files || []).length) return;
      e.preventDefault();
      if (view && view.root) this.uploadDropped(e.dataTransfer, view, view.root);
    });
  }

  // ---- listings in a layout's cells ----
  cell(key, opts) {
    let c = this.cells.get(key);
    if (c) return c;
    const view = new Listing(this, { fixed: true, onGo: opts.onGo });
    const footEl = h('div', { class: 'fx-foot', hidden: true });
    const el = h('div', { class: 'fx fx-cell' }, view.el, footEl);
    el.addEventListener('keydown', (e) => { if (e.key === 'Escape' && !e.defaultPrevented && view.selecting) { e.stopPropagation(); view.setSelecting(false); } });
    el.addEventListener('paste', (e) => this.onPaste(e, view));
    this.besideDrop(el, () => view);
    c = { key, view, el, footEl };
    this.cells.set(key, c);
    this.ticking();
    return c;
  }
  dropCell(key) {
    const c = this.cells.get(key);
    if (!c) return;
    c.view.seq++;
    c.el.remove();
    this.cells.delete(key);
    if (this.active === c.view) this.active = this.views[0] || null;
    this.ticking();
  }
  // The panel's own listing that has the keyboard (a cell's listing is not the panel's).
  mine() { return this.views.includes(this.active) ? this.active : this.views[0]; }
  // Every listing there is, and those on the page now.
  all() { return [...this.views, ...[...this.cells.values()].map(c => c.view)]; }
  live() { return [...(this.shown ? this.views : []), ...[...this.cells.values()].filter(c => c.el.isConnected).map(c => c.view)]; }
  // Listings are read again every few seconds while one is on the page.
  ticking() {
    const on = this.shown || this.cells.size > 0;
    if (on && !this.timer) this.timer = setInterval(() => this.relist(), RELIST_MS);
    if (!on && this.timer) { clearInterval(this.timer); this.timer = null; }
  }

  // A tap opens (and ticks choose) on a phone and on any touch screen.
  tapOpens() { return env.isPhone() || matchMedia('(pointer: coarse)').matches; }
  activate(view) { if (this.active === view) return; this.active = view; for (const v of this.views) v.el.classList.toggle('active', v === view && this.views.length > 1); this.sync(); }
  paneOf() { return this.pane ? env.findPane(this.pane.station, this.pane.id) : null; }
  // The pane the panel was opened for, where `view` shows its station.
  paneFor(view) { const p = this.paneOf(); return p && this.pane.station === view.station && this.views.includes(view) ? p : null; }

  open(station, pane) {
    this.pane = pane ? { station, id: pane.id } : null;
    if (!this.views.length) { this.views.push(new Listing(this)); this.viewsEl.append(this.views[0].el); }
    const view = this.views[0];
    this.active = view;
    if (!this.shown) {
      this.shown = true;
      this.el.append(this.input);
      document.body.append(this.el);
      document.getElementById('overlays').append(this.badge);
      document.body.classList.add('files-open');
      this.setWidth(this.width, false);
      this.ticking();
      for (const v of this.views.slice(1)) v.refresh();
    }
    view.show(station, null, pane ? pane.id : null).then(() => this.askJobs());
    this.sync();
    this.drawFoot();
    this.refit();
    view.tree.focus({ preventScroll: true });
  }

  close() {
    if (!this.shown) return;
    this.shown = false;
    this.ticking();
    closeMenu();
    this.el.remove();
    this.badge.remove();
    document.body.classList.remove('files-open');
    this.drawFoot();
    this.refit();
  }
  // The panes beside the panel are as wide as what is left: say so.
  refit() { dispatchEvent(new Event('resize')); }

  setWidth(w, keep) {
    this.width = Math.round(Math.max(300, Math.min(w, Math.max(300, innerWidth - 380))));
    document.body.style.setProperty('--files-w', this.width + 'px');
    if (keep) { store.set('w', this.width); this.refit(); }
  }
  resize(e) {
    e.preventDefault();
    const move = (ev) => this.setWidth(innerWidth - ev.clientX, false);
    const up = () => { removeEventListener('pointermove', move); removeEventListener('pointerup', up); document.body.classList.remove('dragging', 'fx-sizing'); this.setWidth(this.width, true); };
    document.body.classList.add('dragging', 'fx-sizing');
    addEventListener('pointermove', move);
    addEventListener('pointerup', up);
  }

  setHidden(on) { this.showHidden = on; store.set('hidden', on ? '1' : '0'); for (const v of this.all()) v.draw(); }

  // A second listing under the first, with its own station and folder:
  // for dragging from one to the other.
  second() {
    if (this.views.length > 1) {
      const gone = this.views.pop();
      gone.seq++;
      gone.el.remove();
      this.active = this.views[0];
      this.views[0].el.classList.remove('active');
    } else {
      const first = this.views[0], v = new Listing(this);
      this.views.push(v);
      this.viewsEl.append(v.el);
      // Another station if there is one: that is what a second view is for.
      const other = stations().find(s => canFiles(s) && s.station !== first.station);
      v.show(other ? other.station : first.station, other ? null : first.root).then(() => this.askJobs());
    }
    this.sync();
  }

  // The head: what the panel is for, the pane's move, the second view, Close.
  sync() {
    this.syncHead();
    for (const c of this.cells.values()) c.el.classList.toggle('fx-touch', this.tapOpens());
    for (const v of this.live()) { v.drawBar(); v.drawTools(); }
  }
  syncHead() {
    if (!this.shown) return;
    const p = this.paneOf(), view = this.mine();
    const here = p && view && view.root && this.pane.station === view.station && !p.exited && view.root !== (p.cwd || '').replace(/(.)[\\/]$/, '$1') ? view.root : null;
    const shell = p && p.agent === 'shell';
    const sig = JSON.stringify([p?.name, p?.cwd, this.pane, here, this.views.length, env.isPhone()]);
    if (sig !== this.headSig) {
      this.headSig = sig;
      fill(this.headEl,
        h('b', { class: 'fx-title' }, 'Files'),
        p ? h('span', { class: 'chip fx-for', title: `Opened for the pane ${p.name} on ${this.pane.station}. Looking at files never moves it.` }, h('bdi', {}, p.name)) : null,
        h('span', { class: 'grow' }),
        here ? h('button', { type: 'button', class: 'mini fx-move', title: `Start ${p.name} again in ${here}`, onclick: () => this.move(p, here) }, shell ? 'Restart ' : 'Move ', h('bdi', {}, p.name), ' here') : null,
        h('button', { type: 'button', class: 'fx-nav fx-second' + (this.views.length > 1 ? ' on' : ''), title: 'Second view: two listings, to drag from one station to another', 'aria-label': 'Second view', 'aria-pressed': String(this.views.length > 1), onclick: () => this.second() }, ico('split')),
        h('button', { type: 'button', class: 'fx-close', 'aria-label': 'Close the file panel', title: 'Close (Esc)', onclick: () => this.close() }, env.isPhone() ? 'Close' : '✕'));
    }
    this.el.classList.toggle('fx-touch', this.tapOpens());
  }

  // Move agent here. The only thing in the panel that moves a pane.
  move(p, folder) {
    const station = this.pane.station, shell = p.agent === 'shell';
    dialog(`${shell ? 'Restart' : 'Move'} ${p.name} here?`, (close) => {
      const err = h('p', { class: 'fx-move-err', role: 'alert' });
      const go = h('button', { type: 'button', class: 'primary fx-move-go', onclick: async () => {
        go.disabled = true;
        err.textContent = '';
        try { await env.movePane(station, p, folder); close(); toast(`${p.name} was started again in ${folder}`); } catch (e) {
          // What the station refuses, in its own sentence.
          err.textContent = say(e);
          go.disabled = false;
        }
      } }, shell ? 'Restart here' : 'Move here');
      return h('div', {},
        h('p', {}, h('bdi', {}, p.name), ' will be restarted in ', h('bdi', { class: 'mono' }, folder), shell ? '. What is running in it now is stopped.' : ' and starts a new conversation there. Its conversation so far stays in the archive.'),
        err,
        h('div', { class: 'row end' }, h('button', { type: 'button', onclick: close }, 'Cancel'), go));
    });
  }

  // ---- staying current ----
  relist() { if (!document.hidden) for (const v of this.live()) if (!v.draft && !v.renaming && !v.pathEdit && !this.drag) v.refresh(); }
  // Something of this page's own doing changed a station's files.
  changed(station) { for (const v of this.all()) if (v.station === station) v.refresh(); }

  // ---- the clipboard, the page's own ----
  setClip(clip) {
    this.clip = clip;
    for (const v of this.all()) { v.drawTools(); v.drawTree(); }
    this.drawFoot();
  }
  paste(view, dir) {
    const c = this.clip;
    if (!c || !view.root) return;
    const across = c.station !== view.station;
    this.transfer(c.op === 'cut' && !across ? 'move' : 'copy', c, view, dir).then((started) => {
      if (!started) return;
      if (c.op === 'cut') {
        if (across) toast('Copied: between stations files are never moved. The originals are still on ' + c.station + '.');
        this.setClip(null);
      }
    });
  }

  // Copy or move `what` ({station, paths, sep}) into `dir` of the station
  // `view` shows.
  // Names already taken there are asked about first.
  async transfer(op, what, view, dir) {
    const from = what.station, paths = what.paths, sep = what.sep || view.sep;
    const to = view.station, across = from !== to;
    if (across) op = 'copy';
    const name = (p) => baseOf(p, sep);
    if (paths.some(p => view.under(dir, p) && !across)) { toast('A folder cannot be put into itself.', 'error'); return false; }
    const same = !across && paths.every(p => parentOf(p, sep) === trimSep(dir, sep));
    if (same && op === 'move') return false;
    let conflict = 'fail';
    if (!same) {
      let there;
      try { there = await look(to, 'files.list', { path: dir }); } catch (e) { toast(`${to} did not show that folder: ${say(e)}`, 'error'); return false; }
      const taken = new Set((there.entries || []).map(e => e.name));
      const clash = paths.map(name).filter(n => taken.has(n));
      if (clash.length) {
        const a = await askClash(clash.length === 1 ? `${clash[0]} is already there` : `${clash.length} names are already there`,
          `${view.nameOf(dir)} on ${to} already has ${clash.slice(0, 5).join(', ')}${clash.length > 5 ? ` and ${clash.length - 5} more` : ''}. Replace: a file is replaced, and a folder is added to. Keep both: the new one gets a number. Skip: ${clash.length === 1 ? 'it is' : 'those are'} left out.`, false);
        conflict = a.choice;
        if (conflict === 'skip' && clash.length === paths.length) return false;
      }
    }
    let r;
    try { r = await change(from, 'files.transfer', { op, paths, dir, conflict, ...(across ? { to } : {}) }); } catch (e) { toast(say(e), 'error'); return false; }
    this.jobs.set(from + '/' + r.job, { station: from, id: r.job, mine: true, to, job: { id: r.job, op, to: across ? to : null, dir, state: 'running', counting: true, files_done: 0, files_total: 0, bytes_done: 0, bytes_total: 0, errors: [], skipped: [] } });
    this.drawFoot();
    this.watch(200);
    return true;
  }

  // ---- jobs and zips: asked once a second, only while something runs ----
  watch(ms = JOBS_MS) {
    clearTimeout(this.watchTimer);
    this.watchTimer = setTimeout(() => this.poll(), ms);
  }
  running() { return [...this.jobs.values()].some(j => j.job.state === 'running') || this.rows.some(r => r.kind === 'zip' && r.state === 'building'); }
  async askJobs() {
    const asked = new Set(this.live().map(v => v.station).filter(s => s && canFiles(env.stations().find(x => x.station === s))));
    await this.pollStations(asked);
    if (this.running()) this.watch();
  }
  async pollStations(asked) {
    await Promise.all([...asked].map(async (station) => {
      let got;
      try { got = await look(station, 'files.jobs', {}, 6000); } catch (e) { return; }
      for (const job of got.jobs || []) {
        const key = station + '/' + job.id, had = this.jobs.get(key);
        if (!had && job.state !== 'running') continue;
        const was = had?.job.state;
        this.jobs.set(key, { station, id: job.id, mine: had ? had.mine : false, to: job.to || station, job });
        // A job that wrote into a folder on screen has ended: read it again.
        if (job.state !== 'running' && was === 'running') { this.changed(job.to || station); if (job.op === 'move') this.changed(station); this.said(station, job, !!had.mine); }
      }
    }));
  }
  said(station, job, mine) {
    const errs = (job.errors || []).length + (job.errors_more || 0);
    // What a pane is working in was not moved: its own notice, by name.
    const used = (job.errors || []).filter(e => e.code === 'in_use'), other = (job.errors || []).find(e => e.code !== 'in_use');
    if (used.length && mine) inUseNotice('move', used, this.all().find(v => v.station === station)?.sep || '/', (job.created || []).length ? `${items(job.created.length)} moved. Not moved:` : null);
    if (job.state === 'failed') toast(job.message || 'The copy failed.', 'error');
    else if (job.state === 'done' && errs && (other || !used.length || !mine)) toast(`${job.op === 'move' ? 'Moved' : 'Copied'}, except ${errs}: ${(other || job.errors[0])?.message || ''}`, 'error');
  }
  async poll() {
    if (this.polling) return;
    this.polling = true;
    try {
      await this.pollStations(new Set([...this.jobs.values()].filter(j => j.job.state === 'running').map(j => j.station)));
      for (const z of this.rows.filter(r => r.kind === 'zip' && r.state === 'building')) await this.zipStep(z);
      this.drawFoot();
    } finally { this.polling = false; }
    if (this.running()) this.watch();
  }

  // ---- downloads ----
  // A ticket is asked of the station this page is on, never another.
  ticket(station, what) { return env.api.call('files.ticket', { ...what, ...(own(station) ? {} : { station }) }); }
  ticketHeld(station, path) { const t = this.tickets.get(station + '\n' + path); return t && Date.now() - t.at < TICKET_MS ? t.url : null; }
  ticketSoon(station, path) {
    if (this.ticketHeld(station, path) || this.ticketing) return;
    this.ticketing = true;
    this.ticket(station, { path }).then((t) => {
      this.tickets.set(station + '\n' + path, { url: t.url, at: Date.now() });
      for (const [k, v] of this.tickets) if (Date.now() - v.at > TICKET_MS) this.tickets.delete(k);
    }, () => {}).finally(() => { this.ticketing = false; });
  }
  // The browser is handed the link, and the link is also listed for a
  // minute: a phone's browser may not start a download nobody tapped.
  give(name, url) {
    const row = { kind: 'link', name, url, at: Date.now() };
    this.rows.push(row);
    this.lastLink = url;
    setTimeout(() => { this.rows = this.rows.filter(r => r !== row); this.drawFoot(); }, 55000);
    this.drawFoot();
    const a = h('a', { href: url, download: name, hidden: true });
    document.body.append(a);
    a.click();
    a.remove();
  }
  async downloadFile(station, path, name) {
    try { const t = await this.ticket(station, { path }); this.give(name, t.url); } catch (e) { toast(`${name} could not be downloaded: ${say(e)}`, 'error'); }
  }
  async downloadZip(station, paths) {
    let z;
    try { z = await change(station, 'files.zip', { paths }); } catch (e) { return toast(say(e), 'error'); }
    this.rows.push({ kind: 'zip', station, id: z.zip, name: z.name, state: 'building', status: null });
    this.drawFoot();
    this.watch(300);
  }
  async zipStep(z) {
    let s;
    try { s = await look(z.station, 'files.zip.status', { id: z.id }, 6000); } catch (e) { z.fails = (z.fails || 0) + 1; if (z.fails > 8) { z.state = 'failed'; z.message = say(e); } return; }
    z.fails = 0;
    z.status = s;
    if (s.state === 'failed') {
      z.state = 'failed';
      z.message = s.code === 'too_big' ? `${s.message || 'This is too much for one zip.'} Copy it to a station instead: select it, Copy, and Paste on the other station.` : (s.message || 'The zip could not be made.');
    } else if (s.state === 'ready') {
      z.state = 'ready';
      try { const t = await this.ticket(z.station, { zip: z.id }); z.state = 'given'; this.give(z.name || s.name, t.url); } catch (e) { z.state = 'failed'; z.message = say(e); }
    }
  }

  // ---- uploads ----
  pickFiles(view, dir) {
    this.inputFor = { view, dir };
    // (Laid out, as above: with the panel closed the box goes beside the listing that asked.)
    if (!this.input.isConnected) (view.el.parentNode || document.body).append(this.input);
    this.input.click();
  }

  // What was dropped: files, and folders walked to their ends. The
  // entries are taken before anything is waited for (a browser empties a
  // drop once its event is over).
  uploadDropped(dt, view, dir) {
    const entries = [], loose = [];
    for (const item of [...(dt.items || [])]) {
      if (item.kind !== 'file') continue;
      const entry = item.webkitGetAsEntry ? item.webkitGetAsEntry() : null;
      if (entry) entries.push(entry); else { const f = item.getAsFile(); if (f) loose.push(f); }
    }
    if (!entries.length && !loose.length) loose.push(...(dt.files || []));
    const station = view.station;
    (async () => {
      const list = loose.map(file => ({ file, parts: [] }));
      const all = (reader) => new Promise((res, rej) => { const out = []; const more = () => reader.readEntries((got) => { if (!got.length) return res(out); out.push(...got); more(); }, rej); more(); });
      const walk = async (entry, parts) => {
        if (entry.isFile) { const file = await new Promise((res, rej) => entry.file(res, rej)); list.push({ file, parts }); return; }
        const inside = [...parts, entry.name];
        list.push({ file: null, parts: inside });
        for (const kid of await all(entry.createReader())) await walk(kid, inside);
      };
      try { for (const entry of entries) await walk(entry, []); } catch (e) { toast(`Not everything that was dropped could be read: ${say(e)}`, 'error'); }
      if (list.length) this.upload(list, station, dir, view);
    })();
  }

  // One batch: `list` is [{file, parts}], `parts` the folders (inside
  // `dir`) the file goes into; a row with no file is a folder to make.
  upload(list, station, dir, view) {
    const files = list.filter(x => x.file);
    const batch = { kind: 'upload', station, dir, list, sep: view.sep, state: 'waiting', filesDone: 0, filesTotal: files.length, bytesDone: 0, bytesTotal: files.reduce((n, x) => n + x.file.size, 0), current: '', errors: [], skipped: 0, policy: null, cancelled: false };
    this.rows.push(batch);
    this.queue.push(batch);
    this.drawFoot();
    this.pump();
  }
  async pump() {
    if (this.pumping) return;
    this.pumping = true;
    while (this.queue.length) {
      const batch = this.queue.shift();
      batch.state = 'running';
      try { await this.runBatch(batch); } catch (e) { batch.errors.push({ path: batch.current, message: say(e) }); }
      batch.state = batch.cancelled ? 'cancelled' : 'done';
      batch.current = '';
      this.drawFoot();
      this.changed(batch.station);
      const sent = batch.filesDone;
      if (batch.errors.length) toast(`${plural(sent, 'file')} sent to ${batch.station}, ${batch.errors.length} not: ${batch.errors[0].message}`, 'error');
      else if (!batch.cancelled) toast(`${plural(sent, 'file')} sent to ${batch.station}${batch.skipped ? `, ${batch.skipped} skipped` : ''}`);
    }
    this.pumping = false;
  }
  async runBatch(batch) {
    const join = (dir, name) => (dir.endsWith(batch.sep) ? dir + name : dir + batch.sep + name);
    // A folder that was dropped is made once (added to, when it is there).
    const made = new Map([['', batch.dir]]);
    const folder = async (parts) => {
      const key = parts.join('\n');
      if (made.has(key)) return made.get(key);
      const parent = await folder(parts.slice(0, -1));
      const r = await change(batch.station, 'files.mkdir', { dir: parent, name: parts[parts.length - 1], merge: true });
      if (r.ok === false) throw new Error(r.message || 'the folder could not be made');
      made.set(key, r.path || join(parent, parts[parts.length - 1]));
      return made.get(key);
    };
    let last = 0;
    const tell = () => { if (Date.now() - last > 200) { last = Date.now(); this.drawFoot(); } };
    for (const x of batch.list) {
      if (batch.cancelled) return;
      const shown = [...x.parts, x.file ? x.file.name : ''].filter(Boolean).join('/');
      batch.current = shown;
      const before = batch.bytesDone;
      try {
        const dir = await folder(x.parts);
        if (!x.file) continue;
        const how = await this.send(batch, dir, x.file, (sent) => { batch.bytesDone = before + sent; tell(); });
        if (how === 'done') batch.filesDone++; else if (how === 'skipped') batch.skipped++;
      } catch (e) { batch.errors.push({ path: shown, message: say(e) }); }
      if (x.file) batch.bytesDone = before + x.file.size;
      tell();
    }
  }
  // One file, in pieces (the protocol is in DECISIONS.md, "An upload").
  // To another station a piece has six seconds, so it starts small and is
  // sized by how long the last one took.
  async send(batch, dir, file, sent) {
    const station = batch.station, near = own(station), id = hex(16), name = file.name;
    const most = near ? 4 * MIB : 2 * MIB;
    let piece = near ? MIB : 256 * KIB, offset = 0, tries = 0, gaps = 0;
    const put = (p) => env.api.on(station, 'files.put', { id, dir, name, ...p }, { within: near ? 60000 : 6000 });
    const drop = () => put({ abort: true }).catch(() => {});
    for (;;) {
      if (batch.cancelled) { drop(); return 'cancelled'; }
      const end = Math.min(file.size, offset + piece), last = end >= file.size, began = performance.now();
      let r;
      try {
        r = await put({ offset, data_b64: await toB64(file.slice(offset, end)), ...(last ? { done: true, conflict: batch.policy && batch.policy !== 'skip' ? batch.policy : 'fail', ...(file.lastModified ? { mtime_ms: Math.round(file.lastModified) } : {}) } : {}) });
      } catch (e) {
        // Sent again from the same place, half as large: the same bytes
        // there, and the station's part ends where this piece ends.
        if (++tries > 4) { drop(); throw e; }
        piece = Math.max(64 * KIB, piece >> 1);
        await sleep(300 * tries);
        continue;
      }
      if (r.ok === false && r.code === 'gap') {
        if (++gaps > 6) { drop(); throw new Error(r.message || 'the station lost what was sent'); }
        offset = Math.max(0, Math.min(file.size, Number(r.have) || 0));
        sent(offset);
        continue;
      }
      if (r.ok === false && r.code === 'exists') {
        const choice = await this.clash(batch, name, dir);
        if (choice === 'skip') { await drop(); return 'skipped'; }
        // The part stays at the station: only the end is sent again.
        for (let k = 0; ; k++) {
          try { r = await put({ offset: Number(r.received) || file.size, data_b64: '', done: true, conflict: choice }); break; } catch (e) { if (k >= 3) { drop(); throw e; } await sleep(400); }
        }
        if (r.ok === false) { drop(); throw new Error(r.message || 'it could not be saved under that name'); }
        return 'done';
      }
      if (r.ok === false) { drop(); throw new Error(r.message || 'the station refused it'); }
      tries = 0;
      if (r.done) return 'done';
      if (last) { drop(); throw new Error('the station did not finish the file'); }
      offset = Math.max(offset, Math.min(file.size, Number(r.received) || end));
      sent(offset);
      const took = performance.now() - began;
      if (took < (near ? 400 : 900)) piece = Math.min(most, piece * 2);
      else if (took > (near ? 4000 : 3000)) piece = Math.max(64 * KIB, piece >> 1);
    }
  }
  // Asked once for a batch when the owner ticks "for every other".
  async clash(batch, name, dir) {
    if (batch.policy) return batch.policy;
    const a = await askClash(`${name} is already there`, `${dir} on ${batch.station} already has a file called ${name}. Replace it, keep both (the new one gets a number), or skip this file.`, batch.filesTotal > 1);
    if (a.all) batch.policy = a.choice;
    return a.choice;
  }

  // ---- the foot: the clipboard, and what is on its way ----
  dismiss(row) { this.rows = this.rows.filter(r => r !== row); this.drawFoot(); }
  // In the panel when it is open; with it closed, under each listing in a cell.
  drawFoot() {
    const cells = [...this.cells.values()].map(c => c.footEl);
    if (!this.shown) { for (const foot of cells) this.drawFootIn(foot); return; }
    for (const foot of cells) { foot.replaceChildren(); foot.hidden = true; }
    this.drawFootIn(this.footEl);
  }
  drawFootIn(footEl) {
    const kids = [], c = this.clip;
    if (c) kids.push(h('div', { class: 'fx-clip', role: 'status' }, h('span', {}, `${items(c.paths.length)} ${c.op === 'cut' ? 'cut' : 'copied'} from ${c.station}`),
      h('button', { type: 'button', class: 'mini fx-clip-clear', 'aria-label': 'Forget what was copied', title: 'Forget what was copied', onclick: () => this.setClip(null) }, '✕')));
    const line = (o) => {
      const pct = o.total > 0 ? Math.max(0, Math.min(100, Math.round(100 * o.done / o.total))) : (o.state === 'running' ? 0 : 100);
      const open = o.errors && o.errors.length ? h('details', { class: 'fx-errs' }, h('summary', {}, o.except),
        h('ul', {}, o.errors.slice(0, 50).map(e => h('li', {}, h('bdi', { class: 'mono' }, e.path || ''), ' ', e.message || e.code || ''))), o.more ? h('p', { class: 'muted' }, `and ${o.more} more`) : null) : null;
      return h('div', { class: 'fx-xfer ' + o.state, dataset: { kind: o.kind, state: o.state } },
        h('div', { class: 'fx-xfer-top' }, h('span', { class: 'fx-xfer-what' }, o.what), h('span', { class: 'grow' }),
          o.cancel ? h('button', { type: 'button', class: 'mini fx-cancel', onclick: o.cancel }, 'Cancel') : null,
          o.close ? h('button', { type: 'button', class: 'mini fx-dismiss', 'aria-label': 'Take this off the list', onclick: o.close }, '✕') : null),
        o.state === 'running' ? h('div', { class: 'fx-meter', role: 'progressbar', 'aria-valuemin': 0, 'aria-valuemax': 100, 'aria-valuenow': pct }, h('i', { style: { width: pct + '%' } })) : null,
        h('div', { class: 'fx-xfer-says' + (o.bad ? ' bad' : '') }, o.says),
        open);
    };
    const progress = (fd, ft, bd, bt, cur, counting) => [`${fd} of ${ft}${counting ? '+' : ''} ${ft === 1 ? 'file' : 'files'}`, `${fmtSize(bd)} of ${fmtSize(bt)}`, cur].filter(Boolean).join(' · ');
    for (const r of this.rows) {
      if (r.kind === 'upload') {
        const run = r.state === 'running' || r.state === 'waiting';
        kids.push(line({ kind: 'upload', state: run ? 'running' : r.errors.length ? 'failed' : 'done', what: `Upload to ${r.station}`, done: r.bytesDone, total: r.bytesTotal,
          says: run ? (r.state === 'waiting' ? 'waiting its turn' : progress(r.filesDone, r.filesTotal, r.bytesDone, r.bytesTotal, r.current))
            : r.state === 'cancelled' ? `cancelled after ${plural(r.filesDone, 'file')}` : r.errors.length ? `${plural(r.filesDone, 'file')} sent` : `done: ${plural(r.filesDone, 'file')}, ${fmtSize(r.bytesTotal)}${r.skipped ? `, ${r.skipped} skipped` : ''}`,
          errors: r.errors, except: `done, except ${r.errors.length}`, bad: !run && r.errors.length > 0,
          cancel: run ? () => { r.cancelled = true; this.queue = this.queue.filter(b => b !== r); if (r.state === 'waiting') r.state = 'cancelled'; this.drawFoot(); } : null, close: run ? null : () => this.dismiss(r) }));
      } else if (r.kind === 'zip') {
        const s = r.status || {}, run = r.state === 'building' || r.state === 'ready';
        kids.push(line({ kind: 'zip', state: run ? 'running' : r.state === 'failed' ? 'failed' : 'done', what: `Zip on ${r.station}: ${r.name || 'download'}`, done: s.bytes_done || 0, total: s.bytes_total || 0,
          says: run ? `building · ${progress(s.files_done || 0, s.files_total || 0, s.bytes_done || 0, s.bytes_total || 0)}` : r.state === 'failed' ? r.message : `ready, ${fmtSize(s.size || 0)}: the download has started${(s.skipped || []).length ? ` (${s.skipped.length} left out)` : ''}`,
          bad: r.state === 'failed', cancel: run ? () => { change(r.station, 'files.job.cancel', { id: r.id }).catch(() => {}); r.state = 'failed'; r.message = 'cancelled'; this.drawFoot(); } : null, close: run ? null : () => this.dismiss(r) }));
      } else if (r.kind === 'link') {
        kids.push(h('div', { class: 'fx-xfer done', dataset: { kind: 'link' } }, h('div', { class: 'fx-xfer-top' }, h('span', { class: 'fx-xfer-what' }, 'Download: ', h('bdi', {}, r.name)), h('span', { class: 'grow' }),
          h('a', { class: 'btn mini fx-link', href: r.url, download: r.name, title: 'Good for a minute' }, 'Save'), h('button', { type: 'button', class: 'mini fx-dismiss', 'aria-label': 'Take this off the list', onclick: () => this.dismiss(r) }, '✕'))));
      }
    }
    for (const [key, j] of this.jobs) {
      const job = j.job, run = job.state === 'running', errs = (job.errors || []).length + (job.errors_more || 0), skipped = (job.skipped || []).length + (job.skipped_more || 0);
      const verb = job.op === 'move' ? 'Move' : 'Copy', where = job.to && job.to !== j.station ? `${j.station} to ${job.to}` : `on ${j.station}`;
      kids.push(line({ kind: 'job', state: run ? 'running' : job.state === 'done' && !errs ? 'done' : 'failed', what: `${verb} ${where}`, done: job.bytes_done || 0, total: job.bytes_total || 0,
        says: run ? progress(job.files_done || 0, job.files_total || 0, job.bytes_done || 0, job.bytes_total || 0, job.current ? String(job.current).split(/[\\/]/).pop() : '', job.counting)
          : job.state === 'failed' ? (job.message || 'failed') : job.state === 'cancelled' ? `cancelled after ${plural(job.files_done || 0, 'file')}`
          : errs ? `${plural(job.files_done || 0, 'file')} done` : `done: ${plural(job.files_done || 0, 'file')}, ${fmtSize(job.bytes_done || 0)}${skipped ? `, ${skipped} left out` : ''}`,
        errors: run ? null : (job.errors || []), more: job.errors_more || 0, except: `done, except ${errs}`, bad: !run && (job.state === 'failed' || errs > 0),
        cancel: run ? () => change(j.station, 'files.job.cancel', { id: j.id }).catch(e => toast(say(e), 'error')) : null, close: run ? null : () => { this.jobs.delete(key); this.drawFoot(); } }));
    }
    const open = new Set([...footEl.querySelectorAll('details[open]')].map(d => d.closest('.fx-xfer')?.querySelector('.fx-xfer-what')?.textContent));
    footEl.replaceChildren(...kids);
    for (const d of footEl.querySelectorAll('details')) if (open.has(d.closest('.fx-xfer')?.querySelector('.fx-xfer-what')?.textContent)) d.open = true;
    footEl.hidden = !kids.length;
  }

  // ---- where a drag is, and what a drop there would do ----
  dragOver(at) {
    for (const el of document.querySelectorAll('.fx-drop')) el.classList.remove('fx-drop');
    if (!at) { this.badge.hidden = true; return; }
    if (!this.badge.isConnected) document.getElementById('overlays').append(this.badge);
    (at.row || at.view.tree).classList.add('fx-drop');
    this.badge.textContent = at.says;
    this.badge.hidden = false;
    this.badge.style.left = Math.min(innerWidth - 180, at.x + 14) + 'px';
    this.badge.style.top = Math.min(innerHeight - 30, at.y + 16) + 'px';
  }
}

// A drag that ended anywhere (dropped outside, or given up) is over.
addEventListener('dragend', () => { if (panel) { panel.drag = null; panel.dragOver(null); } });

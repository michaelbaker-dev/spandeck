// A pane's header says more at a glance (DECISIONS.md, 19 Nov 2026, the
// owner's choice of "the more graphical chips"): the model, how full its
// context is, its folder's git branch, what the conversation has cost, and
// the processor and memory of its programs; and, only when it is so, that a
// newer version of its agent program is published, that the vault is not
// usable, and that it has unread mail.
//
// One module builds them for both faces of a pane: a row of chips under the
// folder in a conversation's bar, pieces of the one line of a terminal's
// strip, and on a phone a row of two chips and any warning that opens a
// sheet with everything.
//
// Rules kept here:
// - Everything is put on the page as text (`h()`): a branch's name, a
//   model's name and a version are data. An agent can name a branch anything.
// - Nothing here makes a station ask anybody anything. What the pane's view
//   says (model, context, cost) is the page's already; the rest is
//   `pane.glance`, which answers from what the station holds (glance.rs),
//   asked every four seconds of the stations that have a pane on screen and
//   of no other. A station from before has no such call: its panes show
//   the chips the page can make without it.
// - A chip that is not known is not there. Never a made-up number.

import { h, dialog, fmtCost, fmtTokens, modelShort, contextText, contextSays, usageGauge, usageTitle } from './ui.js';
import { branchIcon, keyIcon } from './icons.js';

export const CAP = 'pane_glance';
let env = null;
const held = new Map();       // station -> { data, sig, at, n, asking }
const watchers = new Set();   // { st, live, redraw }: a bar or a strip that shows chips

const slot = (st) => { let k = held.get(st); if (!k) { k = { data: null, sig: '', at: 0, n: 0, asking: false }; held.set(st, k); } return k; };

function ask(st, k) {
  k.asking = true;
  k.at = Date.now();
  env.api.on(st, 'pane.glance', {}, { within: 6000 }).then((d) => {
    k.n += 1;
    k.failed = 0;
    const sig = JSON.stringify([d?.panes, d?.programs]);
    k.data = d && typeof d === 'object' ? d : null;
    if (sig === k.sig) return false;
    k.sig = sig;
    return true;
  }, () => {
    // A station that has not answered twice running: what it said is from
    // before, and is no longer shown as if it were now.
    k.failed = (k.failed || 0) + 1;
    if (k.failed < 2 || !k.data) return false;
    k.data = null; k.sig = ''; k.n = 0;
    return true;
  }).then((changed) => {
    k.asking = false;
    if (!changed) return;
    // (One that cannot be drawn does not stop the others, or the asking.)
    for (const w of watchers) if (w.st === st && w.live()) { try { w.redraw(); } catch (e) { console.error(e); } }
  }).then(() => {
    // The first question is what starts the station working it out: the
    // second, soon after, has the answer.
    if (k.n === 1) setTimeout(() => { if (!k.asking && env.api.connected) ask(st, k); }, 400);
  });
}

// Once a second: the stations with a pane on screen are asked, each every
// four seconds.
function tick() {
  if (!env || document.visibilityState !== 'visible' || !env.api.connected) return;
  const now = Date.now(), live = new Set();
  for (const w of watchers) if (w.live()) live.add(w.st);
  for (const st of live) {
    if (!env.hasCap(st, CAP)) continue;
    const k = slot(st);
    if (k.asking) continue;
    if (now - k.at > 20000) k.n = 0;
    if (now - k.at >= (k.n < 1 ? 1000 : 4000)) ask(st, k);
  }
}

// `e`: { api, hasCap, isPhone, vault(st), openVault(), openAgents(), openInbox() }.
export function initChips(e) {
  env = e;
  setInterval(tick, 1000);
}

// A bar or a strip that shows a pane of `st`: it is drawn again when that
// station says something new, for as long as `live()`. Returns the way to
// stop.
export function watchGlance(st, live, redraw) {
  const w = { st, live, redraw };
  watchers.add(w);
  if (env) setTimeout(tick, 0);
  return () => watchers.delete(w);
}

// ------------------------------------------------------------------ facts

const known = (n) => n !== null && n !== undefined;
export function fmtBytes(n) {
  if (!known(n)) return '';
  const mb = n / 1048576;
  if (mb < 1) return '<1 MB';
  if (mb < 1000) return Math.round(mb) + ' MB';
  const gb = mb / 1024;
  return (gb >= 100 ? String(Math.round(gb)) : gb.toFixed(1)) + ' GB';
}
const VAULT_SAYS = { locked: 'Vault locked', unreachable: 'Vault unreachable', not_connected: 'Vault not connected', token_rejected: 'Vault token refused' };
const VAULT_WHY = {
  locked: 'The vault is locked, so agents here cannot use your keys. Unlock it on the vault\'s own page.',
  unreachable: 'The vault does not answer, so agents here cannot use your keys.',
  not_connected: 'This station is not connected to the vault, so agents here cannot use your keys.',
  token_rejected: 'The vault refuses this station\'s token, so agents here cannot use your keys.',
};

// What is known of a pane, and nothing else. `null` for what is not known.
export function factsOf(st, p) {
  const g = env?.hasCap(st, CAP) ? held.get(st)?.data : null;
  const row = (!p.exited && g?.panes?.[p.id]) || {};
  const u = p.agent !== 'shell' ? p.usage || null : null;
  const prog = p.agent !== 'shell' ? g?.programs?.[p.agent] || null : null;
  const f = { model: null, ctx: null, git: null, cost: null, tokens: null, cpu: null, mem: null, update: null, program: null, vault: null, mail: 0, load: null, station: st };
  if (u?.model) f.model = { short: modelShort(u.model), full: String(u.model) };
  if (u && known(u.context_tokens)) {
    const pct = typeof u.context_pct === 'number' ? u.context_pct : null;
    f.ctx = { text: contextText(u), pct, level: pct === null ? '' : pct >= 90 ? 'full' : pct >= 70 ? 'high' : '', says: contextSays(u) };
  }
  if (u && known(u.cost_estimate)) f.cost = { text: fmtCost(u.cost_estimate), says: `Estimated cost of this conversation at list prices: ${fmtCost(u.cost_estimate)}. An estimate, never a bill.` };
  if (u && known(u.total_tokens)) f.tokens = u.total_tokens;
  const git = row.git;
  if (git && typeof git.branch === 'string' && git.branch) {
    const n = Number(git.changed) || 0;
    f.git = { branch: git.branch.slice(0, 200), detached: git.detached === true, changed: n,
      more: n ? `${n} changed` : 'clean',
      says: `${git.detached === true ? 'Not on a branch: at commit' : 'Git branch:'} ${git.branch.slice(0, 200)}, ${n ? `${n} file${n === 1 ? '' : 's'} changed or new` : 'nothing changed'}` };
  }
  if (typeof row.cpu_pct === 'number') f.cpu = { text: Math.round(row.cpu_pct) + '%', says: `Processor used by this pane's programs: ${Math.round(row.cpu_pct)}% of one core over the last few seconds` };
  if (typeof row.mem_bytes === 'number') f.mem = { text: fmtBytes(row.mem_bytes), says: `Memory held by this pane's programs${Number(row.procs) > 0 ? ` (${Number(row.procs)} of them)` : ''}: ${fmtBytes(row.mem_bytes)}` };
  if (typeof row.mail === 'number' && row.mail > 0) f.mail = row.mail;
  if (prog && typeof prog.version === 'string') {
    f.program = { name: String(prog.name || p.agent_name || p.agent), version: prog.version };
    if (typeof prog.available === 'string' && prog.available) {
      f.update = { name: f.program.name, version: prog.version, available: prog.available,
        says: `${f.program.name} ${prog.available} is published; ${st} has ${prog.version}. As ${st} was last told when you asked. Opens Agent programs, where it is updated.` };
    }
  }
  if (typeof g?.load === 'number') f.load = g.load;
  // The vault is what an agent's keys come through: a shell says nothing of it.
  const v = env?.vault && p.agent !== 'shell' ? env.vault(st) : null;
  if (v && v.found) {
    const broken = v.health === 'problem' ? (v.checks || []).find(c => !c.ok) : null;
    if (v.state === 'connected' && !broken) f.vault = { ok: true, says: 'connected' };
    // "Not now" was the owner's answer: it is not said again on every pane.
    else if (v.state === 'not_connected' && v.prompt_dismissed) f.vault = { ok: true, says: 'not connected' };
    else {
      const label = VAULT_SAYS[v.state] || 'Vault needs a look';
      f.vault = { ok: false, label, level: v.state === 'not_connected' || v.state === 'connected' ? 'warn' : 'bad',
        says: `${String(broken?.detail || VAULT_WHY[v.state] || v.problem || label).slice(0, 300)} Opens Vault.` };
    }
  }
  return f;
}

// What a conversation's bar compares to know whether it would say what it
// says already. (Processor, memory and load are not on it: they are in the
// sheet, which reads them as it opens, so the bar is not built again every
// few seconds under a finger.)
export const chipsSig = (st, p) => JSON.stringify({ ...factsOf(st, p), cpu: null, mem: null, load: null });

// ------------------------------------------------------------------ chips

// One chip. With `run` it is a button. `drop`: what a narrow row lets go of
// (see `fit`); a chip with none is never dropped.
function chip(cls, kids, { says, run, drop } = {}) {
  const attrs = { class: 'gchip ' + cls, title: says, 'aria-label': says, dataset: drop ? { drop } : {} };
  return run ? h('button', { ...attrs, type: 'button', onclick: (e) => { e.stopPropagation(); e.currentTarget.blur(); run(e); } }, kids) : h('span', attrs, kids);
}
const meter = (ctx) => (ctx.pct === null ? null : h('span', { class: 'gmeter ' + ctx.level, dataset: { drop: 'meter' }, 'aria-hidden': 'true' }, h('i', { style: { width: Math.max(2, Math.min(100, ctx.pct)) + '%' } })));

function alwaysChips(f, usage) {
  return [
    f.model ? chip('g-model', h('b', {}, f.model.short), { says: `Model: ${f.model.full}`, run: usage, drop: 'model' }) : null,
    f.ctx ? chip('g-ctx ' + f.ctx.level, ['context ', h('b', {}, f.ctx.text), meter(f.ctx)], { says: f.ctx.says, run: usage, drop: 'ctx' }) : null,
    f.git ? chip('g-git', [branchIcon(), h('b', {}, f.git.branch), h('span', { class: 'g-more', dataset: { drop: 'gitmore' } }, ' · ' + f.git.more)], { says: f.git.says, drop: 'git' }) : null,
    f.cost ? chip('g-cost', h('b', {}, f.cost.text), { says: f.cost.says, run: usage, drop: 'cost' }) : null,
  ];
}
function warningChips(f, short = false) {
  return [
    // (Its short form is what a row with no room for the long one shows.)
    f.update ? chip('g-update warn', short ? 'Update available' : [h('span', { dataset: { drop: 'long' } }, `${f.update.name} ${f.update.available} available`), h('span', { dataset: { alt: 'long' }, hidden: true }, 'Update available')],
      { says: f.update.says, run: () => env.openAgents() }) : null,
    f.vault && !f.vault.ok ? chip('g-vault ' + f.vault.level, [keyIcon(), f.vault.label], { says: f.vault.says, run: () => env.openVault() }) : null,
    f.mail ? chip('g-mail info', `${f.mail} unread`, { says: `${f.mail} message${f.mail === 1 ? '' : 's'} for this pane that it has not read. Opens the Inbox.`, run: () => env.openInbox() }) : null,
  ];
}

// A narrow row lets go of the least needed first, each whole (a number cut
// in half would be another number): the cost, then how many files are
// changed, the gauge, the branch, an update's long words for its short
// ones, the model, the context. Never a warning: with nothing left to let
// go of, the row takes a second line.
const DROP = ['cost', 'gitmore', 'meter', 'git', 'long', 'model', 'ctx'];
function letGo(box, fits, order) {
  for (const el of box.querySelectorAll('[data-drop]')) el.hidden = false;
  for (const el of box.querySelectorAll('[data-alt]')) el.hidden = true;
  for (const key of order) {
    if (fits()) return true;
    for (const el of box.querySelectorAll(`[data-drop="${key}"]`)) el.hidden = true;
    for (const el of box.querySelectorAll(`[data-alt="${key}"]`)) el.hidden = false;
  }
  return fits();
}
function fit(row) {
  if (!row.isConnected) return;
  if (row.classList.contains('strip')) return fitStrip(row);
  row.classList.remove('wraps');
  if (!letGo(row, () => row.scrollWidth <= row.clientWidth + 1, DROP)) row.classList.add('wraps');
}
const fitter = typeof ResizeObserver === 'function' ? new ResizeObserver((all) => {
  for (const one of all) { if (one.target.isConnected) fit(one.target); else if (!one.target.classList.contains('strip')) fitter.unobserve(one.target); }
}) : null;

// A terminal's strip is one line that other things share (the folder, a
// reason, the buttons), and they give way by the strip's own rules
// (app.css). Before any of them is squeezed, the pieces from here are let
// go of whole, the least needed first: the cost, how many files are
// changed, the branch, an update's long words, the model, the context,
// then the processor and the memory. Called after the strip is drawn, and
// when its width changes.
const STRIP_DROP = ['cost', 'gitmore', 'git', 'long', 'model', 'ctx', 'use'];
const watched = new WeakSet();
export function fitStrip(strip) {
  if (fitter && !watched.has(strip)) { watched.add(strip); fitter.observe(strip); }
  if (!strip.isConnected || !strip.querySelector('[data-drop]')) return;
  const cs = getComputedStyle(strip), gap = parseFloat(cs.columnGap) || 0;
  const pad = (parseFloat(cs.paddingLeft) || 0) + (parseFloat(cs.paddingRight) || 0), room = strip.clientWidth;
  // What everything on the strip would take at its full width (the folder
  // at no more than the width it is allowed).
  const fits = () => {
    let need = pad, n = 0;
    for (const el of strip.children) {
      if (el.hidden) continue;
      n += 1;
      if (el.classList.contains('grow')) continue;
      const cap = parseFloat(getComputedStyle(el).maxWidth);
      need += Math.min(Math.max(el.scrollWidth, el.offsetWidth), Number.isFinite(cap) ? cap : Infinity);
    }
    return need + gap * Math.max(0, n - 1) <= room + 1;
  };
  letGo(strip, fits, STRIP_DROP);
}
function fitted(row) {
  if (fitter) fitter.observe(row);
  return row;
}

// The row under the folder in a conversation's bar, at a computer. Null
// when there is nothing to say. `usage`: opens what is known of the usage.
export function chatChips(st, p, usage) {
  const f = factsOf(st, p);
  const kids = [...alwaysChips(f, usage), ...warningChips(f)].filter(Boolean);
  return kids.length ? fitted(h('div', { class: 'gchips', role: 'group', 'aria-label': 'At a glance' }, kids)) : null;
}

// On a phone: the model, the context and any warning in one row under the
// bar; a tap anywhere on it opens the sheet with everything.
export function phoneChips(st, p) {
  const f = factsOf(st, p);
  const lone = (c) => (c ? h('span', { class: c.className, dataset: { ...c.dataset } }, ...c.childNodes) : null);
  const kids = [
    f.model ? chip('g-model', h('b', {}, f.model.short), { drop: 'model' }) : null,
    f.ctx ? chip('g-ctx ' + f.ctx.level, ['context ', h('b', {}, f.ctx.text)], { drop: 'ctx' }) : null,
    // (Drawn as chips, not as buttons of their own: the row is the button.)
    ...warningChips(f, true).map(lone),
  ].filter(Boolean);
  if (!kids.length && !f.git && !f.mem) return null;
  if (!kids.length) kids.push(chip('g-more', 'Branch, processor and memory'));
  const says = ['At a glance', f.model ? `model ${f.model.full}` : null, f.ctx ? f.ctx.says : null, f.update ? 'an update is available' : null, f.vault && !f.vault.ok ? f.vault.label : null, f.mail ? `${f.mail} unread` : null].filter(Boolean).join(', ') + '. Opens everything known of this pane.';
  return fitted(h('button', { type: 'button', class: 'gchips gchips-phone', 'aria-label': says, 'aria-haspopup': 'dialog', onclick: (e) => { e.currentTarget.blur(); glanceSheet(st, p); } }, kids,
    h('span', { class: 'g-open', 'aria-hidden': 'true' }, '›')));
}

// Everything, as lines of a name and what is known of it. It follows what
// the station says while it is open.
export function glanceSheet(st, p) {
  let off = () => {};
  const box = dialog('At a glance', (close) => {
    const fact = (label, value, cls, act) => (value ? h('div', { class: 'fact' + (cls ? ' ' + cls : '') }, h('span', { class: 'muted' }, label), h('b', {}, value),
      act ? h('button', { type: 'button', class: 'mini g-act', onclick: () => { close(); act.run(); } }, act.label) : null) : null);
    const facts = h('div', { class: 'facts' });
    let said = '';
    const fill = () => {
      const f = factsOf(st, p), sig = JSON.stringify(f);
      if (sig === said) return;
      said = sig;
      // replaceChildren would print a missing line as the word "null".
      facts.replaceChildren(...[
        fact('Model', f.model?.full),
        fact('Context used', f.ctx ? (f.ctx.pct === null ? f.ctx.text + ' tokens' : f.ctx.text) : ''),
        fact('Branch', f.git ? `${f.git.branch} · ${f.git.more}` : ''),
        fact('This conversation', [f.cost ? f.cost.text + ' (estimated)' : null, known(f.tokens) ? fmtTokens(f.tokens) + ' tokens' : null].filter(Boolean).join(' · ')),
        f.program ? fact(f.program.name, f.update ? `${f.update.version} → ${f.update.available} available` : f.program.version, f.update ? 'g-warn' : '', f.update ? { label: 'Update…', run: () => env.openAgents() } : null) : null,
        f.vault ? fact('Vault', f.vault.ok ? f.vault.says : f.vault.label.replace(/^Vault /, ''), f.vault.ok ? '' : 'g-' + f.vault.level, f.vault.ok ? null : { label: 'Vault…', run: () => env.openVault() }) : null,
        fact('Mail', f.mail ? `${f.mail} unread` : ''),
        fact('Pane', [f.cpu ? 'CPU ' + f.cpu.text : null, f.mem ? f.mem.text : null].filter(Boolean).join(' · ')),
        fact(st, known(f.load) ? 'load ' + f.load.toFixed(1) : ''),
      ].filter(Boolean));
    };
    fill();
    off = watchGlance(st, () => facts.isConnected, fill);
    return h('div', { class: 'usage-sheet glance-sheet' }, facts,
      h('p', { class: 'muted small' }, 'Only what is known is listed. A cost is an estimate at list prices, never a bill. Processor is the share of one core this pane\'s programs used over the last few seconds; memory is what they hold now.'),
      h('div', { class: 'row end sheet-foot' }, h('button', { type: 'button', class: 'primary', onclick: close }, 'Close')));
  }, { onClose: () => off() });
  box.body.parentNode.classList.add('sheet', 'chat-sheet');
}

// The pieces of a terminal's one line, at a computer: `left` goes after the
// folder (model, context, branch, cost, then any warning), `right` before
// the Chat button (processor and memory). A shell has no model, context or
// cost. What gives way in a narrow strip is said in app.css; a warning
// never does.
export function stripChips(st, p, usage) {
  if (!env || env.isPhone()) return { left: [], right: [], old: usageGauge(p.usage, usage) };
  const f = factsOf(st, p);
  const act = usage ? { role: 'button', tabIndex: 0, onclick: usage, onkeydown: (e) => { if (e.key === 'Enter') usage(e); } } : {};
  const gauge = usageGauge(p.agent !== 'shell' ? p.usage : null, usage, true);
  gauge.forEach((el, i) => { el.dataset.drop = i ? 'ctx' : 'model'; });
  return {
    left: [
      ...gauge,
      f.git ? h('span', { class: 'strip-git', title: f.git.says, 'aria-label': f.git.says, dataset: { drop: 'git' } }, branchIcon(), h('b', {}, f.git.branch), h('span', { class: 'g-more', dataset: { drop: 'gitmore' } }, ' · ' + f.git.more)) : null,
      f.cost ? h('span', { class: 'strip-cost', title: f.cost.says + (p.usage ? '\n' + usageTitle(p.usage) : ''), 'aria-label': f.cost.says, dataset: { drop: 'cost' }, ...act }, h('b', {}, f.cost.text)) : null,
      ...warningChips(f),
    ].filter(Boolean),
    right: [
      f.cpu ? h('span', { class: 'strip-use strip-cpu', title: f.cpu.says, 'aria-label': f.cpu.says, dataset: { drop: 'use' } }, 'CPU ', h('b', {}, f.cpu.text)) : null,
      f.mem ? h('span', { class: 'strip-use strip-mem', title: f.mem.says, 'aria-label': f.mem.says, dataset: { drop: 'use' } }, 'Mem ', h('b', {}, f.mem.text)) : null,
    ].filter(Boolean),
    old: [],
  };
}

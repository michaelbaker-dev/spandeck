// Small DOM helpers: elements, context menus, dialogs, toasts.

export function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k === 'class') el.className = v;
    else if (k === 'style' && typeof v === 'object') Object.assign(el.style, v);
    else if (k.startsWith('on')) el.addEventListener(k.slice(2).toLowerCase(), v);
    else if (k === 'dataset') Object.assign(el.dataset, v);
    else if (k in el && k !== 'list') el[k] = v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat(Infinity)) {
    if (c === undefined || c === null || c === false) continue;
    // Text from stations and agents is always inserted as text, never HTML.
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

const overlays = () => document.getElementById('overlays');

let openMenu = null;
export function closeMenu() { if (openMenu) { openMenu.remove(); openMenu = null; } }

// items: [{label, action, danger, disabled, hint} | '-' | {title} | {note}]
export function showMenu(x, y, items) {
  closeMenu();
  const menu = h('div', { class: 'menu', role: 'menu' });
  for (const it of items.filter(Boolean)) {
    if (it === '-') { menu.append(h('div', { class: 'menu-sep' })); continue; }
    if (it.title) { menu.append(h('div', { class: 'menu-title' }, it.title)); continue; }
    if (it.note) { menu.append(h('div', { class: 'menu-note' }, it.note)); continue; }
    menu.append(h('button', {
      class: 'menu-item' + (it.danger ? ' danger' : ''), role: 'menuitem', disabled: it.disabled,
      onclick: () => { closeMenu(); it.action(); },
    }, h('span', {}, it.label), it.hint ? h('span', { class: 'menu-hint' }, it.hint) : null));
  }
  overlays().append(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = Math.max(4, Math.min(x, innerWidth - r.width - 4)) + 'px';
  menu.style.top = Math.max(4, Math.min(y, innerHeight - r.height - 4)) + 'px';
  openMenu = menu;
}
addEventListener('mousedown', (e) => { if (openMenu && !openMenu.contains(e.target)) closeMenu(); }, true);
// Esc closes an open menu and does nothing else: it neither zooms the page out
// nor reaches the terminal under the menu.
addEventListener('keydown', (e) => { if (e.key === 'Escape' && openMenu) { e.stopPropagation(); closeMenu(); } }, true);
addEventListener('blur', closeMenu);

export function toast(text, kind = 'info') {
  const t = h('div', { class: 'toast ' + kind, role: 'status' }, text);
  let box = document.getElementById('toasts');
  if (!box) { box = h('div', { id: 'toasts' }); overlays().append(box); }
  box.append(t);
  setTimeout(() => t.remove(), kind === 'error' ? 7000 : 3500);
}

// Put text on the clipboard. The deck is usually opened over plain http on the
// tailnet, where the browser offers no clipboard object: then (or when the
// browser refuses) the text is copied the old way, from a box off screen.
export async function copyText(text, said = 'Copied') {
  const viaBox = () => {
    const was = document.activeElement;
    const box = h('textarea', { class: 'copy-buffer', readOnly: true, 'aria-hidden': 'true', tabIndex: -1 });
    box.value = text;
    overlays().append(box);
    box.focus();
    box.select();
    let ok = false;
    try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
    box.remove();
    if (was && was.focus) was.focus();
    return ok;
  };
  let ok = false;
  if (navigator.clipboard?.writeText) { try { await navigator.clipboard.writeText(text); ok = true; } catch (e) { ok = false; } }
  if (!ok) ok = viaBox();
  toast(ok ? said : 'The browser did not allow copying. Select the text and use the browser\'s own Copy.', ok ? 'info' : 'error');
  return ok;
}

// A modal dialog. `build(close)` returns the body; Esc and the backdrop close it.
// `beside` puts it at the right with the page left clear to see, and a click
// on the page does not close it: for the dialog whose changes show on the page.
export function dialog(title, build, opts = {}) {
  const close = () => { back.remove(); removeEventListener('keydown', onKey, true); opts.onClose && opts.onClose(); };
  const onKey = (e) => { if (e.key === 'Escape') { e.stopPropagation(); close(); } };
  const box = h('div', { class: 'dialog' + (opts.wide ? ' wide' : ''), role: 'dialog', 'aria-label': title },
    h('div', { class: 'dialog-head' }, h('h2', {}, title), h('button', { class: 'icon-btn', title: 'Close', onclick: close }, '✕')));
  const body = h('div', { class: 'dialog-body' });
  box.append(body);
  const back = h('div', { class: 'backdrop' + (opts.beside ? ' beside' : ''), onmousedown: (e) => { if (e.target === back && !opts.beside) close(); } }, box);
  overlays().append(back);
  addEventListener('keydown', onKey, true);
  body.append(build(close, body));
  const first = body.querySelector('input,select,textarea,button');
  if (first) first.focus();
  return { close, body };
}

export function confirmBox(title, text, okLabel, action, danger = true) {
  dialog(title, (close) => h('div', {},
    h('p', {}, text),
    h('div', { class: 'row end' },
      h('button', { onclick: close }, 'Cancel'),
      h('button', { class: danger ? 'primary danger' : 'primary', onclick: () => { close(); action(); } }, okLabel))));
}

export function field(label, input, hint) {
  return h('label', { class: 'field' }, h('span', {}, label), input, hint ? h('small', {}, hint) : null);
}

export function ago(ms) {
  if (!ms) return '';
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return s + 's';
  if (s < 3600) return Math.floor(s / 60) + 'm ' + (s % 60) + 's';
  return Math.floor(s / 3600) + 'h ' + Math.floor((s % 3600) / 60) + 'm';
}

export const STATE_LABEL = { idle: 'idle', working: 'working', needs_you: 'needs you', done: 'done', error: 'error' };
export function stateOf(p) { return p.exited ? 'exited' : p.state; }
// A pane that is held (`held` in its view): stopped and staying stopped, or
// started again after a restart and waiting for the owner. It takes no mail
// until it is released. What the station says is put on the page as text.
export function heldWords(held) {
  if (!held) return '';
  const at = (ms) => new Date(Number(ms) || 0).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  const parts = [];
  const s = held.stopped, r = held.restored;
  if (s && s.reached === false) parts.push('this program is not reading input: Stop could not reach it; use Kill');
  else if (s) parts.push(`stopped by ${String(s.by || 'you').slice(0, 60)} at ${at(s.at_ms)}`);
  if (r) parts.push('started again after a restart');
  parts.push(held.mail > 0 ? `mail is waiting (${Number(held.mail)})` : 'takes no mail until released');
  return parts.join(' · ');
}
// ---- the folder-trust question (the station's foldertrust.rs) ----
// What a pane says while its program asks whether to trust the folder and
// nothing was pressed for the owner, by the station's reason.
export function trustAsksWords(p) {
  const asks = p?.trust?.asks;
  if (!asks) return '';
  const name = p.agent_name || 'The program';
  return ({
    not_chosen: `${name} asks whether to trust this folder. You did not choose that when the pane was started, so nothing was pressed.`,
    own_choice_is_no: `${name} asks whether to trust this folder. Its own choice on that screen is "No, exit", so spandeck pressed nothing: one tap here moves it to "Yes" and confirms.`,
    not_the_known_screen: `${name} asks whether to trust this folder, and its screen is not the one spandeck knows (it may list what the folder's settings switch on, or be a newer version). Read it before you answer; nothing was pressed.`,
    switched_off: `${name} asks whether to trust this folder. Nothing is pressed for you here: you typed into this pane, or it is switched off on this station.`,
  })[asks.why] || `${name} asks whether to trust this folder.`;
}
// Said on a pane for ten minutes after spandeck answered for the owner.
export function trustAnsweredWords(p) {
  const a = p?.trust?.answered;
  if (!a || a.how !== 'standing' || Date.now() - Number(a.at_ms || 0) > 600000) return '';
  return `spandeck answered ${p.agent_name || 'the program'}'s folder-trust question, as you chose for this ${p.trust.chosen?.why === 'project' ? 'project' : 'folder'}`;
}
// The card a pane shows on a phone, and in its conversation, while its
// program asks: the words, the folder, the owner's one tap with the choice
// to remember it, and (where given) the way to the terminal.
export function trustCard(p, tap, terminal) {
  const asks = p.trust.asks;
  const keep = h('input', { type: 'checkbox', style: { width: 'auto' } });
  return h('div', { class: 'chat-card wait trust-card' },
    h('b', {}, trustAsksWords(p)),
    h('div', { class: 'muted small mono trust-folder' }, p.trust.folder || p.cwd || ''),
    asks.button ? h('label', { class: 'row' }, keep, `Remember this for ${p.agent_name || 'this program'} in this folder`) : null,
    h('div', { class: 'row' },
      asks.button ? h('button', { type: 'button', class: 'primary trust-tap', onclick: (e) => { e.currentTarget.blur(); tap(keep.checked); } }, 'Trust this folder') : null,
      terminal ? h('button', { type: 'button', onclick: terminal }, 'Open terminal') : null));
}
export const TRUST_WARNING = 'Do not tick it for a folder copied from somewhere you do not trust: what is in it can then run on this machine as you.';
// The tick in the New pane dialog and the New session sheet. `ask`: calls
// the station (`foldertrust.choice`). `get`: what is chosen in the dialog
// now. The row is hidden for a program that does not ask, and on a
// station from before this.
export function trustTick(ask, get) {
  const box = h('input', { type: 'checkbox', style: { width: 'auto' }, 'aria-describedby': 'trust-note' });
  const label = h('span', {});
  const note = h('small', { class: 'muted trust-note', id: 'trust-note' });
  const row = h('div', { class: 'trust-row', hidden: true }, h('label', { class: 'row' }, box, label), note);
  let n = 0, shown = false, touched = false;
  box.addEventListener('change', () => { touched = true; });
  const refresh = async () => {
    const mine = ++n, at = get();
    let r = null;
    if (at.agent && at.agent !== 'shell') {
      try { r = await ask({ agent: at.agent, project: at.project || undefined, cwd: at.cwd || undefined }); } catch (e) { r = null; }
    }
    if (mine !== n) return;
    shown = !!(r && r.asks);
    row.hidden = !shown;
    if (!shown) return;
    if (!touched || box.dataset.for !== r.folder + '|' + r.agent) { box.checked = r.on === true; touched = false; }
    box.dataset.for = r.folder + '|' + r.agent;
    label.textContent = `Trust this folder for ${r.agent_name} if it asks`;
    // replaceChildren would print a missing piece as the word "null".
    note.replaceChildren(...[
      h('span', { class: 'mono trust-folder' }, r.folder), h('br'),
      r.why === 'folder' ? h('span', {}, 'On because you ticked it for this folder before. Untick it to stop. ') : null,
      r.why === 'project' ? h('span', {}, `On because this project's agents may start without asking you. `) : null,
      h('span', {}, r.allows + ' ' + r.reach + ' '),
      h('span', { class: 'trust-warn' }, TRUST_WARNING),
      r.pressed === false ? h('span', {}, ` ${r.agent_name}'s own choice on that screen is "No, exit", so spandeck presses nothing there: the pane will offer "Trust this folder", one tap.`) : null].filter(Boolean));
  };
  return { row, refresh, value: () => (shown ? box.checked : undefined) };
}
export const RELEASE_SAYS = 'Release: this agent takes mail again. Typing into the pane does the same. A paused pane stays paused.';
export function stateRank(p) {
  return { needs_you: 0, error: 1, working: 2, done: 3, idle: 4, exited: 5 }[stateOf(p)] ?? 6;
}
export function dot(p) {
  const s = stateOf(p);
  return h('span', { class: 'dot ' + s, title: p.needs_reason || STATE_LABEL[s] || s });
}

// ---- usage: tokens, context, estimated cost ----
// Numbers for people (12.4k, 1.2M); the exact figure goes in the tooltip.
export function fmtTokens(n) {
  if (n === null || n === undefined) return '—';
  const one = (v, unit) => (v >= 100 ? String(Math.round(v)) : v.toFixed(1).replace(/\.0$/, '')) + unit;
  if (n < 1000) return String(n);
  if (n < 1e6) return one(n / 1e3, 'k');
  if (n < 1e9) return one(n / 1e6, 'M');
  return one(n / 1e9, 'B');
}
export const exact = (n) => (n === null || n === undefined) ? 'unknown' : Number(n).toLocaleString('en-US');
export function fmtCost(c) {
  if (c === null || c === undefined) return '—';
  if (c === 0) return '$0';
  if (c < 0.01) return '<$0.01';
  return '$' + (c >= 100 ? Math.round(c).toLocaleString('en-US') : c.toFixed(2));
}
// "claude-opus-5-5-20260901" is "opus-5-5" in a narrow strip.
export function modelShort(model) {
  if (!model) return '';
  return String(model).replace(/\[.*$/, '').replace(/^.*\//, '').replace(/[-@]\d{8}$/, '').replace(/-\d{4}-\d{2}-\d{2}$/, '').replace(/^claude-/, '');
}
// The context as a share of the window, or as a count when no window is known.
export function contextText(u) {
  if (!u || u.context_tokens === null || u.context_tokens === undefined) return '';
  return u.context_pct !== null && u.context_pct !== undefined ? `${u.context_pct}%` : fmtTokens(u.context_tokens);
}
// One short phrase for a muted line: "opus-5-5 · 62%".
export function usageBrief(u) {
  if (!u || !u.model) return '';
  return [modelShort(u.model), contextText(u)].filter(Boolean).join(' · ');
}
export function costLine(u) {
  if (!u || u.total_tokens === null || u.total_tokens === undefined) return '';
  if (!u.price_source) return 'Cost: price list not fetched yet';
  const missing = (u.unpriced_models || []).map(m => `no price for ${m}`).join(', ');
  if (u.cost_estimate === null || u.cost_estimate === undefined) return 'Cost: ' + (missing || 'not estimated');
  return `Estimated cost at list prices: ${fmtCost(u.cost_estimate)}${missing ? ` (${missing})` : ''}`;
}
// The full numbers, for a tooltip.
export function usageTitle(u) {
  if (!u) return '';
  if (u.total_tokens === null || u.total_tokens === undefined) return u.note === 'not reported by this agent' ? 'Usage is not reported by this agent' : 'No usage reported yet';
  return [
    `Model: ${u.model || 'unknown'}`,
    u.context_tokens !== null && u.context_tokens !== undefined
      ? `Context: ${exact(u.context_tokens)} tokens` + (u.context_window ? ` of ${exact(u.context_window)} (${u.context_pct}%; window from the ${u.context_window_source})` : ' (window not known)') : null,
    `Input ${exact(u.input_tokens)} · output ${exact(u.output_tokens)}`,
    `Cache read ${exact(u.cache_read_tokens)} · cache write ${exact(u.cache_write_tokens)}`,
    `Total ${exact(u.total_tokens)} tokens` + (u.turns !== null && u.turns !== undefined ? ` in ${u.turns} turn${u.turns === 1 ? '' : 's'}` : ''),
    costLine(u),
  ].filter(Boolean).join('\n');
}
// The model's short name and a small context gauge: two pieces, so a narrow
// strip can drop them one after the other.
export function usageGauge(u, onclick) {
  if (!u || !u.model) return [];
  const title = usageTitle(u);
  const pct = u.context_pct;
  const level = pct >= 90 ? ' full' : pct >= 70 ? ' high' : '';
  const ctx = contextText(u);
  const attrs = (cls) => ({ class: cls, title, ...(onclick ? { role: 'button', tabIndex: 0, onclick, onkeydown: (e) => { if (e.key === 'Enter') onclick(e); } } : {}) });
  return [
    h('span', { ...attrs('usage-model'), 'aria-label': `Model ${u.model}` }, modelShort(u.model)),
    ctx ? h('span', { ...attrs('usage-ctx' + level), 'aria-label': `Context ${ctx}` },
      pct !== null && pct !== undefined ? h('span', { class: 'gauge' }, h('i', { style: { width: Math.max(2, Math.min(100, pct)) + '%' } })) : null, ctx) : null,
  ].filter(Boolean);
}

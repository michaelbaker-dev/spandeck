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
export function dialog(title, build, opts = {}) {
  const close = () => { back.remove(); removeEventListener('keydown', onKey, true); opts.onClose && opts.onClose(); };
  const onKey = (e) => { if (e.key === 'Escape') { e.stopPropagation(); close(); } };
  const box = h('div', { class: 'dialog' + (opts.wide ? ' wide' : ''), role: 'dialog', 'aria-label': title },
    h('div', { class: 'dialog-head' }, h('h2', {}, title), h('button', { class: 'icon-btn', title: 'Close', onclick: close }, '✕')));
  const body = h('div', { class: 'dialog-body' });
  box.append(body);
  const back = h('div', { class: 'backdrop', onmousedown: (e) => { if (e.target === back) close(); } }, box);
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
export function stateRank(p) {
  return { needs_you: 0, error: 1, working: 2, done: 3, idle: 4, exited: 5 }[stateOf(p)] ?? 6;
}
export function dot(p) {
  const s = stateOf(p);
  return h('span', { class: 'dot ' + s, title: p.needs_reason || STATE_LABEL[s] || s });
}

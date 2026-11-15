// Appearance on a phone: a sheet, not the editor (lookedit.js is made for a
// wide screen and docks at its side). Three things can be done here:
//
// - choose the deck's theme, which is the fleet's: it goes through `look.set`
//   like the editor's Save, so it is the same on every device and station;
// - set the size of the text in a conversation and in a terminal, which is
//   this device's own (look.js, "this device's own text sizes"): kept in this
//   browser, never sent to a station;
// - choose the terminal's font from the ones that are drawn on this device,
//   which is the fleet's like the theme.
//
// Colours, a layout's or a pane's own look, and importing themes stay on a
// computer. Names of themes and fonts come from files and other stations:
// they are put on the page as text, and a colour or a font reaches it only
// as the value of a custom property, as everywhere else (look.js).

import { h, dialog, toast } from './ui.js';
import { KEYS, BASE, DEFAULT_THEME, DEVICE_SIZES, lookState, allThemes, themeById, pageLook, rememberPage, fontList, fontHere, sameFont, warnings, chatSize, termSize, setDeviceSize } from './look.js';

const SAME = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const SYSTEM = ['ui-monospace', 'monospace'];
// What a theme decides and a choice of theme must show: a single change to
// one of these, made in the editor on top of the last theme, goes when
// another theme is chosen here (as it does in the editor). Fonts, sizes and
// the cursor are kept.
const ofTheme = (k) => KEYS[k].kind === 'colour' || k === 'deck.scheme' || k === 'term.contrast';

// env: { api, apply(): show the looks again, reload(): read the document
// again, update(): draw the page again }
// scope: { layoutKey (the layout on screen), paneKey, name (opened from a pane's menu) }
export function phoneLook(env, scope = {}) {
  let asked = 0;
  const deck = () => lookState.preview?.choice || lookState.doc.deck || { theme: null, set: {} };
  const themed = () => ({ ...BASE, ...(themeById(deck().theme || DEFAULT_THEME)?.settings || {}) });
  const look = () => ({ ...themed(), ...(deck().set || {}) });

  // Shown at once, then saved; if the station refuses, what it holds is shown again.
  const save = async (choice) => {
    const n = ++asked;
    lookState.preview = { level: 'deck', choice };
    env.apply();
    draw();
    let failed = null;
    try {
      await env.api.call('look.set', { level: 'deck', choice: choice.theme || Object.keys(choice.set).length ? { theme: choice.theme || undefined, set: choice.set } : null });
    } catch (e) { failed = e; }
    if (n !== asked) return;
    await env.reload();
    if (n !== asked) return;
    lookState.preview = null;
    rememberPage(pageLook(null));
    env.apply();
    draw();
    if (failed) toast('Not changed: ' + failed.message, 'error');
  };

  // ---- theme ----
  const themes = h('div', { class: 'pl-themes', role: 'radiogroup', 'aria-label': 'Theme' });
  const warnBox = h('div', { class: 'look-warn', role: 'status' });
  const ownNote = h('p', { class: 'muted small pl-own' });
  const swatch = (t) => {
    const s = { ...BASE, ...t.settings }, on = (deck().theme || DEFAULT_THEME) === t.id;
    const b = h('button', { type: 'button', class: 'pl-theme' + (on ? ' on' : ''), role: 'radio', 'aria-checked': String(on), dataset: { theme: t.id },
      onclick: () => { if (!on) save({ theme: t.id, set: Object.fromEntries(Object.entries(deck().set || {}).filter(([k]) => !ofTheme(k))) }); } },
    h('span', { class: 'pl-swatch', 'aria-hidden': 'true' }, h('span', { class: 'pl-aa' }, 'Aa'),
      ['blue', 'green', 'amber', 'red'].map(c => h('i', { style: { background: `var(--sw-${c})` } }))),
    h('span', { class: 'pl-name' }, t.name), h('span', { class: 'pl-tick', 'aria-hidden': 'true' }, on ? '✓' : ''));
    for (const [name, key] of [['bg', 'deck.bg'], ['panel', 'deck.panel'], ['text', 'deck.text'], ['line', 'deck.line'], ['blue', 'deck.blue'], ['green', 'deck.green'], ['amber', 'deck.amber'], ['red', 'deck.red']]) b.style.setProperty('--sw-' + name, s[key]);
    return b;
  };

  // ---- text size: this device's own ----
  const sizeRow = (which, label, read, sample, note) => {
    const b = DEVICE_SIZES[which];
    const value = h('output', { class: 'pl-value', 'aria-live': 'polite' });
    const step = (by) => { setDeviceSize(which, read().px + by); env.apply(); draw(); };
    const less = h('button', { type: 'button', class: 'pl-step', 'aria-label': `${label} text smaller`, onclick: () => step(-1) }, 'A−');
    const more = h('button', { type: 'button', class: 'pl-step', 'aria-label': `${label} text larger`, onclick: () => step(1) }, 'A+');
    const el = h('div', { class: 'pl-size', role: 'group', 'aria-label': `${label} text size`, dataset: { size: which } },
      h('div', { class: 'pl-size-row' }, h('b', {}, label), h('span', { class: 'grow' }), less, value, more), sample, note ? h('small', { class: 'muted' }, note) : null);
    return { el, sync: () => {
      const s = read();
      value.textContent = `${s.px} px`;
      value.title = s.chosen ? `This device's own. The deck's is ${s.deck} px.` : 'The deck\'s size';
      less.disabled = s.px <= b.min;
      more.disabled = s.px >= b.max;
    } };
  };
  const termSample = h('pre', { class: 'pl-term-sample', 'aria-hidden': 'true' }, 'you@station:~/code$ git status\nOn branch main   => != 0O 1lI');
  const chat = sizeRow('chat', 'Conversation', () => chatSize(pageLook(scope.layoutKey)),
    h('div', { class: 'pl-chat-sample', 'aria-hidden': 'true' }, h('div', { class: 'chat-bubble' }, 'Count the herons.'), h('div', {}, 'There are ', h('b', {}, '37'), ' herons in ', h('code', { class: 'md-code-in' }, 'survey.csv'), '.')));
  const term = sizeRow('term', 'Terminal', () => termSize(pageLook(scope.layoutKey)), termSample,
    'For a pane fitted to this screen ("Fit to screen"). A pane you are only watching keeps its own size and is shown whole, shrunk to fit.');
  const reset = h('button', { type: 'button', class: 'pl-reset', onclick: () => { setDeviceSize('chat', null); setDeviceSize('term', null); env.apply(); draw(); } }, 'Reset to the deck\'s size');

  // ---- the terminal's font ----
  const fonts = h('div', { class: 'pl-fonts', role: 'radiogroup', 'aria-label': 'Terminal font' });
  // The first name of a list that this device draws.
  const drawnAs = (list) => list.find(n => fontHere(n) === 'yes') || 'monospace';
  const fontRows = () => {
    const mine = themed()['font.family'], now = look()['font.family'];
    const rows = [{ label: 'The theme\'s font', list: mine, set: null }];
    const families = lookState.doc.fonts.map(f => f.name).filter((n, i, all) => all.findIndex(o => sameFont(o, n)) === i && fontHere(n) === 'yes');
    for (const n of families) rows.push({ label: n, list: [n, ...mine.filter(m => !sameFont(m, n))].slice(0, 8) });
    if (!SAME(mine, SYSTEM)) rows.push({ label: 'Each device\'s own monospace', list: SYSTEM });
    // What the deck has now, when it is none of these: chosen on a computer.
    if (!rows.some(r => SAME(r.list, now))) rows.push({ label: 'The deck\'s font now', list: now });
    return rows.map((r) => {
      const on = SAME(r.list, now), shown = drawnAs(r.list);
      const sample = h('span', { class: 'pl-font-sample' }, 'Sphinx of black quartz 0O 1lI => !=');
      sample.style.setProperty('--pl-font', fontList(r.list));
      return h('button', { type: 'button', class: 'pl-font' + (on ? ' on' : ''), role: 'radio', 'aria-checked': String(on),
        onclick: () => { if (on) return; const set = { ...(deck().set || {}) }; if (r.set === null) delete set['font.family']; else set['font.family'] = r.list; save({ theme: deck().theme, set }); } },
      h('span', { class: 'pl-font-text' }, h('span', { class: 'pl-name' }, r.label), sample,
        h('span', { class: 'muted small' }, sameFont(shown, r.list[0]) ? `${shown} on this device` : `${r.list[0]} is not on this device: drawn in ${shown} here`)),
      h('span', { class: 'pl-tick', 'aria-hidden': 'true' }, on ? '✓' : ''));
    });
  };

  function draw() {
    const l = look(), page = pageLook(scope.layoutKey);
    themes.replaceChildren(...allThemes().map(swatch));
    warnBox.replaceChildren(...warnings(l, 'deck').map(w => h('p', { class: 'warn-text' }, '⚠ ' + w)));
    const own = [scope.layoutKey && lookState.doc.layouts[scope.layoutKey] ? 'The layout on screen has a look of its own, so it does not follow the theme chosen here.' : '',
      scope.paneKey && lookState.doc.panes[scope.paneKey] ? `${scope.name || 'This pane'} has a look of its own, which stays as it is. It is changed on a computer.` : ''].filter(Boolean);
    ownNote.textContent = own.join(' ');
    ownNote.hidden = !own.length;
    chat.sync();
    term.sync();
    termSample.style.setProperty('--pl-font', fontList(page['font.family']));
    termSample.style.setProperty('--pl-px', termSize(page).px + 'px');
    termSample.style.setProperty('--pl-bg', page['term.background']);
    termSample.style.setProperty('--pl-fg', page['term.foreground']);
    reset.disabled = !chatSize(page).chosen && !termSize(page).chosen;
    fonts.replaceChildren(...fontRows());
  }

  const box = dialog('Appearance', (close) => {
    draw();
    return h('div', { class: 'phone-look' },
      h('section', {}, h('h3', {}, 'Theme'), h('p', { class: 'muted small' }, 'Changes the theme everywhere: on every device and station.'), themes, warnBox, ownNote),
      h('section', {}, h('h3', {}, 'Text size'), h('p', { class: 'muted small' }, 'Only on this device.'), chat.el, term.el, reset),
      h('section', {}, h('h3', {}, 'Terminal font'), h('p', { class: 'muted small' }, 'Changes the font everywhere. These are the fonts this device can draw.'), fonts),
      h('p', { class: 'muted' }, 'More (colours, per-pane looks, importing themes) is on a computer.'),
      h('div', { class: 'row end sheet-foot' }, h('button', { type: 'button', class: 'primary', onclick: close }, 'Done')));
  }, { onClose: () => { asked += 1; if (lookState.preview) { lookState.preview = null; env.apply(); } env.update(); } });
  box.body.parentNode.classList.add('sheet');
}

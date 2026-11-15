// How the Control Deck looks: colours, fonts, the cursor. A theme is a set of
// settings by name; the deck uses one, a layout may use another, and a pane
// may change single settings or name a theme of its own. The most specific
// wins.
//
// What is stored (the owner's themes and the choices) is one document the
// whole fleet keeps, read with `look.get`. The built-in themes are here, in
// the code, and cannot be changed.
//
// Everything in the document came from a file someone imported or from
// another station, so it is data. It is checked again here (the station
// checks it first), and it reaches the page in two ways only: as the value of
// a custom property set with style.setProperty, and as an option of the
// terminal. No style sheet or markup is ever built from it.

const TERM_COLOURS = ['background', 'foreground', 'cursor', 'cursorAccent', 'selection',
  'black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white',
  'brightBlack', 'brightRed', 'brightGreen', 'brightYellow', 'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite'];
const DECK_COLOURS = ['bg', 'panel', 'panel2', 'line', 'text', 'muted', 'blue', 'green', 'amber', 'red', 'violet', 'focus'];

const TERM_LABELS = { background: 'Background', foreground: 'Text', cursor: 'Cursor', cursorAccent: 'Text under the cursor', selection: 'Selection' };
const DECK_LABELS = { bg: 'Page background', panel: 'Bars and panels', panel2: 'Buttons and menus', line: 'Lines and borders', text: 'Text', muted: 'Quiet text',
  blue: 'Working, and what is selected (blue)', green: 'Done (green)', amber: 'Needs you (amber)', red: 'Error and stop (red)', violet: 'Messages and guests (violet)', focus: 'Outline of the pane with the keyboard' };
const words = (camel) => camel.replace(/[A-Z]/g, (c) => ' ' + c.toLowerCase()).replace(/^./, (c) => c.toUpperCase());

// Every setting there is: what kind of value it takes and what the editor
// calls it. The station has the same list (crates/core/src/look.rs).
export const KEYS = {};
for (const c of TERM_COLOURS.slice(0, 5)) KEYS['term.' + c] = { kind: 'colour', group: 'term', label: TERM_LABELS[c] };
for (const c of TERM_COLOURS.slice(5)) KEYS['term.' + c] = { kind: 'colour', group: 'ansi', label: words(c) };
Object.assign(KEYS, {
  'term.contrast': { kind: 'number', min: 1, max: 21, step: 0.5, group: 'read', label: 'Minimum contrast of terminal text',
    hint: '1 leaves colours as they are. At 4.5 the terminal lightens or darkens any text that would be harder to read than that against its background.' },
  'deck.scheme': { kind: 'one', options: [['dark', 'Dark'], ['light', 'Light']], group: 'deck', label: 'Scroll bars and form controls', page: true },
});
for (const c of DECK_COLOURS) KEYS['deck.' + c] = { kind: 'colour', group: 'deck', label: DECK_LABELS[c] };
Object.assign(KEYS, {
  'deck.focusWidth': { kind: 'number', min: 0, max: 4, step: 1, group: 'deck', label: 'Outline width (0 is none)' },
  'font.family': { kind: 'fonts', group: 'font', label: 'Font', hint: 'Names of fonts installed on the device showing the deck, first choice first, separated by commas.' },
  'font.size': { kind: 'number', min: 6, max: 40, step: 1, group: 'font', label: 'Size' },
  'font.lineHeight': { kind: 'number', min: 1, max: 2, step: 0.05, group: 'font', label: 'Line height' },
  'font.letterSpacing': { kind: 'number', min: -2, max: 8, step: 0.5, group: 'font', label: 'Letter spacing' },
  'font.weight': { kind: 'weight', group: 'font', label: 'Weight' },
  'font.weightBold': { kind: 'weight', group: 'font', label: 'Weight of bold text' },
  'font.ligatures': { kind: 'switch', group: 'font', label: 'Ligatures (=> and != drawn as one sign, when the font has them)' },
  'ui.family': { kind: 'fonts', group: 'ui', label: 'Font', page: true },
  'ui.size': { kind: 'number', min: 10, max: 22, step: 1, group: 'ui', label: 'Size', page: true },
  'cursor.style': { kind: 'one', options: [['block', 'Block'], ['bar', 'Bar'], ['underline', 'Underline']], group: 'cursor', label: 'Shape' },
  'cursor.blink': { kind: 'switch', group: 'cursor', label: 'Blink' },
});
export const GROUPS = [['term', 'Terminal colours'], ['ansi', 'The 16 colours programs use'], ['deck', 'Deck colours'], ['font', 'Terminal font'],
  ['ui', 'Deck font'], ['cursor', 'Cursor'], ['read', 'Readability']];

const COLOUR = /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/;
const FONT_NAME = /^[A-Za-z0-9._-](?:[A-Za-z0-9 ._-]{0,62}[A-Za-z0-9._-])?$/;
const THEME_ID = /^[a-z0-9-]{1,40}$/;
export const isColour = (v) => typeof v === 'string' && COLOUR.test(v);
export const isFontName = (v) => typeof v === 'string' && FONT_NAME.test(v);

export function validValue(key, v) {
  const k = KEYS[key];
  if (!k) return false;
  if (k.kind === 'colour') return isColour(v);
  if (k.kind === 'number') return typeof v === 'number' && Number.isFinite(v) && v >= k.min && v <= k.max;
  if (k.kind === 'weight') return Number.isInteger(v) && v >= 100 && v <= 900 && v % 100 === 0;
  if (k.kind === 'switch') return typeof v === 'boolean';
  if (k.kind === 'one') return k.options.some(([o]) => o === v);
  if (k.kind === 'fonts') return Array.isArray(v) && v.length > 0 && v.length <= 8 && v.every(isFontName);
  return false;
}

// Keep what is a known setting with a value of its kind. `bad` collects the
// names of the ones left out, for an import to report.
export function cleanSettings(raw, bad) {
  const out = {};
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return out;
  for (const key of Object.keys(KEYS)) {
    if (!Object.prototype.hasOwnProperty.call(raw, key)) continue;
    if (validValue(key, raw[key])) out[key] = raw[key];
    else if (bad) bad.push(key);
  }
  return out;
}
const cleanChoice = (c) => {
  const theme = c && typeof c.theme === 'string' && THEME_ID.test(c.theme) ? c.theme : null;
  return { theme, set: cleanSettings(c && c.set) };
};

// ---- the built-in themes ----
// "Spandeck Dark" is the deck as it was before there were themes, value for
// value. The other palettes are their authors' (see NOTICE); the deck colours
// beside them are chosen from the same palette.

const ansi = (list) => Object.fromEntries(list.map((c, i) => ['term.' + TERM_COLOURS[5 + i], c]));

export const BASE = {
  'term.background': '#0f1318', 'term.foreground': '#d6dde6', 'term.cursor': '#6cb6ff', 'term.cursorAccent': '#000000', 'term.selection': '#2b4a6b',
  ...ansi(['#1b2229', '#ff6b6b', '#57d38c', '#e5c07b', '#6cb6ff', '#c792ea', '#56c8d8', '#d6dde6',
    '#5c6773', '#ff8e8e', '#7ee2a8', '#f0d399', '#8fc7ff', '#d9b3f2', '#7fdbe6', '#ffffff']),
  'term.contrast': 1,
  'deck.scheme': 'dark',
  'deck.bg': '#0f1318', 'deck.panel': '#161c23', 'deck.panel2': '#1c242d', 'deck.line': '#2a3541', 'deck.text': '#d6dde6', 'deck.muted': '#8b98a7',
  'deck.blue': '#6cb6ff', 'deck.green': '#57d38c', 'deck.amber': '#f0b65a', 'deck.red': '#ff6b6b', 'deck.violet': '#c792ea',
  'deck.focus': '#6cb6ff', 'deck.focusWidth': 0,
  'font.family': ['ui-monospace', 'SF Mono', 'Menlo', 'Cascadia Mono', 'Consolas', 'monospace'],
  'font.size': 13, 'font.lineHeight': 1, 'font.letterSpacing': 0, 'font.weight': 400, 'font.weightBold': 700, 'font.ligatures': true,
  'ui.family': ['system-ui', '-apple-system', 'Segoe UI', 'sans-serif'], 'ui.size': 13,
  'cursor.style': 'block', 'cursor.blink': true,
};

const deck = (scheme, bg, panel, panel2, line, text, muted, blue, green, amber, red, violet) => ({
  'deck.scheme': scheme, 'deck.bg': bg, 'deck.panel': panel, 'deck.panel2': panel2, 'deck.line': line, 'deck.text': text, 'deck.muted': muted,
  'deck.blue': blue, 'deck.green': green, 'deck.amber': amber, 'deck.red': red, 'deck.violet': violet, 'deck.focus': blue });
const term = (background, foreground, cursor, cursorAccent, selection, colours, contrast = 1) => ({
  'term.background': background, 'term.foreground': foreground, 'term.cursor': cursor, 'term.cursorAccent': cursorAccent, 'term.selection': selection,
  ...ansi(colours), 'term.contrast': contrast });

const SOLARIZED = ['#073642', '#dc322f', '#859900', '#b58900', '#268bd2', '#d33682', '#2aa198', '#eee8d5',
  '#002b36', '#cb4b16', '#586e75', '#657b83', '#839496', '#6c71c4', '#93a1a1', '#fdf6e3'];

export const BUILTIN = [
  { id: 'spandeck-dark', name: 'Spandeck Dark', settings: {} },
  { id: 'spandeck-light', name: 'Spandeck Light', settings: {
    ...deck('light', '#f4f6f8', '#ffffff', '#eaeef2', '#cfd7df', '#1f2933', '#5a6978', '#0b63c5', '#17794a', '#8f5f00', '#c4262b', '#7a3db8'),
    // Programs choose colours for a dark screen: the contrast floor keeps
    // their pale ones readable on white.
    ...term('#ffffff', '#1f2933', '#0b63c5', '#ffffff', '#b9d6f7',
      ['#1f2933', '#c4262b', '#17794a', '#8f5f00', '#0b63c5', '#7a3db8', '#0c7b85', '#c5ced8',
        '#66727f', '#e0383d', '#23985f', '#a87400', '#2f83e2', '#9a5fd3', '#1596a2', '#f4f6f8'], 4.5) } },
  // Solarized's "bright black" is its background colour, which hides the dim
  // text agents print; the contrast floor shows it without changing the palette.
  { id: 'solarized-dark', name: 'Solarized Dark', settings: {
    ...deck('dark', '#002b36', '#073642', '#0b4250', '#1d5564', '#93a1a1', '#6f8589', '#268bd2', '#859900', '#b58900', '#dc322f', '#6c71c4'),
    ...term('#002b36', '#839496', '#93a1a1', '#002b36', '#0e4b5a', SOLARIZED, 4.5) } },
  { id: 'solarized-light', name: 'Solarized Light', settings: {
    ...deck('light', '#fdf6e3', '#eee8d5', '#e6dfc8', '#d1c9ae', '#475b62', '#657b83', '#1f76b5', '#5f6e00', '#8f6c00', '#c9302c', '#5a5fb5'),
    ...term('#fdf6e3', '#657b83', '#586e75', '#fdf6e3', '#e3dcc5', SOLARIZED, 4.5) } },
  { id: 'dracula', name: 'Dracula', settings: {
    ...deck('dark', '#282a36', '#21222c', '#343746', '#44475a', '#f8f8f2', '#9aa3c7', '#bd93f9', '#50fa7b', '#ffb86c', '#ff5555', '#ff79c6'),
    ...term('#282a36', '#f8f8f2', '#f8f8f2', '#282a36', '#44475a',
      ['#21222c', '#ff5555', '#50fa7b', '#f1fa8c', '#bd93f9', '#ff79c6', '#8be9fd', '#f8f8f2',
        '#6272a4', '#ff6e6e', '#69ff94', '#ffffa5', '#d6acff', '#ff92df', '#a4ffff', '#ffffff']) } },
  { id: 'gruvbox-dark', name: 'Gruvbox Dark', settings: {
    ...deck('dark', '#282828', '#32302f', '#3c3836', '#504945', '#ebdbb2', '#a89984', '#83a598', '#b8bb26', '#fabd2f', '#fb4934', '#d3869b'),
    ...term('#282828', '#ebdbb2', '#ebdbb2', '#282828', '#504945',
      ['#282828', '#cc241d', '#98971a', '#d79921', '#458588', '#b16286', '#689d6a', '#a89984',
        '#928374', '#fb4934', '#b8bb26', '#fabd2f', '#83a598', '#d3869b', '#8ec07c', '#ebdbb2']) } },
  { id: 'nord', name: 'Nord', settings: {
    ...deck('dark', '#2e3440', '#3b4252', '#434c5e', '#4c566a', '#d8dee9', '#a3adbf', '#88c0d0', '#a3be8c', '#ebcb8b', '#bf616a', '#b48ead'),
    ...term('#2e3440', '#d8dee9', '#d8dee9', '#2e3440', '#434c5e',
      ['#3b4252', '#bf616a', '#a3be8c', '#ebcb8b', '#81a1c1', '#b48ead', '#88c0d0', '#e5e9f0',
        '#4c566a', '#bf616a', '#a3be8c', '#ebcb8b', '#81a1c1', '#b48ead', '#8fbcbb', '#eceff4']) } },
];
export const DEFAULT_THEME = 'spandeck-dark';
export const isBuiltin = (id) => BUILTIN.some(t => t.id === id);

// ---- the document, and what applies where ----

const EMPTY = { themes: [], deck: { theme: null, set: {} }, layouts: {}, panes: {} };
export const lookState = {
  doc: EMPTY,
  // While the editor is open: { level, key, choice } shown in place of what
  // is saved for that level, in this window only.
  preview: null,
};

export function setDoc(raw) {
  const themes = [];
  for (const t of Array.isArray(raw?.themes) ? raw.themes : []) {
    if (!t || typeof t.id !== 'string' || !THEME_ID.test(t.id) || isBuiltin(t.id) || typeof t.name !== 'string') continue;
    themes.push({ id: t.id, name: t.name.slice(0, 48), settings: cleanSettings(t.settings) });
  }
  const level = (m) => Object.fromEntries(Object.entries(m && typeof m === 'object' ? m : {}).map(([k, c]) => [k, cleanChoice(c)]));
  lookState.doc = { updated_ms: raw?.updated_ms || 0, themes, deck: cleanChoice(raw?.deck), layouts: level(raw?.layouts), panes: level(raw?.panes) };
}

export const allThemes = () => [...BUILTIN, ...lookState.doc.themes];
export const themeById = (id) => allThemes().find(t => t.id === id) || null;

export function choiceAt(level, key) {
  const p = lookState.preview;
  if (p && p.level === level && (level === 'deck' || p.key === key)) return p.choice;
  const d = lookState.doc;
  return (level === 'deck' ? d.deck : level === 'layout' ? d.layouts[key] : d.panes[key]) || null;
}
const has = (c) => !!c && (!!c.theme || Object.keys(c.set || {}).length > 0);

// One level laid over what it inherits: its theme first, then its settings.
function over(look, choice) {
  if (!has(choice)) return look;
  return { ...look, ...(choice.theme ? themeById(choice.theme)?.settings || {} : {}), ...(choice.set || {}) };
}
// The look of the whole page: the deck's, and the layout's when one is on screen.
export function pageLook(layoutKey) {
  const look = over(over({ ...BASE }, choiceAt('deck')), layoutKey ? choiceAt('layout', layoutKey) : null);
  return look;
}
// A pane's look, and whether the pane has one of its own.
export function paneLook(layoutKey, paneKey) {
  const page = pageLook(layoutKey), c = choiceAt('pane', paneKey);
  return { look: over(page, c), own: has(c) };
}
// What a level would look like with nothing of its own: what it inherits.
export function inheritedLook(level, key, layoutKey) {
  if (level === 'deck') return { ...BASE };
  if (level === 'layout') return over({ ...BASE }, choiceAt('deck'));
  return pageLook(layoutKey);
}

// ---- putting a look on the page ----

const IDENT = /^-?[A-Za-z][A-Za-z-]*$/;
// `"SF Mono", Menlo, monospace`: from names that were each checked, so
// nothing in it can end the value.
export const fontList = (names) => names.filter(isFontName).map(n => (IDENT.test(n) ? n : `"${n}"`)).join(', ');

const DECK_VARS = { bg: '--bg', panel: '--panel', panel2: '--panel2', line: '--line', text: '--text', muted: '--muted',
  blue: '--blue', green: '--green', amber: '--amber', red: '--red', violet: '--violet', focus: '--focus' };
const FRAME_KEYS = [...DECK_COLOURS.map(c => 'deck.' + c), 'deck.focusWidth', 'term.background'];
const sameFrame = (a, b) => FRAME_KEYS.every(k => String(a[k]).toLowerCase() === String(b[k]).toLowerCase());

// The colours of a frame: the page, or one pane that has its own.
function setFrame(el, look) {
  for (const [name, prop] of Object.entries(DECK_VARS)) el.style.setProperty(prop, look['deck.' + name]);
  el.style.setProperty('--focus-w', look['deck.focusWidth'] + 'px');
  el.style.setProperty('--term-bg', look['term.background']);
}
function clearFrame(el) {
  for (const prop of [...Object.values(DECK_VARS), '--focus-w', '--term-bg']) el.style.removeProperty(prop);
}

export function applyPage(look) {
  const root = document.documentElement;
  setFrame(root, look);
  root.style.setProperty('color-scheme', look['deck.scheme']);
  root.style.setProperty('--ui-font', fontList(look['ui.family']));
  root.style.setProperty('--ui-scale', String(look['ui.size'] / 13));
  // The shades worked out from these colours (a pressed button, a chip's
  // border) are written out for Spandeck Dark and computed for any other.
  if (sameFrame(look, BASE)) delete root.dataset.themed; else root.dataset.themed = '1';
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.setAttribute('content', look['deck.bg']);
}

// A pane with a look of its own carries its colours on its own frame.
export function applyPaneFrame(el, look, page) {
  if (page && !sameFrame(look, page)) { setFrame(el, look); el.dataset.themed = '1'; }
  else { clearFrame(el); delete el.dataset.themed; }
  el.style.setProperty('--lig', look['font.ligatures'] ? 'normal' : 'none');
}

// The terminal's options for a look. `smaller` is the phone-sized terminal.
export function termOptions(look, smaller) {
  return {
    theme: {
      background: look['term.background'], foreground: look['term.foreground'], cursor: look['term.cursor'], cursorAccent: look['term.cursorAccent'],
      selectionBackground: look['term.selection'],
      ...Object.fromEntries(TERM_COLOURS.slice(5).map(c => [c, look['term.' + c]])),
    },
    fontFamily: fontList(look['font.family']),
    fontSize: Math.max(6, look['font.size'] - (smaller ? 2 : 0)),
    lineHeight: look['font.lineHeight'],
    letterSpacing: look['font.letterSpacing'],
    fontWeight: look['font.weight'], fontWeightBold: look['font.weightBold'],
    cursorStyle: look['cursor.style'], cursorBlink: look['cursor.blink'],
    minimumContrastRatio: look['term.contrast'],
  };
}
// The options that change how many cells fit.
export const SIZING = ['fontFamily', 'fontSize', 'lineHeight', 'letterSpacing', 'fontWeight', 'fontWeightBold'];

// The deck's own look is remembered in this browser, so the page opens in it
// (and the sign-in page has it) before the station has answered.
const REMEMBER = 'spandeck.look';
export function rememberPage(look) {
  try { localStorage.setItem(REMEMBER, JSON.stringify(look)); } catch (e) { /* not kept, then */ }
}
export function applyRemembered() {
  let kept = null;
  try { kept = JSON.parse(localStorage.getItem(REMEMBER) || 'null'); } catch (e) { kept = null; }
  if (kept) applyPage({ ...BASE, ...cleanSettings(kept) });
}

// ---- can it be read ----

function rgb(colour) {
  let x = colour.slice(1);
  if (x.length === 3) x = [...x].map(c => c + c).join('');
  return [0, 2, 4].map(i => parseInt(x.slice(i, i + 2), 16));
}
const lin = (c) => { const s = c / 255; return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4; };
const luminance = (colour) => { const [r, g, b] = rgb(colour).map(lin); return 0.2126 * r + 0.7152 * g + 0.0722 * b; };
// The WCAG contrast of two colours, 1 to 21.
export function contrast(a, b) {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}
function lab(colour) {
  const [r, g, b] = rgb(colour).map(lin);
  const f = (t) => (t > 0.008856 ? Math.cbrt(t) : 7.787 * t + 16 / 116);
  const x = f((0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047), y = f(0.2126 * r + 0.7152 * g + 0.0722 * b), z = f((0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883);
  return [116 * y - 16, 500 * (x - y), 200 * (y - z)];
}
// How far apart two colours look (CIE76): under about 20 they are easy to mix up.
export function apart(a, b) {
  const [p, q] = [lab(a), lab(b)];
  return Math.hypot(p[0] - q[0], p[1] - q[1], p[2] - q[2]);
}
const TOO_CLOSE = 20;

// What to warn about. Nothing here stops a look being saved.
export function warnings(look, level) {
  const out = [];
  const pair = (what, fg, bg) => {
    const c = contrast(look[fg], look[bg]);
    if (c < 4.5) out.push(`${what} has a contrast of ${c.toFixed(1)}:1, under the 4.5:1 that is comfortable to read.`);
  };
  if (look['term.contrast'] < 4.5) pair('Terminal text on its background', 'term.foreground', 'term.background');
  pair(level === 'pane' ? 'Text on the pane\'s bar' : 'Deck text on bars and panels', 'deck.text', 'deck.panel');
  if (level !== 'pane') {
    pair('Deck text on the page background', 'deck.text', 'deck.bg');
    pair('Deck text on buttons and menus', 'deck.text', 'deck.panel2');
  }
  const states = [['green', 'done'], ['amber', 'needs you'], ['red', 'error'], ['blue', 'working']];
  for (let i = 0; i < states.length; i += 1) {
    for (let j = i + 1; j < states.length; j += 1) {
      const [a, b] = [states[i], states[j]];
      if (apart(look['deck.' + a[0]], look['deck.' + b[0]]) < TOO_CLOSE) out.push(`The colours for ${a[1]} (${a[0]}) and ${b[1]} (${b[0]}) are close: the two states may be hard to tell apart.`);
    }
  }
  return out;
}

// ---- a theme as a file ----

// spandeck's own format: {"spandeck_theme": 1, "name": …, "settings": {…}}.
export const exportTheme = (theme) => JSON.stringify({ spandeck_theme: 1, name: theme.name, settings: { ...BASE, ...theme.settings } }, null, 2) + '\n';
// A theme from a file's text, or a sentence saying why it is not one.
export function importTheme(text) {
  let raw;
  try { raw = JSON.parse(text); } catch (e) { return { problem: 'That file is not JSON.' }; }
  if (!raw || typeof raw !== 'object' || raw.spandeck_theme !== 1) return { problem: 'That file is not a spandeck theme. Themes from other terminals cannot be imported yet.' };
  const bad = [];
  const settings = cleanSettings(raw.settings, bad);
  if (bad.length) return { problem: `That theme has a value spandeck does not accept for: ${bad.slice(0, 6).join(', ')}${bad.length > 6 ? '…' : ''}. Nothing was imported.` };
  if (!Object.keys(settings).length) return { problem: 'That theme has no settings spandeck knows.' };
  // Shown as text wherever it appears; kept to one short line.
  const name = String(typeof raw.name === 'string' ? raw.name : '').replace(/[\u0000-\u001f\u007f]/g, ' ').trim().slice(0, 48) || 'Imported theme';
  return { name, settings };
}

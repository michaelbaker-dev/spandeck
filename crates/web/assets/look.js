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
//
// Fonts: a theme names fonts. A name is a font installed on the device, or
// one the owner uploaded (the document lists those, and the station serves
// each file by its id). An uploaded font becomes a FontFace object made from
// the file's bytes; its name is never written into a style sheet. Behind
// every terminal font comes the bundled symbols font (app.css), which the
// browser fetches only when a prompt icon or a powerline sign is on screen.

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
  'font.family': { kind: 'fonts', group: 'font', label: 'Font', hint: 'Names of fonts, first choice first, separated by commas: fonts you uploaded, or fonts installed on the device showing the deck.' },
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

const EMPTY = { themes: [], deck: { theme: null, set: {} }, layouts: {}, panes: {}, fonts: [] };
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
  const fonts = [];
  for (const f of Array.isArray(raw?.fonts) ? raw.fonts : []) {
    if (!f || typeof f.id !== 'string' || !FONT_ID.test(f.id) || !isFontName(f.name) || !FACES[f.face] || fonts.some(o => o.id === f.id)) continue;
    fonts.push({ id: f.id, name: f.name, face: f.face, format: String(f.format || '').replace(/[^a-z0-9]/g, '').slice(0, 5), size: Number(f.size) || 0 });
  }
  lookState.doc = { updated_ms: raw?.updated_ms || 0, themes, deck: cleanChoice(raw?.deck), layouts: level(raw?.layouts), panes: level(raw?.panes), fonts };
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

// ---- fonts ----

// The bundled symbols font (app.css), last in every terminal's list.
const SYMBOLS = '"Spandeck Symbols"';
const FONT_ID = /^[0-9a-f]{32}$/;
// What each face of a family says of itself to the browser.
export const FACES = {
  regular: { label: 'Regular', desc: {} },
  bold: { label: 'Bold', desc: { weight: '700' } },
  italic: { label: 'Italic', desc: { style: 'italic' } },
  'bold italic': { label: 'Bold italic', desc: { weight: '700', style: 'italic' } },
  variable: { label: 'Every weight (a variable font)', desc: { weight: '1 1000' } },
  'variable italic': { label: 'Every weight, italic (a variable font)', desc: { weight: '1 1000', style: 'italic' } },
};
// Names that mean a kind of font, not a font.
const GENERIC = ['serif', 'sans-serif', 'monospace', 'cursive', 'fantasy', 'system-ui', 'ui-serif', 'ui-sans-serif', 'ui-monospace', 'ui-rounded', 'emoji', 'math', 'fangsong', '-apple-system', 'blinkmacsystemfont'];
const lower = (name) => name.toLowerCase();
export const sameFont = (a, b) => lower(a) === lower(b);
export const uploadedAs = (name) => lookState.doc.fonts.filter(f => sameFont(f.name, name));

// Told when a font has finished loading, so terminals measure their cells
// again: a terminal measures once, with whatever font was there then.
export const fontListeners = new Set();
let told = null;
const tellFonts = () => { clearTimeout(told); told = setTimeout(() => { for (const fn of fontListeners) fn(); }, 40); };
// The symbols font, and any other the style sheet names, load when first needed.
if (document.fonts?.addEventListener) document.fonts.addEventListener('loadingdone', (e) => { if (e.fontfaces?.length) tellFonts(); });

// id -> { name, face (which), font (the FontFace), state: 'loading' | 'ok' | 'missing' | 'failed', at }
const loaded = new Map();
// Make a FontFace of every uploaded font, from its bytes. `get(id)` fetches
// one file. A font the station does not hold yet is asked for again later.
export async function loadFonts(get) {
  if (typeof FontFace === 'undefined' || !document.fonts) return;
  const want = lookState.doc.fonts;
  let changed = false;
  for (const [id, f] of loaded) {
    if (want.some(w => w.id === id && w.name === f.name && w.face === f.face)) continue;
    if (f.font) { document.fonts.delete(f.font); changed = true; }
    loaded.delete(id);
  }
  await Promise.all(want.map(async (w) => {
    const have = loaded.get(w.id);
    if (have && (have.state === 'ok' || have.state === 'loading' || have.state === 'failed' || Date.now() - have.at < 10000)) return;
    const entry = { name: w.name, face: w.face, state: 'loading', at: Date.now() };
    loaded.set(w.id, entry);
    let bytes;
    try { bytes = await get(w.id); } catch (e) { entry.state = 'missing'; return; }
    try {
      // The name was checked (isFontName): it has no quote or comma in it.
      // Chrome takes the name as it is written, and would keep quotes as
      // part of it, so that no font list could ever name it. A browser that
      // reads the name as a style sheet would refuse one that starts with a
      // digit unless it is quoted; for that browser, the quoted form.
      let font;
      try { font = new FontFace(w.name, bytes, FACES[w.face].desc); await font.load(); } catch (e) {
        if (e?.name !== 'SyntaxError') throw e;
        font = new FontFace(`"${w.name}"`, bytes, FACES[w.face].desc);
        await font.load();
      }
      if (loaded.get(w.id) !== entry) return;
      document.fonts.add(font);
      entry.font = font;
      entry.state = 'ok';
      changed = true;
    } catch (e) { entry.state = 'failed'; }
  }));
  if (changed) tellFonts();
}

let probe = null;
const widthOf = (font) => { probe ||= document.createElement('canvas').getContext('2d'); probe.font = `72px ${font}`; return probe.measureText('mmmmmmmmlli.WQ@0O').width; };
// Whether text set in this font is drawn in it here: 'yes', 'no', or for an
// uploaded font that is not ready 'loading', 'missing' (the station does not
// hold the file yet) or 'failed' (this browser could not read the file).
export function fontHere(name) {
  if (GENERIC.includes(lower(name))) return 'yes';
  const up = uploadedAs(name).map(f => loaded.get(f.id)?.state || 'loading');
  if (up.includes('ok')) return 'yes';
  if (up.length) return up.includes('loading') ? 'loading' : up.includes('missing') ? 'missing' : 'failed';
  // An installed font changes the width of text that names it first.
  const q = fontList([name]);
  return ['monospace', 'serif'].some(g => widthOf(`${q}, ${g}`) !== widthOf(g)) ? 'yes' : 'no';
}
// A family name for a font file, from the file's name, and which face it is.
export function fontFromFile(fileName) {
  const base = String(fileName).replace(/\.[A-Za-z0-9]+$/, '');
  const face = (/variable|\bVF\b|\[wght/i.test(base) ? 'variable' : /bold/i.test(base) ? 'bold' : '') + (/italic|oblique/i.test(base) ? ' italic' : '');
  const name = base.replace(/\[[^\]]*\]/g, ' ').replace(/[-_ ]*(regular|bold|italic|oblique|variable|VF|webfont)\b/gi, ' ')
    .replace(/[^A-Za-z0-9 ._-]+/g, ' ').replace(/[-_]+/g, ' ').replace(/\s+/g, ' ').trim().slice(0, 64).trim();
  return { name: name || 'My font', face: face.trim() || 'regular' };
}

// The terminal's options for a look. `smaller` is the phone-sized terminal.
export function termOptions(look, smaller) {
  return {
    theme: {
      background: look['term.background'], foreground: look['term.foreground'], cursor: look['term.cursor'], cursorAccent: look['term.cursorAccent'],
      selectionBackground: look['term.selection'],
      ...Object.fromEntries(TERM_COLOURS.slice(5).map(c => [c, look['term.' + c]])),
    },
    fontFamily: fontList(look['font.family']) + ', ' + SYMBOLS,
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

// Read here: spandeck's own format, an iTerm2 colour file (.itermcolors) and
// a Windows Terminal colour scheme, alone or in a whole settings.json. The
// last two say only the terminal's colours; the deck's are worked out from
// them. Whatever the file is, its content is data: colours are read out of
// it one by one and checked like any other, and its names are shown as text.

const hex = (parts) => '#' + parts.map(c => Math.round(Math.min(255, Math.max(0, c))).toString(16).padStart(2, '0')).join('');
// `t` of the way from colour a to colour b.
const mix = (a, b, t) => { const [p, q] = [rgb(a), rgb(b)]; return hex(p.map((c, i) => c + (q[i] - c) * t)); };
// Move a colour toward another until it stands out enough against a third.
function readable(colour, against, least, toward) {
  let c = colour;
  for (let i = 0; i < 12 && contrast(c, against) < least; i += 1) c = mix(c, toward, 0.2);
  return c;
}
// The deck's colours for a terminal palette: the page is the terminal's
// background, bars and lines are steps from it toward the text, and the
// state colours are the palette's own, whichever of each pair reads better.
export function deckFromTerminal(t) {
  const bg = t['term.background'], fg = t['term.foreground'];
  const dark = contrast(bg, '#ffffff') >= contrast(bg, '#000000');
  const far = dark ? '#ffffff' : '#000000';
  const panel = mix(bg, fg, 0.05), panel2 = mix(bg, fg, 0.1);
  const text = readable(fg, panel2, 7, far);
  const accent = (a, b) => {
    const best = [t['term.' + a], t['term.' + b]].sort((x, y) => contrast(y, panel) - contrast(x, panel))[0];
    return readable(best, panel, 3, text);
  };
  const blue = accent('blue', 'brightBlue');
  return {
    'deck.scheme': dark ? 'dark' : 'light', 'deck.bg': bg, 'deck.panel': panel, 'deck.panel2': panel2, 'deck.line': mix(bg, fg, 0.2),
    'deck.text': text, 'deck.muted': readable(mix(text, bg, 0.4), panel2, 4.5, text),
    'deck.blue': blue, 'deck.green': accent('green', 'brightGreen'), 'deck.amber': accent('yellow', 'brightYellow'),
    'deck.red': accent('red', 'brightRed'), 'deck.violet': accent('magenta', 'brightMagenta'), 'deck.focus': blue,
  };
}
// A whole theme from terminal colours by our names. `c` may lack the cursor,
// the selection and the bright colours; it must have the rest.
function fromTerminal(c, name, from) {
  const t = {};
  for (const k of TERM_COLOURS) {
    const v = typeof c[k] === 'string' ? c[k].trim().toLowerCase() : c[k];
    if (v === undefined || v === null || v === '') continue;
    if (!isColour(v)) return null;
    t[k] = v.length === 9 && k !== 'selection' ? v.slice(0, 7) : v;
  }
  if (!t.background || !t.foreground || TERM_COLOURS.slice(5, 13).some(k => !t[k])) return null;
  for (const k of TERM_COLOURS.slice(5, 13)) t['bright' + k[0].toUpperCase() + k.slice(1)] ||= t[k];
  t.cursor ||= t.foreground;
  t.cursorAccent ||= t.background;
  t.selection ||= mix(t.background, t.foreground, 0.25);
  const term = Object.fromEntries(TERM_COLOURS.map(k => ['term.' + k, t[k]]));
  const settings = cleanSettings({ ...term, ...deckFromTerminal(term) });
  return { name: cleanName(name) || 'Imported theme', settings, from };
}
// Shown as text wherever it appears; kept to one short line.
const cleanName = (name) => String(typeof name === 'string' ? name : '').replace(/[\u0000-\u001f\u007f]/g, ' ').trim().slice(0, 48).trim();

// iTerm2: a property list whose keys ("Ansi 0 Color", "Background Color")
// each hold a dict of components from 0 to 1. Parsed as XML, which runs
// nothing and fetches nothing, and only numbers are taken from it.
const ITERM = { 'Background Color': 'background', 'Foreground Color': 'foreground', 'Cursor Color': 'cursor', 'Cursor Text Color': 'cursorAccent', 'Selection Color': 'selection' };
function fromIterm(text, fileName) {
  const not = { problem: 'That file is not an iTerm2 colour file.' };
  let doc;
  try { doc = new DOMParser().parseFromString(text, 'application/xml'); } catch (e) { return not; }
  const top = doc.documentElement;
  if (!top || top.nodeName !== 'plist' || doc.getElementsByTagName('parsererror').length) return not;
  // A dict is keys and values in turn.
  const pairs = (dict) => {
    const out = new Map(), kids = dict ? [...dict.children] : [];
    for (let i = 0; i + 1 < kids.length; i += 2) if (kids[i].nodeName === 'key') out.set(kids[i].textContent.trim(), kids[i + 1]);
    return out;
  };
  const all = pairs([...top.children].find(c => c.nodeName === 'dict'));
  const colour = (key) => {
    const el = all.get(key) || all.get(key + ' (Dark)');
    if (!el || el.nodeName !== 'dict') return undefined;
    const parts = pairs(el);
    const part = (n) => { const text = (parts.get(n + ' Component')?.textContent || '').trim(), v = Number(text); return text && Number.isFinite(v) ? v * 255 : NaN; };
    const got = [part('Red'), part('Green'), part('Blue')];
    return got.some(Number.isNaN) ? undefined : hex(got);
  };
  const c = {};
  for (const [key, ours] of Object.entries(ITERM)) c[ours] = colour(key);
  TERM_COLOURS.slice(5).forEach((ours, i) => { c[ours] = colour(`Ansi ${i} Color`); });
  return fromTerminal(c, String(fileName || '').replace(/\.[A-Za-z0-9]+$/, ''), 'iTerm2') || { problem: 'That iTerm2 colour file does not have a background, a text colour and the eight colours.' };
}

// Windows Terminal: {"name", "background", "foreground", "cursorColor",
// "selectionBackground", "black" … "brightWhite"}; it says purple for magenta.
function fromWindowsTerminal(s) {
  if (!s || typeof s !== 'object' || Array.isArray(s)) return null;
  const c = { cursor: s.cursorColor, selection: s.selectionBackground, magenta: s.purple ?? s.magenta, brightMagenta: s.brightPurple ?? s.brightMagenta };
  for (const k of TERM_COLOURS) if (!(k in c)) c[k] = s[k];
  return fromTerminal(c, s.name, 'Windows Terminal');
}
// Windows Terminal's settings file has comments and commas JSON does not
// allow, and a scheme copied out of one ends with the comma that followed it.
function looseJson(text) {
  try { return JSON.parse(text); } catch (e) { /* try it without them */ }
  let out = '', i = 0, quoted = false;
  while (i < text.length) {
    const c = text[i], d = text[i + 1];
    if (quoted) {
      out += c;
      if (c === '\\' && d !== undefined) { out += d; i += 1; } else if (c === '"') quoted = false;
      i += 1;
    } else if (c === '/' && d === '/') { while (i < text.length && text[i] !== '\n') i += 1; }
    else if (c === '/' && d === '*') { const end = text.indexOf('*/', i + 2); i = end < 0 ? text.length : end + 2; }
    else if (c === ',' && /^\s*([}\]]|$)/.test(text.slice(i + 1, i + 200).replace(/\/\/[^\n]*|\/\*[\s\S]*?\*\//g, ''))) i += 1;
    else { quoted = c === '"'; out += c; i += 1; }
  }
  return JSON.parse(out);
}

// spandeck's own format: {"spandeck_theme": 1, "name": …, "settings": {…}}.
export const exportTheme = (theme) => JSON.stringify({ spandeck_theme: 1, name: theme.name, settings: { ...BASE, ...theme.settings } }, null, 2) + '\n';
// A theme from a file's text: { name, settings, from } (`from` is the other
// terminal it was read from; nothing for spandeck's own), or { choices: […] }
// when the file holds several, or { problem } saying why it is not one.
export function importTheme(text, fileName) {
  text = String(text).replace(/^\ufeff/, '');
  if (text.trimStart().startsWith('<')) return fromIterm(text, fileName);
  const NOT = 'That file is not a theme spandeck can read. It reads its own theme files, iTerm2 colour files (.itermcolors) and Windows Terminal colour schemes or settings.';
  let raw;
  try { raw = looseJson(text); } catch (e) { return { problem: NOT }; }
  if (!raw || typeof raw !== 'object') return { problem: NOT };
  if (Array.isArray(raw.schemes)) {
    const choices = raw.schemes.slice(0, 500).map(fromWindowsTerminal).filter(Boolean);
    return choices.length > 1 ? { choices } : choices[0] || { problem: 'That Windows Terminal settings file has no colour schemes in it.' };
  }
  if (raw.spandeck_theme !== 1) return fromWindowsTerminal(raw) || { problem: NOT };
  const bad = [];
  const settings = cleanSettings(raw.settings, bad);
  if (bad.length) return { problem: `That theme has a value spandeck does not accept for: ${bad.slice(0, 6).join(', ')}${bad.length > 6 ? '…' : ''}. Nothing was imported.` };
  if (!Object.keys(settings).length) return { problem: 'That theme has no settings spandeck knows.' };
  return { name: cleanName(raw.name) || 'Imported theme', settings };
}

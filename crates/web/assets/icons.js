// Every small picture in the Control Deck. This module is the only place that
// knows what an icon looks like: the rest of the deck asks for one by what it
// stands for. The marks here are plain placeholders (a letter on a coloured
// tile, or a simple shape) and are not any company's logo.
//
// An icon is decoration (aria-hidden): whatever carries it says the name, for
// which `iconName` gives the words.

const NS = 'http://www.w3.org/2000/svg';

function el(tag, attrs, ...kids) {
  const e = document.createElementNS(NS, tag);
  for (const [k, v] of Object.entries(attrs || {})) e.setAttribute(k, v);
  for (const k of kids) e.append(k);
  return e;
}
const frame = (name, ...kids) => el('svg', { class: 'ico ico-' + name, width: 16, height: 16, viewBox: '0 0 16 16', 'aria-hidden': 'true', focusable: 'false' }, ...kids);
const tile = (colour) => el('rect', { x: 1, y: 1, width: 14, height: 14, rx: 3.5, fill: colour });
const letter = (name, ch, colour) => frame(name, tile(colour),
  el('text', { x: 8, y: 11.7, 'text-anchor': 'middle', 'font-size': 10, 'font-weight': 700, 'font-family': 'system-ui, -apple-system, "Segoe UI", sans-serif', fill: '#10151b' }, ch));
const line = (d, colour, width = 1.5) => el('path', { d, fill: 'none', stroke: colour, 'stroke-width': width, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' });

// kind -> [what to call it, how to draw it]. The kinds are the values of a
// pane's `icon` field.
const PANES = {
  claude: ['Claude Code', () => letter('claude', 'C', '#e0a070')],
  codex: ['Codex', () => letter('codex', 'X', '#5cc8ad')],
  gemini: ['Gemini', () => letter('gemini', 'G', '#7da2ff')],
  grok: ['Grok', () => letter('grok', 'K', '#c3cbd6')],
  opencode: ['OpenCode', () => letter('opencode', 'O', '#c792ea')],
  agent: ['Agent', () => frame('agent', tile('#8b98a7'), el('circle', { cx: 8, cy: 6.2, r: 2.2, fill: '#10151b' }), el('path', { d: 'M4 12.6c.5-2.2 2-3.2 4-3.2s3.5 1 4 3.2z', fill: '#10151b' }))],
  shell: ['Shell', () => frame('shell', el('rect', { x: 1.5, y: 2.5, width: 13, height: 11, rx: 2.5, fill: '#10151b', stroke: '#57d38c', 'stroke-width': 1.2 }), line('M4.3 6l2.4 2-2.4 2', '#57d38c'), line('M8.5 10.5h3', '#57d38c'))],
};

export const iconName = (kind) => (PANES[kind] || PANES.shell)[0];
export const paneIcon = (kind) => (PANES[kind] || PANES.shell)[1]();

// Several panes on one screen.
export const layoutIcon = () => frame('layout',
  el('rect', { x: 1.5, y: 2.5, width: 13, height: 11, rx: 2, fill: 'none', stroke: '#6cb6ff', 'stroke-width': 1.3 }),
  line('M8 2.5v11M8 8h6.5', '#6cb6ff', 1.3));

// Watch: who works for whom, drawn as three joined dots.
export const watchIcon = () => frame('watch',
  line('M8 4.5L4 11.5M8 4.5l4 7', '#8b98a7', 1.2),
  el('circle', { cx: 8, cy: 4, r: 2.2, fill: '#57d38c' }), el('circle', { cx: 3.8, cy: 12, r: 2, fill: '#6cb6ff' }), el('circle', { cx: 12.2, cy: 12, r: 2, fill: '#6cb6ff' }));

export const stationIcon = () => frame('station',
  el('rect', { x: 1.5, y: 2.5, width: 13, height: 8.5, rx: 1.5, fill: 'none', stroke: '#8b98a7', 'stroke-width': 1.3 }),
  line('M5.5 13.5h5M8 11v2.5', '#8b98a7', 1.3));

export const pinIcon = () => frame('pin',
  el('path', { d: 'M5.5 2h5l-.8 4.2 2.3 2.3H4l2.3-2.3z', fill: '#f0b65a' }), line('M8 8.5V14', '#f0b65a', 1.4));

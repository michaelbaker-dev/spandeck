// Every small picture in the Control Deck. This module is the only place that
// knows what an icon looks like: the rest of the deck asks for one by what it
// stands for.
//
// Claude, Gemini and OpenCode are shown with their own marks, which are
// trademarks of Anthropic, Google and the OpenCode project. The shapes come
// from Simple Icons (CC0-1.0, simpleicons.org); see NOTICE. They are used only
// to say which program a pane runs. Codex, Grok and Antigravity have no mark
// in that set, so they are a letter on a coloured tile, as is anything else.
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
// A product's own mark, drawn from a 24-unit path.
const mark = (name, d, colour) => el('svg', { class: 'ico ico-' + name, width: 16, height: 16, viewBox: '0 0 24 24', 'aria-hidden': 'true', focusable: 'false' }, el('path', { d, fill: colour }));
const MARKS = {
  "claude": "m4.7144 15.9555 4.7174-2.6471.079-.2307-.079-.1275h-.2307l-.7893-.0486-2.6956-.0729-2.3375-.0971-2.2646-.1214-.5707-.1215-.5343-.7042.0546-.3522.4797-.3218.686.0608 1.5179.1032 2.2767.1578 1.6514.0972 2.4468.255h.3886l.0546-.1579-.1336-.0971-.1032-.0972L6.973 9.8356l-2.55-1.6879-1.3356-.9714-.7225-.4918-.3643-.4614-.1578-1.0078.6557-.7225.8803.0607.2246.0607.8925.686 1.9064 1.4754 2.4893 1.8336.3643.3035.1457-.1032.0182-.0728-.164-.2733-1.3539-2.4467-1.445-2.4893-.6435-1.032-.17-.6194c-.0607-.255-.1032-.4674-.1032-.7285L6.287.1335 6.6997 0l.9957.1336.419.3642.6192 1.4147 1.0018 2.2282 1.5543 3.0296.4553.8985.2429.8318.091.255h.1579v-.1457l.1275-1.706.2368-2.0947.2307-2.6957.0789-.7589.3764-.9107.7468-.4918.5828.2793.4797.686-.0668.4433-.2853 1.8517-.5586 2.9021-.3643 1.9429h.2125l.2429-.2429.9835-1.3053 1.6514-2.0643.7286-.8196.85-.9046.5464-.4311h1.0321l.759 1.1293-.34 1.1657-1.0625 1.3478-.8804 1.1414-1.2628 1.7-.7893 1.36.0729.1093.1882-.0183 2.8535-.607 1.5421-.2794 1.8396-.3157.8318.3886.091.3946-.3278.8075-1.967.4857-2.3072.4614-3.4364.8136-.0425.0304.0486.0607 1.5482.1457.6618.0364h1.621l3.0175.2247.7892.522.4736.6376-.079.4857-1.2142.6193-1.6393-.3886-3.825-.9107-1.3113-.3279h-.1822v.1093l1.0929 1.0686 2.0035 1.8092 2.5075 2.3314.1275.5768-.3218.4554-.34-.0486-2.2039-1.6575-.85-.7468-1.9246-1.621h-.1275v.17l.4432.6496 2.3436 3.5214.1214 1.0807-.17.3521-.6071.2125-.6679-.1214-1.3721-1.9246L14.38 17.959l-1.1414-1.9428-.1397.079-.674 7.2552-.3156.3703-.7286.2793-.6071-.4614-.3218-.7468.3218-1.4753.3886-1.9246.3157-1.53.2853-1.9004.17-.6314-.0121-.0425-.1397.0182-1.4328 1.9672-2.1796 2.9446-1.7243 1.8456-.4128.164-.7164-.3704.0667-.6618.4008-.5889 2.386-3.0357 1.4389-1.882.929-1.0868-.0062-.1579h-.0546l-6.3385 4.1164-1.1293.1457-.4857-.4554.0608-.7467.2307-.2429 1.9064-1.3114Z",
  "gemini": "M11.04 19.32Q12 21.51 12 24q0-2.49.93-4.68.96-2.19 2.58-3.81t3.81-2.55Q21.51 12 24 12q-2.49 0-4.68-.93a12.3 12.3 0 0 1-3.81-2.58 12.3 12.3 0 0 1-2.58-3.81Q12 2.49 12 0q0 2.49-.96 4.68-.93 2.19-2.55 3.81a12.3 12.3 0 0 1-3.81 2.58Q2.49 12 0 12q2.49 0 4.68.96 2.19.93 3.81 2.55t2.55 3.81",
  "opencode": "M22 24H2V0h20zM17 4.8H7v14.4h10z"
};
const line = (d, colour, width = 1.5) => el('path', { d, fill: 'none', stroke: colour, 'stroke-width': width, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' });

// kind -> [what to call it, how to draw it]. The kinds are the values of a
// pane's `icon` field.
const PANES = {
  claude: ['Claude Code', () => mark('claude', MARKS.claude, '#D97757')],
  codex: ['Codex', () => letter('codex', 'X', '#5cc8ad')],
  gemini: ['Gemini', () => mark('gemini', MARKS.gemini, '#8E75B2')],
  grok: ['Grok', () => letter('grok', 'K', '#c3cbd6')],
  opencode: ['OpenCode', () => mark('opencode', MARKS.opencode, '#d6dde6')],
  antigravity: ['Antigravity', () => letter('antigravity', 'A', '#7fd1c7')],
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

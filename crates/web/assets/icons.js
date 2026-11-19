// Every small picture in the Control Deck. This module is the only place that
// knows what an icon looks like: the rest of the deck asks for one by what it
// stands for.
//
// Each agent program is shown with its maker's own mark. The marks are
// trademarks of their owners (Anthropic, OpenAI, Google, xAI, the OpenCode
// project) and are used only to say which program a pane runs; spandeck is not
// affiliated with or endorsed by any of them. NOTICE has the source of each.
//   Claude, Gemini, OpenCode: shapes from Simple Icons (CC0-1.0).
//   Codex: OpenAI's mark, the one the Codex CLI and chatgpt.com/codex show,
//     from that page's own icon; its white-on-dark form.
//   Grok: from grok.com's own icon, the two shapes of the mark in their own
//     colour, without the dark tile behind them.
//   Antigravity: the one-colour mark in antigravity.google's own icon sheet.
// The path data of those three is copied as published: scaled, never redrawn.
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
// A product's own mark: its path, in the coordinates it was published in.
const mark = (name, d, colour, box = '0 0 24 24') => el('svg', { class: 'ico ico-' + name, width: 16, height: 16, viewBox: box, 'aria-hidden': 'true', focusable: 'false' }, el('path', { d, fill: colour }));
const MARKS = {
  "claude": "m4.7144 15.9555 4.7174-2.6471.079-.2307-.079-.1275h-.2307l-.7893-.0486-2.6956-.0729-2.3375-.0971-2.2646-.1214-.5707-.1215-.5343-.7042.0546-.3522.4797-.3218.686.0608 1.5179.1032 2.2767.1578 1.6514.0972 2.4468.255h.3886l.0546-.1579-.1336-.0971-.1032-.0972L6.973 9.8356l-2.55-1.6879-1.3356-.9714-.7225-.4918-.3643-.4614-.1578-1.0078.6557-.7225.8803.0607.2246.0607.8925.686 1.9064 1.4754 2.4893 1.8336.3643.3035.1457-.1032.0182-.0728-.164-.2733-1.3539-2.4467-1.445-2.4893-.6435-1.032-.17-.6194c-.0607-.255-.1032-.4674-.1032-.7285L6.287.1335 6.6997 0l.9957.1336.419.3642.6192 1.4147 1.0018 2.2282 1.5543 3.0296.4553.8985.2429.8318.091.255h.1579v-.1457l.1275-1.706.2368-2.0947.2307-2.6957.0789-.7589.3764-.9107.7468-.4918.5828.2793.4797.686-.0668.4433-.2853 1.8517-.5586 2.9021-.3643 1.9429h.2125l.2429-.2429.9835-1.3053 1.6514-2.0643.7286-.8196.85-.9046.5464-.4311h1.0321l.759 1.1293-.34 1.1657-1.0625 1.3478-.8804 1.1414-1.2628 1.7-.7893 1.36.0729.1093.1882-.0183 2.8535-.607 1.5421-.2794 1.8396-.3157.8318.3886.091.3946-.3278.8075-1.967.4857-2.3072.4614-3.4364.8136-.0425.0304.0486.0607 1.5482.1457.6618.0364h1.621l3.0175.2247.7892.522.4736.6376-.079.4857-1.2142.6193-1.6393-.3886-3.825-.9107-1.3113-.3279h-.1822v.1093l1.0929 1.0686 2.0035 1.8092 2.5075 2.3314.1275.5768-.3218.4554-.34-.0486-2.2039-1.6575-.85-.7468-1.9246-1.621h-.1275v.17l.4432.6496 2.3436 3.5214.1214 1.0807-.17.3521-.6071.2125-.6679-.1214-1.3721-1.9246L14.38 17.959l-1.1414-1.9428-.1397.079-.674 7.2552-.3156.3703-.7286.2793-.6071-.4614-.3218-.7468.3218-1.4753.3886-1.9246.3157-1.53.2853-1.9004.17-.6314-.0121-.0425-.1397.0182-1.4328 1.9672-2.1796 2.9446-1.7243 1.8456-.4128.164-.7164-.3704.0667-.6618.4008-.5889 2.386-3.0357 1.4389-1.882.929-1.0868-.0062-.1579h-.0546l-6.3385 4.1164-1.1293.1457-.4857-.4554.0608-.7467.2307-.2429 1.9064-1.3114Z",
  "gemini": "M11.04 19.32Q12 21.51 12 24q0-2.49.93-4.68.96-2.19 2.58-3.81t3.81-2.55Q21.51 12 24 12q-2.49 0-4.68-.93a12.3 12.3 0 0 1-3.81-2.58 12.3 12.3 0 0 1-2.58-3.81Q12 2.49 12 0q0 2.49-.96 4.68-.93 2.19-2.55 3.81a12.3 12.3 0 0 1-3.81 2.58Q2.49 12 0 12q2.49 0 4.68.96 2.19.93 3.81 2.55t2.55 3.81",
  "opencode": "M22 24H2V0h20zM17 4.8H7v14.4h10z",
  "codex": "M101.228 164.247C96.2776 164.247 91.5751 163.307 87.1201 161.426C82.6651 159.545 78.7051 156.921 75.2401 153.555C71.4781 154.842 67.5676 155.486 63.5086 155.486C56.8756 155.486 50.7376 153.852 45.0946 150.585C39.4516 147.318 34.8976 142.863 31.4326 137.22C28.0666 131.577 26.3836 125.291 26.3836 118.361C26.3836 115.49 26.7796 112.371 27.5716 109.005C23.6116 105.342 20.5426 101.135 18.3646 96.3828C16.1866 91.5318 15.0976 86.4828 15.0976 81.2358C15.0976 75.8898 16.2361 70.7418 18.5131 65.7918C20.7901 60.8418 23.9581 56.5848 28.0171 53.0208C32.1751 49.3578 36.9766 46.8333 42.4216 45.4473C43.5106 39.8043 45.7876 34.7553 49.2526 30.3003C52.8166 25.7463 57.1726 22.1823 62.3206 19.6083C67.4686 17.0343 72.9631 15.7473 78.8041 15.7473C83.7541 15.7473 88.4566 16.6878 92.9116 18.5688C97.3666 20.4498 101.327 23.0733 104.792 26.4393C108.554 25.1523 112.464 24.5088 116.523 24.5088C123.156 24.5088 129.294 26.1423 134.937 29.4093C140.58 32.6763 145.085 37.1313 148.451 42.7743C151.916 48.4173 153.648 54.7038 153.648 61.6338C153.648 64.5048 153.252 67.6233 152.46 70.9893C156.42 74.6523 159.489 78.9093 161.667 83.7603C163.845 88.5123 164.934 93.5118 164.934 98.7588C164.934 104.105 163.796 109.253 161.519 114.203C159.242 119.153 156.024 123.459 151.866 127.122C147.807 130.686 143.055 133.161 137.61 134.547C136.521 140.19 134.195 145.239 130.631 149.694C127.166 154.248 122.859 157.812 117.711 160.386C112.563 162.96 107.069 164.247 101.228 164.247ZM64.5481 145.685C69.4981 145.685 73.8046 144.645 77.4676 142.566L105.386 126.528C106.376 125.835 106.871 124.895 106.871 123.707V110.936L70.9336 131.577C68.7556 132.864 66.5776 132.864 64.3996 131.577L36.3331 115.391C36.3331 115.688 36.2836 116.034 36.1846 116.43C36.1846 116.826 36.1846 117.42 36.1846 118.212C36.1846 123.261 37.3726 127.914 39.7486 132.171C42.2236 136.329 45.6391 139.596 49.9951 141.972C54.3511 144.447 59.2021 145.685 64.5481 145.685ZM66.0331 121.479C66.6271 121.776 67.1716 121.925 67.6666 121.925C68.1616 121.925 68.6566 121.776 69.1516 121.479L80.2891 115.094L44.5006 94.3038C42.3226 93.0168 41.2336 91.0863 41.2336 88.5123V56.2878C36.2836 58.4658 32.3236 61.8318 29.3536 66.3858C26.3836 70.8408 24.8986 75.7908 24.8986 81.2358C24.8986 86.0868 26.1361 90.7398 28.6111 95.1948C31.0861 99.6498 34.3036 103.016 38.2636 105.293L66.0331 121.479ZM101.228 154.446C106.475 154.446 111.227 153.258 115.484 150.882C119.741 148.506 123.107 145.239 125.582 141.081C128.057 136.923 129.294 132.27 129.294 127.122V95.0463C129.294 93.8583 128.799 92.9673 127.809 92.3733L116.523 85.8393V127.271C116.523 129.845 115.434 131.775 113.256 133.062L85.1896 149.249C90.0406 152.714 95.3866 154.446 101.228 154.446ZM106.871 100.095V79.8993L90.09 70.3953L73.1611 79.8993V100.095L90.09 109.599L106.871 100.095ZM63.5086 52.7238C63.5086 50.1498 64.5976 48.2193 66.7756 46.9323L94.8421 30.7458C89.9911 27.2808 84.6451 25.5483 78.8041 25.5483C73.5571 25.5483 68.8051 26.7363 64.5481 29.1123C60.2911 31.4883 56.9251 34.7553 54.4501 38.9133C52.0741 43.0713 50.8861 47.7243 50.8861 52.8723V84.7998C50.8861 85.9878 51.3811 86.9283 52.3711 87.6213L63.5086 94.1553V52.7238ZM138.947 123.707C143.897 121.529 147.807 118.163 150.678 113.609C153.648 109.055 155.133 104.105 155.133 98.7588C155.133 93.9078 153.896 89.2548 151.421 84.7998C148.946 80.3448 145.728 76.9788 141.768 74.7018L113.999 58.6638C113.405 58.2678 112.86 58.1193 112.365 58.2183C111.87 58.2183 111.375 58.3668 110.88 58.6638L99.7426 64.9008L135.68 85.8393C136.769 86.4333 137.561 87.2253 138.056 88.2153C138.65 89.1063 138.947 90.1953 138.947 91.4823V123.707ZM109.098 48.2688C111.276 46.8828 113.454 46.8828 115.632 48.2688L143.847 64.7523C143.847 64.0593 143.847 63.1683 143.847 62.0793C143.847 57.3273 142.659 52.8228 140.283 48.5658C138.006 44.2098 134.69 40.7448 130.334 38.1708C126.077 35.5968 121.127 34.3098 115.484 34.3098C110.534 34.3098 106.227 35.3493 102.564 37.4283L74.6461 53.4663C73.6561 54.1593 73.1611 55.0998 73.1611 56.2878V69.0588L109.098 48.2688Z",
  "grok": "M210.484 312.759L343.465 210.383C349.984 205.364 359.302 207.322 362.408 215.117C378.758 256.231 371.454 305.64 338.925 339.563C306.397 373.487 261.137 380.927 219.768 363.983L174.577 385.803C239.394 432.008 318.104 420.581 367.289 369.251C406.303 328.564 418.386 273.104 407.088 223.091L407.19 223.198C390.807 149.726 411.218 120.359 453.03 60.3072C454.02 58.8833 455.01 57.4595 456 56L400.978 113.382V113.204L210.45 312.794 M183.042 337.641C136.519 291.294 144.54 219.567 184.236 178.203C213.59 147.59 261.683 135.096 303.666 153.464L348.755 131.75C340.632 125.627 330.221 119.042 318.275 114.414C264.277 91.2407 199.63 102.774 155.735 148.516C113.513 192.549 100.236 260.254 123.036 318.027C140.069 361.206 112.148 391.748 84.0229 422.575C74.0561 433.503 64.0553 444.431 56 456L183.007 337.677",
  "antigravity": "M482.04-859.61Q557.08-860 597-803.84t70,127.77q25.23,61.92 49.23,137.5t50.93,151.03q26.3,72.47 57.65,137.05T897-139.62q6.46,8.08 6.46,16.96T897-107.69t-16.35,6.38t-17.1-6.11q-76.71-64.2-122.32-140.01T646.48-376.6q-34.64-38.4-74.56-61.21t-89.63-22.42q-49.72-0.38-89.89,22.42T317.82-376.6q-49.36,53.37-94.98,129.18T100.52-107.42q-7.21,6.42-16.91,6.11t-16.54-6.58q-6.46-5.88-6.27-14.77t6.65-16.96q40.46-45.92 71.81-110.69t57.65-137.24Q223.85-463 247.85-538.3t49.23-137.77q29.18-71.36 69.55-127.64t115.41-55.9Zm-87.46,93.88q-31.27,48.5-53.88,102.27q-35.31,81.54-60.92,169.19T222.31-326.61q24.73-33.65 48.21-62.32t48.87-51.52q36.15-31.54 75.31-48.35t87.35-16.81t87.35,16.81t75.69,48.35q25,22.85 48.48,51.52t48.21,62.32q-31.85-80-57.46-167.65T623.77-663.46q-23-53.77-54.27-102.27t-87.46-48.5t-87.46,48.5Z"
};
const line = (d, colour, width = 1.5) => el('path', { d, fill: 'none', stroke: colour, 'stroke-width': width, 'stroke-linecap': 'round', 'stroke-linejoin': 'round' });

// kind -> [what to call it, how to draw it]. The kinds are the values of a
// pane's `icon` field.
const PANES = {
  claude: ['Claude Code', () => mark('claude', MARKS.claude, '#D97757')],
  codex: ['Codex', () => mark('codex', MARKS.codex, '#fff', '0 0 180 180')],
  gemini: ['Gemini', () => mark('gemini', MARKS.gemini, '#8E75B2')],
  grok: ['Grok', () => mark('grok', MARKS.grok, '#FCFCFC', '0 0 512 512')],
  opencode: ['OpenCode', () => mark('opencode', MARKS.opencode, '#d6dde6')],
  antigravity: ['Antigravity', () => mark('antigravity', MARKS.antigravity, '#d6dde6', '0 -960 960 960')],
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

// Hide a pane of a layout: put it away, as a window is. Drawn in the colour
// of the text around it.
export const hideIcon = () => frame('hide', line('M3.5 12h9', 'currentColor', 1.6));

// On a pane's header chips (chips.js), in the colour of the chip's text: a
// git branch, and the vault's key.
const small = (name, ...kids) => el('svg', { class: 'ico ico-' + name, width: 12, height: 12, viewBox: '0 0 16 16', 'aria-hidden': 'true', focusable: 'false' }, ...kids);
const ring = (cx, cy, r) => el('circle', { cx, cy, r, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.5 });
export const branchIcon = () => small('branch', ring(4, 3.5, 1.7), ring(4, 12.5, 1.7), ring(12, 5.5, 1.7), line('M4 5.2v5.6M12 7.2c0 3-8 1.5-8 3.6', 'currentColor'));
export const keyIcon = () => small('key', ring(5, 8, 2.8), line('M7.8 8h6.2M11.5 8v2.6M14 8v2', 'currentColor'));

export const stationIcon = () => frame('station',
  el('rect', { x: 1.5, y: 2.5, width: 13, height: 8.5, rx: 1.5, fill: 'none', stroke: '#8b98a7', 'stroke-width': 1.3 }),
  line('M5.5 13.5h5M8 11v2.5', '#8b98a7', 1.3));

export const pinIcon = () => frame('pin',
  el('path', { d: 'M5.5 2h5l-.8 4.2 2.3 2.3H4l2.3-2.3z', fill: '#f0b65a' }), line('M8 8.5V14', '#f0b65a', 1.4));

// ---- the file explorer (files.js) ----
// A file's picture by its kind, and the panel's own small signs. All are
// drawn in the colour of the text around them (`currentColor`): the panel's
// styles give each kind one of the theme's colours, so they follow a theme.
const cc = (d, w = 1.3) => line(d, 'currentColor', w);
const sheet = () => cc('M4 1.8h5l3 3v9.4H4zM9 1.8v3h3');
const FILES = {
  folder: () => [el('path', { d: 'M1.8 4.2c0-.6.4-1 1-1h3.1l1.4 1.5h5.9c.6 0 1 .4 1 1v6.6c0 .6-.4 1-1 1H2.8c-.6 0-1-.4-1-1z', fill: 'currentColor', 'fill-opacity': '.28', stroke: 'currentColor', 'stroke-width': 1.2, 'stroke-linejoin': 'round' })],
  'folder-open': () => [cc('M1.8 12.3V4.2c0-.6.4-1 1-1h3.1l1.4 1.5h5c.6 0 1 .4 1 1v1.2', 1.2), el('path', { d: 'M1.9 12.6l1.7-5c.1-.4.5-.7 1-.7h9.3c.6 0 .9.5.7 1l-1.5 4.4c-.1.4-.5.7-1 .7H2.6c-.5 0-.8-.2-.7-.4z', fill: 'currentColor', 'fill-opacity': '.28', stroke: 'currentColor', 'stroke-width': 1.2, 'stroke-linejoin': 'round' })],
  link: () => [cc('M6.8 9.2a2.6 2.6 0 0 0 3.7 0l2.2-2.2a2.6 2.6 0 0 0-3.7-3.7l-.9.9M9.2 6.8a2.6 2.6 0 0 0-3.7 0L3.3 9a2.6 2.6 0 0 0 3.7 3.7l.9-.9', 1.4)],
  code: () => [cc('M5.5 4.5L2 8l3.5 3.5M10.5 4.5L14 8l-3.5 3.5M9 3.2l-2 9.6', 1.4)],
  text: () => [sheet(), cc('M6 8h4M6 10.2h4M6 12.2h2.5', 1.1)],
  image: () => [el('rect', { x: 1.8, y: 2.8, width: 12.4, height: 10.4, rx: 1.6, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3 }), el('circle', { cx: 5.4, cy: 6.2, r: 1.2, fill: 'currentColor' }), cc('M2.4 12l3.4-3.2 2.4 2.2 2.3-2.6 3.2 3.4', 1.2)],
  audio: () => [cc('M6 11.5V3.5l6.5-1.4v8', 1.3), el('circle', { cx: 4.4, cy: 11.6, r: 1.8, fill: 'currentColor' }), el('circle', { cx: 10.9, cy: 10.2, r: 1.8, fill: 'currentColor' })],
  video: () => [el('rect', { x: 1.8, y: 3.2, width: 12.4, height: 9.6, rx: 1.6, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3 }), el('path', { d: 'M6.6 5.9v4.2L10.4 8z', fill: 'currentColor' })],
  archive: () => [el('rect', { x: 2.2, y: 2, width: 11.6, height: 12, rx: 1.6, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3 }), cc('M8 2.4v1.2M8 5v1.2M8 7.6v1.2', 1.5), el('rect', { x: 6.8, y: 10, width: 2.4, height: 2.2, rx: 0.5, fill: 'currentColor' })],
  pdf: () => [sheet(), el('rect', { x: 5.6, y: 8.2, width: 4.8, height: 3.6, rx: 0.6, fill: 'currentColor' })],
  data: () => [cc('M6 2.6c-1.4 0-1.8.7-1.8 1.8v1.8c0 .9-.5 1.6-1.4 1.8.9.2 1.4.9 1.4 1.8v1.8c0 1.1.4 1.8 1.8 1.8M10 2.6c1.4 0 1.8.7 1.8 1.8v1.8c0 .9.5 1.6 1.4 1.8-.9.2-1.4.9-1.4 1.8v1.8c0 1.1-.4 1.8-1.8 1.8', 1.3)],
  binary: () => [sheet()],
  // The panel's signs.
  up: () => [cc('M8 13V3.5M4 7.2l4-4 4 4', 1.5)],
  home: () => [cc('M2.5 8L8 3l5.5 5M4 7v6h3V9.6h2V13h3V7', 1.3)],
  refresh: () => [cc('M13 8a5 5 0 1 1-1.6-3.7M13 2.6v2.8h-2.8', 1.4)],
  hidden: () => [cc('M1.8 8s2.3-4 6.2-4 6.2 4 6.2 4-2.3 4-6.2 4-6.2-4-6.2-4z', 1.2), el('circle', { cx: 8, cy: 8, r: 1.8, fill: 'currentColor' })],
  edit: () => [cc('M3 13l.6-2.6 6.9-6.9 2 2-6.9 6.9zM9.4 4.6l2 2', 1.2)],
  split: () => [el('rect', { x: 2, y: 2, width: 12, height: 12, rx: 1.8, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3 }), cc('M2.4 8h11.2', 1.3)],
  pane: () => [el('rect', { x: 1.8, y: 2.8, width: 12.4, height: 10.4, rx: 1.8, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.3 }), cc('M4.4 6.2l2 1.8-2 1.8M8 10h3', 1.3)],
  tri: () => [el('path', { d: 'M6 4l4.5 4L6 12z', fill: 'currentColor' })],
};
export const fileIcon = (name) => frame('f-' + (FILES[name] ? name : 'binary'), ...(FILES[name] || FILES.binary)());

// Which picture a file gets, by the end of its name. Nothing is read from
// the file for this, and the name is only ever compared with this list.
const EXT = {
  code: 'rs js mjs cjs ts tsx jsx py rb go c h cc cpp hpp java kt swift sh bash zsh fish ps1 bat cmd pl php lua sql html htm css scss vue svelte zig nim ex exs erl hs ml clj scala dart r m mm cs fs',
  text: 'txt md markdown rst log text org tex adoc',
  image: 'png jpg jpeg gif webp svg bmp ico tif tiff heic heif avif',
  audio: 'mp3 wav flac ogg oga m4a aac opus aiff mid midi',
  video: 'mp4 mov mkv webm avi m4v mpg mpeg wmv',
  archive: 'zip tar gz tgz bz2 xz zst 7z rar jar deb rpm dmg iso',
  pdf: 'pdf',
  data: 'json jsonl yaml yml toml xml csv tsv ini conf cfg env lock plist db sqlite',
};
const KIND_OF = new Map(Object.entries(EXT).flatMap(([kind, list]) => list.split(' ').map(x => [x, kind])));
export function fileKind(entry, open = false) {
  if (entry.kind === 'link') return 'link';
  if (entry.kind === 'dir') return open ? 'folder-open' : 'folder';
  if (entry.kind !== 'file') return 'binary';
  const name = String(entry.name || '').toLowerCase(), dot = name.lastIndexOf('.');
  if (dot <= 0) return /^(makefile|dockerfile|readme|license|notice)$/.test(name) ? 'text' : 'binary';
  return KIND_OF.get(name.slice(dot + 1)) || 'binary';
}

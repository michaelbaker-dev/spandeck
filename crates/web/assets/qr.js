// A QR code drawn in the page, so that an address on this screen can be
// opened on a phone without the address going to any outside service.
// Bytes, error correction level M, versions 1 to 13 (up to 331 bytes).

// Error correction codewords per block, and blocks, for versions 1 to 13 at M.
const EC = [10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22], BLOCKS = [1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9];
const rawModules = (v) => { let r = (16 * v + 128) * v + 64; if (v >= 2) { const k = Math.floor(v / 7) + 2; r -= (25 * k - 10) * k - 55; if (v >= 7) r -= 36; } return r; };

// Reed-Solomon over GF(256), polynomial 0x11d.
const EXP = new Uint8Array(510), LOG = new Uint8Array(256);
for (let i = 0, x = 1; i < 255; i += 1) { EXP[i] = EXP[i + 255] = x; LOG[x] = i; x <<= 1; if (x & 256) x ^= 0x11d; }
const mul = (a, b) => (a && b ? EXP[LOG[a] + LOG[b]] : 0);
function divisor(degree) {
  let g = [1];
  for (let i = 0; i < degree; i += 1) { const next = new Array(g.length + 1).fill(0); g.forEach((c, j) => { next[j] ^= c; next[j + 1] ^= mul(c, EXP[i]); }); g = next; }
  return g.slice(1);
}
function remainder(data, gen) {
  const r = new Array(gen.length).fill(0);
  for (const b of data) { const f = b ^ r.shift(); r.push(0); gen.forEach((c, i) => { r[i] ^= mul(c, f); }); }
  return r;
}

const MASKS = [(x, y) => (x + y) % 2 === 0, (x, y) => y % 2 === 0, (x) => x % 3 === 0, (x, y) => (x + y) % 3 === 0, (x, y) => (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0,
  (x, y) => (x * y) % 2 + (x * y) % 3 === 0, (x, y) => ((x * y) % 2 + (x * y) % 3) % 2 === 0, (x, y) => ((x + y) % 2 + (x * y) % 3) % 2 === 0];

// How hard a pattern is to read, by the standard's four rules (the third
// one counted simply). Lower is better; any mask gives a valid code.
function penalty(m) {
  const n = m.length;
  let p = 0, dark = 0;
  const lines = [];
  for (let i = 0; i < n; i += 1) { lines.push(m[i].map(Number).join('')); lines.push(m.map(row => Number(row[i])).join('')); }
  for (const line of lines) {
    for (const run of line.match(/0{5,}|1{5,}/g) || []) p += run.length - 2;
    for (let at = 0; at + 11 <= n; at += 1) { const s = line.substr(at, 11); if (s === '10111010000' || s === '00001011101') p += 40; }
  }
  for (let y = 0; y < n; y += 1) for (let x = 0; x < n; x += 1) {
    if (m[y][x]) dark += 1;
    if (x + 1 < n && y + 1 < n && m[y][x] === m[y][x + 1] && m[y][x] === m[y + 1][x] && m[y][x] === m[y + 1][x + 1]) p += 3;
  }
  return p + Math.max(0, Math.ceil(Math.abs(dark * 20 - n * n * 10) / (n * n)) - 1) * 10;
}

// The modules of the code for this text, as rows of true (dark) and false;
// null when the text is too long for a version 13 code.
export function qrMatrix(text) {
  const bytes = [...new TextEncoder().encode(String(text))];
  let v = 1, room = 0;
  for (; ; v += 1) {
    if (v > 13) return null;
    room = Math.floor(rawModules(v) / 8) - EC[v - 1] * BLOCKS[v - 1];
    if (bytes.length <= room - (v < 10 ? 2 : 3)) break;
  }
  const bits = [];
  const put = (value, count) => { for (let i = count - 1; i >= 0; i -= 1) bits.push((value >>> i) & 1); };
  put(4, 4); put(bytes.length, v < 10 ? 8 : 16);
  for (const b of bytes) put(b, 8);
  put(0, Math.min(4, room * 8 - bits.length));
  while (bits.length % 8) bits.push(0);
  const data = [];
  for (let i = 0; i < bits.length; i += 8) data.push(bits.slice(i, i + 8).reduce((a, b) => a * 2 + b, 0));
  for (let pad = 0xec; data.length < room; pad ^= 0xec ^ 0x11) data.push(pad);

  // Blocks, each with its error correction, then interleaved.
  const nb = BLOCKS[v - 1], ec = EC[v - 1], total = Math.floor(rawModules(v) / 8), short = Math.floor(total / nb), gen = divisor(ec);
  const blocks = [];
  for (let i = 0, k = 0; i < nb; i += 1) { const len = short - ec + (i < nb - total % nb ? 0 : 1); const d = data.slice(k, k + len); k += len; blocks.push({ d, e: remainder(d, gen) }); }
  const words = [];
  for (let i = 0; i <= short - ec; i += 1) for (const b of blocks) if (i < b.d.length) words.push(b.d[i]);
  for (let i = 0; i < ec; i += 1) for (const b of blocks) words.push(b.e[i]);

  const n = v * 4 + 17;
  const m = Array.from({ length: n }, () => new Array(n).fill(false)), fixed = Array.from({ length: n }, () => new Array(n).fill(false));
  const set = (x, y, dark) => { m[y][x] = !!dark; fixed[y][x] = true; };
  for (let i = 0; i < n; i += 1) { set(6, i, i % 2 === 0); set(i, 6, i % 2 === 0); }
  for (const [cx, cy] of [[3, 3], [n - 4, 3], [3, n - 4]]) for (let dy = -4; dy <= 4; dy += 1) for (let dx = -4; dx <= 4; dx += 1) {
    const d = Math.max(Math.abs(dx), Math.abs(dy)), x = cx + dx, y = cy + dy;
    if (x >= 0 && x < n && y >= 0 && y < n) set(x, y, d !== 2 && d !== 4);
  }
  const align = [];
  if (v > 1) { const k = Math.floor(v / 7) + 2, step = Math.ceil((n - 13) / (k * 2 - 2)) * 2; for (let at = n - 7; align.length < k - 1; at -= step) align.unshift(at); align.unshift(6); }
  for (const y of align) for (const x of align) {
    if ((x === 6 && y === 6) || (x === 6 && y === n - 7) || (x === n - 7 && y === 6)) continue;
    for (let dy = -2; dy <= 2; dy += 1) for (let dx = -2; dx <= 2; dx += 1) set(x + dx, y + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
  }
  const format = (mask) => {
    let rem = mask;
    for (let i = 0; i < 10; i += 1) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
    const f = ((mask << 10) | rem) ^ 0x5412, bit = (i) => ((f >>> i) & 1) !== 0;
    for (let i = 0; i <= 5; i += 1) set(8, i, bit(i));
    set(8, 7, bit(6)); set(8, 8, bit(7)); set(7, 8, bit(8));
    for (let i = 9; i < 15; i += 1) set(14 - i, 8, bit(i));
    for (let i = 0; i < 8; i += 1) set(n - 1 - i, 8, bit(i));
    for (let i = 8; i < 15; i += 1) set(8, n - 15 + i, bit(i));
    set(8, n - 8, true);
  };
  format(0);
  if (v >= 7) {
    let rem = v;
    for (let i = 0; i < 12; i += 1) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
    const f = (v << 12) | rem;
    for (let i = 0; i < 18; i += 1) { const a = n - 11 + i % 3, b = Math.floor(i / 3), dark = ((f >>> i) & 1) !== 0; set(a, b, dark); set(b, a, dark); }
  }
  // The codewords, in a zigzag from the bottom right, two columns at a time.
  let i = 0;
  for (let right = n - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5;
    for (let vert = 0; vert < n; vert += 1) for (let j = 0; j < 2; j += 1) {
      const x = right - j, y = ((right + 1) & 2) === 0 ? n - 1 - vert : vert;
      if (!fixed[y][x] && i < words.length * 8) { m[y][x] = ((words[i >>> 3] >>> (7 - (i & 7))) & 1) !== 0; i += 1; }
    }
  }
  const flip = (mask) => { for (let y = 0; y < n; y += 1) for (let x = 0; x < n; x += 1) if (!fixed[y][x] && MASKS[mask](x, y)) m[y][x] = !m[y][x]; };
  let best = 0, least = Infinity;
  for (let mask = 0; mask < 8; mask += 1) { flip(mask); format(mask); const p = penalty(m); if (p < least) { least = p; best = mask; } flip(mask); }
  flip(best); format(best);
  return m;
}

// The code as a picture: dark on white whatever the theme, with the quiet
// border a reader needs. Null when the text does not fit.
export function qrSvg(text, size = 140) {
  const m = qrMatrix(text);
  if (!m) return null;
  const n = m.length, NS = 'http://www.w3.org/2000/svg';
  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('viewBox', `0 0 ${n + 8} ${n + 8}`); svg.setAttribute('width', size); svg.setAttribute('height', size);
  svg.setAttribute('role', 'img'); svg.setAttribute('shape-rendering', 'crispEdges'); svg.setAttribute('class', 'qr');
  const back = document.createElementNS(NS, 'rect');
  back.setAttribute('width', n + 8); back.setAttribute('height', n + 8); back.setAttribute('fill', '#fff');
  const dark = document.createElementNS(NS, 'path');
  let d = '';
  for (let y = 0; y < n; y += 1) for (let x = 0; x < n; x += 1) if (m[y][x]) d += `M${x + 4} ${y + 4}h1v1h-1z`;
  dark.setAttribute('d', d); dark.setAttribute('fill', '#000');
  svg.append(back, dark);
  return svg;
}

// A terminal's history as rows the phone scrolls by itself.
//
// The owner, 16 Nov 2026, on an iPhone: "On the phone, all terminals are
// choppy still, not like the chat window which is smooth.. have to fix
// terminal to be like chat." The chat is smooth because it is text that
// stays as it was drawn, inside an element the phone scrolls. The terminal
// drew its history again for every line the view moved. So on a phone the
// history is drawn here once, as plain rows above the live screen, and the
// terminal draws only its screen (term.js puts the two in one element that
// scrolls; DECISIONS.md, "On a phone a terminal's history is scrolled by the
// phone").
//
// The rows are made from the terminal's own record of its lines: one row a
// line, wrapped where the terminal wrapped it, in its font, size, line
// height and colours. What a program printed is put in as text and as style
// properties, never as markup.
//
// Rows are held in blocks of a hundred lines, each as tall as its lines
// whether it has been drawn or not, so nothing moves when one is drawn. A
// block is drawn when it is made if there are few of them (a program
// printing as usual), and otherwise a little later, the ones in sight first
// (a pane just opened, a resize, a program printing thousands of lines a
// second). Until it is drawn it holds a mark on the terminal's line, which
// follows the line as older ones are dropped.

export const CHUNK = 100;   // lines in a block
export const KEEP = 5000;   // lines kept while the end is in sight (what a station keeps)
export const MOST = 10000;  // lines kept while earlier output is being read: nothing is taken from under a reader before this
const EAGER = 300;          // more new lines than this at once are drawn later
const SLICE = 2;            // blocks drawn in one turn of the page, later

const ANSI = ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white',
  'brightBlack', 'brightRed', 'brightGreen', 'brightYellow', 'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite'];
// The terminal's own colours, where a look names none.
const PLAIN = ['#2e3436', '#cc0000', '#4e9a06', '#c4a000', '#3465a4', '#75507b', '#06989a', '#d3d7cf',
  '#555753', '#ef2929', '#8ae234', '#fce94f', '#729fcf', '#ad7fa8', '#34e2e2', '#eeeeec'];

// One of the 256 colours past the first sixteen: the cube, then the greys.
export function colour256(i) {
  const hex = (r, g, b) => '#' + [r, g, b].map(v => v.toString(16).padStart(2, '0')).join('');
  if (i >= 232) { const v = 8 + (i - 232) * 10; return hex(v, v, v); }
  const n = i - 16, step = (v) => (v ? 55 + v * 40 : 0);
  return hex(step(Math.floor(n / 36)), step(Math.floor(n / 6) % 6), step(n % 6));
}

const BOLD = 1, ITALIC = 2, DIM = 4, UNDER = 8, INVERSE = 16, HIDDEN = 32, STRIKE = 64, OVER = 128;
// A colour of a cell as CSS: null for the default one.
function css(palette, rgb, value, brighter) {
  if (rgb) return '#' + value.toString(16).padStart(6, '0');
  if (!palette) return null;
  const i = brighter && value < 8 ? value + 8 : value;
  return i < 16 ? `var(--a${i})` : colour256(i);
}

// One line of the terminal as a row. `cell` is the terminal's own object for
// reading a cell, used again for every one.
export function rowOf(line, cell, boldBright = true) {
  const row = document.createElement('div');
  row.className = 'hist-row';
  if (!line) return row;
  let text = '', key = null, style = null;
  const flush = () => {
    if (!text) return;
    if (!style) row.append(text);
    else {
      const s = document.createElement('span'), f = style.flags;
      let fg = css(style.fgp, style.fgr, style.fg, boldBright && (f & BOLD)), bg = css(style.bgp, style.bgr, style.bg, false);
      if (f & INVERSE) [fg, bg] = [bg || 'var(--tb)', fg || 'var(--tf)'];
      if (fg) s.style.color = fg;
      if (bg) s.style.backgroundColor = bg;
      if (f & BOLD) s.style.fontWeight = 'var(--tw)';
      if (f & ITALIC) s.style.fontStyle = 'italic';
      if (f & DIM) s.style.opacity = '.5';
      if (f & HIDDEN) s.style.visibility = 'hidden';
      const deco = [f & UNDER ? 'underline' : '', f & STRIKE ? 'line-through' : '', f & OVER ? 'overline' : ''].filter(Boolean).join(' ');
      if (deco) s.style.textDecoration = deco;
      s.textContent = text;
      row.append(s);
    }
    text = '';
  };
  // Up to the last cell that holds something: the rest of the line is empty.
  let end = line.length;
  for (; end > 0; end -= 1) {
    const c = line.getCell(end - 1, cell);
    if (!c || c.getChars() || !c.isBgDefault() || c.isInverse()) break;
  }
  for (let x = 0; x < end; x += 1) {
    const c = line.getCell(x, cell);
    if (!c || c.getWidth() === 0) continue;   // the second half of a wide character
    const fgd = c.isFgDefault(), bgd = c.isBgDefault();
    const flags = (c.isBold() ? BOLD : 0) | (c.isItalic() ? ITALIC : 0) | (c.isDim() ? DIM : 0) | (c.isUnderline() ? UNDER : 0)
      | (c.isInverse() ? INVERSE : 0) | (c.isInvisible() ? HIDDEN : 0) | (c.isStrikethrough() ? STRIKE : 0) | (c.isOverline() ? OVER : 0);
    const fgr = !fgd && c.isFgRGB(), bgr = !bgd && c.isBgRGB(), fg = fgd ? 0 : c.getFgColor(), bg = bgd ? 0 : c.getBgColor();
    const k = fgd && bgd && !flags ? '' : `${fgd ? '' : (fgr ? 'r' : 'p') + fg}/${bgd ? '' : (bgr ? 'r' : 'p') + bg}/${flags}`;
    if (k !== key) {
      flush();
      key = k;
      style = k ? { fgp: !fgd && !fgr, fgr, fg, bgp: !bgd && !bgr, bgr, bg, flags } : null;
    }
    text += c.getChars() || ' ';
  }
  flush();
  return row;
}

export class Hist {
  // `term`: the terminal whose history this is. It is never scrolled: it
  // draws its screen, and what has left the screen is drawn here.
  constructor(term) {
    this.term = term;
    this.el = document.createElement('div');
    this.el.className = 'term-hist';
    this.el.setAttribute('role', 'log');
    this.el.setAttribute('aria-label', 'Earlier output');
    this.top = document.createElement('div');
    this.top.className = 'hist-top';
    this.top.hidden = true;
    this.el.append(this.top);
    this.chunks = [];   // { el, n, mark, drawn, gone }
    this.lines = 0;
    this.end = null;    // a mark on the first line of the screen, as of the last look
    this.dirty = true;  // to be made again from the terminal's lines
    this.made = 0;      // how many times it was made again (tests, and term.js)
    this.reading = false;
    this._cell = null;
  }

  // The look: the terminal's colours as variables, so a change of theme
  // redraws nothing, and its font. `th`: the height of one of its rows.
  look(th) {
    const o = this.term.options, t = o.theme || {}, s = this.el.style;
    ANSI.forEach((name, i) => s.setProperty('--a' + i, t[name] || PLAIN[i]));
    s.setProperty('--tf', t.foreground || '#ffffff');
    s.setProperty('--tb', t.background || '#000000');
    s.setProperty('--tw', String(o.fontWeightBold || 'bold'));
    s.fontFamily = o.fontFamily;
    s.fontSize = o.fontSize + 'px';
    s.fontWeight = String(o.fontWeight || 'normal');
    s.letterSpacing = o.letterSpacing ? o.letterSpacing + 'px' : '';
    if (th > 0) { this.th = th; s.setProperty('--th', th + 'px'); }
  }

  _chunk(n, drawn) {
    const el = document.createElement('div');
    el.className = 'hist-chunk';
    el.style.setProperty('--n', String(n));
    const c = { el, n, mark: null, drawn, gone: false };
    this.chunks.push(c);
    this.el.append(el);
    return c;
  }

  // A mark on line `y` of the terminal's own (not a full-screen program's)
  // lines. Only while those are the lines being written to.
  _mark(y) {
    const b = this.term.buffer.active;
    return this.term.registerMarker(y - (b.baseY + b.cursorY));
  }

  _row(buffer, y) {
    this._cell = this._cell || buffer.getNullCell();
    return rowOf(y >= 0 ? buffer.getLine(y) : null, this._cell, this.term.options.drawBoldTextInBrightColors !== false);
  }

  // Bring the rows up to date with the terminal: what has left its screen
  // since the last time is added at the end. Returns how many lines were
  // added, or -1 when everything was made again (a resize, a cleared
  // history, a pane attached again). Nothing is done while a full-screen
  // program has the screen: it has no history, and ours is as it was.
  sync() {
    const t = this.term;
    if (t.buffer.active.type !== 'normal') return 0;
    const b = t.buffer.normal, base = b.baseY;
    if (this.dirty || !this.end) return this.rebuild();
    let from = this.end.line;
    if (this.end.isDisposed) {
      // The line it was on is gone. More lines than the terminal keeps went
      // by since the last look (it is full, and all it has is new); or its
      // history was cleared.
      if (b.length < (t.options.scrollback || 0) + t.rows) return this.rebuild();
      from = 0;
    }
    if (from > base) return this.rebuild();
    const n = base - from;
    if (!n) return 0;
    if (n <= EAGER) {
      for (let y = from; y < base; y += 1) {
        let c = this.chunks[this.chunks.length - 1];
        if (!c || !c.drawn || c.gone || c.plain || c.n >= CHUNK) c = this._chunk(0, true);
        c.el.append(this._row(b, y));
        c.n += 1;
        c.el.style.setProperty('--n', String(c.n));
      }
    } else {
      for (let y = from; y < base; y += CHUNK) {
        const k = Math.min(CHUNK, base - y);
        this._chunk(k, false).mark = this._mark(y + k - 1);
      }
      this._later();
    }
    this.lines += n;
    this.end.dispose();
    this.end = this._mark(base);
    return n;
  }

  // Made again from the terminal's lines: every block as tall as its lines,
  // the last ones drawn now and the rest a little later.
  rebuild() {
    const t = this.term;
    if (t.buffer.active.type !== 'normal') { this.dirty = true; return 0; }
    for (const c of this.chunks) if (c.mark) c.mark.dispose();
    if (this.end) this.end.dispose();
    this.chunks = [];
    this.el.replaceChildren(this.top);
    const b = t.buffer.normal, base = b.baseY;
    for (let y = 0; y < base; y += CHUNK) {
      const k = Math.min(CHUNK, base - y);
      this._chunk(k, false).mark = this._mark(y + k - 1);
    }
    this.lines = base;
    this.end = this._mark(base);
    this.dirty = false;
    this.made += 1;
    for (const c of this.chunks.slice(-SLICE)) this._draw(c);
    this._later();
    return -1;
  }

  // Draw a block from the lines its mark leads to. Lines the terminal has
  // dropped since are empty rows; a block all of whose lines are dropped
  // says so, and is taken away when nobody is reading above the end.
  _draw(c) {
    if (c.drawn) return;
    c.drawn = true;
    const m = c.mark;
    c.mark = null;
    if (!m || m.isDisposed) {
      c.gone = true;
      c.el.classList.add('gone');
      c.el.textContent = `${c.n} lines went by faster than this page keeps them.`;
      return;
    }
    const b = this.term.buffer.normal, last = m.line, rows = [];
    m.dispose();
    for (let y = last - c.n + 1; y <= last; y += 1) rows.push(this._row(b, y));
    c.el.append(...rows);
  }

  // The blocks in sight, and a screen either side, are drawn now. `top`,
  // `bottom`: in this element's own pixels (unscaled), from its first row.
  near(top, bottom) {
    if (!this.pending()) return;
    const th = this.th || 16;
    let y = 0, did = 0;
    for (const c of this.chunks) {
      const h = c.n * th;
      if (!c.drawn && y + h > top && y < bottom && did < 8) { this._draw(c); did += 1; }
      y += h;
    }
  }

  pending() { return this.chunks.some(c => !c.drawn); }

  // The rest, a few blocks in each turn of the page. The newest first; but
  // while someone is reading earlier output (`reading`, set by term.js) the
  // oldest first: those are the lines the terminal drops next, and nothing
  // is thrown away under a reader.
  _later() {
    if (this._timer) return;
    this._timer = setTimeout(() => {
      this._timer = null;
      const todo = this.chunks.filter(c => !c.drawn);
      for (const c of (this.reading ? todo.slice(0, SLICE) : todo.slice(-SLICE))) this._draw(c);
      if (this.pending()) this._later();
    }, 30);
  }

  // No more lines than are kept: whole blocks go from the top, and with the
  // end in sight also the blocks whose lines were lost. Returns how many
  // lines went from above (`reading`: someone is reading earlier output, and
  // then nothing goes until twice what is usually kept).
  trim(reading) {
    let gone = 0;
    if (!reading) {
      this.chunks = this.chunks.filter((c) => {
        if (!c.gone) return true;
        c.el.remove(); gone += c.n;
        return false;
      });
    }
    const most = reading ? MOST : KEEP;
    while (this.lines - gone > most && this.chunks.length > 1) {
      const c = this.chunks.shift();
      if (c.mark) c.mark.dispose();
      c.el.remove();
      gone += c.n;
      // What was fetched from the station is no longer the start.
      this.top.hidden = true;
    }
    if (gone) this.lines -= gone;
    return gone;
  }

  // Which line of the terminal a row is (rows count from the first), or -1
  // for one the terminal no longer has; and back.
  lineOf(row) {
    const y = row - (this.lines - this.term.buffer.normal.baseY);
    return y >= 0 && y < this.term.buffer.normal.baseY ? y : -1;
  }
  rowOfLine(y) { return y + (this.lines - this.term.buffer.normal.baseY); }

  // The text of the first rows that are drawn and say something: to find
  // where they are in what the station has.
  firstTexts(n) {
    const out = [];
    for (const c of this.chunks) {
      if (!c.drawn || c.gone) { if (out.length) break; continue; }
      for (const r of c.el.children) { out.push(r.textContent.replace(/\s+$/, '')); if (out.length >= n) return out; }
    }
    return out;
  }

  // Lines from before the first row, as the station gave them: text, with
  // no colours (that is all it keeps of them for asking). At the top.
  prepend(texts) {
    const blocks = [];
    for (let i = 0; i < texts.length; i += CHUNK) {
      const part = texts.slice(i, i + CHUNK), el = document.createElement('div');
      el.className = 'hist-chunk';
      el.style.setProperty('--n', String(part.length));
      for (const text of part) {
        const row = document.createElement('div');
        row.className = 'hist-row';
        row.textContent = text;
        el.append(row);
      }
      blocks.push({ el, n: part.length, mark: null, drawn: true, gone: false, plain: true });
    }
    this.top.after(...blocks.map(c => c.el));
    this.chunks.unshift(...blocks);
    this.lines += texts.length;
  }

  dispose() {
    clearTimeout(this._timer);
    for (const c of this.chunks) if (c.mark) c.mark.dispose();
    if (this.end) this.end.dispose();
  }
}

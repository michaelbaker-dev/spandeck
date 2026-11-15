// Markdown from an agent, shown safely (AGENTS.md rule 6; DECISIONS.md, "The
// conversation view").
//
// What an agent wrote is data. It is parsed here into a small tree and drawn
// with `h()`, which makes elements with createElement and puts every string
// in as a text node. Nothing is ever given to the browser to parse as HTML,
// so HTML in the text is shown as the characters typed. The elements made
// are the ones in ALLOWED and no others; the only attributes are `class` and,
// on a link, `href`, which is always an http or https address. A picture is
// never fetched: it is a link with its label. A link does not open when
// touched: `openLink` is called with the real destination, and the view asks
// first.

import { h } from './ui.js';

export const ALLOWED = ['DIV', 'P', 'H3', 'H4', 'H5', 'H6', 'STRONG', 'EM', 'DEL', 'CODE', 'PRE', 'UL', 'OL', 'LI', 'BLOCKQUOTE',
  'TABLE', 'THEAD', 'TBODY', 'TR', 'TH', 'TD', 'HR', 'A', 'BR', 'SPAN', 'BUTTON'];

const DEPTH = 8;

// An address a link may lead to, or null. Only http and https, and only one
// the browser itself reads as such.
export function safeUrl(raw) {
  const s = String(raw || '').trim();
  if (!/^https?:\/\//i.test(s) || /[\s<>"]/.test(s)) return null;
  try {
    const u = new URL(s);
    return u.protocol === 'http:' || u.protocol === 'https:' ? u.href : null;
  } catch (e) { return null; }
}

// ---- inline: text within a paragraph, a heading, a cell ----

// Each is tried at one place in the text (sticky), so nothing is copied to look.
const TICKS = /`+/y;
const LINK = /!?\[((?:[^\[\]\\]|\\.)*)\]\(\s*<?([^\s<>()]*(?:\([^\s<>()]*\)[^\s<>()]*)*)>?(?:\s+(?:"[^"]*"|'[^']*'))?\s*\)/y;
const ANGLED = /<(https?:\/\/[^\s<>]+)>/iy;
const BARE = /https?:\/\/[^\s<>`]+/iy;
const at = (re, src, i) => { re.lastIndex = i; return re.exec(src); };

function inline(src, opts, depth = 0, inLink = false) {
  const out = [];
  let text = '';
  const flush = () => { if (text) { out.push(text); text = ''; } };
  // A closing mark that was looked for and not found is not looked for again.
  const none = new Set();
  const closing = (mark, from) => {
    if (none.has(mark)) return -1;
    let at = from;
    for (;;) {
      at = src.indexOf(mark, at);
      if (at < 0) { none.add(mark); return -1; }
      const before = src[at - 1], after = src[at + mark.length];
      // Not after a space; `_` only where a word ends; and not half of a longer run.
      const ok = at > from && !/\s/.test(before) && (mark[0] !== '_' || !after || /[^A-Za-z0-9]/.test(after))
        && (mark.length > 1 || (after !== mark && before !== mark));
      if (ok) return at;
      at += 1;
    }
  };
  const link = (label, target, image) => {
    const url = safeUrl(target);
    if (!url) return null;
    const kids = image ? ['Picture: ' + (label || url)] : depth < DEPTH ? inline(label, opts, depth + 1, true) : [label];
    return h('a', { class: image ? 'md-link md-img' : 'md-link', href: url, onclick: (e) => { e.preventDefault(); opts.openLink && opts.openLink(url); } }, kids.length ? kids : url);
  };
  for (let i = 0; i < src.length;) {
    const c = src[i];
    if (c === '\\' && i + 1 < src.length && /[!-/:-@[-`{-~]/.test(src[i + 1])) { text += src[i + 1]; i += 2; continue; }
    if (c === '`') {
      const run = at(TICKS, src, i)[0];
      const end = src.indexOf(run, i + run.length);
      if (end > 0 && src[end + run.length] !== '`') {
        flush();
        out.push(h('code', { class: 'md-code-in' }, src.slice(i + run.length, end).replace(/^ (.*) $/s, '$1')));
        i = end + run.length;
        continue;
      }
      text += run; i += run.length; continue;
    }
    if ((c === '[' || (c === '!' && src[i + 1] === '[')) && !inLink) {
      const image = c === '!';
      const m = at(LINK, src, i);
      const made = m ? link(m[1], m[2], image) : null;
      if (made) { flush(); out.push(made); i += m[0].length; continue; }
    }
    if (c === '<' && !inLink) {
      const m = at(ANGLED, src, i);
      const made = m ? link(m[1], m[1], false) : null;
      if (made) { flush(); out.push(made); i += m[0].length; continue; }
    }
    if ((c === 'h' || c === 'H') && !inLink && !/[A-Za-z0-9]/.test(src[i - 1] || '')) {
      const m = at(BARE, src, i);
      if (m) {
        // The full stop that ends the sentence is not part of the address.
        let u = m[0].replace(/[.,;:!?'"*_~]+$/, '');
        while (u.endsWith(')') && u.split('(').length <= u.split(')').length - 1) u = u.slice(0, -1);
        const made = link(u, u, false);
        if (made) { flush(); out.push(made); i += u.length; continue; }
      }
    }
    if ((c === '*' || c === '_' || c === '~') && depth < DEPTH) {
      const prevWord = /[A-Za-z0-9]/.test(src[i - 1] || '');
      const marks = c === '~' ? ['~~'] : [c + c, c];
      let done = false;
      for (const mark of marks) {
        if (!src.startsWith(mark, i) || /\s/.test(src[i + mark.length] || ' ') || (c === '_' && prevWord)) continue;
        const end = closing(mark, i + mark.length);
        if (end < 0) continue;
        flush();
        out.push(h(mark === '~~' ? 'del' : mark.length === 2 ? 'strong' : 'em', {}, inline(src.slice(i + mark.length, end), opts, depth + 1, inLink)));
        i = end + mark.length;
        done = true;
        break;
      }
      if (done) continue;
    }
    if (c === '\n') {
      // Two spaces or a backslash before the end of a line break it there.
      if (text.endsWith('  ') || text.endsWith('\\')) { text = text.replace(/( +|\\)$/, ''); flush(); out.push(h('br', {})); }
      else text += '\n';
      i += 1;
      continue;
    }
    text += c;
    i += 1;
  }
  flush();
  return out;
}

// ---- blocks ----

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([^`]*)$/;
const HEADING = /^ {0,3}(#{1,6})[ \t]+(.*?)[ \t]*#*[ \t]*$/;
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const QUOTE = /^ {0,3}>[ ]?/;
const ITEM = /^( *)([-*+]|\d{1,9}[.)])( +|$)(.*)$/;
const TABLE_SEP = /^ *\|?[ \t]*:?-+:?[ \t]*(\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/;
const cells = (line) => {
  const s = line.trim().replace(/^\|/, '').replace(/\|$/, '');
  const out = [];
  let cur = '';
  for (let i = 0; i < s.length; i += 1) {
    if (s[i] === '\\' && s[i + 1] === '|') { cur += '|'; i += 1; } else if (s[i] === '|') { out.push(cur.trim()); cur = ''; } else cur += s[i];
  }
  out.push(cur.trim());
  return out;
};
const tableAt = (lines, i) => lines[i].includes('|') && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]) && lines[i + 1].includes('-')
  && cells(lines[i]).length === cells(lines[i + 1]).length;
const startsBlock = (lines, i) => FENCE.test(lines[i]) || HEADING.test(lines[i]) || RULE.test(lines[i]) || QUOTE.test(lines[i])
  || /^ {0,3}([-*+]|\d{1,9}[.)]) +\S/.test(lines[i]) || tableAt(lines, i);

function blocks(lines, depth) {
  const out = [];
  for (let i = 0; i < lines.length;) {
    const line = lines[i];
    if (!line.trim()) { i += 1; continue; }
    let m;
    if ((m = FENCE.exec(line))) {
      const mark = m[1], lang = (m[2].trim().split(/\s+/)[0] || '').replace(/[^A-Za-z0-9+#._-]/g, '').slice(0, 24);
      const body = [];
      i += 1;
      while (i < lines.length && !(lines[i].trim().startsWith(mark) && /^[`~]+$/.test(lines[i].trim()))) { body.push(lines[i]); i += 1; }
      i += 1;
      out.push({ t: 'code', lang, text: body.join('\n') });
      continue;
    }
    if ((m = HEADING.exec(line))) { out.push({ t: 'h', n: m[1].length, text: m[2] }); i += 1; continue; }
    if (RULE.test(line)) { out.push({ t: 'hr' }); i += 1; continue; }
    if (QUOTE.test(line) && depth < DEPTH) {
      const body = [];
      while (i < lines.length && QUOTE.test(lines[i])) { body.push(lines[i].replace(QUOTE, '')); i += 1; }
      out.push({ t: 'quote', kids: blocks(body, depth + 1) });
      continue;
    }
    if ((m = ITEM.exec(line)) && m[4] !== undefined && (m[3] || !m[4]) && depth < DEPTH) {
      const indent = m[1].length, ordered = /\d/.test(m[2]), first = ordered ? parseInt(m[2], 10) : 0;
      const items = [];
      while (i < lines.length) {
        const im = ITEM.exec(lines[i]);
        if (!im || im[1].length !== indent || /\d/.test(im[2]) !== ordered || RULE.test(lines[i])) break;
        const offset = indent + im[2].length + (im[3].length > 4 || !im[3].length ? 1 : im[3].length);
        const body = [im[4]];
        i += 1;
        while (i < lines.length) {
          const l = lines[i];
          if (!l.trim()) {
            // A blank line stays in the item only when what follows is indented under it.
            let j = i;
            while (j < lines.length && !lines[j].trim()) j += 1;
            if (j < lines.length && /^ */.exec(lines[j])[0].length >= offset) { body.push(''); i += 1; continue; }
            break;
          }
          const lead = /^ */.exec(l)[0].length;
          if (lead >= offset) { body.push(l.slice(offset)); i += 1; continue; }
          if (ITEM.test(l) || startsBlock(lines, i) || !body[body.length - 1].trim()) break;
          body.push(l.trim());
          i += 1;
        }
        const task = /^\[([ xX])\][ \t]+/.exec(body[0]);
        if (task) body[0] = body[0].slice(task[0].length);
        items.push({ task: task ? (task[1] === ' ' ? 'todo' : 'done') : null, kids: blocks(body, depth + 1) });
        // Blank lines between two items of one list.
        let j = i;
        while (j < lines.length && !lines[j].trim()) j += 1;
        const next = j < lines.length ? ITEM.exec(lines[j]) : null;
        if (j > i && next && next[1].length === indent && /\d/.test(next[2]) === ordered) i = j;
      }
      if (items.length) { out.push({ t: 'list', ordered, first, items }); continue; }
    }
    if (tableAt(lines, i)) {
      const head = cells(line), align = cells(lines[i + 1]).map(c => (/^:-+:$/.test(c) ? 'c' : /-:$/.test(c) ? 'r' : ''));
      const rows = [];
      i += 2;
      while (i < lines.length && lines[i].trim() && lines[i].includes('|')) { rows.push(cells(lines[i])); i += 1; }
      out.push({ t: 'table', head, align, rows });
      continue;
    }
    const para = [line.replace(/^ {1,3}/, '')];
    i += 1;
    while (i < lines.length && lines[i].trim() && !startsBlock(lines, i)) { para.push(lines[i].replace(/^ +/, '')); i += 1; }
    out.push({ t: 'p', text: para.join('\n') });
  }
  return out;
}

// The tree for a text: exported so that what the page draws can be checked.
export function parseMarkdown(src) {
  const text = String(src ?? '').replace(/\r\n?/g, '\n').replace(/\t/g, '    ');
  return blocks(text.split('\n'), 0);
}

// ---- drawing ----

function draw(b, opts) {
  const text = (s) => inline(s, opts);
  switch (b.t) {
    case 'p': return h('p', {}, text(b.text));
    // A reply's own headings sit under the page's: never an h1 or h2.
    case 'h': return h('h' + Math.min(6, b.n + 2), { class: 'md-h md-h' + b.n }, text(b.text));
    case 'hr': return h('hr', {});
    case 'quote': return h('blockquote', {}, b.kids.map(k => draw(k, opts)));
    case 'code': {
      const copy = h('button', { class: 'md-copy', onclick: () => opts.copy && opts.copy(b.text) }, 'Copy');
      return h('div', { class: 'md-code' },
        h('div', { class: 'md-code-head' }, h('span', { class: 'md-lang' }, b.lang || 'code'), opts.copy ? copy : null),
        h('pre', {}, h('code', {}, b.text)));
    }
    case 'list': return h(b.ordered ? 'ol' : 'ul', { class: b.ordered ? 'md-ol' : b.items.some(it => it.task) ? 'md-tasks' : null }, b.items.map((it, n) => {
      // A one-paragraph item is its text; anything more keeps its blocks.
      const one = it.kids.length === 1 && it.kids[0].t === 'p';
      const body = one ? text(it.kids[0].text) : it.kids.map(k => draw(k, opts));
      const mark = it.task ? h('span', { class: 'md-task ' + it.task }, it.task === 'done' ? '☑' : '☐')
        : b.ordered ? h('span', { class: 'md-n' }, `${b.first + n}.`) : null;
      return h('li', { class: it.task ? 'md-task-item' : null }, mark, mark ? h('div', { class: 'md-item' }, body) : body);
    }));
    case 'table': return h('div', { class: 'md-table' }, h('table', {},
      h('thead', {}, h('tr', {}, b.head.map((c, n) => h('th', { class: b.align[n] ? 'md-' + b.align[n] : null }, text(c))))),
      h('tbody', {}, b.rows.map(r => h('tr', {}, b.head.map((_, n) => h('td', { class: b.align[n] ? 'md-' + b.align[n] : null }, text(r[n] || ''))))))));
    default: return h('p', {}, String(b.text || ''));
  }
}

// opts: {openLink(url), copy(text)}. Returns one element holding the text.
export function renderMarkdown(src, opts = {}) {
  return h('div', { class: 'md' }, parseMarkdown(src).map(b => draw(b, opts)));
}

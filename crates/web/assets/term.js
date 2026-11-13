// One pane on screen: the activity strip and a live terminal.
//
// Sizing rule: one size per pane, set by whoever has control. A client in
// control fits the pane to its view and tells the station. Anyone else shows
// the pane at its real size, scaled down and letterboxed.

import { h, ago, dot, STATE_LABEL, stateOf, toast } from './ui.js';

const THEME = {
  background: '#0f1318', foreground: '#d6dde6', cursor: '#6cb6ff', selectionBackground: '#2b4a6b',
  black: '#1b2229', red: '#ff6b6b', green: '#57d38c', yellow: '#e5c07b', blue: '#6cb6ff', magenta: '#c792ea', cyan: '#56c8d8', white: '#d6dde6',
  brightBlack: '#5c6773', brightRed: '#ff8e8e', brightGreen: '#7ee2a8', brightYellow: '#f0d399', brightBlue: '#8fc7ff', brightMagenta: '#d9b3f2', brightCyan: '#7fdbe6', brightWhite: '#ffffff',
};

export class PaneTerm {
  // opts: {mode: 'control'|'watch', onMenu(ev, pane), onFocus(), compact}
  constructor(api, station, pane, opts = {}) {
    this.api = api;
    this.station = station;
    this.pane = pane;
    this.opts = opts;
    this.mode = opts.mode || 'control';
    this.control = this.mode === 'control';
    this.chan = null;
    this.disposed = false;
    this.size = { cols: pane.cols, rows: pane.rows };

    this.strip = h('div', { class: 'strip', oncontextmenu: (e) => this._menu(e) });
    this.body = h('div', { class: 'term-body' });
    this.note = h('div', { class: 'term-note', hidden: true });
    this.el = h('div', {
      class: 'pane', dataset: { pane: pane.id },
      onmousedown: () => opts.onFocus && opts.onFocus(this),
      oncontextmenu: (e) => this._menu(e),
      ondragover: (e) => { if (this.control) { e.preventDefault(); this.el.classList.add('dropping'); } },
      ondragleave: () => this.el.classList.remove('dropping'),
      ondrop: (e) => this._drop(e),
    }, this.strip, this.body, this.note);

    this.term = new Terminal({
      fontFamily: 'ui-monospace, "SF Mono", Menlo, "Cascadia Mono", Consolas, monospace',
      fontSize: opts.compact ? 11 : 13, theme: THEME, cursorBlink: true, scrollback: 5000,
      allowProposedApi: true, disableStdin: !this.control, macOptionIsMeta: true,
    });
    this.fit = new FitAddon.FitAddon();
    this.term.loadAddon(this.fit);
    this.term.onData((data) => { if (this.control && this.chan !== null) this.api.send('pane.input', { chan: this.chan, data }); });
    this.term.onResize(() => {});
    // Paste of images or files goes to the station as a file drop.
    this.body.addEventListener('paste', (e) => {
      const files = [...(e.clipboardData?.files || [])];
      if (files.length && this.control) { e.preventDefault(); e.stopPropagation(); files.forEach(f => this._upload(f)); }
    }, true);

    this.ro = new ResizeObserver(() => { clearTimeout(this._rt); this._rt = setTimeout(() => this.layout(), 60); });
    this.renderStrip();
    this._tick = setInterval(() => this.renderStrip(), 1000);
  }

  mount() {
    this.term.open(this.body);
    this.ro.observe(this.body);
    this.attach();
    return this;
  }

  async attach() {
    if (this.disposed) return;
    try {
      const handlers = {
        onData: (b) => this.term.write(b),
        onResync: (b) => { this.term.reset(); this.term.write(b); },
        onExit: (info) => this._note(`This pane's process ended (${describeExit(info)}).`),
        onUnreachable: () => this._note('The station stopped answering. Trying again…', () => this.attach()),
      };
      const res = await this.api.attach(this.station, this.pane.id, this.mode, handlers);
      if (this.disposed) { this.api.detach(res.chan); return; }
      this.chan = res.chan;
      this.size = { cols: res.info.cols, rows: res.info.rows };
      this.term.reset();
      this.term.resize(res.info.cols, res.info.rows);
      this.term.write(res.snapshot);
      this._note(res.exited ? `This pane's process ended (${describeExit(res.exited)}).` : null);
      this.layout();
    } catch (e) {
      this._note(`Cannot show this pane: ${e.message}`, () => this.attach());
    }
  }

  _note(text, retry) {
    this.note.hidden = !text;
    this.note.replaceChildren(...(text ? [text, retry ? h('button', { onclick: retry }, 'Retry') : null].filter(Boolean) : []));
    if (retry && text) { clearTimeout(this._retry); this._retry = setTimeout(() => !this.disposed && retry(), 3000); }
  }

  // Fit (in control) or letterbox (watching).
  layout() {
    if (this.disposed || !this.term.element) return;
    const xterm = this.term.element;
    if (this.control) {
      xterm.style.transform = '';
      xterm.style.width = '';
      this.body.classList.remove('letterbox');
      const dims = this.fit.proposeDimensions();
      if (!dims || !dims.cols || !dims.rows) return;
      if (dims.cols !== this.term.cols || dims.rows !== this.term.rows) this.term.resize(dims.cols, dims.rows);
      if (this.chan !== null && (dims.cols !== this.size.cols || dims.rows !== this.size.rows)) {
        this.size = { cols: dims.cols, rows: dims.rows };
        this.api.send('pane.resize', { chan: this.chan, cols: dims.cols, rows: dims.rows });
      }
    } else {
      if (this.term.cols !== this.size.cols || this.term.rows !== this.size.rows) this.term.resize(this.size.cols, this.size.rows);
      this.body.classList.add('letterbox');
      xterm.style.transform = '';
      const screen = xterm.querySelector('.xterm-screen');
      const w = screen?.offsetWidth || 1, hh = screen?.offsetHeight || 1;
      const scale = Math.min(1, this.body.clientWidth / w, this.body.clientHeight / hh);
      xterm.style.width = w + 'px';
      xterm.style.transform = `scale(${scale})`;
    }
  }

  // Someone else resized the pane: they have control now, we letterbox.
  resizedBy(controller, cols, rows) {
    this.size = { cols, rows };
    if (controller !== this.api.client && this.control) {
      this.control = false;
      this.term.options.disableStdin = true;
      this._lostControl = true;
    }
    this.layout();
    this.renderStrip();
  }

  takeControl() {
    if (this.mode === 'watch') return;
    this.control = true;
    this._lostControl = false;
    this.term.options.disableStdin = false;
    this.size = { cols: 0, rows: 0 };
    this.layout();
    this.renderStrip();
    this.term.focus();
  }

  focus() { if (this.control) this.term.focus(); }

  update(pane) { this.pane = pane; this.renderStrip(); }

  renderStrip() {
    const p = this.pane, s = stateOf(p);
    const activity = p.exited ? 'ended'
      : p.tool ? `${p.tool} · ${ago(p.tool_since_ms)}`
      : `${STATE_LABEL[s] || s}${s === 'working' || s === 'needs_you' ? ' · ' + ago(p.state_since_ms) : ''}`;
    const rpc = (m, extra = {}) => this.api.on(this.station, m, { pane: p.id, ...extra }).catch(e => toast(e.message, 'error'));
    this.strip.className = 'strip ' + s;
    // replaceChildren would print a missing piece as the word "null".
    this.strip.replaceChildren(...[
      dot(p),
      h('span', { class: 'strip-name', title: p.address }, p.name),
      p.agent !== 'shell' ? h('span', { class: 'chip' }, p.agent_name) : null,
      p.role ? h('span', { class: 'chip' }, p.role) : null,
      h('span', { class: 'strip-activity' }, activity),
      p.needs_reason && s === 'needs_you' ? h('span', { class: 'strip-reason', title: p.needs_reason }, p.needs_reason) : null,
      h('span', { class: 'grow' }),
      p.paused ? h('span', { class: 'chip warn' }, 'paused') : null,
      p.task ? h('span', { class: 'chip', title: 'Working on task ' + p.task }, 'task') : null,
      this.mode === 'watch' ? h('span', { class: 'chip' }, 'watching') : null,
      this._lostControl ? h('button', { class: 'mini', title: 'Another client set this pane\'s size', onclick: () => this.takeControl() }, 'Take control') : null,
      p.agent !== 'shell' && !p.exited && (s === 'working' || s === 'needs_you')
        ? h('button', { class: 'mini danger', title: 'Stop: interrupt this turn. The process stays.', onclick: () => rpc('pane.stop') }, 'Stop') : null,
      h('button', { class: 'icon-btn', title: 'Pane menu', 'aria-label': 'Pane menu', onclick: (e) => this._menu(e) }, '⋮'),
    ].filter(Boolean));
  }

  _menu(e) {
    e.preventDefault();
    e.stopPropagation();
    if (this.opts.onMenu) this.opts.onMenu(e, this);
  }

  _drop(e) {
    this.el.classList.remove('dropping');
    const files = [...(e.dataTransfer?.files || [])];
    if (!files.length || !this.control) return;
    e.preventDefault();
    files.forEach(f => this._upload(f));
  }

  // Image and file drop: the station writes the file and types its path.
  async _upload(file) {
    if (file.size > 64 * 1024 * 1024) return toast(`${file.name} is larger than 64 MB`, 'error');
    try {
      const bytes = new Uint8Array(await file.arrayBuffer());
      let bin = '';
      for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
      const res = await this.api.on(this.station, 'pane.drop_file', { pane: this.pane.id, name: file.name || 'pasted.png', data_b64: btoa(bin) });
      toast(`Saved on ${this.station}: ${res.path}`);
    } catch (err) { toast(`Could not send ${file.name}: ${err.message}`, 'error'); }
  }

  dispose() {
    this.disposed = true;
    clearInterval(this._tick);
    clearTimeout(this._retry);
    this.ro.disconnect();
    if (this.chan !== null) this.api.detach(this.chan);
    this.term.dispose();
    this.el.remove();
  }
}

function describeExit(info) {
  if (!info) return 'ended';
  if (info.Code !== undefined) return info.Code === 0 ? 'exit 0' : `exit code ${info.Code}`;
  if (info.Signal !== undefined) return `signal ${info.Signal}`;
  return 'status unknown';
}

// Tools for agents: the Control Deck's screen (DECISIONS.md, "Tools for
// agents"; the calls and their shapes are docs/tools-api.md).
//
// The owner, 18 Nov 2026: "we add a mcp or tool and it automatically do the
// setup on every machine and for every agent… it then would ask for this
// machine or the fleet? … I can come back later and decide to roll out
// across the fleet … It also lets me upgrade across the fleet".
//
// It behaves as the Agent programs dialog does (DECISIONS.md, "Every action
// says what it did, one job at a time, and is this the latest?"):
// - Which tools there are is the document the station serving this page
//   keeps (`tools.list`). How each stands on a station is that station's
//   own answer (`tools.status`): each is asked by itself and drawn when it
//   answers, so a slow one holds up only its own rows.
// - An install is a job its station keeps. What a row shows, and whether
//   its buttons are greyed, comes from that record and not from this page's
//   memory: it is the same on another device, and after this dialog is
//   closed and opened again.
// - A card is drawn again only when what it shows has changed, and the
//   others are left alone: the place the owner has scrolled to, an open
//   "Show what it said" and a button under a finger are not replaced every
//   second. The form a tool is added with is a dialog of its own and is
//   never drawn again while it is typed in.
// Everything a station, a registry or a program said (a line, a reason, a
// path, a command, what an installer printed) is put in as text.

import { h, dialog, field } from './ui.js';

const AGENTS = [['claude', 'Claude Code'], ['codex', 'Codex'], ['opencode', 'OpenCode'], ['antigravity', 'Antigravity']];
const RECENT_MS = 10 * 60 * 1000;
// What a change of version does to running panes (docs/tools-api.md, `restart`).
const RESTART = 'After the version changes on a station, its idle agents that have this tool restart and go back to their conversations; an agent in the middle of a turn, or waiting for you, restarts when its turn is over.';
const STEP = { runtime: 'getting spandeck\'s own Node', package: 'installing the package', setup: 'fetching what the tool needs', starting: 'starting it once' };
const NAME = /^[A-Za-z0-9_-]+$/;

const cmpVersion = (a, b) => {
  const x = String(a || '').split(/[.+-]/).map(n => parseInt(n, 10) || 0), y = String(b || '').split(/[.+-]/).map(n => parseInt(n, 10) || 0);
  for (let i = 0; i < 3; i += 1) if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) - (y[i] || 0);
  return 0;
};
// "3 min ago": whole minutes, so that a card with a time on it is not drawn
// again more often than once a minute.
const since = (ms) => {
  const s = Math.max(0, Date.now() - ms) / 1000;
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  return `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min ago`;
};
const plural = (n, one, many) => `${n} ${n === 1 ? one : many || one + 's'}`;
const agentNames = (list) => (list || []).map(a => (AGENTS.find(x => x[0] === a) || [a, a])[1]);
const commandOf = (t) => t.url || [t.command, ...(t.args || [])].filter(Boolean).join(' ');

let openNow = null;

// `ctx`: { api, stations() (the fleet's list, as the page holds it),
// hasCap(station, cap) }.
export function toolsPanel(ctx) {
  // One at a time: asked for again, the one that is open stays.
  if (openNow) return openNow;
  const { api, hasCap } = ctx;
  const home = api.station;
  const openedAt = Date.now();
  let open = true;
  let doc = { tools: null, updated_ms: 0, problem: null };   // the document, as the serving station holds it
  let cat = null;               // what spandeck can install (`tools.catalogue`)
  const view = new Map();       // station -> { rows: Map(name -> row) | null, updated_ms, problem }
  const pending = new Map();    // tool -> a change to the document that is being asked for
  const asked = new Map();      // "station tool" -> { words, label }: asked of that station, until it answers
  const errs = new Map();       // tool, or "station tool" -> a request that could not be made
  const notes = new Map();      // tool -> something the owner is to know about the last change
  const shown = new Set();      // "station tool": "Show what it said" is open
  const timers = new Map();
  const looking = new Map();
  const key = (st, name) => st + ' ' + name;

  // ---- the stations ----
  const stationsNow = () => ctx.stations().filter(s => s.state !== 'not_linked')
    .sort((a, b) => (b.station === home) - (a.station === home) || a.station.localeCompare(b.station));
  // 'asks': it is asked. 'old': it does not know these calls. 'away': it is not answering this station.
  const kindOf = (s) => (s.state !== 'online' ? 'away' : hasCap(s.station, 'tools') ? 'asks' : 'old');
  const asking = () => stationsNow().filter(s => kindOf(s) === 'asks').map(s => s.station);

  let draw = () => {};

  // The document. One question at a time; one asked for meanwhile is made
  // straight after.
  let listing = null, listAgain = false;
  const relist = () => {
    if (listing) { listAgain = true; return listing; }
    listing = (async () => {
      do {
        listAgain = false;
        try {
          const r = await api.call('tools.list', {}, { within: 9000 });
          doc = { tools: r.tools || [], updated_ms: r.updated_ms || 0, problem: null };
        } catch (e) { doc = { ...doc, problem: e.message }; }
      } while (listAgain && open);
      listing = null;
      if (open) draw();
    })();
    return listing;
  };
  const loadCatalogue = () => api.call('tools.catalogue', {}, { within: 9000 }).then((c) => { cat = c; if (open) draw(); return c; }).catch(() => null);

  // One look at a station at a time (as the Agent programs dialog looks).
  const look = (st) => {
    if (looking.has(st)) { looking.get(st).again = true; return looking.get(st).done; }
    if (!view.has(st)) view.set(st, { rows: null, updated_ms: 0, problem: null });
    const mine = { again: false };
    mine.done = (async () => {
      clearTimeout(timers.get(st));
      const v = view.get(st);
      do {
        mine.again = false;
        try {
          const r = await api.on(st, 'tools.status', {}, { within: 9000 });
          v.rows = new Map((r.tools || []).map(row => [row.name, row]));
          v.updated_ms = r.updated_ms || 0;
          v.problem = null;
        } catch (e) { v.problem = e.message; }
      } while (mine.again && open);
      looking.delete(st);
      if (!open) return;
      // It holds a newer document than the one shown, or a tool not shown.
      const known = new Set((doc.tools || []).map(t => t.name));
      if (v.rows && (v.updated_ms > doc.updated_ms || [...v.rows.keys()].some(n => !known.has(n)))) relist();
      draw();
      // A running job's last line changes without an event: looked at
      // again soon while something is going on there, now and then
      // otherwise (another device may start something).
      const tools = doc.tools || [];
      const live = v.problem || tools.some(t => ['installing', 'waiting'].includes(cell(t, { station: st, state: 'online' }).k))
        || [...(v.rows || new Map()).values()].some(r => r.latest?.checking);
      timers.set(st, setTimeout(() => { if (asking().includes(st)) look(st); }, live ? 1500 : 8000));
    })();
    looking.set(st, mine);
    return mine.done;
  };
  const lookAll = () => Promise.all(asking().map(look));
  const check = (st, again) => api.on(st, 'tools.check', again ? { again: true } : {}).then(() => look(st)).catch(() => {});

  // ---- how a tool stands on a station ----
  const inScope = (t, st) => !!t.fleet || (t.stations || []).includes(st);
  const pinned = (t, st) => (t.station_versions || {})[st] || t.version;
  // { k, r, why }. `k` is the station's own word for the row (`not_chosen`,
  // `installing`, `ready`, `failed`, `needs`), or: `away`, `old`, `asking`
  // (no answer yet), `silent` (it did not answer), `waiting` (its copy of
  // the document is not the one shown yet).
  function cell(t, s) {
    const st = s.station, kind = kindOf(s);
    if (kind !== 'asks') return { k: kind };
    const v = view.get(st);
    if (!v || (!v.rows && !v.problem)) return { k: 'asking' };
    if (v.problem) return { k: 'silent', why: v.problem };
    const r = v.rows.get(t.name);
    if (!r || !!r.chosen !== inScope(t, st) || (r.chosen && t.kind === 'catalogue' && r.wanted !== pinned(t, st))) return { k: 'waiting' };
    return { k: r.state, r };
  }
  const busy = (t, s) => {
    const c = cell(t, s);
    return c.k === 'waiting' || c.k === 'installing' || c.r?.job?.state === 'running' || asked.has(key(s.station, t.name));
  };
  const toolBusy = (t) => pending.has(t.name) || stationsNow().some(s => busy(t, s));

  // ---- what the owner asks for ----
  // A change to the document, asked of the station serving this page. It
  // answers at once; what the stations then do is in their own answers.
  const change = async (name, words, calls) => {
    pending.set(name, words);
    errs.delete(name); notes.delete(name);
    draw();
    try {
      let last = null;
      for (const [method, params] of calls) last = await api.call(method, params);
      if (last?.moved_from_setup) notes.set(name, `${name} was one of Agent setup's MCP servers and is a tool of its own now: a station on an older spandeck no longer gives it to its agents.`);
    } catch (e) {
      // No answer in time is not "no": the document says whether it was done.
      errs.set(name, /did not answer|could not confirm/.test(e.message)
        ? `${home} did not confirm in time. What is shown here is what it holds now: look before you try again.`
        : `Not done: ${e.message}`);
    }
    await relist();
    pending.delete(name);
    draw();
    lookAll();
  };
  // Try the install again, on one station.
  const install = async (t, st) => {
    const k = key(st, t.name);
    asked.set(k, { words: `Asking ${st} to try again…`, label: 'Installing…' });
    errs.delete(k);
    draw();
    const jobOf = () => view.get(st)?.rows?.get(t.name)?.job?.id;
    const before = jobOf();
    let failed = null, unsure = false;
    try { await api.on(st, 'tools.install', { name: t.name }); } catch (e) {
      if (/did not answer|could not confirm/.test(e.message)) unsure = true; else failed = `Could not start: ${e.message}`;
    }
    await look(st);
    asked.delete(k);
    const r = view.get(st)?.rows?.get(t.name);
    const began = r && (r.state === 'installing' || r.job?.state === 'running' || jobOf() !== before);
    if (failed) errs.set(k, failed);
    else if (unsure && !began) errs.set(k, `${st} did not confirm in time, and nothing is running there now: it may not have started. Look at the row, then try again.`);
    draw();
  };

  // A question before something that restarts agents or cannot be taken
  // back with one tap. `restart`: offer to leave running agents as they are.
  const confirm = (title, lines, okLabel, action, { restart = false, danger = false } = {}) => {
    const leave = h('input', { type: 'checkbox' });
    dialog(title, (shut) => h('div', { class: 'tl-confirm' },
      lines.map(l => h('p', {}, l)),
      restart ? h('p', { class: 'muted' }, RESTART) : null,
      restart ? h('label', { class: 'tl-check' }, leave, h('span', {}, 'Leave running agents as they are', h('small', {}, 'The new version then takes effect in panes started from now.'))) : null,
      h('div', { class: 'row end' }, h('button', { type: 'button', onclick: shut }, 'Cancel'),
        h('button', { type: 'button', class: danger ? 'primary danger' : 'primary', onclick: () => { shut(); action(!leave.checked); } }, okLabel))));
  };

  // ---- "Where?", for which agents, and the tick: shared by Add and Change ----
  const wherePart = (now) => {
    const group = 'tl-where-' + Math.random().toString(36).slice(2);
    const all = stationsNow();
    let pick = !now ? 'here' : now.fleet ? 'fleet' : (now.stations || []).length === 1 && now.stations[0] === home ? 'here' : 'some';
    const boxes = all.map(s => {
      const old = kindOf(s) === 'old';
      const box = h('input', { type: 'checkbox', value: s.station, disabled: old, checked: now && !now.fleet ? (now.stations || []).includes(s.station) : s.station === home });
      return { box, row: h('label', { class: 'tl-check' }, box, h('span', {}, s.station, old ? h('small', {}, 'needs a newer spandeck') : kindOf(s) === 'away' ? h('small', {}, 'not answering now: it installs when it is back') : null)) };
    });
    const some = h('div', { class: 'tl-some', hidden: pick !== 'some' }, boxes.map(b => b.row));
    const choice = (value, label, small) => h('label', { class: 'tl-choice' },
      h('input', { type: 'radio', name: group, value, checked: pick === value, onchange: () => { pick = value; some.hidden = pick !== 'some'; } }),
      h('span', {}, label, small ? h('small', {}, small) : null));
    const el = h('fieldset', { class: 'tl-where' }, h('legend', {}, 'Where?'),
      choice('here', `This station (${home})`),
      choice('some', 'Choose stations'), some,
      choice('fleet', 'The whole fleet, and stations that join later', 'Each station installs it for itself.'));
    return { el, value: () => (pick === 'fleet' ? { fleet: true } : pick === 'here' ? { stations: [home] } : { stations: boxes.filter(b => b.box.checked).map(b => b.box.value) }) };
  };
  const agentsPart = (now) => {
    const boxes = AGENTS.map(([id, name]) => ({ id, box: h('input', { type: 'checkbox', value: id, checked: !(now || []).length || now.includes(id) }), name }));
    const el = h('fieldset', { class: 'tl-agents' }, h('legend', {}, 'For which agents?'), boxes.map(b => h('label', { class: 'tl-check' }, b.box, h('span', {}, b.name))));
    // Every one ticked is "all", which includes a program spandeck learns later.
    return { el, value: () => { const on = boxes.filter(b => b.box.checked).map(b => b.id); return on.length === boxes.length ? [] : on; }, none: () => !boxes.some(b => b.box.checked) };
  };
  const tickPart = (now) => {
    const box = h('input', { type: 'checkbox', checked: !!now });
    const el = h('label', { class: 'tl-check tl-tick' }, box, h('span', {}, 'Agents may use this without asking me',
      h('small', {}, 'Claude Code and Codex honour this today. OpenCode and Antigravity go on asking: spandeck writes no such rule for them. It takes effect in panes started from now.')));
    return { el, value: () => box.checked };
  };
  const sameList = (a, b) => JSON.stringify([...(a || [])].sort()) === JSON.stringify([...(b || [])].sort());

  // ---- add a tool ----
  let adding = false;
  const addDialog = () => {
    if (adding) return;
    adding = true;
    const box = dialog('Add a tool', (shut) => {
      let what = null;
      const have = new Set((doc.tools || []).map(t => t.name));
      const name = h('input', { placeholder: 'name', 'aria-label': 'Name of the tool', autocapitalize: 'off', autocomplete: 'off', spellcheck: false });
      const cmd = h('input', { placeholder: 'npx -y some-mcp-server   or   https://mcp.example.com', 'aria-label': 'Command or URL', autocapitalize: 'off', autocomplete: 'off', spellcheck: false });
      const own = h('div', { class: 'tl-own', hidden: true },
        field('Name', name, 'What agents see it called. Letters, digits, - and _.'),
        field('Command or URL', cmd, 'spandeck installs nothing for a tool of your own: it only looks whether the command is on each station.'));
      const choice = (value, title, lines, off) => h('label', { class: 'tl-choice' + (off ? ' off' : '') },
        h('input', { type: 'radio', name: 'tl-what', value, disabled: off, onchange: () => { what = value; own.hidden = what !== 'own'; says.textContent = ''; if (what === 'own') name.focus(); } }),
        h('span', {}, h('b', {}, title), lines.filter(Boolean).map(l => h('small', {}, l))));
      const list = h('div', { class: 'tl-what' }, h('p', { class: 'muted' }, 'Asking what can be installed…'));
      const fill = (c) => {
        if (!open) return;
        const entries = c?.tools || [];
        list.replaceChildren(
          ...entries.map(e => choice(e.id, e.name, [e.gives, e.needs ? `Needs: ${e.needs}` : null, have.has(e.id) ? 'Already added.' : 'spandeck installs it and keeps it at a version.'], have.has(e.id))),
          choice('own', 'A tool of your own', ['A command or an address, as the MCP servers of Agent setup.']),
          own);
      };
      if (cat) fill(cat);
      loadCatalogue().then(c => fill(c || cat));
      const where = wherePart(null), agents = agentsPart(null), tick = tickPart(false);
      const says = h('div', { class: 'ap-job failed tl-says', role: 'alert' });
      const go = h('button', { class: 'primary tl-add-go', type: 'submit' }, 'Add');
      const submit = async () => {
        const w = where.value();
        let params;
        if (!what) return void (says.textContent = 'Choose a tool first.');
        if (what === 'own') {
          const n = name.value.trim(), line = cmd.value.trim();
          if (!NAME.test(n)) return void (says.textContent = 'Give it a name: letters, digits, - and _ only.');
          if (!line) return void (says.textContent = 'Give the command that starts it, or its address.');
          params = /^https?:\/\//.test(line) ? { name: n, url: line } : { name: n, command: line.split(/\s+/)[0], args: line.split(/\s+/).slice(1) };
        } else params = { catalogue: what };
        if (w.stations && !w.stations.length) return void (says.textContent = 'Choose at least one station.');
        if (agents.none()) return void (says.textContent = 'Choose at least one agent.');
        Object.assign(params, w, { agents: agents.value(), allow_without_asking: tick.value() });
        go.disabled = true; go.setAttribute('aria-disabled', 'true'); go.textContent = 'Adding…';
        says.textContent = '';
        try {
          const r = await api.call('tools.add', params);
          const t = r.tool || {};
          // Not the registry's answer: the owner is told which version, and why.
          if (r.version_from === 'catalogue') notes.set(t.name, `Added at ${t.version}, the version spandeck was tested with: the registry did not say which is the newest.`);
          shut();
          await relist();
          lookAll();
        } catch (e) {
          go.disabled = false; go.setAttribute('aria-disabled', 'false'); go.textContent = 'Add';
          says.textContent = /did not answer|could not confirm/.test(e.message) ? `${home} did not confirm in time. Close this and look whether the tool is listed before you add it again.` : e.message;
          relist();
        }
      };
      return h('form', { class: 'tools-add', onsubmit: (e) => { e.preventDefault(); submit(); } },
        h('fieldset', {}, h('legend', {}, 'Which tool?'), list),
        where.el, agents.el, tick.el, says,
        h('div', { class: 'row end sheet-foot' }, h('button', { type: 'button', onclick: shut }, 'Cancel'), go));
    }, { onClose: () => { adding = false; } });
    box.body.parentNode.classList.add('sheet', 'tools-sheet');
  };

  // ---- change where, for whom, and the tick ----
  const changeDialog = (t) => {
    const box = dialog(`Change ${t.title || t.name}`, (shut) => {
      const where = wherePart(t), agents = agentsPart(t.agents), tick = tickPart(t.allow_without_asking);
      const says = h('div', { class: 'ap-job failed tl-says', role: 'alert' });
      const save = () => {
        const w = where.value(), calls = [];
        if (w.stations && !w.stations.length) return void (says.textContent = 'Choose at least one station.');
        if (agents.none()) return void (says.textContent = 'Choose at least one agent.');
        if (!!w.fleet !== !!t.fleet || (!w.fleet && !sameList(w.stations, t.stations))) calls.push(['tools.scope', { name: t.name, ...w }]);
        const set = {};
        if (!sameList(agents.value(), t.agents)) set.agents = agents.value();
        if (tick.value() !== !!t.allow_without_asking) set.allow_without_asking = tick.value();
        if (Object.keys(set).length) calls.push(['tools.set', { name: t.name, ...set }]);
        shut();
        if (calls.length) change(t.name, 'Changing it…', calls);
      };
      return h('form', { class: 'tools-add', onsubmit: (e) => { e.preventDefault(); save(); } },
        t.from_setup ? h('p', { class: 'muted' }, 'This is one of Agent setup\'s MCP servers. A change here makes it a tool of its own: a station on an older spandeck then stops giving it to its agents.') : null,
        where.el,
        h('p', { class: 'muted small' }, 'A station that comes in installs it for itself. One that leaves stops giving it to agents started from now, and keeps its files.'),
        agents.el, tick.el, says,
        h('div', { class: 'row end sheet-foot' }, h('button', { type: 'button', onclick: shut }, 'Cancel'), h('button', { class: 'primary', type: 'submit' }, 'Save')));
    });
    box.body.parentNode.classList.add('sheet', 'tools-sheet');
  };

  // ---- one station's row of a tool's card ----
  // A result stays on its row while it is news: ten minutes, or since this
  // dialog was opened; one that did not end well stays until something
  // else is done.
  const quietJob = (j) => !['failed', 'needs', 'interrupted'].includes(j.outcome) && j.ended_ms < openedAt && Date.now() - j.ended_ms > RECENT_MS;
  const doing = (j) => (j?.action === 'upgrade' ? 'Upgrading…' : j?.action === 'rollback' ? 'Going back…' : 'Installing…');
  const CHIP = {
    away: ['warn', 'not answering'], silent: ['warn', 'not answering'], old: ['warn', 'needs a newer spandeck'], asking: ['', 'asking…'],
    waiting: ['', 'waiting…'], not_chosen: ['', 'not chosen here'], installing: ['', 'installing'], failed: ['bad', 'failed'], needs: ['warn', 'needs something'],
  };
  const chipOf = (t, c) => {
    if (c.k === 'ready') return ['ok', c.r.version ? `ready ${c.r.version}` : 'ready'];
    return CHIP[c.k] || ['', String(c.k)];
  };
  const stationRow = (t, s) => {
    const st = s.station, c = cell(t, s), r = c.r, k = key(st, t.name), j = r?.job, off = busy(t, s);
    const [cls, word] = chipOf(t, c);
    const marks = [], status = [], acts = [];
    const button = (label, kind, on, title) => h('button', { type: 'button', class: kind, disabled: off, 'aria-disabled': off ? 'true' : 'false', 'aria-busy': off ? 'true' : null, title, onclick: on }, label);
    if (c.k === 'away') status.push(h('div', { class: 'muted small' }, `${st} is off or out of reach. What it is to have is kept for it: it sets itself up when it is back.`));
    if (c.k === 'silent') status.push(h('div', { class: 'muted small' }, c.why));
    if (c.k === 'old') status.push(h('div', { class: 'muted small' }, t.from_setup ? `${st} gives it to its agents as before, and cannot say more: update spandeck there.` : `${st} runs spandeck ${s.version || 'from before'}, which installs no tools and gives its agents none of these. Update spandeck there.`));
    if (c.k === 'waiting') status.push(h('div', { class: 'ap-job running' }, h('span', { class: 'ap-spin' }), `Waiting for ${st} to hear of the change…`));
    if (r) {
      if (r.own_pin) marks.push(h('span', { class: 'chip tl-own-pin', title: `${st} runs a version of its own; the fleet's is ${t.version}.` }, 'its own version'));
      if (t.kind === 'catalogue' && r.chosen && r.wired_version && r.wanted && r.wired_version !== r.wanted) marks.push(h('span', { class: 'tl-mark warn-text tl-drift' }, `agents get ${r.wired_version} until ${r.wanted} is ready`));
      else if (t.kind === 'catalogue' && r.chosen && c.k !== 'ready' && !r.wired) marks.push(h('span', { class: 'tl-mark muted' }, 'agents start without it'));
      if (t.kind === 'custom' && c.k === 'ready') marks.push(h('span', { class: 'tl-mark muted' }, r.path ? ['found at ', h('span', { class: 'mono' }, r.path)] : 'an address: not asked whether it answers'));
      if (t.kind === 'custom' && c.k === 'needs') marks.push(h('span', { class: 'tl-mark muted' }, 'agents are given it all the same'));
      if (r.chosen && r.panes && t.kind === 'catalogue') marks.push(h('span', { class: 'tl-mark muted', title: RESTART }, `${plural(r.panes, 'agent')} running here`));
    }
    let said = false;
    if (asked.has(k)) status.push(h('div', { class: 'ap-job running' }, h('span', { class: 'ap-spin' }), asked.get(k).words));
    else if (c.k === 'installing') {
      const what = j?.state === 'running'
        ? `${j.action === 'upgrade' ? `Upgrading ${j.from || '?'} → ${j.to || '?'}` : j.action === 'rollback' ? `Going back to ${j.to || '?'}` : `Installing ${j.to || ''}`.trim()}…${STEP[j.step] ? ' ' + STEP[j.step] : ''}`
        : 'Starting…';
      status.push(h('div', { class: 'ap-job running', title: j?.command || '' }, h('span', { class: 'ap-spin' }),
        h('span', {}, what, j?.state === 'running' && j.by ? h('span', { class: 'muted' }, ` · asked by ${j.by}`) : null)));
      if (j?.state === 'running' && j.last_line) status.push(h('div', { class: 'ap-line mono' }, j.last_line));
    } else if (r && j && j.state !== 'running' && j.ended_ms && !quietJob(j) && (j.says || j.reason)) {
      const kind = j.outcome === 'failed' ? 'failed' : ['needs', 'interrupted'].includes(j.outcome) ? 'unsure' : 'good';
      // Said only where it is about the version this station is to run.
      if (kind === 'good' || !r.wanted || !j.to || j.to === r.wanted) {
        said = true;
        const now = (j.restarted || []).length, later = (j.restart_when_idle || []).length;
        status.push(h('div', { class: 'ap-job tl-result ' + kind }, h('span', {}, j.says || j.reason,
          now ? ` · ${now} restarted into ${now === 1 ? 'its conversation' : 'their conversations'}` : '',
          later ? ` · ${later} ${later === 1 ? 'restarts' : 'restart'} when the turn ends` : ''),
        h('span', { class: 'muted' }, ` · ${since(j.ended_ms)}`)));
        if (kind !== 'good' && (j.output || '').trim()) status.push(h('details', { class: 'ap-output', open: shown.has(k), ontoggle: (e) => { if (e.target.open) shown.add(k); else shown.delete(k); } },
          h('summary', {}, 'Show what it said'), h('pre', { class: 'mono' }, j.output)));
      }
    }
    if (!said && ['failed', 'needs'].includes(c.k) && r.line) status.push(h('div', { class: 'ap-job ' + (c.k === 'failed' ? 'failed' : 'unsure') }, r.line));
    if (errs.has(k)) status.push(h('div', { class: 'ap-job failed' }, errs.get(k)));

    if (r && r.chosen && t.kind === 'catalogue') {
      const stuck = ['failed', 'needs'].includes(c.k), newer = r.latest?.state === 'available' ? r.latest.version : null;
      const before = (r.on_disk || []).filter(v => r.wanted && cmpVersion(v, r.wanted) < 0).sort(cmpVersion).pop();
      const title = t.title || t.name;
      if (off) acts.push(button(asked.get(k)?.label || doing(j), 'tl-doing', () => {}));
      else {
        if (before) acts.push(button(`Go back to ${before}`, 'quiet tl-back', () => confirm(`Go back to ${before} on ${st}?`,
          [`${title} ${before} is still on ${st}'s disk, so this is at once. The other stations stay as they are.`], `Go back to ${before}`,
          (restart) => change(t.name, `Going back to ${before} on ${st}…`, [['tools.upgrade', { name: t.name, station: st, to: before, restart }]]), { restart: true }), `${before} is still on ${st}'s disk`));
        if (stuck) acts.push(button('Try again', newer ? 'tl-retry' : 'primary tl-retry', () => install(t, st), `Install ${r.wanted || ''} on ${st} again`));
        if (newer) acts.push(button(`Upgrade on ${st}`, 'primary tl-upgrade', () => confirm(`Upgrade ${title} to ${newer} on ${st}?`,
          [`${st} installs ${newer} for itself and the other stations stay as they are. When it works there, roll it out to the fleet.`], `Upgrade on ${st}`,
          (restart) => change(t.name, `Upgrading to ${newer} on ${st}…`, [['tools.upgrade', { name: t.name, station: st, to: newer, restart }]]), { restart: true }), `${r.wanted} → ${newer}`));
      }
    }
    return h('div', { class: 'tl-st', 'data-station': st, 'data-state': c.k, 'data-busy': off ? '1' : null },
      h('div', { class: 'tl-st-name' }, h('b', {}, st), st === home ? h('span', { class: 'muted small' }, 'this station') : null),
      h('div', { class: 'tl-st-state' }, h('span', { class: 'chip tl-chip ' + cls }, word), marks),
      h('div', { class: 'tl-st-status' }, status),
      h('div', { class: 'tl-st-acts' }, acts));
  };

  // ---- is it the newest? one mark for the tool, from what its stations say ----
  const latestMark = (t) => {
    // Of the version the fleet runs, where a station runs it; a station on
    // a version of its own says in its own row what it can go to.
    const rows = stationsNow().map(s => cell(t, s).r).filter(r => r?.latest);
    const all = (rows.some(r => r.wanted === t.version) ? rows.filter(r => r.wanted === t.version) : rows).map(r => r.latest);
    const of = (state) => all.find(l => l.state === state);
    if (of('available')) return h('span', { class: 'tl-mark ap-available tl-latest' }, `${of('available').version} available`);
    if (all.some(l => l.checking)) return h('span', { class: 'tl-mark muted tl-latest' }, 'checking for a newer one…');
    if (of('latest')) return h('span', { class: 'tl-mark ap-latest tl-latest' }, '✓ latest');
    if (of('unknown')) return h('span', { class: 'tl-mark ap-unknown tl-latest' }, `could not check: ${of('unknown').why || 'no reason given'}`);
    if (of('off')) return h('span', { class: 'tl-mark muted tl-latest', title: '[agents] check_latest = false in that station\'s config.toml: it asks no registry.' }, 'checks are off');
    return all.length ? h('span', { class: 'tl-mark muted tl-latest' }, 'not checked yet') : null;
  };

  // ---- one tool's card ----
  const card = (t) => {
    const stations = stationsNow(), title = t.title || t.name, off = toolBusy(t), wait = pending.has(t.name);
    const entry = (cat?.tools || []).find(e => e.id === t.catalogue);
    const button = (label, kind, on, dis = off) => h('button', { type: 'button', class: kind, disabled: dis, 'aria-disabled': dis ? 'true' : 'false', onclick: on }, label);
    // A version one station runs for itself and that is ready there: it can go to the fleet.
    const proven = [...new Set(stations.map(s => cell(t, s)).filter(c => c.k === 'ready' && c.r.own_pin && c.r.version && c.r.version !== t.version).map(c => c.r.version))];
    const pins = Object.entries(t.station_versions || {});
    const inIt = stations.filter(s => inScope(t, s.station)).map(s => s.station);
    const agents = (t.agents || []).length ? agentNames(t.agents).join(', ') : 'every agent';
    const acts = [];
    for (const v of proven) acts.push(button(`Roll out ${v} to the fleet`, (cmpVersion(v, t.version) > 0 ? 'primary ' : '') + 'tl-rollout-version', () => confirm(`Roll out ${title} ${v} to the fleet?`,
      [`Every station the tool is for installs ${v} for itself: ${inIt.join(', ') || 'none yet'}.`], `Roll out ${v}`,
      (restart) => change(t.name, `Rolling out ${v}…`, [['tools.rollout', { name: t.name, to: v, restart }]]), { restart: true })));
    if (!t.fleet) acts.push(button('Roll out to the whole fleet', proven.length ? 'tl-rollout' : 'primary tl-rollout', () => confirm(`Roll out ${title} to the whole fleet?`,
      [t.kind === 'catalogue' ? `Each station installs it for itself: ${stations.map(s => s.station).join(', ')}. A station that joins later gets it too.` : `Every station gives it to its agents, and one that joins later too.`,
        'Nothing restarts: agents get it at their next start.',
        t.from_setup ? 'It is one of Agent setup\'s MCP servers and becomes a tool of its own.' : null].filter(Boolean), 'Roll out',
      () => change(t.name, 'Rolling out to the whole fleet…', [['tools.scope', { name: t.name, fleet: true }]]))));
    acts.push(button('Change where', 'tl-change', () => changeDialog(t)));
    acts.push(button('Remove', 'quiet danger tl-remove', () => confirm(`Remove ${title}?`,
      ['Agents lose the tool at their next start, on every station. Agents that are running keep it until then.',
        t.kind === 'catalogue' ? 'Its files stay on each station\'s disk, so adding it again is at once.' : null,
        t.from_setup ? 'It is taken out of Agent setup\'s MCP servers.' : null].filter(Boolean), 'Remove',
      () => change(t.name, 'Removing it…', [['tools.remove', { name: t.name }]]), { danger: true }), wait));
    return h('section', { class: 'note tl-card', 'data-tool': t.name, 'data-busy': off ? '1' : null },
      h('div', { class: 'tl-head' },
        h('h3', {}, title),
        title !== t.name ? h('span', { class: 'muted mono' }, t.name) : null,
        h('span', { class: 'chip' }, t.kind === 'catalogue' ? 'installed by spandeck' : t.from_setup ? 'from Agent setup' : 'of your own')),
      entry?.gives ? h('div', { class: 'muted small tl-gives' }, entry.gives) : null,
      t.kind === 'custom' ? h('div', { class: 'mono small tl-command' }, commandOf(t)) : null,
      h('div', { class: 'small tl-scope' },
        h('b', {}, t.fleet ? 'For the whole fleet, and stations that join later' : `For ${(t.stations || []).join(', ') || 'no station'}`),
        h('span', { class: 'muted' }, ` · ${agents} · ${t.allow_without_asking ? 'agents may use it without asking you' : 'agents ask you before they use it'}`)),
      t.kind === 'catalogue' ? h('div', { class: 'tl-version' },
        h('span', {}, 'Version ', h('b', {}, t.version || 'not set'), pins.length || !t.fleet ? '' : ' on every station'),
        pins.map(([st, v]) => h('span', { class: 'tl-mark warn-text tl-pin' }, `${st} runs ${v}`)),
        latestMark(t)) : null,
      wait ? h('div', { class: 'ap-job running tl-pending' }, h('span', { class: 'ap-spin' }), pending.get(t.name)) : null,
      errs.has(t.name) ? h('div', { class: 'ap-job failed tl-err' }, errs.get(t.name)) : null,
      notes.has(t.name) ? h('div', { class: 'ap-job unsure tl-note' }, notes.get(t.name)) : null,
      h('div', { class: 'tl-stations' }, stations.map(s => stationRow(t, s))),
      h('div', { class: 'tl-acts' }, acts));
  };
  const cardSig = (t) => JSON.stringify([t, pending.get(t.name), errs.get(t.name), notes.get(t.name), (cat?.tools || []).find(e => e.id === t.catalogue)?.gives,
    stationsNow().map(s => { const k = key(s.station, t.name), v = view.get(s.station); return [s.station, s.state, s.version, kindOf(s), !!v?.rows, v?.problem, v?.rows?.get(t.name), asked.get(k), errs.get(k), shown.has(k)]; }),
    Math.floor(Date.now() / 60000)]);

  // ---- the whole fleet in a line ----
  const top = () => {
    const tools = doc.tools || [], stations = stationsNow();
    const cells = tools.flatMap(t => stations.map(s => ({ t, s, c: cell(t, s) })));
    const bad = cells.filter(x => ['failed', 'needs'].includes(x.c.k) && x.t.kind === 'catalogue');
    const going = cells.filter(x => ['installing', 'waiting'].includes(x.c.k) || asked.has(key(x.s.station, x.t.name)));
    const unsure = cells.filter(x => x.c.k === 'asking');
    const checking = cells.some(x => x.c.r?.latest?.checking);
    let words, cls = 'muted';
    if (doc.tools === null) words = doc.problem ? `${home} did not say which tools there are: ${doc.problem}` : 'Asking which tools there are…';
    else if (!tools.length) words = 'No tools yet';
    else if (bad.length) { words = `${plural(bad.length, 'install')} not ready: ${bad.slice(0, 3).map(x => `${x.t.title || x.t.name} on ${x.s.station}`).join(', ')}${bad.length > 3 ? '…' : ''}`; cls = 'tl-bad'; }
    else if (going.length) words = `Setting up on ${plural(new Set(going.map(x => x.s.station)).size, 'station')}…`;
    else if (unsure.length) words = 'Asking the stations…';
    else { words = 'Everything is ready where you chose it'; cls = 'ap-latest'; }
    return h('div', { class: 'ap-fleet tl-top' },
      h('div', { class: 'ap-fleet-words' }, h('b', { class: cls }, words)),
      h('div', { class: 'ap-fleet-actions' },
        tools.some(t => t.kind === 'catalogue') ? h('button', { type: 'button', class: 'quiet tl-check-again', disabled: checking, 'aria-disabled': checking ? 'true' : 'false', title: 'Ask the registry again whether a newer version is published', onclick: () => asking().forEach(st => check(st, true)) }, checking ? 'Checking…' : 'Check again') : null,
        h('button', { type: 'button', class: 'primary tl-add', onclick: addDialog }, 'Add a tool')));
  };
  // The grid at a glance, where there is room for a table: tool by station.
  const matrix = () => {
    const tools = doc.tools || [], stations = stationsNow();
    if (!tools.length || stations.length < 2) return h('div', { class: 'tl-matrix', hidden: true });
    return h('div', { class: 'tl-matrix' }, h('table', { class: 'table' },
      h('thead', {}, h('tr', {}, h('th', {}, 'Tool'), stations.map(s => h('th', {}, s.station)))),
      h('tbody', {}, tools.map(t => h('tr', {}, h('td', {}, h('b', {}, t.title || t.name)),
        stations.map(s => { const [cls, word] = chipOf(t, cell(t, s)); return h('td', { class: 'tl-m-' + (cls || 'plain') }, word); }))))));
  };
  const slotSig = () => JSON.stringify([doc, stationsNow().map(s => [s.station, s.state, kindOf(s)]), [...asked.keys()],
    [...view].map(([st, v]) => [st, !!v.rows, v.problem, v.rows && [...v.rows.values()].map(r => [r.name, r.state, r.version, r.chosen, r.wanted, r.latest?.checking])])]);

  // ---- the dialog ----
  const off = api.onEvent((ev) => {
    // Both mean "ask again", and carry nothing to show. However many come,
    // a station is asked one question at a time (`look`, `relist`), and no
    // more often than four times a second.
    if (ev.kind === 'tools.updated') soon(null);
    else if (ev.kind === 'tools.status') soon(ev.data?.station || home);
  });
  let soonTimer = null, soonList = false;
  const soonLooks = new Set();
  function soon(st) {
    if (st) soonLooks.add(st); else soonList = true;
    if (soonTimer) return;
    soonTimer = setTimeout(() => {
      soonTimer = null;
      const all = soonList, some = [...soonLooks];
      soonList = false; soonLooks.clear();
      if (!open) return;
      if (all) { relist(); lookAll(); } else for (const s of some) if (asking().includes(s)) look(s);
    }, 250);
  }
  // Now and then: a station that came back, or joined, is asked too, and
  // the MCP servers of Agent setup are read again (no event tells of them).
  const round = setInterval(() => {
    for (const st of asking()) if (!view.has(st)) look(st).then(() => check(st, false));
    for (const st of [...view.keys()]) if (!asking().includes(st)) { clearTimeout(timers.get(st)); view.delete(st); }
    relist();
  }, 8000);

  const box = dialog('Tools', (shut) => {
    const topSlot = h('div', {}), gridSlot = h('div', {}), cards = h('div', { class: 'tl-cards' }), empty = h('div', {});
    const body = h('div', { class: 'tools-panel' },
      topSlot,
      h('p', { class: 'muted small tl-rule' }, 'Tools (MCP servers) for your agents. Add one once: each station you choose installs it for itself, and gives it to its agents when they start.'),
      gridSlot, empty, cards,
      h('div', { class: 'row end sheet-foot' }, h('button', { type: 'button', class: 'tl-close', onclick: shut }, 'Close')));
    const drawn = new Map();   // tool -> { sig, el }
    let slots = '';
    draw = () => {
      if (!open) return;
      const scroller = body.closest('.dialog-body'), at = scroller?.scrollTop;
      let changed = false;
      const sig = slotSig();
      if (sig !== slots) {
        slots = sig; changed = true;
        topSlot.replaceChildren(top());
        gridSlot.replaceChildren(matrix());
        empty.replaceChildren(doc.tools && !doc.tools.length ? h('p', { class: 'tl-empty' }, 'Add a tool, say where, and spandeck sets it up there. You can roll it out to the rest of the fleet later, and upgrade it one station first.') : '');
      }
      const tools = doc.tools || [];
      for (const name of [...drawn.keys()]) if (!tools.some(t => t.name === name)) { drawn.get(name).el.remove(); drawn.delete(name); changed = true; }
      tools.forEach((t, i) => {
        const s = cardSig(t), had = drawn.get(t.name);
        let el = had?.el;
        if (!had || had.sig !== s) {
          el = card(t);
          if (had) had.el.replaceWith(el);
          drawn.set(t.name, { sig: s, el });
          changed = true;
        }
        if (cards.children[i] !== el) { cards.insertBefore(el, cards.children[i] || null); changed = true; }
      });
      if (changed && at !== undefined && scroller.scrollTop !== at) scroller.scrollTop = at;
    };
    draw();
    return body;
  }, { wide: true, onClose: () => {
    open = false; openNow = null;
    off(); clearInterval(round); clearTimeout(soonTimer);
    for (const t of timers.values()) clearTimeout(t);
  } });
  // On a phone it is a sheet: the whole screen, its Close clear of the clock.
  box.body.parentNode.classList.add('sheet', 'tools-sheet');
  openNow = box.close;
  relist();
  loadCatalogue();
  // Each station by itself; and each is asked whether a newer version is
  // published (it reuses an answer under three hours old).
  for (const st of asking()) look(st).then(() => check(st, false));
  return box.close;
}

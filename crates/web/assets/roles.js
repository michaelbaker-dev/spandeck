// Roles for agents: the Control Deck's screen (DECISIONS.md, "Roles for
// agents"; the calls and their shapes are docs/roles-api.md).
//
// The owner, 18 Nov 2026: "I wonder if we should have roles defined when we
// start a agent…" and "be detailed on the prompt for the role, I should be
// able to view and change the role prompt".
//
// - The library is one document, the same on every station: it is read from
//   and changed at the station serving this page (`roles.list`,
//   `roles.set`), and reaches the others by itself.
// - A prompt, a title and a summary are the owner's words, and what a
//   station said is a station's: all of it is put in as text.
// - What is being typed is kept in `draft`, outside anything that is drawn:
//   the list being read again, or another station's change arriving, never
//   replaces the boxes being typed in, and closing the dialog asks first.
// - A button that asks the station something is greyed until it answers,
//   and every action says what it did.

import { h, dialog, field } from './ui.js';

const AGENTS = [['', 'Whatever is chosen when the pane is started'], ['claude', 'Claude Code'], ['codex', 'Codex'], ['opencode', 'OpenCode'], ['antigravity', 'Antigravity']];
const NAME = /^[A-Za-z0-9_-]{1,32}$/;
const KIND = { standing: 'on the team', visit: 'a short visit' };
const KIND_MORE = { standing: 'On the team: a member for the whole job', visit: 'A short visit: one review, then done' };
const FIELDS = ['title', 'summary', 'prompt', 'kind', 'budget', 'agent', 'model', 'effort', 'permission_mode'];
const bytes = (s) => new TextEncoder().encode(s || '').length;
const agentName = (a) => (AGENTS.find(x => x[0] === a) || [a, a])[1];
const draftOf = (r) => Object.fromEntries(FIELDS.map(k => [k, r?.[k] ?? '']));

let openNow = null;
// The library as this page last read it, for the places a role is chosen.
let known = null;
const waiting = new Set();

// Read the library once for whoever chooses a role, and again when it changes.
export function rolesKnown(api) {
  if (!known) {
    known = { roles: null, asking: null };
    api.onEvent((ev) => { if (ev.kind === 'roles.updated') { known.roles = null; askKnown(api); } });
  }
  if (known.roles) return Promise.resolve(known.roles);
  return askKnown(api);
}
function askKnown(api) {
  known.asking ||= api.call('roles.list', {}, { within: 9000 })
    .then((r) => { known.roles = r.roles || []; return known.roles; })
    .catch(() => [])
    .finally(() => { known.asking = null; for (const f of waiting) f(); });
  return known.asking;
}

// "Role", where a pane is started: none unless chosen, the role's summary
// under it. `ctx`: { api, hasCap, station() (where the pane will start),
// isAgent() }. Not offered for a shell, or for a station that sends no role.
export function rolePick(ctx) {
  const pick = h('select', { class: 'role-pick', 'aria-label': 'Role' });
  const says = h('small', { class: 'role-pick-says' });
  const row = h('label', { class: 'field role-pick-row' }, h('span', {}, 'Role'), pick, says);
  let roles = [];
  const tell = () => {
    const r = roles.find(x => x.name === pick.value);
    says.textContent = r
      ? `${r.summary || r.title} It is sent this role's prompt once it has started.`
      : 'No role: it is started as it is, and sent nothing.';
  };
  const fill = () => {
    const had = pick.value;
    pick.replaceChildren(h('option', { value: '' }, '(no role)'), ...roles.map(r => h('option', { value: r.name }, r.title === r.name ? r.name : `${r.title} (${r.name})`)));
    pick.value = roles.some(r => r.name === had) ? had : '';
    tell();
  };
  pick.addEventListener('change', tell);
  const refresh = () => {
    const on = ctx.isAgent() && ctx.hasCap(ctx.station(), 'roles') && ctx.hasCap(ctx.api.station, 'roles');
    row.hidden = !on;
    row.style.display = on ? '' : 'none';
    if (!on) pick.value = '';
  };
  fill();
  refresh();
  const again = () => { if (!row.isConnected) { waiting.delete(again); return; } rolesKnown(ctx.api).then((r) => { roles = r; fill(); }); };
  rolesKnown(ctx.api).then((r) => { roles = r; fill(); waiting.add(again); });
  return { row, refresh, value: () => (row.hidden ? undefined : pick.value || undefined), title: () => roles.find(r => r.name === pick.value)?.title };
}

// `ctx`: { api, hasCap }.
export function rolesPanel(ctx) {
  // One at a time: asked for again, the one that is open stays.
  if (openNow) return openNow;
  const { api } = ctx;
  let open = true;
  let lib = { roles: null, limits: { prompt_bytes: 16384 }, placeholders: [], problem: null };
  // Where the dialog is: the list, one role, or the editor.
  let at = { view: 'list', name: null };
  // What is being typed, and what it was started from. Never drawn from the
  // station's answer once it exists.
  let draft = null;
  let busy = null;      // what is being asked of the station, in words
  let said = null;      // { text, kind } what the last action did
  let leaving = null;   // a way out that waits for "Discard"
  let draw = () => {};

  const role = (name) => (lib.roles || []).find(r => r.name === name);
  const dirty = () => !!draft && (draft.isNew
    ? FIELDS.some(k => (draft.now[k] || '') !== (draft.from[k] || '')) || !!draft.name
    : FIELDS.some(k => (draft.now[k] || '') !== (draft.from[k] || '')));

  let listing = null, again = false;
  const relist = () => {
    if (listing) { again = true; return listing; }
    listing = (async () => {
      do {
        again = false;
        try {
          const r = await api.call('roles.list', {}, { within: 9000 });
          lib = { roles: r.roles || [], limits: r.limits || lib.limits, placeholders: r.placeholders || [], problem: null };
        } catch (e) { lib = { ...lib, problem: e.message }; }
      } while (again && open);
      listing = null;
      if (open) draw();
    })();
    return listing;
  };

  // Ask the station for a change. Greyed from the press until it answers.
  const ask = async (words, method, params) => {
    if (busy) return null;
    busy = words; said = null; draw();
    try {
      return await api.call(method, params, { within: 15000 });
    } catch (e) {
      said = { text: e.message, kind: 'failed' };
      return null;
    } finally {
      busy = null;
      if (open) draw();
    }
  };

  // Leave the editor: at once when nothing was changed, else after asking.
  const leave = (to) => {
    if (dirty()) { leaving = to; draw(); return; }
    draft = null; leaving = null; to();
  };
  const go = (view, name) => { at = { view, name }; said = null; draw(); };

  const edit = (r) => {
    draft = { name: r.name, isNew: false, was: r.changed_ms || 0, from: draftOf(r), now: draftOf(r), showBuiltin: false, conflict: null };
    go('edit', r.name);
  };
  const startNew = (from) => {
    const base = from ? draftOf(from) : { ...draftOf(null), kind: 'standing' };
    draft = { name: '', isNew: true, was: 0, from: { ...draftOf(null), kind: 'standing' }, now: base, copied: from ? from.name : '', showBuiltin: false, conflict: null };
    go('edit', null);
  };

  const save = async (over) => {
    const d = draft;
    if (!d) return;
    const name = d.isNew ? d.name.trim() : d.name;
    if (d.isNew && !NAME.test(name)) { said = { text: 'Give the role a name: letters, digits, - and _, 32 at most.', kind: 'failed' }; return draw(); }
    if (d.isNew && role(name)) { said = { text: `There is already a role called ${name}. Choose another name, or open that one and edit it.`, kind: 'failed' }; return draw(); }
    if (!d.now.prompt.trim()) { said = { text: 'A role needs a prompt.', kind: 'failed' }; return draw(); }
    if (bytes(d.now.prompt) > lib.limits.prompt_bytes) { said = { text: `The prompt is ${bytes(d.now.prompt).toLocaleString()} bytes; the most is ${lib.limits.prompt_bytes.toLocaleString()}.`, kind: 'failed' }; return draw(); }
    const change = Object.fromEntries(FIELDS.map(k => [k, d.now[k] || '']));
    if (d.isNew && !change.title.trim()) change.title = name;
    const r = await ask('Saving…', 'roles.set', { name, role: change, was: over ?? d.was });
    if (!r) return;
    if (r.saved === false) {
      // Another station's change came first. The owner's text stays here.
      d.conflict = { why: r.why || 'this role was changed somewhere else after you opened it', role: r.role, stamp: r.role?.changed_ms || 0 };
      said = null;
      await relist();
      return draw();
    }
    draft = null;
    await relist();
    at = { view: 'role', name };
    said = { text: r.role?.source === 'builtin'
      ? `${name} is exactly the built-in text, so nothing of yours is kept for it.`
      : `Saved on ${api.station}; the other stations take it by themselves. Panes are sent it from now. One that is running keeps what it was sent until you choose "Send its role again" in its menu.`, kind: 'good' };
    draw();
  };
  const reset = async (name) => {
    const r = await ask('Going back to the built-in text…', 'roles.reset', { name });
    if (!r) return;
    draft = null;
    await relist();
    at = { view: 'role', name };
    said = { text: `${name} is the built-in text again. What you had changed is gone.`, kind: 'good' };
    draw();
  };
  const remove = async (name) => {
    const r = await ask('Removing it…', 'roles.remove', { name });
    if (!r) return;
    draft = null;
    await relist();
    at = { view: 'list', name: null };
    said = { text: `Removed the role ${name}. A pane that has it keeps the name and is sent nothing from now.`, kind: 'good' };
    draw();
  };

  // ---- pieces ----
  const btn = (label, cls, on, off = false) => h('button', { type: 'button', class: cls, disabled: !!busy || off, 'aria-disabled': busy || off ? 'true' : 'false', onclick: on }, label);
  const saidBox = () => (busy ? h('div', { class: 'ap-job running ro-said', role: 'status' }, h('span', { class: 'ap-spin' }), busy)
    : said ? h('div', { class: `ap-job ${said.kind} ro-said`, role: said.kind === 'failed' ? 'alert' : 'status' }, said.text) : null);
  const marks = (r) => [
    h('span', { class: 'chip ro-kind' }, KIND[r.kind] || r.kind),
    r.source === 'changed' ? h('span', { class: 'chip ro-changed' }, 'changed by you') : null,
    r.source === 'own' ? h('span', { class: 'chip ro-own' }, 'yours') : null,
  ];
  const defaults = (r) => {
    const rows = [['Budget', r.budget], ['Program', r.agent && agentName(r.agent)], ['Model', r.model], ['Effort', r.effort], ['Permission mode', r.permission_mode]].filter(x => x[1]);
    return rows.length ? h('dl', { class: 'ro-defaults' }, rows.flatMap(([k, v]) => [h('dt', {}, k), h('dd', {}, v)])) : null;
  };

  // ---- the list ----
  const listView = () => {
    const roles = lib.roles;
    return h('div', { class: 'ro-list-view' },
      h('div', { class: 'ro-top' },
        h('p', { class: 'muted small ro-rule' }, 'What each kind of agent is asked to be. A pane started with a role is sent its prompt once, as a message from you. The same on every station.'),
        btn('New role', 'primary ro-new', () => startNew(null))),
      saidBox(),
      roles === null ? h('p', { class: 'muted' }, lib.problem ? `${api.station} did not say which roles there are: ${lib.problem}` : 'Asking which roles there are…') : null,
      h('div', { class: 'ro-list', role: 'list' }, (roles || []).map(r => h('button', { type: 'button', class: 'ro-row', role: 'listitem', 'data-role': r.name, onclick: () => go('role', r.name) },
        h('span', { class: 'ro-row-head' }, h('b', {}, r.title), h('span', { class: 'muted mono ro-name' }, r.name), marks(r)),
        h('span', { class: 'ro-summary' }, r.summary)))));
  };

  // ---- one role ----
  const roleView = () => {
    const r = role(at.name);
    if (!r) return h('div', {}, h('button', { type: 'button', class: 'quiet ro-back', onclick: () => go('list') }, '‹ Roles'), saidBox(),
      h('p', { class: 'muted' }, lib.roles === null ? 'Asking…' : `There is no role called ${at.name} any more.`));
    return h('div', { class: 'ro-role-view', 'data-role': r.name },
      h('button', { type: 'button', class: 'quiet ro-back', onclick: () => go('list') }, '‹ Roles'),
      h('div', { class: 'ro-head' }, h('h3', {}, r.title), h('span', { class: 'muted mono ro-name' }, r.name), marks(r)),
      r.summary ? h('p', { class: 'ro-summary' }, r.summary) : null,
      r.source === 'changed' ? h('p', { class: 'muted small ro-by' }, `Changed by ${r.changed_by || 'you'}. The built-in text is kept, and a later spandeck may improve it: yours stays as you wrote it.`) : null,
      r.source === 'builtin' ? h('p', { class: 'muted small ro-by' }, 'Built in. A later spandeck may improve this text; once you change it, yours is kept as you wrote it.') : null,
      defaults(r),
      saidBox(),
      h('div', { class: 'ro-acts' },
        btn('Edit', 'primary ro-edit', () => edit(r)),
        btn('Copy to a new role', 'ro-copy', () => startNew(r)),
        r.source === 'changed' ? btn('Reset to the built-in text', 'quiet ro-reset', () => { leaving = () => reset(r.name); leavingWords = [`Go back to the built-in text of ${r.name}?`, 'What you changed in it is gone, on every station.', 'Reset']; draw(); }) : null,
        r.source === 'own' ? btn('Remove', 'quiet danger ro-remove', () => { leaving = () => remove(r.name); leavingWords = [`Remove the role ${r.name}?`, 'It is taken out on every station. A pane that has it keeps the name and is sent nothing from now.', 'Remove']; draw(); }) : null),
      askBar(),
      h('h4', { class: 'ro-prompt-head' }, 'The prompt', h('span', { class: 'muted small' }, ` · ${bytes(r.prompt).toLocaleString()} bytes · the words in {{…}} are filled in when it is sent`)),
      h('pre', { class: 'ro-prompt', tabIndex: 0, 'aria-label': `The prompt of ${r.title}` }, r.prompt),
      r.source === 'changed' && r.builtin_role ? h('details', { class: 'ro-builtin' }, h('summary', {}, 'Show the built-in text'), h('pre', { class: 'ro-prompt' }, r.builtin_role.prompt)) : null);
  };

  // A question before something is lost: in the dialog, not over it.
  let leavingWords = null;
  const askBar = () => {
    if (!leaving) return null;
    const [q, more, yes] = leavingWords || ['Discard what you changed?', 'What you typed here is not saved.', 'Discard'];
    return h('div', { class: 'ro-ask', role: 'alertdialog', 'aria-label': q },
      h('b', {}, q), h('span', { class: 'muted' }, more),
      h('div', { class: 'ro-ask-acts' },
        h('button', { type: 'button', class: 'ro-keep', onclick: () => { leaving = null; leavingWords = null; draw(); } }, leavingWords ? 'Cancel' : 'Keep editing'),
        h('button', { type: 'button', class: 'primary danger ro-discard', onclick: () => { const to = leaving; leaving = null; leavingWords = null; if (!to) return; if (at.view === 'edit') draft = null; to(); } }, yes)));
  };

  // ---- the editor ----
  // Built once for a draft and kept: a redraw moves it and never rebuilds
  // the boxes, so the caret, the place scrolled to and the words stay.
  let editor = null;
  const editorFor = (d) => {
    if (editor && editor.d === d) return editor;
    const r = d.isNew ? null : role(d.name);
    const bind = (el, key) => { el.value = d.now[key] || ''; el.addEventListener('input', () => { d.now[key] = el.value; tick(); }); el.addEventListener('change', () => { d.now[key] = el.value; tick(); }); return el; };
    const name = h('input', { class: 'ro-f-name', 'aria-label': 'Name', autocapitalize: 'none', autocomplete: 'off', spellcheck: false, maxLength: 32, placeholder: 'copy-editor' });
    name.value = d.name;
    name.addEventListener('input', () => { d.name = name.value; tick(); });
    const copy = h('select', { class: 'ro-f-copy', 'aria-label': 'Copy from' }, h('option', { value: '' }, 'Start empty'), (lib.roles || []).map(x => h('option', { value: x.name }, `${x.title} (${x.name})`)));
    copy.value = d.copied || '';
    const prompt = bind(h('textarea', { class: 'ro-f-prompt mono', 'aria-label': 'Prompt', spellcheck: true, rows: 18 }), 'prompt');
    const title = bind(h('input', { class: 'ro-f-title', maxLength: 80 }), 'title');
    const summary = bind(h('input', { class: 'ro-f-summary', maxLength: 200 }), 'summary');
    const budget = bind(h('input', { class: 'ro-f-budget', maxLength: 200, placeholder: 'One round a stage; 20 minutes a stage' }), 'budget');
    const kind = bind(h('select', { class: 'ro-f-kind' }, Object.entries(KIND_MORE).map(([v, l]) => h('option', { value: v }, l))), 'kind');
    const agent = bind(h('select', { class: 'ro-f-agent' }, AGENTS.map(([v, l]) => h('option', { value: v }, l))), 'agent');
    const model = bind(h('input', { class: 'ro-f-model', autocapitalize: 'none', placeholder: 'as Agent setup says' }), 'model');
    const effort = bind(h('input', { class: 'ro-f-effort', autocapitalize: 'none', placeholder: 'as Agent setup says' }), 'effort');
    const mode = bind(h('input', { class: 'ro-f-mode', autocapitalize: 'none', placeholder: 'as Agent setup says' }), 'permission_mode');
    copy.addEventListener('change', () => {
      const from = role(copy.value);
      const mine = d.now.prompt.trim() && d.now.prompt !== (role(d.copied)?.prompt || '');
      if (mine && !confirmCopy.checked) { copyNote.hidden = false; return; }
      copyNote.hidden = true; confirmCopy.checked = false;
      d.copied = copy.value;
      d.now = from ? draftOf(from) : { ...draftOf(null), kind: 'standing' };
      for (const [el, key] of [[prompt, 'prompt'], [title, 'title'], [summary, 'summary'], [budget, 'budget'], [kind, 'kind'], [agent, 'agent'], [model, 'model'], [effort, 'effort'], [mode, 'permission_mode']]) el.value = d.now[key] || '';
      tick();
    });
    const confirmCopy = h('input', { type: 'checkbox', class: 'ro-copy-over' });
    confirmCopy.addEventListener('change', () => { if (confirmCopy.checked) copy.dispatchEvent(new Event('change')); });
    const copyNote = h('label', { class: 'ro-copy-note warn-text', hidden: true }, confirmCopy, h('span', {}, 'Replace what you typed with that role\'s text'));
    const count = h('span', { class: 'ro-count', role: 'status' });
    const tick = () => {
      const n = bytes(d.now.prompt), max = lib.limits.prompt_bytes;
      count.textContent = `${n.toLocaleString()} of ${max.toLocaleString()} bytes`;
      count.classList.toggle('over', n > max);
      const unknown = [...new Set((d.now.prompt.match(/\{\{[^{}]*\}\}/g) || []).filter(p => !(lib.placeholders || []).some(x => x.name === p)))];
      odd.textContent = unknown.length ? `${unknown.join(', ')} ${unknown.length === 1 ? 'is' : 'are'} not filled in: sent as written.` : '';
      state.textContent = dirty() ? 'Not saved yet' : '';
    };
    const odd = h('small', { class: 'warn-text ro-odd' }), state = h('span', { class: 'muted small ro-state' });
    const slots = { said: h('div', {}), conflict: h('div', {}), ask: h('div', {}), extra: h('div', { class: 'ro-acts ro-extra' }), acts: h('div', { class: 'row end sheet-foot ro-edit-acts' }), builtin: h('div', {}), head: h('div', {}) };
    const el = h('form', { class: 'ro-editor', onsubmit: (e) => { e.preventDefault(); save(); } },
      slots.head,
      d.isNew ? h('div', { class: 'ro-new-top' },
        field('Name', name, 'Letters, digits, - and _. It is what a pane or a project names.'),
        field('Copy from', copy, 'Start from another role\'s prompt and settings.'), copyNote) : null,
      field('Title', title), field('Summary', summary, 'One line: what this role is for. Shown where a role is chosen.'),
      h('div', { class: 'field ro-prompt-field' },
        h('span', {}, 'Prompt'),
        prompt,
        h('div', { class: 'ro-under' }, count, state), odd),
      h('details', { class: 'ro-places' }, h('summary', {}, 'Words that are filled in when the prompt is sent'),
        h('dl', {}, (lib.placeholders || []).flatMap(p => [h('dt', { class: 'mono' }, p.name), h('dd', {}, p.becomes)])),
        h('p', { class: 'muted small' }, 'Anything else in {{…}} is sent as written.')),
      field('Kind', kind),
      field('Budget', budget, 'One line. It becomes {{budget}} in the prompt.'),
      h('details', { class: 'ro-more', open: !!(d.now.agent || d.now.model || d.now.effort || d.now.permission_mode) }, h('summary', {}, 'Defaults for a pane with this role'),
        field('Program', agent, 'Used when a pane is started with this role and no program is named.'),
        field('Model', model), field('Effort', effort), field('Permission mode', mode, 'Each is passed to the program in place of Agent setup\'s, for a pane with this role. Leave empty to use Agent setup\'s.')),
      slots.extra, slots.builtin, slots.conflict, slots.said, slots.ask, slots.acts);
    editor = { d, el, slots, tick, r };
    tick();
    return editor;
  };
  const editView = () => {
    const d = draft, ed = editorFor(d), r = d.isNew ? null : role(d.name);
    const changedElsewhere = !d.isNew && r && (r.changed_ms || 0) !== d.was && !d.conflict;
    ed.slots.head.replaceChildren(
      h('button', { type: 'button', class: 'quiet ro-back', onclick: () => leave(() => go(d.isNew ? 'list' : 'role', d.name)) }, d.isNew ? '‹ Roles' : `‹ ${r?.title || d.name}`),
      h('div', { class: 'ro-head' }, h('h3', {}, d.isNew ? 'New role' : `Edit ${r?.title || d.name}`), d.isNew ? null : h('span', { class: 'muted mono ro-name' }, d.name), r ? marks(r) : []),
      changedElsewhere ? h('p', { class: 'warn-text small ro-moved', role: 'status' }, 'This role was changed somewhere else while you were editing. Your text is still here; saving will ask which to keep.') : '');
    const c = d.conflict;
    ed.slots.conflict.replaceChildren(c ? h('div', { class: 'ro-conflict', role: 'alert' },
      h('b', {}, `Not saved: ${c.why}.`),
      h('span', {}, ' Your text is still in the box. The other one is the one in use now.'),
      c.role ? h('details', {}, h('summary', {}, 'Show the other one'), h('pre', { class: 'ro-prompt' }, c.role.prompt)) : null,
      h('div', { class: 'ro-ask-acts' }, btn('Save mine over it', 'primary ro-over', () => { const stamp = c.stamp; d.conflict = null; save(stamp); }))) : '');
    ed.slots.said.replaceChildren(saidBox() || '');
    ed.slots.ask.replaceChildren(askBar() || '');
    const builtin = r?.builtin ? (r.builtin_role || (r.source === 'builtin' ? r : null)) : null;
    ed.slots.builtin.replaceChildren(d.showBuiltin && builtin ? h('div', { class: 'ro-builtin open' }, h('h4', {}, 'The built-in text'), h('pre', { class: 'ro-prompt' }, builtin.prompt)) : '');
    ed.slots.extra.replaceChildren(
      r?.source === 'changed' ? btn('Reset to the built-in text', 'quiet ro-reset', () => { leaving = () => reset(d.name); leavingWords = [`Go back to the built-in text of ${d.name}?`, 'What you changed in it, and what you typed here, is gone on every station.', 'Reset']; draw(); }) : null,
      builtin ? btn(d.showBuiltin ? 'Hide the built-in text' : 'Show the built-in text', 'quiet ro-show-builtin', () => { d.showBuiltin = !d.showBuiltin; draw(); }) : null);
    ed.slots.acts.replaceChildren(
      btn('Cancel', 'ro-cancel', () => leave(() => go(d.isNew ? 'list' : 'role', d.name))),
      h('button', { type: 'submit', class: 'primary ro-save', disabled: !!busy, 'aria-disabled': busy ? 'true' : 'false' }, busy === 'Saving…' ? 'Saving…' : 'Save'));
    ed.tick();
    return ed.el;
  };

  const off = api.onEvent((ev) => { if (ev.kind === 'roles.updated' && open) relist(); });
  const round = setInterval(() => { if (open && !busy) relist(); }, 20000);

  const box = dialog('Roles', (shut) => {
    const slot = h('div', { class: 'ro-view' });
    const body = h('div', { class: 'roles-panel' }, slot,
      h('div', { class: 'row end sheet-foot ro-foot' }, h('button', { type: 'button', class: 'ro-close', onclick: shut }, 'Close')));
    let shown = null;
    draw = () => {
      if (!open) return;
      if (at.view !== 'edit') editor = null;
      const scroller = body.closest('.dialog-body'), top = scroller?.scrollTop;
      const el = at.view === 'edit' && draft ? editView() : at.view === 'role' ? roleView() : listView();
      // The editor is the same element each time: it is left where it is.
      if (slot.firstChild !== el) slot.replaceChildren(el);
      body.dataset.view = at.view;
      body.querySelector('.ro-foot').hidden = at.view === 'edit';
      if (scroller && shown === at.view + ' ' + at.name && scroller.scrollTop !== top) scroller.scrollTop = top;
      else if (scroller && shown !== at.view + ' ' + at.name) scroller.scrollTop = 0;
      shown = at.view + ' ' + at.name;
    };
    draw();
    return body;
  }, { wide: true,
    // Closing with something typed asks first, in the dialog.
    mayClose: () => { if (!dirty() || at.view !== 'edit') return true; leaving = () => { draft = null; box.close(true); }; leavingWords = null; draw(); document.querySelector('.ro-ask')?.scrollIntoView({ block: 'nearest' }); return false; },
    onClose: () => { open = false; openNow = null; off(); clearInterval(round); } });
  // On a phone it is a sheet: the whole screen, its Close clear of the clock.
  box.body.parentNode.classList.add('sheet', 'roles-sheet');
  openNow = box.close;
  relist();
  return box.close;
}

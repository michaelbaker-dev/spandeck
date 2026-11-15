// The Appearance editor: one dialog for the deck, for a layout or for a pane.
// While it is open the real page shows what is being edited, in this window
// only; Cancel puts everything back. Names of themes and of fonts come from
// files and other stations and are put on the page as text.

import { h, dialog, toast } from './ui.js';
import { newTerminal, remeasureTerminal } from './term.js';
// (One line: the station gives each import its build's name, line by line.)
import { KEYS, GROUPS, BASE, BUILTIN, DEFAULT_THEME, lookState, allThemes, themeById, isBuiltin, inheritedLook, termOptions, applyPaneFrame, isColour, validValue, isFontName, warnings, exportTheme, importTheme, FACES, fontHere, fontFromFile, sameFont, uploadedAs, loadFonts, fontListeners } from './look.js';

const E = '\x1b[';
// A fixed sample: the colours, a prompt, a small diff, lines, ligatures,
// emoji, wide characters, and a powerline prompt with icons (drawn from the
// bundled symbols font where the device has no Nerd Font).
const SAMPLE = [
  `${E}1mSample${E}0m  ${E}2mdim${E}0m ${E}1mbold${E}0m ${E}3mitalic${E}0m ${E}4munderline${E}0m ${E}7mreverse${E}0m`,
  [0, 1, 2, 3, 4, 5, 6, 7].map(i => `${E}4${i}m    ${E}0m`).join('') + '  normal',
  [0, 1, 2, 3, 4, 5, 6, 7].map(i => `${E}10${i}m    ${E}0m`).join('') + '  bright',
  [0, 1, 2, 3, 4, 5, 6, 7].map(i => `${E}3${i}m Aa ${E}0m`).join(''),
  [0, 1, 2, 3, 4, 5, 6, 7].map(i => `${E}9${i}m Aa ${E}0m`).join(''),
  '',
  `${E}32myou@station${E}0m:${E}34m~/code/web${E}0m$ git diff`,
  `${E}1mdiff --git a/app.js b/app.js${E}0m`,
  `${E}36m@@ -12,3 +12,3 @@${E}0m`,
  ' const fleet = stations();',
  `${E}31m-const size = 12;${E}0m`,
  `${E}32m+const size = 13;${E}0m`,
  '┌──────────┬──────────┐',
  `│ ${E}32m✔ tests${E}0m  │ ${E}31m✘ lint${E}0m   │`,
  '└──────────┴──────────┘',
  'Ligatures: => != === -> <= >= |> ::',
  'Emoji: 🚀 ✅ 🔥 👍🏽 🇨🇦 👨‍👩‍👧  |<- lined up',
  'Wide:  日本語 中文 한국어 |<- lined up',
  'Icons: \uf07c \ue702 \uf09b \ue718 \uf120 \u{f0306} \uf013 \u23fb |<- lined up',
  `${E}44;97m \uf015 ~/code/web ${E}0;34;42m\ue0b0${E}30m \ue0a0 main ${E}0;32;43m\ue0b0${E}30m \uf00c 3 ${E}0;33m\ue0b0${E}0m`,
  `${E}32myou@station${E}0m:${E}34m~/code/web${E}0m$ `,
].join('\r\n');

const six = (c) => { const x = c.slice(1); return '#' + (x.length === 3 ? [...x].map(d => d + d).join('') : x.slice(0, 6)).toLowerCase(); };
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const WEIGHTS = [[100, 'Thin'], [200, 'Extra light'], [300, 'Light'], [400, 'Normal'], [500, 'Medium'], [600, 'Semi bold'], [700, 'Bold'], [800, 'Extra bold'], [900, 'Black']];

// env: { api, apply(): show the looks again, reload(): read the document again }
// scope: { level: 'deck' | 'layout' | 'pane', key, name, layoutKey }
export function lookEditor(env, scope) {
  const { level, key } = scope;
  const stored = () => { const d = lookState.doc; return level === 'deck' ? d.deck : (level === 'layout' ? d.layouts : d.panes)[key]; };
  const draft = { theme: stored()?.theme || null, set: { ...(stored()?.set || {}) } };
  let done = false, sample = null;

  const inherited = () => inheritedLook(level, key, scope.layoutKey);
  // What the chosen theme alone gives here, and with the single changes.
  const themed = () => ({ ...inherited(), ...(draft.theme ? themeById(draft.theme)?.settings || {} : {}) });
  const effective = () => ({ ...themed(), ...draft.set });
  // The theme the buttons act on: the one chosen here, else the deck's default.
  const current = () => themeById(draft.theme || (level === 'deck' ? DEFAULT_THEME : '')) || null;

  const controls = [];
  const warnBox = h('div', { class: 'look-warn', role: 'status' });
  const sampleBox = h('div', { class: 'look-sample', 'aria-label': 'A sample terminal in this look' });

  const show = () => {
    lookState.preview = { level, key, choice: draft };
    env.apply();
    const look = effective();
    for (const c of controls) c.sync(look);
    const warns = warnings(look, level);
    warnBox.replaceChildren(...warns.map(w => h('p', { class: 'warn-text' }, '⚠ ' + w)));
    drawFonts();
    if (sample) {
      for (const [k, v] of Object.entries(termOptions(look))) if (!same(sample.options[k], v)) sample.options[k] = v;
      applyPaneFrame(sampleBox, look, null);
      sampleBox.style.setProperty('--term-bg', look['term.background']);
    }
    syncButtons();
  };
  const setKey = (k, v) => {
    if (!validValue(k, v)) return;
    if (same(themed()[k], v)) delete draft.set[k]; else draft.set[k] = v;
    show();
  };

  // ---- one control for each setting ----
  const control = (k) => {
    const def = KEYS[k];
    const reset = h('button', { type: 'button', class: 'icon-btn look-reset', title: 'Back to the theme\'s value', 'aria-label': `${def.label}: back to the theme's value`,
      onclick: () => { delete draft.set[k]; show(); } }, '↺');
    let input, sync, note = null;
    if (def.kind === 'colour') {
      const pick = h('input', { type: 'color', 'aria-label': def.label });
      const text = h('input', { type: 'text', class: 'look-hex', maxLength: 9, spellcheck: false, 'aria-label': `${def.label}, as #rrggbb` });
      pick.addEventListener('input', () => { text.value = pick.value; text.classList.remove('bad'); setKey(k, pick.value); });
      text.addEventListener('input', () => {
        const v = text.value.trim().toLowerCase();
        text.classList.toggle('bad', !isColour(v));
        if (isColour(v)) setKey(k, v);
      });
      input = h('span', { class: 'look-colour' }, pick, text);
      sync = (look) => { pick.value = six(look[k]); if (document.activeElement !== text) { text.value = look[k]; text.classList.remove('bad'); } };
    } else if (def.kind === 'number') {
      input = h('input', { type: 'number', min: def.min, max: def.max, step: def.step, 'aria-label': def.label });
      input.addEventListener('input', () => {
        const v = Number(input.value);
        const ok = input.value !== '' && validValue(k, v);
        input.classList.toggle('bad', !ok);
        if (ok) setKey(k, v);
      });
      sync = (look) => { if (document.activeElement !== input) { input.value = String(look[k]); input.classList.remove('bad'); } };
    } else if (def.kind === 'weight' || def.kind === 'one') {
      const options = def.kind === 'weight' ? WEIGHTS.map(([v, label]) => [v, `${label} (${v})`]) : def.options;
      input = h('select', { 'aria-label': def.label }, options.map(([v, label]) => h('option', { value: String(v) }, label)));
      input.addEventListener('change', () => setKey(k, def.kind === 'weight' ? Number(input.value) : input.value));
      sync = (look) => { input.value = String(look[k]); };
    } else if (def.kind === 'switch') {
      input = h('input', { type: 'checkbox', 'aria-label': def.label });
      input.addEventListener('change', () => setKey(k, input.checked));
      sync = (look) => { input.checked = !!look[k]; };
    } else {
      // Free typing, with the uploaded fonts offered by name.
      const offered = h('datalist', { id: 'look-fonts-' + k.replace('.', '-') });
      input = h('input', { type: 'text', spellcheck: false, 'aria-label': def.label, placeholder: 'Fira Code, ui-monospace, monospace' });
      input.setAttribute('list', offered.id);
      input.addEventListener('input', () => {
        const names = input.value.split(',').map(n => n.trim().replace(/^["']|["']$/g, '').trim()).filter(Boolean);
        const ok = validValue(k, names);
        input.classList.toggle('bad', !ok);
        input.title = ok ? '' : 'Up to 8 names; a name uses letters, digits, spaces, - _ . only';
        if (ok) setKey(k, names);
      });
      note = h('small', { class: 'warn-text look-note', role: 'status' }, offered);
      sync = (look) => {
        if (document.activeElement !== input) { input.value = look[k].join(', '); input.classList.remove('bad'); }
        // Choosing one puts it first, ahead of what is there.
        const rest = look[k].join(', ');
        offered.replaceChildren(...familyNames().filter(n => !sameFont(n, look[k][0])).map(n => h('option', { value: `${n}, ${rest}` }, `${n} (uploaded)`)));
        note.replaceChildren(offered, fontNote(look[k]));
      };
    }
    const row = h('div', { class: 'look-row look-' + def.kind, dataset: { key: k } }, h('span', { class: 'look-label' }, def.label), input, reset, note,
      def.hint ? h('small', { class: 'muted look-hint' }, def.hint) : null);
    controls.push({ sync: (look) => { sync(look); const own = k in draft.set; reset.style.visibility = own ? 'visible' : 'hidden'; row.classList.toggle('own', own); } });
    return row;
  };

  // ---- fonts: is the chosen one here, and the ones the owner uploaded ----
  const familyNames = () => lookState.doc.fonts.map(f => f.name).filter((n, i, all) => all.findIndex(o => sameFont(o, n)) === i);
  // What to say when the first font of a list is not what text is drawn in.
  const fontNote = (names) => {
    const first = names[0], here = fontHere(first);
    if (here === 'yes') return '';
    if (here === 'loading') return `${first} is being fetched.`;
    const next = names.slice(1).find(n => fontHere(n) === 'yes');
    const instead = next ? `Text here is drawn in ${next}, the next font in the list that is.` : 'Text here is drawn in the browser\'s own font.';
    if (here === 'missing') return `${first} has not reached this station yet. ${instead}`;
    if (here === 'failed') return `This browser could not read the uploaded file for ${first}. ${instead}`;
    return `${first} is not on this device. ${instead} Upload its file below to have it everywhere.`;
  };
  // Everything saved that names a family, for the question before removing it.
  const namedBy = (family) => {
    const names = (set) => ['font.family', 'ui.family'].some(k => (set?.[k] || []).some(n => sameFont(n, family)));
    const d = lookState.doc, out = d.themes.filter(t => names(t.settings)).map(t => `the theme ${t.name}`);
    if (names(d.deck.set)) out.push('the deck');
    const count = (map, one) => { const n = Object.values(map).filter(c => names(c.set)).length; if (n) out.push(n === 1 ? `1 ${one}` : `${n} ${one}s`); };
    count(d.layouts, 'layout');
    count(d.panes, 'pane');
    return out;
  };
  const fontList = h('div', { class: 'look-font-list' });
  const fontSay = h('p', { class: 'warn-text', role: 'status', hidden: true });
  const size = (n) => (n >= 1 << 20 ? (n / (1 << 20)).toFixed(1) + ' MB' : Math.max(1, Math.round(n / 1024)) + ' KB');
  let removing = null;
  const useFont = (family) => {
    const now = effective()['font.family'];
    setKey('font.family', [family, ...now.filter(n => !sameFont(n, family))].slice(0, 8));
  };
  const removeFont = (f) => {
    // The second press removes. The first says who still names the font.
    if (removing !== f.id) {
      removing = f.id;
      const others = uploadedAs(f.name).length > 1, by = others ? [] : namedBy(f.name);
      fontSay.textContent = by.length ? `${f.name} is still named by ${by.join(', ')}. Each keeps the name and is drawn in the next font of its list.` : '';
      fontSay.hidden = !by.length;
      drawFonts();
      setTimeout(() => { if (removing === f.id) { removing = null; fontSay.hidden = true; drawFonts(); } }, 6000);
      return;
    }
    removing = null;
    fontSay.hidden = true;
    env.api.call('look.font.remove', { id: f.id }).then(() => env.reload()).then(() => { show(); toast(`Removed ${f.name} from every station`); }).catch(fail);
  };
  const drawFonts = () => {
    const fonts = lookState.doc.fonts;
    fontList.replaceChildren(...(fonts.length ? fonts.map(f => h('div', { class: 'look-font', dataset: { font: f.id } },
      h('span', { class: 'grow' }, h('b', {}, f.name), h('span', { class: 'muted' }, ` · ${FACES[f.face].label.toLowerCase()} · ${f.format.toUpperCase()} · ${size(f.size)}`)),
      h('button', { type: 'button', class: 'mini', title: 'Put this font first in the terminal\'s list', onclick: () => useFont(f.name) }, 'Use'),
      h('button', { type: 'button', class: 'mini' + (removing === f.id ? ' danger' : ''), title: 'Take this file off every station', onclick: () => removeFont(f) }, removing === f.id ? 'Remove it on every station?' : 'Remove')))
      : [h('small', { class: 'muted' }, 'None yet.')]));
  };
  // Uploading: choose a file, say what it is called, add it.
  const fontName = h('input', { type: 'text', maxLength: 64, spellcheck: false, 'aria-label': 'Name of the font' });
  const fontFace = h('select', { 'aria-label': 'Which face of the font this file is' }, Object.entries(FACES).map(([v, f]) => h('option', { value: v }, f.label)));
  let picked = null;
  const closeAdder = () => { adder.hidden = true; picked = null; };
  const adder = h('form', { class: 'look-adder row', hidden: true, onsubmit: async (e) => {
    e.preventDefault();
    const name = fontName.value.trim().replace(/\s+/g, ' '), f = picked;
    if (!f) return;
    if (!isFontName(name)) return toast('A font\'s name uses letters, digits, spaces, - _ . only.', 'error');
    try {
      const bytes = new Uint8Array(await f.arrayBuffer());
      let bin = '';
      for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
      await env.api.call('look.font.add', { name, face: fontFace.value, data_b64: btoa(bin) });
      await env.reload();
      // Wait for the browser to have it, so the sample is drawn in it at once.
      await loadFonts((id) => env.api.file('/api/font/' + id));
      closeAdder();
      useFont(name);
      toast(`Added ${name}. It is first in this look's font list; Save keeps that.`);
    } catch (err) { toast(`Could not add ${f.name}: ${err.message}`, 'error'); }
  } }, fontName, fontFace, h('button', { type: 'submit', class: 'primary' }, 'Add font'), h('button', { type: 'button', onclick: () => closeAdder() }, 'Not now'));
  const fontFile = h('input', { type: 'file', accept: '.woff2,.woff,.ttf,.otf,font/woff2,font/woff,font/ttf,font/otf', hidden: true, 'aria-label': 'A font file' });
  fontFile.addEventListener('change', () => {
    const f = fontFile.files[0];
    fontFile.value = '';
    if (!f) return;
    if (f.size > 5 * 1024 * 1024) return toast(`${f.name} is larger than 5 MB.`, 'error');
    const guess = fontFromFile(f.name);
    picked = f;
    fontName.value = guess.name;
    fontFace.value = guess.face;
    adder.hidden = false;
    fontName.focus();
    fontName.select();
  });
  const uploaded = () => h('div', { class: 'look-uploaded' },
    h('h4', {}, 'Fonts you uploaded'), fontList, fontSay,
    h('div', { class: 'row' }, h('button', { type: 'button', class: 'mini', title: 'Add a font file: WOFF2, WOFF, TTF or OTF, up to 5 MB', onclick: () => fontFile.click() }, 'Upload a font…'), fontFile),
    adder,
    h('small', { class: 'muted' }, 'A font you upload is kept on your stations and sent to every browser that opens the deck, a phone too. Upload only fonts whose licence lets you.'));

  // ---- themes: choose, duplicate, rename, delete, export, import ----
  const inheritName = level === 'layout' ? 'The deck\'s' : scope.layoutKey ? 'The layout\'s' : 'The deck\'s';
  const select = h('select', { 'aria-label': 'Theme' });
  const fillSelect = () => {
    const opt = (t) => h('option', { value: t.id }, t.name);
    select.replaceChildren(...[
      level === 'deck' ? null : h('option', { value: '' }, `${inheritName} (no theme of its own)`),
      h('optgroup', { label: 'Built in' }, BUILTIN.map(opt)),
      lookState.doc.themes.length ? h('optgroup', { label: 'Your themes' }, lookState.doc.themes.map(opt)) : null,
    ].filter(Boolean));
    select.value = draft.theme && themeById(draft.theme) ? draft.theme : (level === 'deck' ? DEFAULT_THEME : '');
  };
  // Choosing a theme starts from that theme: single changes made on top of
  // the last one are dropped, so the theme shows as it is.
  select.addEventListener('change', () => { draft.theme = select.value || null; draft.set = {}; closeNamer(); show(); });

  const freeName = (base) => { const taken = allThemes().map(t => t.name); let n = base, i = 1; while (taken.includes(n)) n = `${base} ${i += 1}`; return n.slice(0, 48); };
  const fail = (e) => toast(e.message, 'error');
  const saveTheme = async (theme) => {
    const r = await env.api.call('look.theme.save', { theme });
    await env.reload();
    return r.id;
  };
  const choose = (id) => { draft.theme = id; draft.set = {}; fillSelect(); show(); };

  // One line that asks for a name, used by "Save as theme" and "Rename".
  const nameInput = h('input', { type: 'text', maxLength: 48, 'aria-label': 'Name of the theme' });
  let named = null;
  const namer = h('form', { class: 'look-namer row', hidden: true, onsubmit: (e) => { e.preventDefault(); const n = nameInput.value.trim(); if (n && named) named(n); } },
    nameInput, h('button', { type: 'submit', class: 'primary' }, 'Save theme'), h('button', { type: 'button', onclick: () => closeNamer() }, 'Not now'));
  const closeNamer = () => { namer.hidden = true; named = null; };
  const askName = (suggest, then) => { named = then; namer.hidden = false; nameInput.value = suggest; nameInput.focus(); nameInput.select(); };

  const btn = (label, title, onclick) => h('button', { type: 'button', class: 'mini', title, onclick }, label);
  const dup = btn('Duplicate', 'Make a theme of your own from this one', () => {
    const t = current();
    if (t) saveTheme({ name: freeName(t.name + ' copy'), settings: { ...BASE, ...t.settings } }).then(choose).catch(fail);
  });
  const ren = btn('Rename', 'Give this theme another name', () => {
    const t = current();
    if (t) askName(t.name, (name) => saveTheme({ ...t, name }).then(() => { closeNamer(); fillSelect(); }).catch(fail));
  });
  let sure = 0;
  const del = btn('Delete', 'Delete this theme on every station', () => {
    const t = current();
    if (!t) return;
    // The second press within a few seconds deletes.
    if (Date.now() - sure > 4000) { sure = Date.now(); del.textContent = 'Delete it on every station?'; setTimeout(() => { del.textContent = 'Delete'; }, 4000); return; }
    sure = 0;
    del.textContent = 'Delete';
    env.api.call('look.theme.remove', { id: t.id }).then(() => env.reload()).then(() => { if (draft.theme === t.id) draft.theme = null; fillSelect(); show(); }).catch(fail);
  });
  const exp = btn('Export', 'Save this theme as a file', () => {
    const t = current();
    if (!t) return;
    const url = URL.createObjectURL(new Blob([exportTheme(t)], { type: 'application/json' }));
    const a = h('a', { href: url, download: (t.name.replace(/[^A-Za-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '') || 'theme') + '.spandeck-theme.json' });
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  });
  // Colours read from another terminal's file are shown here first, on top
  // of the theme chosen, to be changed and then saved under a name.
  const take = (got) => {
    for (const [k, v] of Object.entries(got.settings)) { if (same(themed()[k], v)) delete draft.set[k]; else draft.set[k] = v; }
    show();
    askSaveAs(got.name);
    toast(`Read the colours of ${got.name} from ${got.from}. The deck's colours are worked out from them. Change what you like, then save the theme.`);
  };
  // A file with several schemes in it: which one.
  const pickSelect = h('select', { 'aria-label': 'Colour scheme to import' });
  let picks = [];
  const closePicker = () => { picker.hidden = true; picks = []; };
  const picker = h('form', { class: 'look-picker row', hidden: true, onsubmit: (e) => { e.preventDefault(); const got = picks[Number(pickSelect.value)]; closePicker(); if (got) take(got); } },
    pickSelect, h('button', { type: 'submit', class: 'primary' }, 'Import this one'), h('button', { type: 'button', onclick: () => closePicker() }, 'Not now'));
  const file = h('input', { type: 'file', accept: '.json,.itermcolors,application/json', hidden: true, 'aria-label': 'A theme file' });
  file.addEventListener('change', async () => {
    const f = file.files[0];
    file.value = '';
    if (!f) return;
    if (f.size > 1024 * 1024) return toast('That file is too large to be a theme.', 'error');
    const got = importTheme(await f.text(), f.name);
    if (got.problem) return toast(got.problem, 'error');
    closePicker();
    if (got.choices) {
      picks = got.choices;
      pickSelect.replaceChildren(...picks.map((c, i) => h('option', { value: String(i) }, c.name)));
      picker.hidden = false;
      pickSelect.focus();
      return;
    }
    if (got.from) return take(got);
    saveTheme({ name: freeName(got.name), settings: got.settings }).then((id) => { choose(id); toast(`Imported ${themeById(id)?.name || 'the theme'}`); }).catch(fail);
  });
  const imp = btn('Import…', 'Add a theme from a file: spandeck\'s own, an iTerm2 colour file (.itermcolors), or a Windows Terminal colour scheme or settings.json', () => file.click());

  // ---- the buttons at the bottom ----
  const askSaveAs = (suggest) => askName(freeName(suggest), (name) =>
    saveTheme({ name, settings: effective() }).then((id) => { closeNamer(); choose(id); toast(`Saved the theme ${name}`); }).catch(fail));
  const saveAs = h('button', { type: 'button', title: 'Keep this look as a theme you can choose anywhere', onclick: () => {
    const base = current();
    askSaveAs(base && !isBuiltin(base.id) ? base.name + ' copy' : 'My theme');
  } }, 'Save as theme…');
  const updateTheme = h('button', { type: 'button', onclick: () => {
    const t = current();
    if (t && !isBuiltin(t.id)) saveTheme({ ...t, settings: { ...t.settings, ...draft.set } }).then(() => { draft.set = {}; show(); toast(`Updated the theme ${t.name}`); }).catch(fail);
  } });
  const syncButtons = () => {
    const t = current(), own = !!t && !isBuiltin(t.id), changed = Object.keys(draft.set).length > 0;
    dup.disabled = exp.disabled = !t;
    ren.disabled = del.disabled = !own;
    updateTheme.hidden = !(own && changed);
    if (own) updateTheme.textContent = `Update the theme ${t.name}`;
  };

  // A font arrived: the sample measures its cells again, and what is said
  // about the chosen font may no longer be so.
  const fontArrived = () => { if (sample && !done) remeasureTerminal(sample); if (!done) show(); };
  fontListeners.add(fontArrived);

  dialog(level === 'deck' ? 'Appearance of the deck' : `Appearance of ${scope.name}`, (close) => {
    const save = async () => {
      const choice = draft.theme || Object.keys(draft.set).length ? { theme: draft.theme || undefined, set: draft.set } : null;
      try {
        await env.api.call('look.set', { level, key: level === 'deck' ? undefined : key, choice });
        await env.reload();
      } catch (e) { return fail(e); }
      done = true;
      close();
    };
    const clear = async () => {
      try {
        await env.api.call('look.set', { level, key, choice: null });
        await env.reload();
      } catch (e) { return fail(e); }
      done = true;
      close();
    };
    const groups = GROUPS.map(([g, title]) => {
      const keys = Object.keys(KEYS).filter(k => KEYS[k].group === g && !(level === 'pane' && KEYS[k].page));
      return keys.length ? h('section', { class: 'look-group look-g-' + g }, h('h3', {}, title), h('div', { class: 'look-rows' }, keys.map(control)), g === 'font' ? uploaded() : null) : null;
    });
    fillSelect();
    // The sample stays in view at the top and the buttons at the bottom;
    // the settings scroll between them.
    const body = h('div', { class: 'look-ed' },
      h('div', { class: 'look-side' }, sampleBox),
      h('div', { class: 'look-form' },
        h('p', { class: 'muted' }, level === 'deck' ? 'The look of the whole Control Deck, on every station and in every browser.'
          : level === 'layout' ? `Used while ${scope.name} is on screen, in place of the deck's. What you do not change here follows the deck.`
            : `Only ${scope.name}: its terminal and its bar. What you do not change here follows the layout it is shown in, or the deck.`,
        ' The page shows each change as you make it; nothing is saved until you press Save.'),
        h('div', { class: 'look-theme' }, h('label', { class: 'look-label', htmlFor: 'look-theme' }, 'Theme'), Object.assign(select, { id: 'look-theme' }),
          h('div', { class: 'row look-theme-buttons' }, dup, ren, del, exp, imp, file)),
        picker, namer, warnBox, groups),
      h('div', { class: 'row end look-foot' },
        h('button', { type: 'button', onclick: close }, 'Cancel'),
        level === 'deck' ? null : h('button', { type: 'button', title: 'Take away everything set here', onclick: clear }, level === 'layout' ? 'Use the deck\'s' : `Use ${inheritName.toLowerCase()}`),
        saveAs, updateTheme,
        h('button', { type: 'button', class: 'primary', onclick: save }, 'Save')));
    requestAnimationFrame(() => {
      if (done || !sampleBox.isConnected) return;
      sample = newTerminal({ ...termOptions(effective()), cols: 48, rows: 21, disableStdin: true, scrollback: 0 });
      sample.open(sampleBox);
      sample.write(SAMPLE);
      show();
    });
    show();
    return body;
  }, {
    wide: true, beside: true,
    // Closed any way but Save: the page goes back to what is saved.
    onClose: () => { done = true; lookState.preview = null; fontListeners.delete(fontArrived); if (sample) sample.dispose(); env.apply(); },
  });
}

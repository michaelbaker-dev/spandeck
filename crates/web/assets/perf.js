// The Performance page: where time goes, across the fleet, and what this
// browser itself measured. Every number is asked of the station that holds
// it, from every station that answers; one that does not answer, or runs a
// version from before it could, is said to be so and is never shown as zero.
// Everything a station says is put on the page as text.

import { h, ago, copyText } from './ui.js';

const DAY = 86400000;

// ---- what this browser measures (its own; sent nowhere) ----

const keep = (list, v, max = 200) => { list.push(v); if (list.length > max) list.shift(); };
export const mine = { echo: [], refresh: [], asked: new Map() };
const typed = new WeakMap();
// A key was typed into a pane this browser controls; the next output of that
// pane is taken as its echo. A pane that is printing anyway reads too short.
export function keyTyped(term) { const t = typed.get(term); if (t === undefined || performance.now() - t > 2000) typed.set(term, performance.now()); }
export function paneWrote(term) { const t = typed.get(term); if (t !== undefined) { typed.delete(term); keep(mine.echo, performance.now() - t); } }
export const refreshTook = (ms) => keep(mine.refresh, ms, 50);

const pct = (list, p) => { if (!list.length) return null; const s = list.slice().sort((a, b) => a - b); return s[Math.min(s.length - 1, Math.ceil(s.length * p) - 1)]; };

// ---- words ----

const num = (v) => (typeof v === 'number' && isFinite(v) ? v : null);
export function ms(v) {
  v = num(v);
  if (v === null) return '—';
  if (v >= 10000) return (v / 1000).toFixed(0) + ' s';
  if (v >= 1000) return (v / 1000).toFixed(1) + ' s';
  if (v >= 100) return v.toFixed(0) + ' ms';
  if (v >= 10) return v.toFixed(1) + ' ms';
  return v.toFixed(2) + ' ms';
}
const span = (v) => { v = num(v); if (v === null) return '—'; const s = Math.round(v / 1000); return s < 90 ? s + ' s' : s < 5400 ? Math.round(s / 60) + ' min' : s < 172800 ? Math.round(s / 3600) + ' h' : Math.round(s / 86400) + ' days'; };
const clock = (at) => (num(at) ? new Date(at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '');
const day = (at) => new Date(at).toLocaleDateString([], { day: 'numeric', month: 'short' });
const days = (n) => `${n} day${n === 1 ? '' : 's'}`;
// What a station says its last restart left of its sessions. Numbers from
// another station: each is a number or is not shown.
function cameBack(x) {
  const n = (k) => num(x[k]) || 0, parts = [];
  if (n('kept')) parts.push(`${n('kept')} kept running`);
  if (n('resumed')) parts.push(`${n('resumed')} started again`);
  if (n('held_back')) parts.push(`${n('held_back')} not started again (3 restarts in 10 minutes)`);
  if (n('closed')) parts.push(`${n('closed')} ended`);
  if (n('tasks_failed')) parts.push(`${n('tasks_failed')} task${n('tasks_failed') === 1 ? '' : 's'} failed`);
  const away = num(x.away_ms) !== null ? `away ${span(x.away_ms)}; ` : '';
  return `${x.why === 'crash' ? 'ended without being stopped' : x.why === 'upgrade' ? 'an upgrade lost terminals' : 'stopped and started'}: ${away}${parts.length ? parts.join(', ') : 'no session was open'}`;
}
export const clockWords = (diff) => { const s = Math.round(Math.abs(diff) / 1000); return s >= 5400 ? `${Math.round(s / 3600)} h` : s >= 90 ? `${Math.round(s / 60)} min` : `${s} s`; };

// A Tailscale key, in words: null when nothing is known of it.
export function keyInfo(k, now = Date.now()) {
  if (!k || k.known === false) return null;
  const e = num(k.expires_ms);
  if (e === null) return k.expired ? { text: 'expired', level: 'bad', expired: true } : { text: 'never expires', level: 'ok' };
  if (k.expired || e <= now) { const n = Math.floor((now - e) / DAY); return { text: n < 1 ? `expired today (${day(e)})` : `expired ${days(n)} ago (${day(e)})`, level: 'bad', expired: true, at: e }; }
  const n = Math.floor((e - now) / DAY);
  return { text: n < 1 ? `expires today (${day(e)})` : `expires in ${days(n)} (${day(e)})`, level: n <= 7 ? 'bad' : n <= 30 ? 'warn' : 'ok', left: n, at: e };
}
const TS_STATE = { NeedsLogin: 'signed out of Tailscale', Stopped: 'Tailscale is stopped', NoState: 'Tailscale is starting', Starting: 'Tailscale is starting', not_running: 'Tailscale is not running' };
// Tailscale's admin console (the address its own help gives), and the same
// page listing only machines whose key has expired, which is a filter its
// help documents. No address is built from a device's name.
export const TS_MACHINES = 'https://console.tailscale.com/admin/machines';
export const TS_EXPIRED = TS_MACHINES + '?q=disabled%3Aexpired';
const MACHINES = { a: TS_MACHINES, text: 'Machines page of Tailscale\'s admin console' };
const REAUTH = { code: 'sudo tailscale up --force-reauth' };
// An address a station gave for a machine: shown only if it reads as one.
const addresses = (list) => (Array.isArray(list) ? list : []).filter(x => typeof x === 'string' && /^[0-9a-f.:]{3,45}$/i.test(x)).slice(0, 3);
// What to do about a key, in the order Tailscale's help gives the steps and
// in its words for what to click. Groups of steps; a step is text, links
// ({a, text}) and commands ({code}). `more`: { station, os, lan }.
// spandeck does none of it: every step is the owner's.
export function keySteps(name, info, more = {}) {
  const phone = /^(ios|android)$/i.test(more.os || ''), lan = addresses(more.lan);
  const row = `Find the row for ${name}`;
  const there = phone ? ['On the device, open the Tailscale app and sign in again.']
    : ['Open Tailscale on it and sign in again. Or, in a terminal on it, run ', REAUTH, ' and open the address it prints.'];
  if (info?.expired) {
    const groups = [
      { title: 'From here, on any device', steps: [
        ['Open the ', MACHINES, ' and sign in. (Or ', { a: TS_EXPIRED, text: 'the same page showing only machines whose key has expired' }, '.)'],
        [`${row}, open the … menu at the far right of it, and choose "Temporarily extend key". Tailscale offers that only for a machine whose key has expired. If it offers "Reauthorize machine" in its place, choose that.`],
        ['The key then works for 30 minutes. Within them, in the same menu choose "Disable Key Expiry", after which it stays on your network; or sign in again at the machine, as below. If neither is done it drops off again.'],
      ] },
      { title: phone ? 'Or on the device itself' : 'Or at the machine itself, at any time', steps: [there] }];
    if (more.station) groups.push({ title: 'Or over its own network, if you are on it', steps: [
      lan.length ? ['Connect to it without Tailscale: ', { code: `ssh ${lan[0]}` }, lan.length > 1 ? ` (its last addresses there: ${lan.join(', ')}; they may have changed since).` : ' (its last address there; it may have changed since).']
        : ['Connect to it without Tailscale: ssh to its address on that network. spandeck has not seen one for it.'],
      ['There, run ', REAUTH, ' and open the address it prints, on any device. That connection does not go through Tailscale, so signing in again cannot cut it.'],
    ] });
    return { groups, notes: [more.station ? 'When it is back, spandeck finds it by itself and delivers what was waiting. Wake and Start do not help: the machine is on, and only Tailscale is refusing it.'
      : 'spandeck changes nothing in Tailscale: these steps are yours to take.'] };
  }
  const never = { title: 'Stop it expiring: from here, on any device', steps: [
    ['Open the ', MACHINES, ' and sign in.'],
    [`${row} and open the … menu at the far right of it.`],
    ['Choose "Disable Key Expiry". Its key then no longer expires, and spandeck says so within a minute.']] };
  const renew = { title: phone ? 'Renew it: on the device' : 'Renew it: at the machine itself', steps: [there,
    ['That renews the key for your network\'s usual time: 180 days unless you changed it.' + (phone ? '' : ' Do it at the machine and not over a remote connection: Tailscale\'s help says signing in again may bring the connection down.')]] };
  return { groups: phone ? [renew, never] : [never, renew], notes: [
    phone ? 'Which one: for a phone or tablet, renew. It is the device most likely to be lost, and expiry is what takes a lost device off your network without your doing anything.'
      : 'Which one: stop the key expiring on a machine that is always on and that nobody sits at, or it drops off on a day nobody is there to sign in (Tailscale\'s help names "trusted servers" and devices "that are hard to reach"). A laptop is your call: with no expiry, one that is lost or stolen stays on your network until you remove it. A phone should keep its expiry.',
    'spandeck warns at 14, 7, 3 and 1 day before, and when the key has expired. It does not renew a key or change anything in Tailscale itself.'] };
}
// What the deck opened on a machine itself says when that machine's
// Tailscale client is signed out or its key has expired. `s`: what
// `tailnet.signin` said; `link`: whether there is an address to open.
export function signInWords(station, s, link) {
  return {
    title: s.expired && s.state !== 'NeedsLogin' ? `The Tailscale key of this machine (${station}) has expired` : `Tailscale is signed out on this machine (${station})`,
    why: `${station} is off your Tailscale network: no other station or device can reach it, and this Control Deck opens only here. Its agents keep running.`,
    how: link ? ['Sign in at this address, here or on a phone (scan the code), with the account your other machines are under: ']
      : ['To sign in: open Tailscale on this machine and sign in. Or, in a terminal here, run ', { code: s.expired ? 'sudo tailscale up --force-reauth' : 'sudo tailscale up' }, ' and open the address it prints. Once Tailscale offers an address, it is shown here with a code to scan.'],
    after: 'spandeck does not sign in for you. This goes away by itself within a few seconds of the sign-in.' };
}
// Where a station is on its own network, for the steps: what the station
// serving this deck knows first, then what any other station answered.
export function lanOf(data, station, local) {
  const all = [...addresses(local?.stations?.[station]?.lan)];
  for (const r of data?.rows || []) for (const a of addresses(r.report?.tailnet?.stations?.[station]?.lan)) if (!all.includes(a)) all.push(a);
  return all.slice(0, 3);
}

// ---- gathering ----

// Every member of the fleet, with what it answered. `fleet` is fleet.view.
export async function gather(api, fleet) {
  const began = performance.now();
  const members = fleet.stations.filter(s => s.state !== 'not_linked');
  const rows = await Promise.all(members.map(async (s) => {
    if (s.state !== 'online') return { s, report: null };
    const t = performance.now();
    try { const report = await api.on(s.station, 'perf.get'); keep(mine.asked.get(s.station) || mine.asked.set(s.station, []).get(s.station), performance.now() - t, 50); return { s, report, asked: performance.now() - t }; } catch (e) {
      // A station from before the page: it has no such method.
      return { s, report: null, old: /method perf\.get|not available/.test(e.message || ''), problem: String(e.message || 'no answer').slice(0, 200) };
    }
  }));
  return { at: Date.now(), took: performance.now() - began, rows, outside: fleet.stations.filter(s => s.state === 'not_linked').map(s => s.station) };
}

const linkOf = (row, to) => (row.report?.links || []).find(l => l.station === to) || null;
// A station's key as the fleet knows it: what any station that answered says
// of it, the most recently checked first. The same from every station.
export function keyOf(data, station) {
  let best = null;
  for (const r of data.rows) { const k = r.report?.tailnet?.stations?.[station]; if (k && k.known !== false && (!best || (num(k.as_of_ms) || 0) > (num(best.as_of_ms) || 0))) best = k; }
  return best;
}
// The owner's other devices near expiry: one list, whichever station is asked.
export function otherDevices(data) {
  const byId = new Map();
  for (const r of data.rows) for (const d of r.report?.tailnet?.devices || []) if (d && typeof d.id === 'string' && !byId.has(d.id)) byId.set(d.id, d);
  return [...byId.values()].sort((a, b) => (num(a.expires_ms) || 0) - (num(b.expires_ms) || 0)).slice(0, 64);
}

// One sentence per station: the first thing that is wrong, or "fine".
export function verdict(data, row) {
  const s = row.s, r = row.report, now = Date.now();
  const seenBy = data.rows.map(x => linkOf(x, s.station)).filter(Boolean);
  const off = seenBy.map(l => l.clock).filter(c => c && c.refused);
  const key = keyInfo(keyOf(data, s.station), now);
  if (s.state !== 'online') {
    if (off.length || s.why === 'clock') return { level: 'bad', text: `clock differs by ${clockWords(off[0]?.diff_ms ?? s.clock_diff_ms ?? 0)}: calls to and from it are refused until the clocks agree` };
    if (key?.expired) return { level: 'bad', text: `Tailscale key ${key.text}: it cannot be reached until that is put right` };
    if (s.state === 'update_needed') return { level: 'warn', text: 'on a version this station cannot control' };
    return { level: 'bad', text: num(s.last_seen_ms) ? `not answering since ${clock(s.last_seen_ms)} (${ago(s.last_seen_ms)} ago)` : 'not answering' };
  }
  if (!r) return row.old ? { level: 'info', text: `not reported (${s.version || 'an older version'}): update it to see its numbers` } : { level: 'warn', text: `did not answer the question (${row.problem || 'no answer'})` };
  const tn = r.tailnet || {};
  if (tn.read && tn.state && tn.state !== 'Running') return { level: 'bad', text: TS_STATE[tn.state] || `Tailscale: ${String(tn.state).slice(0, 24)}` };
  if (key && key.level !== 'ok' && (key.expired || key.left <= 14)) return { level: key.level, text: `Tailscale key ${key.text}` };
  // A clock that differs from one other station's is that station's to
  // answer for, unless this one differs from most of those it has read.
  const read = (r.links || []).map(l => ({ l, c: l.clock })).filter(x => x.c);
  const apart = read.filter(x => Math.abs(x.c.diff_ms) >= 30000).sort((a, b) => Math.abs(b.c.diff_ms) - Math.abs(a.c.diff_ms));
  const skew = apart.length * 2 > read.length ? apart[0] : null;
  if (skew) return { level: skew.c.refused ? 'bad' : 'warn', text: `clock differs from ${skew.l.station}'s by ${clockWords(skew.c.diff_ms)}` + (skew.c.refused ? '' : ' (past 2 min, calls are refused)') };
  if ((num(r.restarts?.last_hour) || 0) >= 3) return { level: 'bad', text: `restarted ${r.restarts.last_hour} times in the last hour` };
  if (r.database && r.database.healthy === false) return { level: 'bad', text: `cannot write its database (${num(r.database.waiting) || 0} changes waiting)` };
  const late = num(r.periodic?.late?.worst_1m_ms) || 0;
  if (late >= 250) return { level: 'warn', text: `busy: its work ran ${ms(late)} late` };
  const slow = (r.links || []).filter(l => l.state === 'online' && (num(l.rtt?.p95_ms) || 0) >= 150).sort((a, b) => b.rtt.p95_ms - a.rtt.p95_ms)[0];
  if (slow) return { level: 'warn', text: `slow link to ${slow.station} (p95 ${ms(slow.rtt.p95_ms)})` };
  const waiting = (r.messages?.waiting || []).filter(w => (num(w.oldest_ms) || 0) >= 30000)[0];
  if (waiting) return { level: 'warn', text: `messages to ${waiting.station} are waiting (oldest ${span(waiting.oldest_ms)})` };
  return { level: 'ok', text: 'fine' };
}
// Is anything on the page other than fine? For the dot on the top bar.
export const anyTrouble = (data) => !!data && data.rows.some(row => ['bad', 'warn'].includes(verdict(data, row).level));

// ---- the page ----

const stat = (x, what) => (x && num(x.count) ? `${ms(x.p50_ms)} / ${ms(x.p95_ms)}` : what || 'none yet');
const statTitle = (x, of) => (x && num(x.count) ? `${of}: half took under ${ms(x.p50_ms)}, 95% under ${ms(x.p95_ms)}, the longest ${ms(x.max_ms)}; ${x.count} in the last 5 minutes` : `${of}: none in the last 5 minutes`);
const KINDS = [['deck', 'Control Deck'], ['local', 'agents and command line'], ['station', 'other stations'], ['http', 'HTTP']];

function stationFacts(data, row) {
  const s = row.s, r = row.report, v = verdict(data, row), key = keyInfo(keyOf(data, s.station));
  const na = s.state !== 'online' ? 'not answering' : row.old ? 'not reported' : 'no answer';
  const f = (label, value, title) => ({ label, value, title });
  const req = r ? KINDS.filter(([k]) => num(r.requests?.handled?.[k]?.count)).map(([k, name]) => ({ k, name, x: r.requests.handled[k] })) : [];
  return { s, r, v, facts: [
    f('Version', s.version || '?', s.state === 'online' ? '' : 'The version it last had'),
    f('Up for', r ? span(r.now_ms - r.started_ms) : na, r ? `Since ${new Date(r.started_ms).toLocaleString()}; ${r.panes} panes` : ''),
    f('Restarts, 24 h', r ? String(num(r.restarts?.last_24h) ?? 0) : na, r ? (r.restarts?.list || []).slice(0, 4).map(x => `${clock(x.at_ms)} ${x.what}: ${x.why}`).join('\n') : ''),
    f('Requests (p50 / p95)', r ? (req.length ? req.map(q => `${q.k} ${stat(q.x)}`).join(' · ') : 'none yet') : na, r ? KINDS.map(([k, name]) => statTitle(r.requests?.handled?.[k], `From ${name}`)).join('\n') : ''),
    f('Waited in line', r ? stat(r.requests?.waited?.deck, 'no deck open') : na, r ? statTitle(r.requests?.waited?.deck, 'How long a request from a Control Deck stood behind the ones before it on its connection') : ''),
    f('Ran late, worst 1 min', r ? ms(r.periodic?.late?.worst_1m_ms) : na, r ? statTitle(r.periodic?.late, 'How late its twice-a-second work began') + '\n' + statTitle(r.periodic?.work, 'How long that work took') : ''),
    f('Database', r ? (r.database ? `${num(r.database.waiting) || 0} waiting, ${num(r.database.log_dropped) || 0} dropped` + (r.database.queued === false ? ' (writer off)' : '') : 'not reported') : na, r && r.database ? `Changes of state not yet written: ${num(r.database.waiting) || 0}, the oldest for ${span(num(r.database.oldest_ms) || 0)}; log entries waiting: ${num(r.database.log_waiting) || 0}, dropped since start: ${num(r.database.log_dropped) || 0}; changes dropped: ${num(r.database.state_dropped) || 0}; tries that failed: ${num(r.database.failures) || 0}; writes over 50 ms: ${num(r.database.slow) || 0}` + (r.database.last_error ? `\nFailing: ${String(r.database.last_error).slice(0, 120)}` : '') : ''),
    f('Look at the fleet', r ? stat(r.look) : na, r ? statTitle(r.look, 'One look at every station, every 10 seconds') : ''),
    f('Repaints', r ? `${num(r.terminal?.resyncs?.last_5m) ?? 0} (${num(r.terminal?.resyncs?.since_start) ?? 0} since start)` : na, 'A client that fell behind a pane and was sent the whole screen again'),
    f('Messages', r ? `there ${stat(r.messages?.received, '—')} · told ${stat(r.messages?.woken, '—')}` + ((r.messages?.waiting || []).length ? ` · ${r.messages.waiting.reduce((a, w) => a + (num(w.count) || 0), 0)} waiting` : '') : na,
      r ? statTitle(r.messages?.received, 'From sent here to stored at another station') + '\n' + statTitle(r.messages?.woken, 'From stored here to the agent being told') + (r.messages?.waiting || []).map(w => `\n${w.count} waiting for ${w.station}, the oldest for ${span(w.oldest_ms)}`).join('') : ''),
    f('Tailscale key', key ? key.text : r && r.tailnet && !r.tailnet.read ? 'not read here' : 'not known', key ? '' : 'A station reads its key when it finds stations through Tailscale (discovery).'),
  ], key };
}

function linkCell(data, from, to) {
  if (from.s.station === to.s.station) return h('td', { class: 'self' }, '·');
  if (from.s.state !== 'online') return h('td', { class: 'na', title: `${from.s.station} is not answering, so how it sees ${to.s.station} is not known` }, 'not answering');
  if (!from.report) return h('td', { class: 'na' }, 'not reported');
  const l = linkOf(from, to.s.station);
  if (!l) return h('td', { class: 'na' }, 'not listed');
  const c = l.clock, f = num(l.failures?.last_5m) || 0, up = l.state === 'online';
  const lines = [
    h('b', {}, up ? ms(l.rtt_now_ms) : c?.refused ? 'clock differs' : l.state === 'unreachable' ? 'no answer' : String(l.state).replace('_', ' ')),
    up && num(l.rtt?.count) ? h('span', { class: 'muted' }, ` p95 ${ms(l.rtt.p95_ms)}`) : null,
    h('div', { class: 'muted' }, [l.path ? (l.path === 'relayed' ? `relayed${l.relay ? ' via ' + l.relay : ''}` : 'direct') : null, f ? `${f} failed` : null, c && Math.abs(c.diff_ms) >= 30000 ? `clock ${c.diff_ms > 0 ? '+' : '−'}${clockWords(c.diff_ms)}` : null].filter(Boolean).join(' · ') || (up ? 'path not known' : '')),
    num(l.since_ms) ? h('div', { class: 'muted' }, `${up ? 'up' : 'so'} since ${clock(l.since_ms)}`) : null];
  const title = [`${from.s.station} to ${to.s.station}`, statTitle(l.rtt, 'Round trip'), statTitle(l.calls, 'Calls, with the time they took there'),
    `${f} calls failed in the last 5 minutes, ${num(l.failures?.since_start) || 0} since ${from.s.station} started` + (l.failures?.last ? `; the last at ${clock(l.failures.last.at_ms)}: ${l.failures.last.why}` : ''),
    c ? `${to.s.station}'s clock is ${clockWords(c.diff_ms)} ${c.diff_ms > 0 ? 'ahead of' : 'behind'} ${from.s.station}'s (give or take ${ms(c.bound_ms)})` : 'Clock difference not known',
    l.was ? `Was ${l.was} until ${clock(l.since_ms)}` : ''].filter(Boolean).join('\n');
  return h('td', { class: up ? (f || (c && Math.abs(c.diff_ms) >= 30000) ? 'warn' : '') : 'bad', title }, lines);
}
const linkLine = (data, from, to) => { const td = linkCell(data, from, to); return h('div', { class: 'perf-pair ' + td.className, title: td.title }, h('span', { class: 'muted' }, `${from.s.station} → ${to.s.station}`), h('div', {}, [...td.childNodes])); };

export function asText(data, facts) {
  const out = [`spandeck performance, ${new Date(data.at).toLocaleString()}`];
  for (const x of facts) { out.push('', `${x.s.station}: ${x.v.text}`); for (const f of x.facts) out.push(`  ${f.label}: ${f.value}`); }
  out.push('', 'Links (from → to):');
  for (const a of data.rows) for (const b of data.rows) if (a !== b) { const l = linkOf(a, b.s.station); out.push(`  ${a.s.station} → ${b.s.station}: ` + (l ? `${l.state}, ${ms(l.rtt_now_ms)} now, p95 ${ms(l.rtt?.p95_ms)}, ${l.path || 'path not known'}, ${num(l.failures?.last_5m) || 0} failed` + (l.clock ? `, clock ${l.clock.diff_ms} ms` : '') : 'not reported')); }
  out.push('', `This browser: echo p50 ${ms(pct(mine.echo, 0.5))} p95 ${ms(pct(mine.echo, 0.95))} (${mine.echo.length} keys); refresh p95 ${ms(pct(mine.refresh, 0.95))}`);
  return out.join('\n');
}

// `ctx`: { data, api, phone, reload, explain(name, info, more) }.
export function perfPage(ctx) {
  const { data, api, phone } = ctx;
  const head = (extra) => h('div', { class: 'row perf-head' }, phone ? h('a', { class: 'btn', href: '#/fleet' }, '‹ Back') : null, h('h1', {}, 'Performance'), h('span', { class: 'grow' }), extra);
  if (!data) return h('main', { class: 'perf-page' + (phone ? ' phone' : '') }, head(), h('p', { class: 'muted' }, 'Asking every station…'));
  const facts = data.rows.map(row => stationFacts(data, row));
  const labels = facts[0] ? facts[0].facts.map(f => f.label) : [];
  const others = otherDevices(data);
  const slow = data.rows.flatMap(row => (row.report?.requests?.slowest || []).map(x => ({ ...x, on: row.s.station }))).sort((a, b) => (num(b.ms) || 0) - (num(a.ms) || 0)).slice(0, 10);
  const restarts = data.rows.flatMap(row => (row.report?.restarts?.list || []).map(x => ({ ...x, on: row.s.station }))).sort((a, b) => (num(b.at_ms) || 0) - (num(a.at_ms) || 0)).slice(0, 12);
  const back = data.rows.filter(row => row.report?.recovered && num(row.report.recovered.at_ms)).map(row => ({ ...row.report.recovered, on: row.s.station }));
  const versions = [...new Set(data.rows.map(r => r.s.version).filter(Boolean))];
  const keyCell = (x) => { const name = x.key ? (x.r?.tailnet?.stations?.[x.s.station]?.name || keyOf(data, x.s.station)?.name || x.s.station) : x.s.station;
    return x.key && x.key.level !== 'ok' ? h('button', { class: 'mini key-' + x.key.level, onclick: () => ctx.explain(name, x.key, { station: true, lan: lanOf(data, x.s.station) }), title: 'What to do' }, x.facts[x.facts.length - 1].value) : x.facts[x.facts.length - 1].value; };
  const cell = (x, i) => { const f = x.facts[i]; return h('td', { title: f.title || null, class: x.r || i === 0 || i === labels.length - 1 ? '' : 'na' }, i === labels.length - 1 ? keyCell(x) : f.value); };
  const sec = (title, note, ...kids) => h('section', { 'aria-label': title }, h('h2', {}, title), note ? h('p', { class: 'muted small' }, note) : null, ...kids);
  const echo = mine.echo, asked = [...mine.asked.entries()].sort();
  return h('main', { class: 'perf-page' + (phone ? ' phone' : '') },
    head([h('span', { class: 'muted small', 'aria-live': 'polite' }, `as of ${new Date(data.at).toLocaleTimeString()} · asked in ${ms(data.took)}`),
      h('button', { onclick: ctx.reload }, 'Refresh'), h('button', { onclick: () => copyText(asText(data, facts), 'Copied as text') }, 'Copy as text')]),
    h('p', { class: 'muted small' }, `Each station answers for itself, so this page is the same from any of them. Figures are of the last 5 minutes unless they say otherwise. It refreshes by itself.`),
    h('ul', { class: 'perf-verdicts', 'aria-label': 'Each station in a sentence' }, facts.map(x => h('li', { class: 'verdict ' + x.v.level },
      h('span', { class: 'dot ' + ({ ok: 'idle', info: 'idle', warn: 'needs_you', bad: 'error' })[x.v.level], 'aria-hidden': 'true' }), h('b', {}, x.s.station), x.s.self ? h('span', { class: 'chip' }, 'this station') : null, h('span', {}, x.v.text)))),
    data.outside.length ? h('p', { class: 'muted small' }, `Not in this fleet, so not shown: ${data.outside.join(', ')}.`) : null,
    versions.length > 1 ? h('p', { class: 'warn-text small' }, `The stations do not all run the same version: ${data.rows.map(r => `${r.s.station} ${r.s.version || '?'}`).join(', ')}.`) : null,

    sec('Stations', 'Hold the pointer over a figure for what it is.',
      phone ? facts.map(x => h('div', { class: 'perf-card' }, h('b', {}, x.s.station), h('dl', {}, x.facts.map((f, i) => [h('dt', {}, f.label), h('dd', { title: f.title || null }, i === labels.length - 1 ? keyCell(x) : f.value)]))))
        : h('div', { class: 'table-wrap' }, h('table', { class: 'table perf-table' },
          h('thead', {}, h('tr', {}, h('th', { scope: 'col' }, 'Station'), labels.map(l => h('th', { scope: 'col' }, l)))),
          h('tbody', {}, facts.map(x => h('tr', {}, h('th', { scope: 'row' }, x.s.station), x.facts.map((f, i) => cell(x, i)))))))),

    sec('Links', 'Each station\'s own view of each other one: the round trip of a call that does no work there, which way it goes, calls that failed, and when the link last changed.',
      data.rows.length < 2 ? h('p', { class: 'muted' }, 'This fleet has one station.')
        : phone ? [data.rows.filter(a => a.report).flatMap(a => data.rows.filter(b => b !== a).map(b => linkLine(data, a, b))),
          data.rows.some(a => !a.report) ? h('p', { class: 'muted small' }, `Their own view of the others is not known: ${data.rows.filter(a => !a.report).map(a => `${a.s.station} (${a.s.state !== 'online' ? 'not answering' : 'not reported'})`).join(', ')}.`) : null]
          : h('div', { class: 'table-wrap' }, h('table', { class: 'table perf-links' },
            h('thead', {}, h('tr', {}, h('th', { scope: 'col' }, 'From ↓ to →'), data.rows.map(b => h('th', { scope: 'col' }, b.s.station)))),
            h('tbody', {}, data.rows.map(a => h('tr', {}, h('th', { scope: 'row' }, a.s.station), data.rows.map(b => linkCell(data, a, b)))))))),

    sec('Slowest requests', 'The longest of the last 5 minutes on each station: the method and how it came in, never what it carried. Calls that wait on purpose are left out.',
      slow.length ? h('div', { class: 'table-wrap' }, h('table', { class: 'table perf-slow' },
        h('thead', {}, h('tr', {}, ['Station', 'Method', 'From', 'Took', 'When'].map(t => h('th', { scope: 'col' }, t)))),
        h('tbody', {}, slow.map(x => h('tr', {}, h('td', {}, x.on), h('td', {}, String(x.method || '') + (x.station ? ` on ${x.station}` : '')), h('td', {}, String(x.kind || '')), h('td', { class: 'num' }, ms(x.ms)), h('td', {}, clock(x.at_ms)))))))
        : h('p', { class: 'muted' }, 'None took 20 ms or more.')),

    sec('Restarts', 'Each station\'s starts, upgrades and restarts of the last 24 hours, and why.',
      restarts.length ? h('ul', { class: 'perf-list' }, restarts.map(x => h('li', {}, h('b', {}, x.on), ` ${new Date(x.at_ms).toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' })} · ${x.what}: ${x.why}`,
        num(x.panes) !== null ? ` (${num(x.kept) ?? '?'} of ${x.panes} terminals kept)` : num(x.resumed) ? ` (${x.resumed} agents resumed)` : '')))
        : h('p', { class: 'muted' }, 'None in the last 24 hours.'),
      // What each station's last restart without a handoff left of its
      // sessions (a station from before this says nothing of it).
      back.length ? h('ul', { class: 'perf-list', 'aria-label': 'What came back' }, back.map(x => h('li', {}, h('b', {}, x.on), ` ${clock(x.at_ms)} · ${cameBack(x)}`))) : null),

    sec('Tailscale keys', 'A machine\'s sign-in to Tailscale, which Tailscale ends after 180 days unless told otherwise. When it ends the machine drops off your network, and spandeck with it.',
      h('ul', { class: 'perf-list' }, facts.map(x => h('li', {}, h('b', {}, x.s.station), ' ', keyCell(x)))),
      h('h3', {}, 'Other devices under your login'),
      others.length ? h('ul', { class: 'perf-list' }, others.map(d => { const k = keyInfo({ known: true, expires_ms: d.expires_ms, expired: d.expired });
        return h('li', {}, h('b', {}, String(d.name || 'a device')), d.os ? h('span', { class: 'muted' }, ` ${d.os}`) : null, ' ', h('button', { class: 'mini key-' + k.level, onclick: () => ctx.explain(String(d.name || 'the device'), k, { os: String(d.os || '') }) }, k.text), d.deck ? h('span', { class: 'chip' }, 'uses the deck') : null); }))
        : h('p', { class: 'muted' }, facts.some(x => x.r?.tailnet?.read) ? 'None expires within 30 days, and none expired in the last 30.' : 'No station has read its Tailscale client.')),

    sec('This browser', 'Measured in this page, on this device; it is sent nowhere and starts again when the page is reloaded.',
      h('div', { class: 'facts' },
        h('div', { class: 'fact', title: 'From a key typed into a pane this browser controls to the next thing that pane printed. A pane that is printing anyway reads too short.' }, h('span', { class: 'muted' }, 'Key to echo (p50 / p95)'), h('b', {}, echo.length ? `${ms(pct(echo, 0.5))} / ${ms(pct(echo, 0.95))}` : 'nothing typed yet'), h('span', { class: 'muted' }, `${echo.length} keys`)),
        h('div', { class: 'fact' }, h('span', { class: 'muted' }, 'Reconnects'), h('b', {}, String(api.reconnects || 0)), h('span', { class: 'muted' }, api.lastReconnect ? `last ${ago(api.lastReconnect)} ago` : 'none since the page opened')),
        h('div', { class: 'fact', title: 'One round of the questions this page asks every station, every 2.5 seconds' }, h('span', { class: 'muted' }, 'Refresh (last / p95)'), h('b', {}, mine.refresh.length ? `${ms(mine.refresh[mine.refresh.length - 1])} / ${ms(pct(mine.refresh, 0.95))}` : 'not yet')),
        asked.map(([st, list]) => h('div', { class: 'fact', title: `A question to ${st} and its answer, through ${api.station}` }, h('span', { class: 'muted' }, `Asking ${st}`), h('b', {}, `${ms(list[list.length - 1])}`), h('span', { class: 'muted' }, `p95 ${ms(pct(list, 0.95))}`))))));
}

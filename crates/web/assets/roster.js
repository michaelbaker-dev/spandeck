// The fleet's stations, as this device knows them, and which one to work
// through (DECISIONS.md, "The list of stations on the device, and choosing
// one"). No station is first in it: "home" is only the address this page
// was loaded from.
//
// The rules are plain functions of what is given, so that they can be
// checked one by one (crates/cli/tests/failover.rs runs them in a browser).

export const KEY = 'spandeck.roster';
// A station's "was it on" is counted at most this often.
export const LOOK_EVERY = 10 * 60 * 1000;
// "Always on": on at this share of at least this many looks.
export const ALWAYS_SHARE = 0.97, ALWAYS_LOOKS = 24;
// Measuring every station is given this long in all.
export const MEASURE_WITHIN = 1500;
// Latencies closer than this are the same: the always-on station wins.
export const SAME_MS = 10;
// The fastest is taken by itself after this long, unless the owner picks,
// stops it, or is writing.
export const TAKE_AFTER = 5000;
// A key this recently means the owner is writing.
export const WRITING_FOR = 3000;

const text = (v, max = 64) => (typeof v === 'string' ? v.slice(0, max) : '');
const hostOk = (a) => typeof a === 'string' && a.length <= 260 && /^[0-9A-Za-z.:\[\]-]+$/.test(a);
const isLocal = (a) => /^(127\.|\[::1\]|localhost)/.test(a);

export function load() {
  try {
    const r = JSON.parse(localStorage.getItem(KEY) || 'null');
    return r && r.v === 1 && Array.isArray(r.stations) ? r : null;
  } catch (e) { return null; }
}
export function save(r) {
  try { localStorage.setItem(KEY, JSON.stringify(r)); } catch (e) { /* this device keeps no list */ }
}
export function forget() {
  try { localStorage.removeItem(KEY); } catch (e) { /* nothing kept */ }
}

// The list after a look at the fleet (`fleet.view` of the station being
// worked through). Only members are kept: a station that was removed or
// forgotten is gone from the list in this same step. `home`: the station
// this page was loaded from, when the page knows (it is connected to it).
// `local`: the page itself is at a loopback address (several stations on
// one machine); elsewhere a loopback address is another machine's own.
export function fromFleet(prev, stations, now, { home = null, local = false } = {}) {
  const before = new Map((prev?.stations || []).map(s => [s.station, s]));
  const kept = [];
  for (const s of Array.isArray(stations) ? stations : []) {
    const name = text(s?.station);
    if (!name || s.state === 'not_linked') continue;
    const was = before.get(name) || {};
    const on = s.state === 'online';
    // Where its deck is over HTTP: its own addresses when it is the one
    // answering, else the address the answering station reaches it at.
    const addrs = (s.self ? (Array.isArray(s.web) ? s.web : []) : [s.addr]).filter(hostOk);
    const http = addrs.find(a => !isLocal(a)) || (local ? addrs.find(isLocal) : null) || null;
    const said = s.https && s.https.on === true && hostOk(s.https.name) && Number.isInteger(s.https.port) && s.https.port > 0 && s.https.port < 65536
      ? { name: s.https.name.toLowerCase(), port: s.https.port } : null;
    const count = !was.lookAt || now - was.lookAt >= LOOK_EVERY;
    kept.push({
      station: name,
      // A station that is off says nothing: what was known stays.
      http: on || !was.http ? http : was.http,
      https: on ? said : (was.https || null),
      version: on ? text(s.version, 32) : (was.version || text(s.version, 32)),
      caps: on ? (Array.isArray(s.caps) ? s.caps.filter(c => typeof c === 'string').slice(0, 64) : []) : (was.caps || []),
      label: on ? (Array.isArray(s.labels) && s.labels.includes('always-on')) : !!was.label,
      seen: on ? now : (was.seen || (Number.isFinite(s.last_seen_ms) ? s.last_seen_ms : 0)),
      ms: Number.isFinite(was.ms) ? was.ms : null,
      looks: (was.looks || 0) + (count ? 1 : 0),
      on: (was.on || 0) + (count && on ? 1 : 0),
      lookAt: count ? now : was.lookAt,
    });
  }
  kept.sort((a, b) => a.station.localeCompare(b.station));
  return { v: 1, home: home || prev?.home || null, last: prev?.last || null, stations: kept };
}

// Is this station one that is on whenever this device looks? Its own
// label says so, or its history here does. Nobody is asked.
export function alwaysOn(s) {
  return !!s.label || ((s.looks || 0) >= ALWAYS_LOOKS && (s.on || 0) / s.looks >= ALWAYS_SHARE);
}

// How this page may reach each station of the list, or why it cannot.
// `secure`: the page was loaded over HTTPS, and a browser then allows only
// `wss:`. `home`: the page's own station is reached at its own address.
export function candidates(roster, { secure, host, home }) {
  return (roster?.stations || []).map((s) => {
    const base = { station: s.station, version: s.version, caps: s.caps || [], alwaysOn: alwaysOn(s), seen: s.seen, isHome: s.station === home };
    if (base.isHome) return { ...base, url: `${secure ? 'wss' : 'ws'}://${host}` };
    if (!base.caps.includes('deck_via')) return { ...base, cannot: 'needs an update' };
    if (secure) return s.https ? { ...base, url: `wss://${s.https.name}:${s.https.port}` } : { ...base, cannot: 'no secure address' };
    if (s.http) return { ...base, url: `ws://${s.http}` };
    return s.https ? { ...base, url: `wss://${s.https.name}:${s.https.port}` } : { ...base, cannot: 'no address known' };
  });
}

// Versions as numbers: "0.4.10" is after "0.4.9".
export function olderThan(a, b) {
  const n = (v) => String(v || '').split(/[.+-]/).map(x => parseInt(x, 10) || 0);
  const [x, y] = [n(a), n(b)];
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) < (y[i] || 0);
  }
  return false;
}

// Which station to work through. `list`: the candidates, each with `ms`
// (its measured latency, or null: it did not answer).
//   answered   the stations that answered, in the order they are offered:
//              the pick first, then by latency
//   silent     the ones that did not, and the ones that cannot be used
//   pick       the fastest that is current enough: it says `deck_via` and is
//              not on an older version than the newest that answered.
//              Within 10 ms of the fastest, one that is always on is
//              preferred; then the faster; then by name.
//   behind     nothing current answered, so the pick is an older station
// `except`: never picked or offered (the one that just stopped answering).
// `keep`: the station already being counted down to. It stays the pick
// while it answers and is as current as any, so that two stations a
// millisecond apart do not take turns and start the count again for ever.
export function choose(list, { except = null, keep = null } = {}) {
  const usable = list.filter(c => c.station !== except);
  const answered = usable.filter(c => !c.cannot && Number.isFinite(c.ms));
  const silent = usable.filter(c => c.cannot || !Number.isFinite(c.ms));
  const newest = answered.reduce((n, c) => (olderThan(n, c.version) ? c.version : n), '0');
  const current = (c) => (c.isHome || c.caps.includes('deck_via')) && !olderThan(c.version, newest);
  const pool = answered.some(current) ? answered.filter(current) : answered;
  const best = pool.reduce((m, c) => Math.min(m, c.ms), Infinity);
  const rank = (a, b) => (b.alwaysOn - a.alwaysOn) || (a.ms - b.ms) || a.station.localeCompare(b.station);
  const pick = pool.find(c => c.station === keep) || pool.filter(c => c.ms - best < SAME_MS).sort(rank)[0] || null;
  const rest = answered.filter(c => c !== pick).sort((a, b) => (current(b) - current(a)) || (a.ms - b.ms) || a.station.localeCompare(b.station));
  return { pick, behind: !!pick && !current(pick), answered: pick ? [pick, ...rest] : rest, silent: silent.sort((a, b) => a.station.localeCompare(b.station)) };
}

// One step of the countdown to taking the pick: while the owner is
// writing it stands still.
export function countdown(left, dt, { writing, stopped }) {
  return writing || stopped ? left : Math.max(0, left - dt);
}
export const writing = (now, lastKeyAt) => now - (lastKeyAt || 0) < WRITING_FOR;

// What a row of the picker says after the station's name.
export function rowWords(c) {
  if (c.cannot) return c.cannot;
  if (!Number.isFinite(c.ms)) return 'not answering';
  return `${Math.max(1, Math.round(c.ms))} ms${c.alwaysOn ? ' · always on' : ''}`;
}
export const median3 = (list) => (list.length ? list.slice().sort((a, b) => a - b)[list.length >> 1] : null);

// Ask every candidate that has an address how far away it is: a socket to
// `/api/ping`, which says who answers and then sends back what it is sent.
// Three round trips, the middle one kept; all at the same time; `within`
// for all of it. A station that answers under another name is not that
// station (its address was given to another machine).
export function measure(list, { within = MEASURE_WITHIN, make = (url) => new WebSocket(url) } = {}) {
  const one = (c) => new Promise((resolve) => {
    if (!c.url) return resolve({ ...c, ms: null });
    let ws, times = [], sent = 0, hello = null, over = false;
    const done = (ms) => { if (over) return; over = true; clearTimeout(limit); try { ws.close(); } catch (e) { /* gone */ } resolve({ ...c, ms, version: hello?.version || c.version, caps: hello?.caps || c.caps }); };
    const limit = setTimeout(() => done(times.length ? median3(times) : null), within);
    try { ws = make(c.url + '/api/ping'); } catch (e) { return done(null); }
    const ask = () => { sent = performance.now(); ws.send('p' + times.length); };
    ws.onmessage = (ev) => {
      if (typeof ev.data !== 'string') return;
      if (!hello) {
        try { hello = JSON.parse(ev.data); } catch (e) { return done(null); }
        if (!hello || hello.station !== c.station) return done(null);
        hello = { version: text(hello.version, 32), caps: Array.isArray(hello.caps) ? hello.caps.filter(x => typeof x === 'string').slice(0, 64) : [] };
        return ask();
      }
      times.push(performance.now() - sent);
      if (times.length >= 3) done(median3(times)); else ask();
    };
    ws.onerror = () => done(times.length ? median3(times) : null);
    ws.onclose = () => done(times.length ? median3(times) : null);
  });
  return Promise.all(list.map(one));
}

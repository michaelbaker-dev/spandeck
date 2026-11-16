// Carrying on through another station, the secure deck, and the saved app
// (DECISIONS.md, "HTTPS on every station, the app saved on the device, and
// carrying on through another station", 17 Nov 2026).
//
// The owner: "If spark2 goes down while I am using the mobile app, does it
// switch over to another spandeck automatically? ... How should it pick,
// lowest latency one from where I am?" A page cannot look for machines on
// a network, and does not need to: the deck knows every station of the
// fleet, and this device keeps that list (roster.js). When the station it
// works through stops answering, the stations that answer are offered,
// fastest first, and the fastest is taken after a few seconds.
//
// No station is the master: "home" below is only where this page was
// loaded from.

import { h, toast, dialog, field, ago } from './ui.js';
import * as R from './roster.js';

const SECURE = location.protocol === 'https:';
const LOCAL = /^(127\.|\[::1\]|localhost)/.test(location.host);
const TS_DNS = 'https://login.tailscale.com/admin/dns';
const token = () => { try { return localStorage.getItem('spandeck.token') || ''; } catch (e) { return ''; } };
// Only a browser's own token is taken by another station.
const mine = () => token().startsWith('dev.');

export function initFailover(env) {
  const { api } = env;
  const F = {
    roster: R.load(),
    home: null,
    results: null, measuring: false, measuredAt: 0,
    left: R.TAKE_AFTER, pickName: null, stopped: false, tickAt: 0,
    lastKeyAt: 0,
    homeBack: false, homeAsked: 0,
    draftUntil: 0, behind: false,
    none: null, refusedBy: new Set(),
    retired: null,
    shell: null,
  };
  F.home = F.roster?.home || null;
  const key = (e) => { if (e.target?.closest?.('textarea, input, [contenteditable], .pane')) F.lastKeyAt = Date.now(); };
  addEventListener('keydown', key, true);
  addEventListener('input', key, true);

  const cands = () => R.candidates(F.roster, { secure: SECURE, host: location.host, home: F.home })
    .map(c => (F.refusedBy.has(c.station) ? { ...c, url: undefined, cannot: 'did not accept this browser' } : c));
  // The station this page is working through now, by name.
  const through = () => (api.target ? api.target.station : F.home || api.station);
  // May this page carry on through another station at all? Its own
  // station allows it (`deck_via`), the token is a browser's, and the list
  // has another station.
  const homeCaps = () => F.roster?.stations.find(s => s.station === F.home)?.caps || [];
  const can = () => mine() && !!F.home && homeCaps().includes('deck_via') && cands().some(c => !c.isHome);

  // ---- the list, kept from every look at the fleet

  function keep(fleet) {
    if (!api.connected || !Array.isArray(fleet?.stations) || !fleet.stations.length) return;
    if (!api.target) F.home = api.station;
    F.roster = R.fromFleet(F.roster, fleet.stations, Date.now(), { home: F.home, local: LOCAL });
    F.roster.last = api.station;
    // The latency of the station in use is this connection's own.
    const me = F.roster.stations.find(s => s.station === api.station);
    if (me && Number.isFinite(api.rtt())) me.ms = Math.round(api.rtt());
    R.save(F.roster);
    // The page's own station no longer serves the secure deck (its
    // switch): the saved copy would open a deck that is no longer there.
    const homeNow = fleet.stations.find(s => s.station === F.home);
    if (SECURE && !LOCAL && api.target && homeNow?.state === 'online' && homeNow.https && homeNow.https.on === false && homeNow.https.state === 'off' && !F.retired) {
      F.retired = homeNow.addr || '';
      unsave();
    }
  }

  async function measureNow(list = cands()) {
    if (F.measuring) return F.results;
    F.measuring = true;
    try {
      const got = await R.measure(list);
      F.results = got;
      F.measuredAt = Date.now();
      // What was measured is kept with the list.
      if (F.roster) {
        for (const r of got) { const s = F.roster.stations.find(x => x.station === r.station); if (s && Number.isFinite(r.ms)) s.ms = Math.round(r.ms); }
        R.save(F.roster);
      }
      return got;
    } finally { F.measuring = false; }
  }

  const targetOf = (c) => (c.isHome ? null : { station: c.station, url: c.url });
  function go(c, { silent = false } = {}) {
    const from = api.station;
    F.results = null;
    F.left = R.TAKE_AFTER;
    F.stopped = false;
    F.pickName = null;
    F.none = null;
    F.homeBack = false;
    F.homeAsked = Date.now();
    if (env.hasDraft()) F.draftUntil = Date.now() + 15000;
    return api.switchTo(targetOf(c)).then((ok) => {
      // (Moving to another station is said by the line under the top bar.)
      if (ok && !silent && c.isHome && from && from !== c.station) toast(`Back on ${c.station}.`);
      return ok;
    });
  }

  api.onEvent((ev) => {
    if (ev.kind !== 'target.refused') return;
    // It no longer takes this browser (it left the fleet, or the browser
    // was signed out there): not offered again, and the page's own station
    // is tried as before.
    F.refusedBy.add(ev.data.station);
    if (F.roster) { F.roster.stations = F.roster.stations.filter(s => s.station !== ev.data.station); R.save(F.roster); }
    toast(`${ev.data.station} did not accept this browser, so this deck cannot work through it.`, 'error');
    api.switchTo(null);
  });

  // ---- the bar, while the station being worked through does not answer

  let barKey = '';
  // Fills `bar`; `l`: the link's state (api.link()). Returns false when
  // this page cannot carry on elsewhere (the bar is then the old one).
  function drawBar(bar, l, spanWords) {
    if (!can()) { barKey = ''; return false; }
    const now = Date.now(), dt = F.tickAt ? Math.min(1000, now - F.tickAt) : 0;
    F.tickAt = now;
    // The last station that answered this page.
    const silentOne = api.station || F.home;
    if (!F.measuring && now - F.measuredAt >= (F.results ? 5000 : 0)) measureNow().then(() => env.linkTick());
    const ch = F.results ? R.choose(F.results, { except: silentOne, keep: F.pickName }) : null;
    const pick = ch?.pick || null;
    if ((pick?.station || null) !== F.pickName) { F.pickName = pick?.station || null; F.left = R.TAKE_AFTER; }
    const writing = R.writing(now, F.lastKeyAt);
    if (pick) F.left = R.countdown(F.left, dt, { writing, stopped: F.stopped });
    F.behind = !!ch?.behind;
    const state = !ch ? 'measuring' : !pick ? 'none' : F.stopped ? 'stopped' : writing ? 'writing' : 'counting';
    const rows = ch ? [...ch.answered, ...ch.silent] : [];
    const k = JSON.stringify([silentOne, state, rows.map(c => [c.station, R.rowWords(c)]), F.pickName, env.hasDraft(), F.behind]);
    if (k !== barKey || !bar.firstChild) {
      barKey = k;
      bar.classList.add('fo-bar');
      bar.replaceChildren(
        h('div', { class: 'grow' }, h('b', { class: 'link-said' }, `${silentOne} is not answering`), h('span', { class: 'muted link-for' }), h('span', { class: 'muted' }, ' Your agents keep running there.')),
        h('div', { class: 'fo-list', role: 'list', 'aria-label': 'The stations of your fleet' }, ...rows.map(c => (!c.cannot && Number.isFinite(c.ms)
          ? h('button', { class: 'fo-row', role: 'listitem', dataset: { station: c.station }, title: `Work through ${c.station} now`, onclick: () => go(c) }, h('b', {}, c.station), ` · ${R.rowWords(c)}`)
          : h('span', { class: 'fo-row off', role: 'listitem', dataset: { station: c.station } }, h('b', {}, c.station), ` · ${R.rowWords(c)}`)))),
        h('div', { class: 'fo-next' },
          h('span', { class: 'fo-count' }),
          state === 'counting' || state === 'writing' ? h('button', { class: 'mini fo-stop', onclick: () => { F.stopped = true; env.linkTick(); } }, 'Stop') : null,
          state === 'stopped' ? h('button', { class: 'mini', onclick: () => { F.stopped = false; F.left = R.TAKE_AFTER; env.linkTick(); } }, 'Count again') : null));
    }
    const forEl = bar.querySelector('.link-for'), forWords = l.wall ? ` for ${spanWords(l.wall)}.` : '.';
    if (forEl.textContent !== forWords) forEl.textContent = forWords;
    const draft = env.hasDraft() ? ' Your draft is kept.' : '';
    const older = F.behind && pick ? ` (${pick.station} runs an older spandeck: some things may be missing there.)` : '';
    const words = state === 'measuring' ? 'Looking for the stations that answer…'
      : state === 'none' ? 'No other station answers yet. This page keeps trying.'
      : state === 'stopped' ? `Not continuing by itself. Tap a station to work through it.${draft}`
      : state === 'writing' ? `Waiting while you write, then continuing on ${pick.station}.${draft}${older}`
      : `Continuing on ${pick.station} in ${Math.max(1, Math.ceil(F.left / 1000))} s.${draft}${older}`;
    const count = bar.querySelector('.fo-count');
    if (count.textContent !== words) count.textContent = words;
    if (state === 'counting' && F.left <= 0) go(pick);
    return true;
  }
  // The link is good again: the next gap starts its own countdown.
  function calm(bar) {
    if (!barKey) return;
    barKey = '';
    bar.classList.remove('fo-bar');
    F.results = null; F.measuredAt = 0; F.left = R.TAKE_AFTER; F.stopped = false; F.pickName = null; F.tickAt = 0;
  }

  // ---- one line while the page works through another station

  let viaKey = '', viaBox = null;
  function viaBar() {
    if (F.retired !== null) {
      return h('div', { class: 'link-bar via-bar', role: 'status' }, h('div', { class: 'grow' },
        h('b', {}, `${F.home} no longer serves the secure deck.`), ' ',
        h('span', { class: 'muted' }, `The copy saved on this device was removed. The deck is at http://${F.retired} now.`)));
    }
    if (!api.target || !api.connected) { viaKey = ''; viaBox = null; return null; }
    const now = Date.now();
    // Is the page's own station back? Asked every five seconds.
    if (now - F.homeAsked >= 5000) {
      F.homeAsked = now;
      const homeC = cands().find(c => c.isHome);
      if (homeC) R.measure([homeC]).then(([r]) => { const back = Number.isFinite(r.ms); if (back !== F.homeBack) { F.homeBack = back; env.update(); } });
    }
    const draft = now < F.draftUntil && env.hasDraft();
    const k = [api.station, F.home, F.homeBack, draft, F.behind].join('|');
    if (k === viaKey && viaBox) return viaBox;
    viaKey = k;
    viaBox = h('div', { class: 'link-bar via-bar', role: 'status' },
      h('div', { class: 'grow' }, h('b', {}, `This deck is now working through ${api.station}.`), ' ',
        h('span', { class: 'muted' }, F.homeBack ? `${F.home} answers again.` : `${F.home} is not answering; your agents keep running there.`,
          draft ? ' Your draft is kept.' : '',
          F.behind ? ` ${api.station} runs an older spandeck: some things may be missing.` : '')),
      F.homeBack ? h('button', { class: 'fo-back', onclick: () => go({ isHome: true, station: F.home }) }, `Go back to ${F.home}`) : null);
    return viaBox;
  }

  // ---- on opening

  // The page's own station first, since requests to it are the simplest;
  // if it does not answer within the bound, the best of the others, with
  // nothing asked. If none answers, the screen says so.
  let again = null;
  async function boot() {
    const first = api.connect();
    if (!can()) return first;
    const got = await measureNow();
    if (api.connected || api.authError) return first;
    if (Number.isFinite(got.find(c => c.isHome)?.ms)) return first;
    const ch = R.choose(got, { except: F.home });
    if (ch.pick) { F.behind = ch.behind; return go(ch.pick, { silent: true }); }
    F.none = { at: Date.now() };
    env.render();
    clearInterval(again);
    again = setInterval(retry, 5000);
    return first;
  }
  async function retry() {
    if (api.connected || !F.none) { clearInterval(again); again = null; if (F.none) { F.none = null; env.render(); } return; }
    F.none.asking = true;
    const got = await measureNow();
    if (!F.none) return;
    F.none.asking = false;
    F.none.at = Date.now();
    const ch = R.choose(got, { except: F.home });
    // The page's own station is being tried all along (api.js).
    if (ch.pick && !Number.isFinite(got.find(c => c.isHome)?.ms)) { clearInterval(again); again = null; F.behind = ch.behind; go(ch.pick, { silent: true }).then(() => env.render()); } else env.render();
  }
  const none = () => !!F.none && !api.connected && !api.authError;
  function noneScreen() {
    const rows = (F.results || cands()).slice().sort((a, b) => a.station.localeCompare(b.station));
    return h('main', { class: 'login fo-none' }, h('h1', {}, 'spandeck'),
      h('p', {}, h('b', {}, 'No station of your fleet is answering.')),
      h('div', { class: 'fo-none-list', role: 'list' }, ...rows.map(c => h('div', { class: 'fo-none-row', role: 'listitem' }, h('b', {}, c.station),
        h('span', { class: 'muted' }, c.cannot ? c.cannot : c.seen ? `last seen ${ago(c.seen)}` : 'not seen from this device yet')))),
      h('p', { class: 'muted' }, 'Is Tailscale connected on this device? Open the Tailscale app, check that it is on, and come back: this page keeps trying by itself.'),
      h('p', {}, h('button', { class: 'primary', disabled: !!F.none?.asking, onclick: retry }, F.none?.asking ? 'Trying…' : 'Try again')),
      shellHeld() ? h('p', { class: 'muted small' }, 'This is the copy of the deck saved on this device. ', h('button', { class: 'mini', onclick: () => unsave().then(() => location.reload()) }, 'Remove the saved copy')) : null);
  }

  // ---- the copy saved on the device (sw.js)

  const shellHeld = () => !!navigator.serviceWorker?.controller;
  async function unsave() {
    try { for (const r of await navigator.serviceWorker.getRegistrations()) await r.unregister(); } catch (e) { /* none */ }
    try { for (const k of await caches.keys()) if (k.startsWith('spandeck-shell-')) await caches.delete(k); } catch (e) { /* none */ }
  }
  // A new build of the deck: fetched whole by the browser, then taken at
  // once when nothing is being written, or when the owner says.
  let shellBox = null;
  function shell() {
    if (!('serviceWorker' in navigator)) return;
    navigator.serviceWorker.register('sw.js', { updateViaCache: 'none' }).then((reg) => {
      F.shell = reg;
      const waiting = () => { if (reg.waiting && navigator.serviceWorker.controller) { F.shellReady = true; if (!R.writing(Date.now(), F.lastKeyAt) && !env.hasDraft() && F.shellWanted) takeShell(); else env.update(); } };
      reg.addEventListener('updatefound', () => { const w = reg.installing; if (w) w.addEventListener('statechange', () => { if (w.state === 'installed') waiting(); }); });
      waiting();
    }).catch(() => {});
    let reloaded = false;
    navigator.serviceWorker.addEventListener('controllerchange', () => { if (F.taking && !reloaded) { reloaded = true; location.reload(); } });
    // The page's own station runs another build than this page.
    api.onBuild = () => {
      if (!shellHeld()) return location.reload();
      F.shellWanted = true;
      F.shell?.update().catch(() => {});
    };
  }
  function takeShell() {
    const w = F.shell?.waiting;
    if (!w) return location.reload();
    F.taking = true;
    w.postMessage('take');
  }
  function shellBar() {
    if (!F.shellReady) { shellBox = null; return null; }
    shellBox ||= h('div', { class: 'vault-prompt shell-bar', role: 'status' },
      h('div', { class: 'grow' }, h('b', {}, 'A new version of the deck is ready.'), ' ', h('span', { class: 'muted' }, 'It is used the next time the deck is opened, or now.')),
      h('button', { class: 'primary', onclick: takeShell }, 'Reload'));
    return shellBox;
  }

  // ---- the secure deck

  let secureLater = false, secureBox = null, secureKey = '';
  try { secureLater = localStorage.getItem('spandeck.secure.later') === '1'; } catch (e) { /* asked again */ }
  // On a plain-HTTP page at a Tailscale address: one line that offers the
  // secure deck, or the one tap that turns it on.
  function secureBar() {
    const me = env.fleet().stations.find(s => s.self);
    const st = me?.https;
    if (SECURE || LOCAL || secureLater || api.target || !st || !env.full()) return null;
    const offer = st.on && st.name ? 'open' : st.state === 'needs_operator' ? 'turn-on' : null;
    if (!offer) return null;
    const k = `${offer}|${me.station}|${st.name}:${st.port}`;
    if (k === secureKey && secureBox) return secureBox;
    secureKey = k;
    const later = h('button', { onclick: () => { secureLater = true; try { localStorage.setItem('spandeck.secure.later', '1'); } catch (e) { /* this page only */ } env.render(); } }, 'Not now');
    secureBox = offer === 'open'
      ? h('div', { class: 'vault-prompt secure-bar', role: 'region', 'aria-label': 'The secure deck' },
        h('div', { class: 'grow' }, h('b', {}, `${me.station} also serves this deck securely (HTTPS).`), ' ',
          h('span', { class: 'muted' }, 'From there it can be saved to your phone, and it opens even while this station is off.')),
        h('button', { class: 'primary', onclick: () => env.hopTo(`https://${st.name}:${st.port}`, `${me.station}'s secure deck`) }, 'Open the secure deck'), later)
      : h('div', { class: 'vault-prompt secure-bar', role: 'region', 'aria-label': 'Secure access' },
        h('div', { class: 'grow' }, h('b', {}, `Secure access (HTTPS) is not on for ${me.station} yet.`), ' ',
          h('span', { class: 'muted' }, 'With it the deck can be saved to your phone and opens while a station is off.')),
        h('button', { class: 'primary', onclick: () => httpsDialog(me) }, 'Turn on…'), later);
    return secureBox;
  }

  // "Secure access (HTTPS) on <station>": how it stands, and the one tap.
  // The password goes only to the station it is for: over this page's own
  // connection when that is the station, else over a connection made to
  // that station for the one call; never through another station.
  function httpsDialog(s) {
    dialog(`Secure access (HTTPS) on ${s.station}`, (close) => {
      const body = h('div', { class: 'https-dialog' });
      const here = s.station === api.station;
      const c = cands().find(x => x.station === s.station);
      const reach = here ? 'here' : (mine() && c?.url && !c.isHome ? 'direct' : null);
      const call = (method, params) => (here ? api.call(method, params) : api.direct(c.url, method, params));
      const draw = (st, note) => {
        const user = st.user || 'your user';
        const kids = [];
        const days = Number.isFinite(st.days_left) ? st.days_left : null;
        if (st.on) {
          kids.push(h('p', {}, h('b', {}, 'On. '), `The deck is served at https://${st.name}:${st.port}`, days === null ? '.' : days < 0 ? '. Its certificate has run out.' : `. Its certificate is good for ${days} more days and is renewed by itself.`));
          if (st.why) kids.push(h('p', { class: 'warn-text' }, st.why));
          kids.push(h('p', { class: 'muted' }, 'The plain address keeps working. The name of this machine and of your tailnet is in the public list of certificates; the machine is still reached only from your tailnet.'));
        } else if (st.state === 'needs_operator') {
          kids.push(h('p', {}, `spandeck asks ${s.station} to let your user (${user}) manage Tailscale there, so that it can fetch the machine's certificate. This needs your password on ${s.station} once; it is used for that one command and not kept.`));
          kids.push(h('p', { class: 'muted' }, `After this, programs running as ${user} on ${s.station}, your agents included, can change Tailscale's settings on that machine without a password. Undo it here, or with "sudo tailscale set --operator=" there. Turning HTTPS on also puts the name of this machine and of your tailnet in the public list of certificates.`));
        } else if (st.state === 'not_enabled') {
          kids.push(h('p', {}, 'HTTPS certificates are not switched on for your tailnet. In Tailscale\'s admin console, under DNS, turn on "HTTPS Certificates"; then come back.'));
          kids.push(h('p', {}, h('a', { href: TS_DNS, target: '_blank', rel: 'noopener noreferrer' }, 'Open Tailscale\'s DNS settings')));
        } else if (st.state === 'off') {
          kids.push(h('p', {}, `Off: ${st.why || 'turned off on this station'}.`));
        } else {
          kids.push(h('p', {}, `Not on${st.why ? `: ${st.why}` : ''}. Everything works over the plain address meanwhile.`));
        }
        if (note) kids.push(h('p', { class: note.bad ? 'warn-text https-said' : 'https-said' }, note.text));
        const wantsPassword = st.state === 'needs_operator' || (st.on && st.os === 'linux');
        if (wantsPassword && !reach) {
          kids.push(h('p', { class: 'muted' }, `Your password goes only to the station it is for, never through another one, and this page cannot reach ${s.station} directly. Open ${s.station}'s own Control Deck and choose this there.`));
          kids.push(h('div', { class: 'row end' }, h('button', { onclick: close }, 'Close'), h('button', { class: 'primary', onclick: () => { close(); env.hopOpen(s); } }, `Open ${s.station}'s own Control Deck`)));
        } else if (st.state === 'needs_operator') {
          const pw = h('input', { type: 'password', autocomplete: 'off', autocapitalize: 'off', spellcheck: false, 'aria-label': `Your password on ${s.station}` });
          const onBtn = h('button', { class: 'primary' }, 'Turn on');
          const send = async () => {
            const password = pw.value;
            if (!password) return pw.focus();
            pw.value = '';
            onBtn.disabled = true;
            onBtn.textContent = 'Asking…';
            try {
              await call('tailnet.operator', { password });
              draw({ ...st, state: 'starting' }, { text: `Done. ${s.station} is asking Tailscale for its certificate; the first one can take a minute.` });
              watch();
            } catch (e) { draw(st, { bad: true, text: e.message }); }
          };
          onBtn.onclick = send;
          pw.addEventListener('keydown', (e) => { if (e.key === 'Enter') send(); });
          kids.push(field(`Your password on ${s.station}`, pw, 'Sent to that station only, used once, and not kept.'));
          kids.push(h('div', { class: 'row end' }, h('button', { onclick: close }, 'Cancel'), onBtn));
          setTimeout(() => pw.focus(), 30);
        } else {
          const again = h('button', { onclick: () => call('https.refresh', {}).then(() => { draw(st, { text: 'Asked again.' }); watch(); }).catch(e => draw(st, { bad: true, text: e.message })) }, 'Check again');
          kids.push(h('div', { class: 'row end' },
            st.on && !SECURE && here && !LOCAL ? h('button', { class: 'primary', onclick: () => { close(); env.hopTo(`https://${st.name}:${st.port}`, `${s.station}'s secure deck`); } }, 'Open the secure deck') : null,
            st.state !== 'off' && (here || reach) ? again : null,
            h('button', { onclick: close }, 'Close')));
        }
        body.replaceChildren(...kids);
      };
      // What the station says now, asked of it a few times after a change.
      let watching = 0;
      const look = () => (here ? api.call('station.info', {}) : api.on(s.station, 'station.info', {})).then(i => ({ ...i.https, os: i.os }));
      const watch = () => { clearInterval(watching); let n = 0; watching = setInterval(() => { if (!body.isConnected || ++n > 40) return clearInterval(watching); look().then((st) => { if (st.on || (st.state !== 'starting' && n > 3)) { clearInterval(watching); draw(st); env.refresh(); } }).catch(() => {}); }, 3000); };
      draw({ ...(s.https || {}), os: s.os });
      look().then(st => draw(st)).catch(() => {});
      return body;
    });
  }

  return { F, keep, drawBar, calm, viaBar, boot, none, noneScreen, shell, shellBar, secureBar, httpsDialog, can, through, measureNow, unsave };
}

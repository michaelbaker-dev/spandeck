// The copy of the Control Deck saved on this device (DECISIONS.md, "The app
// saved on the device"): the page, its scripts and styles, the terminal and
// the built-in fonts and icons, so that the deck opens while its station is
// off and can then work through another station.
//
// This worker is one build of the deck. The station fills in the two marks
// below when it serves this file, so a station on another build serves
// another worker; that one downloads its whole shell into a cache of its
// own before it replaces this one, and no page ever runs files of two
// builds.
//
// What is kept is exactly the list below. Never an answer of the API, what
// a terminal shows, an uploaded font or anything that carries a token: a
// request that is not a GET of a listed file at this origin is passed
// over, and nothing is added while the page runs.

const BUILD = '__BUILD__';
const FILES = "__FILES__";
const PREFIX = 'spandeck-shell-';
const CACHE = PREFIX + BUILD;

self.addEventListener('install', (e) => {
  // No build: the station was told to take the saved copy away
  // (`[flags] app_shell = false`). This worker replaces the one that kept it.
  if (!BUILD) return self.skipWaiting();
  e.waitUntil((async () => {
    // Whole, or not at all: one file that cannot be had fails the install,
    // and the worker (and cache) in use stays. Each file is read to its
    // end before anything is kept: an answer whose body is not read holds
    // its connection, and a browser has only six to a station.
    const got = await Promise.all(FILES.map(async (f) => {
      const res = await fetch(new Request(f, { cache: 'reload' }));
      if (!res.ok) throw new Error(`${f}: ${res.status}`);
      const body = await res.blob();
      return [f, new Response(body, { status: res.status, statusText: res.statusText, headers: res.headers })];
    }));
    const cache = await caches.open(CACHE);
    await Promise.all(got.map(([f, res]) => cache.put(f, res)));
  })());
});

self.addEventListener('activate', (e) => {
  e.waitUntil((async () => {
    for (const k of await caches.keys()) if (k.startsWith(PREFIX) && k !== CACHE) await caches.delete(k);
    if (!BUILD) { await self.registration.unregister(); return; }
    await self.clients.claim();
  })());
});

// The page says when the owner wants the new build now.
self.addEventListener('message', (e) => { if (e.data === 'take') self.skipWaiting(); });

self.addEventListener('fetch', (e) => {
  const req = e.request;
  if (!BUILD || req.method !== 'GET') return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin || url.pathname.startsWith('/api/')) return;
  const path = url.pathname === '/' ? 'index.html' : url.pathname.slice(1);
  if (!FILES.includes(path)) return;
  // From this build's cache; a file that is not there (the cache was
  // cleared by the browser) is asked of the station as before.
  e.respondWith(caches.open(CACHE).then(c => c.match(path)).then(hit => hit || fetch(req)));
});

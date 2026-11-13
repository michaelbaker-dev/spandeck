// Minimal service worker: makes the Control Deck installable. Nothing is
// cached, because every screen is live state from the station.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));
self.addEventListener('fetch', () => {});

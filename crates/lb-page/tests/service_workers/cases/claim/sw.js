//PRELUDE
self.addEventListener('install', function (e) { slog('install'); var p = self.skipWaiting(); slog('skipWaiting ' + Object.prototype.toString.call(p)); e.waitUntil(p.then(function (v) { slog('skipWaiting resolved ' + v) })); });
self.addEventListener('activate', function (e) {
  slog('activate');
  e.waitUntil(self.clients.claim().then(function (v) { slog('claimed ' + v) }));
});
self.addEventListener('fetch', function (e) {
  slog('fetch ' + rel(e.request.url) + ' ' + e.request.mode + ' client=' + (e.clientId ? 'id' : '""') + ' resulting=' + (e.resultingClientId ? 'id' : '""'));
  if (e.request.url.endsWith('data.txt')) e.respondWith(new Response('from sw'));
});

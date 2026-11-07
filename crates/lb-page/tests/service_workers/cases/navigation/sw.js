//PRELUDE
self.addEventListener('install', function (e) {
  e.waitUntil(caches.open('v1').then(function (c) { return c.add('./') }).then(function () { slog('cached page') }));
});
self.addEventListener('fetch', function (e) {
  slog('fetch ' + rel(e.request.url) + ' ' + e.request.mode + ' ' + e.request.destination + ' resulting=' + (e.resultingClientId ? 'id' : '""') + ' client=' + (e.clientId ? 'id' : '""'));
  if (e.request.mode !== 'navigate') return;
  e.respondWith(caches.match(e.request).then(function (r) {
    if (!r) return fetch(e.request);
    return r.text().then(function (t) {
      return new Response(t.replace('<!--SW-->', '<script>log("served by sw")</' + 'script>'), { headers: { 'Content-Type': 'text/html' } });
    });
  }));
});

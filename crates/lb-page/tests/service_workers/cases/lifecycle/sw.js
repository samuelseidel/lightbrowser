//PRELUDE
slog('eval scope=' + rel(self.registration.scope) + ' ' + typeof self.clients + ' state=' + (self.serviceWorker && self.serviceWorker.state) + ' installing=' + !!self.registration.installing + ' secure=' + self.isSecureContext);
self.addEventListener('install', function (e) {
  slog('install ' + e.constructor.name + ' ' + (self.registration.installing === self.serviceWorker) + ' ' + self.serviceWorker.state);
  e.waitUntil(new Promise(function (r) { setTimeout(r, 50) }).then(function () { slog('install waited') }));
});
self.addEventListener('activate', function (e) {
  slog('activate ' + e.constructor.name + ' ' + self.serviceWorker.state + ' active=' + (self.registration.active === self.serviceWorker) + ' waiting=' + !!self.registration.waiting);
});
self.addEventListener('fetch', function (e) {
  slog('fetch ' + rel(e.request.url) + ' ' + e.request.mode + ' ' + e.request.destination + ' ' + e.constructor.name);
});

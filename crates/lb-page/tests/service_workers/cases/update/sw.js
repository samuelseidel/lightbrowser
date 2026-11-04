//PRELUDE
var V = '{{v}}';
slog('eval v' + V);
self.addEventListener('install', function () { slog('install v' + V) });
self.addEventListener('activate', function () { slog('activate v' + V) });
self.addEventListener('message', function (e) { if (e.data === 'skip') self.skipWaiting().then(function () { slog('skipped v' + V) }) });
self.addEventListener('fetch', function (e) { slog('fetch v' + V + ' ' + rel(e.request.url) + ' ' + e.request.mode) });

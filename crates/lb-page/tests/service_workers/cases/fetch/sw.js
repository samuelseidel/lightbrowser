//PRELUDE
self.addEventListener('fetch', function (e) {
  var r = e.request, path = rel(r.url).replace('/fetch/', '');
  slog('fetch ' + path + ' ' + r.method + ' ' + r.mode + ' ' + r.credentials + ' ' + r.destination + ' ' + r.redirect + ' ' + r.cache + ' client=' + (e.clientId ? 'id' : '""') + ' resulting=' + (e.resultingClientId ? 'id' : '""') + ' ' + Object.prototype.toString.call(e) + ' preload=' + typeof e.preloadResponse + ' handled=' + typeof e.handled);
  if (path === 'synth.txt') e.respondWith(new Response('synth', { status: 201, statusText: 'Made', headers: { 'X-A': '1', 'Content-Type': 'text/plain' } }));
  else if (path === 'net.txt') e.respondWith(fetch(r));
  else if (path === 'later.txt') e.respondWith(new Promise(function (res) { setTimeout(function () { res(new Response('later')) }, 20) }));
  else if (path === 'reject.txt') e.respondWith(Promise.reject(new Error('nope')));
  else if (path === 'json') e.respondWith(Response.json({ a: 1 }));
  else if (path === 'redirect') e.respondWith(Response.redirect('data.txt'));
  else if (path === 'error') e.respondWith(Response.error());
  else if (path.startsWith('echo.txt')) e.respondWith(r.text().then(function (t) { return new Response(r.method + ' ' + t + ' ' + r.headers.get('x-b')) }));
  else if (path === 'gen.js') e.respondWith(new Response('log("gen.js ran")', { headers: { 'Content-Type': 'text/javascript' } }));
  else if (path === 'frame.html') e.respondWith(new Response('<!doctype html><p>sw frame</p><script>parent.log("frame controlled " + !!navigator.serviceWorker.controller + " " + location.pathname)</' + 'script>', { headers: { 'Content-Type': 'text/html' } }));
});

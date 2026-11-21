//PRELUDE
function d(c) { return rel(c.url) + ' ' + c.type + ' ' + c.frameType + ' ' + Object.prototype.toString.call(c) + ' ' + c.visibilityState + ' focused=' + c.focused + ' idlen=' + c.id.length }
self.addEventListener('message', function (e) {
  if (e.data !== 'clients') return;
  var out = [], port = e.ports[0], src = e.source;
  e.waitUntil(clients.matchAll().then(function (cs) {
    out.push('matchAll ' + cs.length); cs.forEach(function (c) { out.push('  ' + d(c)) });
    return clients.matchAll({ includeUncontrolled: true, type: 'all' });
  }).then(function (cs) {
    out.push('matchAll all ' + cs.length);
    return clients.matchAll({ type: 'worker' });
  }).then(function (cs) {
    out.push('matchAll worker ' + cs.length);
    return clients.get(src.id);
  }).then(function (c) {
    out.push('get ' + d(c) + ' same=' + (c === src));
    return clients.get('nope');
  }).then(function (c) {
    out.push('get nope ' + c);
    return clients.claim();
  }).then(function (v) {
    out.push('claim ' + v + ' openWindow=' + typeof clients.openWindow + ' ' + Object.prototype.toString.call(clients));
    port.postMessage(out);
  }));
});

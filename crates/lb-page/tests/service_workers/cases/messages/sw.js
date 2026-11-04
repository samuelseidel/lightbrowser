//PRELUDE
self.addEventListener('message', function (e) {
  if (e.data === 'log') return;
  var s = e.source;
  slog('message ' + e.constructor.name + ' ' + (e.data && e.data.a !== undefined ? 'a=' + e.data.a + ' d=' + (e.data.d instanceof Date) + ' m=' + e.data.m.get(1) : JSON.stringify(e.data)) + ' origin=' + rel(e.origin) + ' lastEventId="' + e.lastEventId + '" ports=' + e.ports.length + ' wait=' + typeof e.waitUntil);
  slog('source ' + Object.prototype.toString.call(s) + ' url=' + rel(s.url) + ' type=' + s.type + ' frameType=' + s.frameType + ' id=' + typeof s.id + ' visibility=' + s.visibilityState + ' focused=' + s.focused);
  if (e.ports.length) {
    e.ports[0].postMessage('pong via port');
  } else {
    s.postMessage({ reply: e.data.a });
  }
});

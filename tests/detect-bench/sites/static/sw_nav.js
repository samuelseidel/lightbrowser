// Service worker of the `sw_navigator` probe (collector.js): runs the
// page's navValues source (sent with the message) and answers on the
// message's port.
self.addEventListener('message', (e) => {
  const port = e.ports[0];
  if (!port || typeof e.data !== 'string') return;
  e.waitUntil(Promise.resolve()
    .then(() => (0, eval)('(' + e.data + ')')())
    .then((v) => port.postMessage(v), (err) => port.postMessage({ error: String(err && err.message || err) })));
});

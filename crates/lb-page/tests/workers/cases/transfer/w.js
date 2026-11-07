onmessage = function (e) {
  if (!e.data || !e.data.port) return;
  var u = new Uint8Array(e.data.buf);
  var p = e.data.port;
  p.postMessage('buf ' + u.length + ' ' + u[0] + ' ' + Object.prototype.toString.call(p) + ' eports ' + e.ports.length + ' same ' + (e.ports[0] === p));
};

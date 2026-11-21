var got = [];
onmessage = function (e) {
  var d = e.data;
  if (typeof d === 'string') {
    got.push(d);
    postMessage({got: got, origin: e.origin, source: String(e.source), ports: e.ports.length, target: e.target === self, name: self.name});
    return;
  }
  got.push([d.n, d.d instanceof Date && d.d.getTime(), d.m instanceof Map && d.m.get('k'), d.s instanceof Set && d.s.size, String(d.r), d.nested[1][1], 'u' in d, typeof d.big].join(':'));
  postMessage('first ' + got[0]);
};

var n = 0;
onconnect = function (e) {
  n++;
  var p = e.ports[0];
  p.postMessage('n=' + n + ' name=' + self.name + ' ' + e.constructor.name + ' data [' + e.data + '] ports ' + e.ports.length + ' src ' + (e.source === p) + ' tag ' + Object.prototype.toString.call(self));
};

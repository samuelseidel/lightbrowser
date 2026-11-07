var out = [];
fetch('echo-cookie').then(function (r) { out.push('fetch ' + r.status + ' ' + r.headers.get('content-type')); return r.json(); }).then(function (j) {
  out.push('cookie ' + j.cookie);
  var x = new XMLHttpRequest();
  x.open('GET', 'data.txt');
  x.onload = function () {
    out.push('xhr ' + x.status + ' ' + x.responseText.trim() + ' ' + typeof x.responseXML);
    fetch('missing.txt').then(function (r) { out.push('missing ' + r.status); postMessage(out); });
  };
  x.send();
});

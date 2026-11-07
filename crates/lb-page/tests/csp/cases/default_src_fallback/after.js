var cs = getComputedStyle(document.getElementById('d'));
log('color ' + cs.color + ' bg ' + cs.backgroundColor);
a('fetch cross', function () { return fetch('http://localhost:' + location.port + '/default_src_fallback/x.txt').then(function (r) { return r.status; }); })
.then(function () { done(); });

var X = 'http://localhost:' + location.port + '/connect_src/';
a('fetch cross', function () { return fetch(X + 'a.txt').then(function (r) { return r.status; }); })
.then(function () { return a('fetch redirect', function () { return fetch('redirect?to=' + encodeURIComponent(X + 'b.txt')).then(function (r) { return r.status; }); }); })
.then(function () { return new Promise(function (res) { var x = new XMLHttpRequest(); x.open('GET', X + 'c.txt'); x.onerror = function (e) { log('xhr error ' + x.readyState + ' ' + x.status); res(); }; x.onload = function () { log('xhr load'); res(); }; x.send(); }); })
.then(function () { t('sync xhr', function () { var x = new XMLHttpRequest(); x.open('GET', X + 'd.txt', false); x.send(); return x.status; }); })
.then(function () { t('beacon', function () { return navigator.sendBeacon(X + 'e.txt', 'x'); }); })
.then(function () { t('websocket', function () { var ws = new WebSocket('ws://localhost:' + location.port + '/connect_src/ws'); ws.onerror = function () { log('ws error ' + ws.readyState); }; ws.onclose = function (e) { log('ws close ' + e.code); }; return ws.readyState; }); })
.then(function () { done(500); });

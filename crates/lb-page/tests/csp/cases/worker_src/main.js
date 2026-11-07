t('worker', function () { var w = new Worker('w.js'); w.onerror = function (e) { log('worker error event'); }; w.onmessage = function (e) { log('worker said ' + e.data); }; return 'made'; });
t('shared', function () { var w = new SharedWorker('w.js'); w.onerror = function (e) { log('shared error event'); }; return 'made'; });
t('blob worker', function () { var u = URL.createObjectURL(new Blob(["postMessage('blob hi')"], { type: 'text/javascript' })); var w = new Worker(u); w.onmessage = function (e) { log('blob worker said ' + e.data); }; return 'made'; });
a('sw', function () { return navigator.serviceWorker.register('sw.js').then(function () { return 'registered'; }); })
.then(function () { done(800); });

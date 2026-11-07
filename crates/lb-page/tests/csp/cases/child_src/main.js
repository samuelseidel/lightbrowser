var f = document.createElement('iframe'); f.onload = function () { log('frame load'); }; f.src = 'child.html'; document.body.appendChild(f);
t('worker', function () { var w = new Worker('w.js'); w.onerror = function () { log('worker error'); }; w.onmessage = function (e) { log('worker said ' + e.data); }; return 'made'; });
done(800);

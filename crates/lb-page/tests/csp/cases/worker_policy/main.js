var w = new Worker('w.js'); w.onmessage = function (e) { log('w: ' + e.data); };
var u = URL.createObjectURL(new Blob(["var r; try { r = eval('1 + 1'); } catch (e) { r = e.name + ': ' + e.message; } postMessage(r); self.addEventListener('securitypolicyviolation', function (e) { postMessage('violation ' + e.blockedURI + ' ' + e.effectiveDirective); });"], { type: 'text/javascript' }));
var b = new Worker(u); b.onmessage = function (e) { log('blob: ' + e.data); };
done(800);

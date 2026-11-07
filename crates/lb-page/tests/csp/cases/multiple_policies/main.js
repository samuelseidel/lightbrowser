var s = document.createElement('script'); s.src = 'http://localhost:' + location.port + '/multiple_policies/x.js';
s.onerror = function () { log('script error'); }; s.onload = function () { log('script load'); }; document.body.appendChild(s);
var i = new Image(); i.onerror = function () { log('img error'); }; i.onload = function () { log('img load'); }; i.src = 'http://localhost:' + location.port + '/multiple_policies/pixel.png';
done(500);

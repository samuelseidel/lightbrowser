self.addEventListener('securitypolicyviolation', function (e) { postMessage('violation ' + e.blockedURI + ' ' + e.effectiveDirective + ' ' + e.disposition); });
var r; try { r = eval('1 + 1'); } catch (e) { r = e.name + ': ' + e.message; } postMessage(r);
fetch('x.txt').then(function () { postMessage('fetch ok'); }, function (e) { postMessage('fetch ' + e.name + ': ' + e.message); });

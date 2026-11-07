var f = document.createElement('iframe'); f.id = 'f';
f.onload = function () { var s; try { s = f.contentWindow.location.href; } catch (e) { s = 'threw ' + e.name; } log('frame load ' + s); };
f.onerror = function () { log('frame error'); };
f.src = 'http://localhost:' + location.port + '/frame_src/child.html'; document.body.appendChild(f);
var g = document.createElement('iframe'); g.id = 'g'; g.onload = function () { log('same frame load ' + g.contentDocument.title); }; g.src = 'child.html'; document.body.appendChild(g);
done(800);

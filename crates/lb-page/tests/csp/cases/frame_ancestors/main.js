function frame(id, src) { var f = document.createElement('iframe'); f.id = id;
  f.onload = function () { var s; try { s = f.contentDocument ? f.contentDocument.title : 'no document'; } catch (e) { s = 'threw ' + e.name; } log(id + ' load ' + s); };
  f.src = src; document.body.appendChild(f); }
frame('none', 'none.html');
frame('self', 'self.html');
frame('cross', 'http://localhost:' + location.port + '/frame_ancestors/self.html');
frame('listed', 'http://localhost:' + location.port + '/frame_ancestors/listed.html');
done(800);

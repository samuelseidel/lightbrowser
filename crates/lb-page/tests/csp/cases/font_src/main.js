a('cross', function () { return new FontFace('f', 'url(http://localhost:' + location.port + '/font_src/f.woff2)').load().then(function (f) { return f.status; }); })
.then(function () { done(); });

function img(n, src) { var i = new Image(); i.onload = function () { log(n + ' load'); }; i.onerror = function () { log(n + ' error'); }; i.src = src; }
img('same', 'pixel.png');
img('cross', 'http://localhost:' + location.port + '/img_src/pixel.png');
img('data', 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNg+M/AAAAFAAIBrX1uDwAAAABJRU5ErkJggg==');
img('blob', URL.createObjectURL(new Blob(['x'], { type: 'image/png' })));
done(600);

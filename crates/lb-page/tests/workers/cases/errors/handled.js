self.addEventListener('error', function (e) { e.preventDefault(); postMessage('handled ' + e.message + ' ' + (e.error instanceof TypeError)); });
null.x;

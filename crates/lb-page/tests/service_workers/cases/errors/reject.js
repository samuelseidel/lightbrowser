self.addEventListener('install', function (e) { e.waitUntil(Promise.reject(new Error('no'))) });

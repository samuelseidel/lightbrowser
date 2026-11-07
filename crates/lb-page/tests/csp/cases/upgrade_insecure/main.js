var i = new Image(); i.onload = function () { log('img load'); }; i.onerror = function () { log('img error'); }; i.src = 'http://127.0.0.1:' + location.port + '/upgrade_insecure/pixel.png';
done(1000);

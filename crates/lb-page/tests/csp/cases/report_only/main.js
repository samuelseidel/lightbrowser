t('eval', function () { return eval('1 + 1'); });
var i = new Image(); i.onload = function () { log('img load ' + i.naturalWidth); }; i.src = 'pixel.png';
done(500);

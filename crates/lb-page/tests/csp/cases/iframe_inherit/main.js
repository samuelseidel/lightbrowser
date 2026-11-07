var f = document.createElement('iframe'); document.body.appendChild(f);
var d = f.contentDocument;
d.addEventListener('securitypolicyviolation', function (e) { log('blank violation ' + vdesc(e)); });
t('blank eval', function () { return f.contentWindow.eval('1'); });
t('blank inline', function () { var s = d.createElement('script'); s.textContent = "parent.log('blank inline ran')"; d.body.appendChild(s); return 'appended'; });
done(500);

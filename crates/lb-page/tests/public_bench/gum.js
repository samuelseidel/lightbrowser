
const log = (s) => { document.getElementById('o').textContent += s + '\n'; };
const ok = () => log('cb ok'); const er = (n) => (e) => log(n + ' cb err ' + e.name + ': ' + e.message);
const t = (name, f) => { try { const r = f(); log(name + ' ret ' + r); } catch (e) { log(name + ' throw ' + e.name + ': ' + e.message); } };
const g = navigator.getUserMedia;
log('len ' + g.length + ' ' + navigator.webkitGetUserMedia.length + ' same ' + (g === navigator.webkitGetUserMedia) + ' name ' + g.name + '/' + navigator.webkitGetUserMedia.name);
t('empty', () => navigator.getUserMedia({}, ok, er('empty')));
t('nocons', () => navigator.getUserMedia(undefined, ok, er('nocons')));
t('num', () => navigator.getUserMedia(5, ok, er('num')));
t('badok', () => navigator.getUserMedia({ audio: true }, 1, er('badok')));
t('baderr', () => navigator.getUserMedia({ audio: true }, ok, null));
t('two', () => navigator.getUserMedia({ audio: true }, ok));
t('wk2', () => navigator.webkitGetUserMedia({}, ok, 'x'));
t('this', () => g.call({}, { audio: true }, ok, er('this')));

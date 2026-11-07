function c(id) { return getComputedStyle(document.getElementById(id)).color; }
window.onload = function () {
log('a ' + c('a') + ' b ' + c('b') + ' c ' + c('c') + ' d ' + c('d'));
var e = document.getElementById('c'); e.style.color = 'rgb(20, 21, 22)'; log('cssom ' + c('c'));
e.setAttribute('style', 'color: rgb(30, 31, 32)'); log('setAttribute ' + c('c'));
log('sheets ' + document.styleSheets.length);
done();
};

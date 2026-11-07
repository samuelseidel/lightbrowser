document.addEventListener('load', function (e) { log('load ' + who(e.target)); }, true);
document.addEventListener('error', function (e) { log('error ' + who(e.target)); }, true);

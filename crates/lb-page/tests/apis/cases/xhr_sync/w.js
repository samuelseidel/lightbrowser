var o = [];
var x = new XMLHttpRequest(); x.responseType = 'json'; x.open('GET', 'a.json', false); x.send();
o.push(x.status + ' ' + JSON.stringify(x.response));
var y = new XMLHttpRequest(); y.responseType = 'arraybuffer'; y.open('GET', 'a.txt', false); y.send();
o.push(y.response.byteLength);
var z = new XMLHttpRequest(); z.timeout = 5000; z.open('GET', 'a.txt', false); z.send();
o.push(z.status + ' ' + z.responseText);
try { var n = new XMLHttpRequest(); n.open('GET', 'http://127.0.0.1:1/x', false); n.send(); } catch (e) { o.push(e.name + ': ' + e.message); }
postMessage(o.join(' | '));

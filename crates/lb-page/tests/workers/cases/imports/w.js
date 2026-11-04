var out = [];
self.order = [];
var r = importScripts('a.js', 'sub/b.js');
out.push('ret ' + r + ' order ' + order.join() + ' A ' + A + ' B ' + B);
importScripts();
try { importScripts('missing.js'); } catch (e) { out.push('missing ' + e.name + ' ' + e.code + ' ' + e.message.replace(/:\d+\//, ':PORT/')); }
try { importScripts('bad.js'); } catch (e) { out.push('bad ' + e.name + ' ' + (e instanceof DOMException) + ' ' + (e instanceof SyntaxError) + ' ' + e.message); }
try { importScripts('thrower.js'); } catch (e) { out.push('thrower ' + e.name + ' ' + e.message); }
out.push('length ' + importScripts.length);
postMessage(out);

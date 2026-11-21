var LOG=[];function slog(s){LOG.push(String(s))}var O=self.location.origin;function rel(u){return String(u).split(O).join('')}
self.addEventListener('message',function(e){if(e.data==='log'){e.ports[0].postMessage(LOG);LOG=[]}});

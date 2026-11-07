// Answers sw-probe.txt with its own view: clients and caches. Clients are
// the top-level pages and the innermost frames of the chains (the middle
// frames never use navigator.serviceWorker; LightBrowser lists a window
// only once it does or its partition has a registration, ADR 0022).
function leaf(c){var m=/[?&]c=([^&]*)/.exec(c.url);return !m||m[1].length==1}
self.addEventListener('install',function(e){e.waitUntil(self.skipWaiting())});
self.addEventListener('activate',function(e){e.waitUntil(self.clients.claim())});
self.addEventListener('fetch',function(e){if(e.request.url.indexOf('sw-probe.txt')<0)return;
 e.respondWith(Promise.all([self.clients.matchAll({includeUncontrolled:true}),caches.keys(),indexedDB.databases()]).then(function(r){
  return new Response('sw clients='+r[0].filter(leaf).map(function(c){return new URL(c.url).search.replace(/&t=t\d+/,'')||c.url.split('/').pop()}).sort().join(',')+' caches='+r[1].sort().join(',')+' dbs='+r[2].map(function(d){return d.name}).sort().join(','))}))});

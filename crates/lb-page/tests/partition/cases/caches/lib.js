async function keys(){return JSON.stringify((await caches.keys()).sort())}
async function put(name,v){var c=await caches.open(name);await c.put('/caches/x.txt',new Response(v));return name}
async function get(){var r=await caches.match('/caches/x.txt');return r?await r.text():'none'}

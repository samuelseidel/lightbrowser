function req(r){return new Promise(function(res,rej){r.onsuccess=function(){res(r.result)};r.onerror=function(e){e.preventDefault();rej(r.error)}})}
async function dbs(){return JSON.stringify((await indexedDB.databases()).map(function(d){return d.name+':'+d.version}).sort())}
// Open `name` (creating store 's' with one record `v` when new); report
// whether it was new and what it holds.
async function mk(name,v){var r=indexedDB.open(name),fresh=false;r.onupgradeneeded=function(){fresh=true;r.result.createObjectStore('s').put(v,'k')};var db=await req(r);
 var got=await req(db.transaction('s').objectStore('s').get('k'));db.close();return (fresh?'new ':'old ')+name+'='+got}
function workerRun(){return new Promise(function(res){var w=new Worker('/idb/worker.js');w.onmessage=function(e){res(e.data)};w.onerror=function(e){res('worker error '+e.message)}})}

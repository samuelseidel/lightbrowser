// The shared worker counts its connections and keeps a log of its users.
function connect(x){return new Promise(function(r){var w=new SharedWorker('/shared_worker/counter.js','c');w.port.onmessage=function(e){r(e.data)};w.port.postMessage(x)})}
function opendb(n){return new Promise(function(r){var q=indexedDB.open(n);q.onsuccess=function(){q.result.close();r()}})}

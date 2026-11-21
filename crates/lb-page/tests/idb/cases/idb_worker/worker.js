var out = [];
function req(r){return new Promise(function(res,rej){r.onsuccess=function(){res(r.result)};r.onerror=function(e){e.preventDefault();rej(r.error)}})}
(async function(){
  out.push(typeof indexedDB+' '+String(indexedDB)+' '+typeof IDBKeyRange+' '+typeof IDBVersionChangeEvent);
  var db = await req(indexedDB.open('wk'));
  out.push('version '+db.version);
  var v = await req(db.transaction('s').objectStore('s').get('p'));
  out.push('read '+v.from+' '+(v.d instanceof Date)+' '+v.d.getTime());
  db.close();
  var r = indexedDB.open('wk', 2);
  r.onblocked = function(){ out.push('blocked'); };
  r.onupgradeneeded = function(e){ out.push('upgrade '+e.oldVersion+'->'+e.newVersion); r.transaction.objectStore('s').put('worker', 'w'); };
  var db2 = await req(r);
  out.push('done '+db2.version);
  db2.close();
  out.forEach(function(m){ postMessage(m); });
  postMessage('end');
})().catch(function(e){ postMessage('failed '+e); postMessage('end'); });

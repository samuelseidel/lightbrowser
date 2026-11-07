var n=0,users=[];onconnect=function(e){var p=e.ports[0];n++;p.onmessage=function(m){users.push(m.data);
 indexedDB.databases().then(function(l){p.postMessage('n='+n+' users='+users.join(',')+' dbs='+l.map(function(d){return d.name}).sort().join(','))})}}

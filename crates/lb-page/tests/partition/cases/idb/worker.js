indexedDB.databases().then(function(l){postMessage(JSON.stringify(l.map(function(d){return d.name+':'+d.version}).sort()))},function(e){postMessage('rejected '+e.name)});

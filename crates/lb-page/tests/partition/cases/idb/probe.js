async function main(x){var s=document.createElement('script');s.src='/idb/lib.js';await new Promise(function(r){s.onload=r;document.head.appendChild(s)});
 log(await dbs());log(await mk('p',x));log(await mk('q'+x,x));log(await dbs());log('worker '+await workerRun())}

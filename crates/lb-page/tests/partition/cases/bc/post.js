async function main(x){new BroadcastChannel('x').postMessage('from '+x);
 var w=new Worker('/bc/worker.js?'+x+'-w');await new Promise(function(r){w.onmessage=r});log('posted')}

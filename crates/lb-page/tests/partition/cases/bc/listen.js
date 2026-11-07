async function main(x){var got=[];var ch=new BroadcastChannel('x');ch.onmessage=function(e){got.push(e.data)};
 await new Promise(function(r){setTimeout(r,1500)});log(x+' got '+JSON.stringify(got.sort()))}

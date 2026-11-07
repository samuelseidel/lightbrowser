async function main(u){var out=[];
 try{var r=await fetch(u);out.push('fetch '+r.status+' '+await r.text())}catch(e){out.push('fetch '+err(e))}
 out.push(await new Promise(function(res){var x=new XMLHttpRequest();x.open('GET',u);x.onload=function(){res('xhr '+x.status+' '+x.responseText)};x.onerror=function(){res('xhr error')};try{x.send()}catch(e){res('xhr threw '+err(e))}}));
 out.push(await new Promise(function(res){try{var w=new Worker(u);w.onmessage=function(e){res('worker '+e.data)};w.onerror=function(e){e.preventDefault();res('worker error')};setTimeout(function(){res('worker silent')},1000)}catch(e){res('worker threw '+err(e))}}));
 log(out.join(' | ').replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g,'UUID'))}

async function main(x){var s=document.createElement('script');s.src='/caches/lib.js';await new Promise(function(r){s.onload=r;document.head.appendChild(s)});
 log(await keys()+' '+await get());await put('c'+x,x);log(await keys()+' '+await get())}

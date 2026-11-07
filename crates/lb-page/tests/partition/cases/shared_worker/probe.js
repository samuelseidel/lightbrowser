async function main(x){var s=document.createElement('script');s.src='/shared_worker/lib.js';await new Promise(function(r){s.onload=r;document.head.appendChild(s)});
 if(x=='ABA')await opendb('aba');log(await connect(x))}

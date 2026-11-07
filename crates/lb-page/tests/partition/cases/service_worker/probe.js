async function main(x){var s=document.createElement('script');s.src='/service_worker/lib.js';await new Promise(function(r){s.onload=r;document.head.appendChild(s)});
 log(ctl()+' '+await regs()+' '+await probe())}

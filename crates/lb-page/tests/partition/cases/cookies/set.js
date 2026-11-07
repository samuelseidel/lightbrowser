async function main(x){var s=document.createElement('script');s.src='/cookies/lib.js';await new Promise(function(r){s.onload=r;document.head.appendChild(s)});
 var p=x.toLowerCase();
 document.cookie=p+'_lax=1';document.cookie=p+'_none=1; SameSite=None; Secure';document.cookie=p+'_part=1; SameSite=None; Secure; Partitioned';
 document.cookie=p+'_part_nosec=1; SameSite=None; Partitioned';
 await setHttp(p+'h_none=1; Path=/; SameSite=None; Secure');await setHttp(p+'h_part=1; Path=/; SameSite=None; Secure; Partitioned');await setHttp(p+'h_lax=1; Path=/');
 log('set')}

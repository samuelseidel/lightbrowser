async function main(){var s=document.createElement('script');s.src='/cookies/lib.js';await new Promise(function(r){s.onload=r;document.head.appendChild(s)});
 log('doc '+sorted(document.cookie));log('http '+await echo())}

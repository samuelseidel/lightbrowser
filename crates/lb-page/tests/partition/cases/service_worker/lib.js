async function regs(){return JSON.stringify((await navigator.serviceWorker.getRegistrations()).map(function(r){return r.scope.replace(location.origin,'')}).sort())}
async function probe(){var r=await fetch('/service_worker/sw-probe.txt');return r.status==404?'network':await r.text()}
function ctl(){return navigator.serviceWorker.controller?'controlled':'uncontrolled'}
async function reg(){await navigator.serviceWorker.register('/service_worker/sw.js',{scope:'/service_worker/'});await navigator.serviceWorker.ready;return 'registered'}

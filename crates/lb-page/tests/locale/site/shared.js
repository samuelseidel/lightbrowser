importScripts('probe.js');
onconnect = e => { const p = e.ports[0]; p.onmessage = () => p.postMessage(probe()); };

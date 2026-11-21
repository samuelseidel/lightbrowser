var ws = new WebSocket('ws://' + location.host + '/ws_api/ws?deflate=1');
var o = [];
ws.onopen = () => { o.push('open ' + ws.extensions); ws.send('from worker'); };
ws.onmessage = e => { o.push('message ' + e.data + ' ' + e.origin.replace(/:\d+/, ':P')); ws.close(); };
ws.onclose = e => { o.push('close ' + e.code + ' ' + e.wasClean); postMessage(o.join(' | ')); };

importScripts('/worker_dedicated/helpers.js');
var scope=self;
// The page asks; the worker answers with what its own timeline holds after a
// fetch, an importScripts and a mark.
onmessage=function(e){
  var port=e.source;
  var out=scopeLines('service');
  out.push('start entries '+performance.getEntries().map(function(x){return x.entryType+':'+short(x.name)}).join());
  var X='http://127.0.0.1:'+(new URLSearchParams(location.search).get('x'));
  performance.mark('sm');
  fetch('data.txt?size=5').then(function(r){return r.text()})
    .then(function(){return fetch(X+'/data.txt?size=9&cors=1&tao=1').then(function(r){return r.text()})})
    .then(function(){return new Promise(function(r){setTimeout(r,50)})})
    .then(function(){
      out.push('resources '+performance.getEntriesByType('resource').map(res).join('\n  '));
      out.push('marks '+performance.getEntriesByType('mark').map(function(x){return x.name}).join());
      out.push('keys '+keys(performance.getEntriesByType('resource')[0]));
      port.postMessage(out.join('\n'));
    });
};

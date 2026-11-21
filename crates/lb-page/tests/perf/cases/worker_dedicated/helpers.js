// Shared by the worker cases (importScripts in a worker).
function short(u){u=String(u);return u.indexOf(location.origin)===0?'O'+u.slice(location.origin.length):u.replace(/^http:\/\/127\.0\.0\.1:\d+/,'X')}
function keys(e){var k=[];for(var p in e)k.push(p);return k.join(',')}
var SEQ=['startTime','fetchStart','domainLookupStart','domainLookupEnd','connectStart','connectEnd','requestStart','responseStart','responseEnd'];
function chain(e){var last=-1,ok=true;for(var i=0;i<SEQ.length;i++){var v=e[SEQ[i]];if(v===0&&i>0)continue;if(v<last)ok=false;last=v}return ok}
function durOk(e){return Math.abs(e.duration-(e.responseEnd-e.startTime))<1e-6}
function res(e){return [e.entryType,short(e.name),e.initiatorType,'proto='+e.nextHopProtocol,e.renderBlockingStatus,e.responseStatus,'ct='+e.contentType,
 'enc='+e.encodedBodySize+' dec='+e.decodedBodySize+' xfer-enc='+(e.transferSize-e.encodedBodySize),'chain='+chain(e),'dur='+durOk(e)].join(' ')}
function scopeLines(label){
  var out=[];
  out.push(label+' scope '+Object.prototype.toString.call(self));
  out.push(label+' types '+JSON.stringify(PerformanceObserver.supportedEntryTypes));
  out.push(label+' now '+(performance.now()<5000)+' timeOrigin '+(performance.timeOrigin>1e12)+' timing '+typeof performance.timing+' nav '+typeof PerformanceNavigationTiming+' res '+typeof PerformanceResourceTiming+' obs '+typeof PerformanceObserver+' mark '+typeof PerformanceMark);
  return out;
}
// The worker's own run: entries before and after importScripts, fetches,
// marks and an observer. `X` is the other origin (query ?x=PORT of the
// script URL).
function runWorker(label,post){
  var X='http://127.0.0.1:'+(new URLSearchParams(location.search).get('x'));
  var out=scopeLines(label);
  out.push('start entries '+performance.getEntries().map(function(e){return e.entryType+':'+short(e.name)}).join());
  importScripts('dep.js');
  out.push('after importScripts '+performance.getEntriesByType('resource').map(res).join(' ; '));
  var seen=[];
  var po=new PerformanceObserver(function(l){l.getEntries().forEach(function(e){seen.push(e.entryType+':'+short(e.name))})});
  po.observe({entryTypes:['resource','mark']});
  performance.mark('wm');
  var d=function(r){return r.text()};
  return fetch('data.txt?size=5').then(d)
    .then(function(){return fetch('data.txt?size=7&gz=1').then(d)})
    .then(function(){return fetch(X+'/data.txt?size=9&cors=1&tao=1').then(d)})
    .then(function(){return fetch(X+'/data.txt?size=9&cors=1').then(d)})
    .then(function(){return new Promise(function(r){setTimeout(r,50)})})
    .then(function(){
      out.push('resources '+performance.getEntriesByType('resource').map(res).join('\n  '));
      out.push('marks '+performance.getEntriesByType('mark').map(function(e){return e.name+':'+e.entryType}).join());
      out.push('all '+performance.getEntries().map(function(e){return e.entryType}).join());
      out.push('keys '+keys(performance.getEntriesByType('resource')[0]));
      out.push('observed '+seen.join());
      performance.clearResourceTimings();
      out.push('cleared '+performance.getEntriesByType('resource').length);
      post(out.join('\n'));
    });
}

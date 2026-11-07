importScripts('/worker_dedicated/helpers.js');
var done=null;
onconnect=function(e){
  var port=e.ports[0];
  if(!done)done=new Promise(function(r){runWorker('shared',r)});
  done.then(function(m){port.postMessage(m)});
};

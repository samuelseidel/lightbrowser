postMessage('before');
close();
postMessage('after close');
setTimeout(function () { postMessage('timer after close'); }, 0);

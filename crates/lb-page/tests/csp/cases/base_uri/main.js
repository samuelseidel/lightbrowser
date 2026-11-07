log('baseURI ' + document.baseURI);
var b = document.createElement('base'); b.href = 'sub/'; document.head.appendChild(b); log('same-origin base ' + document.baseURI);
done();

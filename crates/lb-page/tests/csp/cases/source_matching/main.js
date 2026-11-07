var P = location.port, S = '/source_matching/';
var urls = ['http://127.0.0.1:' + P + S + 'dir/a.png', 'http://127.0.0.1:' + P + S + 'dir', 'http://127.0.0.1:' + P + S + 'other.png',
 'http://localhost:' + P + S + 'file.png', 'http://localhost:' + P + S + 'file.png?q=1', 'http://localhost:' + P + S + 'file2.png',
 'http://a.localhost:1/x.png', 'http://localhost:1/x.png', 'https://127.0.0.1:' + P + S + 'tls.png', 'http://127.0.0.1:1/x.png',
 'http://127.0.0.1:' + P + S + 'dir/redirect?to=' + encodeURIComponent('http://127.0.0.1:' + P + S + 'after.png'),
 'http://127.0.0.1:' + P + S + 'dir/redirect?to=' + encodeURIComponent('http://localhost:1/far.png')];
urls.forEach(function (u, k) { var i = new Image(); i.src = u; });
done(1500);

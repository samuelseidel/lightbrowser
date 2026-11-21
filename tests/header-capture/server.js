// Loopback logging server for request-header captures (ADR 0011).
//
//   node server.js OUT.jsonl H1_PORT H2_PORT CERT_DIR
//
// Binds 127.0.0.1 only. H1_PORT is clear-text HTTP/1.1 (raw header case and
// order from `rawHeaders`); H2_PORT is HTTPS HTTP/2 with the self-signed
// cert in CERT_DIR (key.pem, cert.pem). Node's http2 `rawHeaders` keep wire
// order, including `cookie` (the Python `h2` package moves it to the end).
// One JSON line per request: scenario (`page`, from the `?s=` of the page
// that started it), proto, request line, headers, body (latin1).
// `/favicon.ico` requests are not logged.
'use strict';
const http = require('http');
const http2 = require('http2');
const fs = require('fs');
const path = require('path');

const [out, h1Port, h2Port, certDir] = process.argv.slice(2);
const log = fs.createWriteStream(out, { flags: 'a' });
let scenario = '';

function formPage(q) {
  const enc = q.get('enc');
  const action = q.get('action') || '/submit';
  const auto = q.get('auto')
    ? '<script>setTimeout(() => document.getElementById("f").submit(), 50)</script>'
    : '';
  return `<!doctype html><html><body>
<form id="f" method="post" action="${action}"${enc ? ` enctype="${enc}"` : ''}>
<input name="email" value="a b@x.test"><input type="password" name="password" value="p&s=w%ö">
<input type="checkbox" name="remember" checked><textarea name="note">l1
l2</textarea>
<button id="go" name="btn" value="1">Go</button></form>
${auto}</body></html>`;
}

function handle(proto, req, res, body) {
  const u = new URL(req.url, 'http://x');
  if (u.pathname === '/favicon.ico') {
    res.statusCode = 404;
    return res.end();
  }
  scenario = u.searchParams.get('s') || scenario;
  const headers = [];
  for (let i = 0; i < req.rawHeaders.length; i += 2) {
    headers.push([req.rawHeaders[i], req.rawHeaders[i + 1]]);
  }
  log.write(JSON.stringify({
    page: scenario,
    proto,
    scheme: proto === 'h2' ? 'https' : 'http',
    request: `${req.method} ${req.url}`,
    headers,
    body: body.toString('latin1'),
  }) + '\n');
  const html = (s) => { res.setHeader('content-type', 'text/html'); res.end(s); };
  switch (u.pathname) {
    case '/form':
      res.setHeader('set-cookie', 'sid=abc; Path=/');
      return html(formPage(u.searchParams));
    case '/outer':
      res.setHeader('set-cookie', 'sid=abc; Path=/');
      return html(`<!doctype html><iframe id="fr" width="600" height="300" src="/form?${u.searchParams.get('q') || ''}"></iframe>`);
    case '/submit':
      res.statusCode = 303;
      res.setHeader('location', '/done');
      return res.end();
    case '/submit307':
      res.statusCode = 307;
      res.setHeader('location', '/submit200');
      return res.end();
    default:
      return html('<!doctype html><p>ok</p>');
  }
}

function wrap(proto) {
  return (req, res) => {
    const chunks = [];
    req.on('data', (d) => chunks.push(d));
    req.on('end', () => handle(proto, req, res, Buffer.concat(chunks)));
  };
}

http.createServer(wrap('h1')).listen(+h1Port, '127.0.0.1');
http2.createSecureServer({
  key: fs.readFileSync(path.join(certDir, 'key.pem')),
  cert: fs.readFileSync(path.join(certDir, 'cert.pem')),
}, wrap('h2')).listen(+h2Port, '127.0.0.1');
console.log('ready');

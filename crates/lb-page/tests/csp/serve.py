# Loopback server for the CSP cases: /<case>/<file> from cases/<case>/
# (a directory's index.html for /<case>/). A file's response headers are in
# <file>.headers ("Name: value" lines; "Status: 302" sets the status), so
# pages and workers get their own Content-Security-Policy headers
# ({PORT} there and in HTML, JS and CSS files is the server's port).
# /prelude.js is ../prelude.js. Every request is logged; GET
# /__log/<case> answers the ones under /<case>/ as JSON lines
# ("METHOD target [content-type body]"), in arrival order.
# /<case>/report (POST) answers 204 (report-uri). /<case>/redirect?to=URL
# answers 302 to URL. Usage: python3 serve.py PORT (0 picks one; the port
# is printed first).
import http.server, json, os, sys, socketserver, threading, urllib.parse
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.join(HERE, 'cases')
CT = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.mjs': 'text/javascript',
      '.css': 'text/css', '.png': 'image/png', '.json': 'application/json', '.txt': 'text/plain',
      '.woff2': 'font/woff2', '.wasm': 'application/wasm'}
LOG = []; LOCK = threading.Lock()
# A 1x1 PNG for image cases.
PNG = bytes.fromhex('89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c489'
                    '0000000d49444154789c6360f8cfc0000005000201ad7d6e0f0000000049454e44ae426082')
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def reply(self, status, ct, b, extra=()):
        self.send_response(status)
        if ct: self.send_header('Content-Type', ct)
        for k, v in extra: self.send_header(k, v)
        self.send_header('Content-Length', str(len(b))); self.send_header('Cache-Control', 'no-store')
        self.end_headers(); self.wfile.write(b)
    def record(self, body=b''):
        if self.path.startswith('/__log/') or self.path == '/prelude.js' or self.path == '/favicon.ico':
            return
        line = self.command + ' ' + self.path
        if self.command == 'POST':
            line += ' ' + self.headers.get('Content-Type', '') + ' ' + body.decode('utf-8', 'replace')
        with LOCK: LOG.append(line)
    def do_POST(self):
        n = int(self.headers.get('Content-Length') or 0)
        body = self.rfile.read(n) if n else b''
        self.record(body)
        return self.reply(204, None, b'')
    def do_GET(self):
        self.record()
        u = urllib.parse.urlsplit(self.path)
        if u.path.startswith('/__log/'):
            case = '/' + u.path[len('/__log/'):] + '/'
            with LOCK: lines = [l for l in LOG if l.split(' ', 2)[1].startswith(case)]
            return self.reply(200, 'application/json', json.dumps(lines).encode())
        if u.path == '/prelude.js':
            return self.reply(200, 'text/javascript', open(os.path.join(HERE, 'prelude.js'), 'rb').read())
        if u.path.endswith('/redirect'):
            to = urllib.parse.parse_qs(u.query).get('to', ['/'])[0]
            return self.reply(302, 'text/plain', b'', [('Location', to)])
        if u.path.endswith('/pixel.png'):
            return self.reply(200, 'image/png', PNG)
        p = u.path.lstrip('/')
        if p.endswith('/') or p == '': p += 'index.html'
        f = os.path.normpath(os.path.join(ROOT, p))
        if not f.startswith(ROOT) or not os.path.isfile(f):
            return self.reply(404, 'text/plain', b'not found')
        status, extra = 200, []
        if os.path.isfile(f + '.headers'):
            for line in open(f + '.headers', encoding='utf-8').read().splitlines():
                if ':' not in line: continue
                k, v = line.split(':', 1)
                v = v.strip().replace('{PORT}', str(self.server.server_address[1]))
                if k.strip().lower() == 'status': status = int(v)
                else: extra.append((k.strip(), v))
        b = open(f, 'rb').read()
        if f.endswith('.html') or f.endswith('.js') or f.endswith('.css'):
            b = b.replace(b'{PORT}', str(self.server.server_address[1]).encode())
        self.reply(status, CT.get(os.path.splitext(f)[1], 'application/octet-stream'), b, extra)
class S(socketserver.ThreadingMixIn, http.server.HTTPServer): daemon_threads = True
srv = S(('127.0.0.1', int(sys.argv[1])), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()

# Loopback server for the Performance Timeline cases (ADR 0046). Listens on
# two loopback ports (two origins): the first line printed is "PORT1 PORT2".
# /<case>/<file> comes from cases/ (<!--LOG--> in HTML becomes prelude.html).
# Query switches on any file: tao=1 adds Timing-Allow-Origin: *, cors=1 adds
# Access-Control-Allow-Origin: *, gz=1 gzip-encodes the body, status=N,
# delay=MS, size=N (body of N 'x' bytes), cache=1 (max-age=3600), st=1 adds a
# Server-Timing header, ct=TYPE. /redir?to=URL[&code=N] redirects (hops chain
# with ?to=/redir?...). Usage: python3 serve.py PORT1 PORT2 (0 picks free
# ports).
import gzip, http.server, os, socketserver, sys, threading, time, urllib.parse
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'cases')
PRE = open(os.path.join(ROOT, '..', 'prelude.html')).read().strip()
CT = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.mjs': 'text/javascript',
      '.css': 'text/css', '.json': 'application/json', '.txt': 'text/plain', '.svg': 'image/svg+xml',
      '.png': 'image/png', '.woff2': 'font/woff2'}
# 1x1 transparent PNG.
PNG = bytes.fromhex('89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4890000000d49444154789c6360000002000001e221bc330000000049454e44ae426082')


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'

    def log_message(self, *a):
        pass

    def do_POST(self):
        n = int(self.headers.get('Content-Length', '0'))
        self.rfile.read(n)
        self.do_GET()

    def do_GET(self):
        u = urllib.parse.urlsplit(self.path)
        q = urllib.parse.parse_qs(u.query)
        extra = []
        if u.path == '/redir':
            to = q['to'][0]
            code = int(q.get('code', ['302'])[0])
            b = b'moved'
            self.send_response(code)
            self.send_header('Location', to)
            if q.get('tao'):
                self.send_header('Timing-Allow-Origin', '*')
            if q.get('cors'):
                self.send_header('Access-Control-Allow-Origin', '*')
            self.send_header('Content-Length', str(len(b)))
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            self.wfile.write(b)
            return
        p = u.path.lstrip('/')
        if p.endswith('/'):
            p += 'index.html'
        f = os.path.normpath(os.path.join(ROOT, p))
        # NAME.redirect (or index.redirect for the directory): 302 to the
        # file's content, keeping the query.
        rd = os.path.splitext(f)[0] + '.redirect'
        if not os.path.isfile(f) and os.path.isfile(rd) and rd.startswith(ROOT):
            to = open(rd).read().strip() + ('?' + u.query if u.query else '')
            self.send_response(302)
            self.send_header('Location', to)
            self.send_header('Content-Length', '0')
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            return
        status = int(q.get('status', ['200'])[0])
        if q.get('size'):
            b = b'x' * int(q['size'][0])
            ctype = q.get('ct', ['text/plain'])[0]
        elif u.path.endswith('.png') and not os.path.isfile(f):
            b = PNG
            ctype = 'image/png'
        elif os.path.isfile(f) and f.startswith(ROOT):
            b = open(f, 'rb').read()
            ctype = CT.get(os.path.splitext(f)[1], 'application/octet-stream')
            if f.endswith('.html'):
                b = b.replace(b'<!--LOG-->', PRE.encode())
        else:
            b = b'not found'
            ctype = 'text/plain'
            status = 404
        if q.get('ct'):
            ctype = q['ct'][0]
        if q.get('delay'):
            time.sleep(int(q['delay'][0]) / 1000)
        if q.get('gz'):
            b = gzip.compress(b, 9, mtime=0)
            extra.append(('Content-Encoding', 'gzip'))
        if q.get('tao'):
            extra.append(('Timing-Allow-Origin', '*'))
        if q.get('cors'):
            extra.append(('Access-Control-Allow-Origin', '*'))
        if q.get('st'):
            extra.append(('Server-Timing', 'db;dur=53.2;desc="Database", cache;desc=hit'))
        self.send_response(status)
        self.send_header('Content-Type', ctype)
        for k, v in extra:
            self.send_header(k, v)
        self.send_header('Cache-Control', 'max-age=3600' if q.get('cache') else 'no-store')
        self.send_header('Content-Length', str(len(b)))
        self.end_headers()
        if self.command != 'HEAD':
            self.wfile.write(b)

    do_HEAD = do_GET
    do_PUT = do_POST


class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


a = S(('127.0.0.1', int(sys.argv[1])), H)
b = S(('127.0.0.1', int(sys.argv[2])), H)
print(a.server_address[1], b.server_address[1], flush=True)
threading.Thread(target=b.serve_forever, daemon=True).start()
a.serve_forever()

# Loopback server for the storage partitioning cases (ADR 0037). Two sites
# on one port: 127.0.0.1 (A) and localhost (B), as rebrowser's check does.
# /<case>/<file> from cases/; <!--LOG--> in HTML becomes prelude.html.
# /<case>/f.html is frame.html unless the case has its own.
# /cookie/set?v=<Set-Cookie value>[&v=...] answers with those Set-Cookie
# headers; /cookie/echo answers with the request's Cookie header (or '-').
# Usage: python3 serve.py PORT (0 picks one; the port is printed first).
# Listens on 127.0.0.1 and, when it can, ::1 on the same port (localhost).
import http.server, os, sys, socket, socketserver, threading, urllib.parse
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'cases')
PRE = open(os.path.join(ROOT, '..', 'prelude.html')).read().strip()
CT = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.json': 'application/json', '.txt': 'text/plain'}
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def reply(self, status, ct, b, extra=()):
        self.send_response(status); self.send_header('Content-Type', ct)
        for k, v in extra: self.send_header(k, v)
        self.send_header('Content-Length', str(len(b))); self.send_header('Cache-Control', 'no-store'); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        u = urllib.parse.urlsplit(self.path)
        q = urllib.parse.parse_qs(u.query)
        if u.path == '/cookie/set':
            return self.reply(200, 'text/plain', b'ok', [('Set-Cookie', v) for v in q.get('v', [])])
        if u.path == '/cookie/echo':
            return self.reply(200, 'text/plain', (self.headers.get('Cookie') or '-').encode())
        p = u.path.lstrip('/')
        if p.endswith('/'): p += 'index.html'
        f = os.path.normpath(os.path.join(ROOT, p))
        if os.path.basename(p) == 'f.html' and not os.path.isfile(f):
            f = os.path.join(ROOT, '..', 'frame.html')
        elif not f.startswith(ROOT) or not os.path.isfile(f):
            return self.reply(404, 'text/plain', b'not found')
        b = open(f, 'rb').read()
        if f.endswith('.html'): b = b.replace(b'<!--LOG-->', PRE.encode())
        self.reply(200, CT.get(os.path.splitext(f)[1], 'application/octet-stream'), b)
class S(socketserver.ThreadingMixIn, http.server.HTTPServer): daemon_threads = True
class S6(S): address_family = socket.AF_INET6
srv = S(('127.0.0.1', int(sys.argv[1])), H)
port = srv.server_address[1]
try:
    s6 = S6(('::1', port), H)
    threading.Thread(target=s6.serve_forever, daemon=True).start()
except OSError:
    pass
print(port, flush=True)
srv.serve_forever()

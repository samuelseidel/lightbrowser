# Loopback server for the IndexedDB cases:
# /<case>/<file> from cases/; <!--LOG--> in HTML becomes prelude.html.
# Usage: python3 serve.py PORT (0 picks one; the port is printed first).
import http.server, json, os, sys, time, socketserver, base64, hashlib, struct, zlib, urllib.parse
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'cases')
PRE = open(os.path.join(ROOT, '..', 'prelude.html')).read().strip()
CT = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.mjs': 'text/javascript', '.json': 'application/json', '.txt': 'text/plain'}
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def reply(self, status, ct, b, extra=()):
        self.send_response(status); self.send_header('Content-Type', ct)
        for k, v in extra: self.send_header(k, v)
        self.send_header('Content-Length', str(len(b))); self.send_header('Cache-Control', 'no-store'); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        p = self.path.split('?')[0].lstrip('/')
        if p.endswith('/'): p += 'index.html'
        f = os.path.normpath(os.path.join(ROOT, p))
        if not f.startswith(ROOT) or not os.path.isfile(f):
            return self.reply(404, 'text/plain', b'not found')
        if os.path.basename(f).startswith('slow-'): time.sleep(0.3)
        b = open(f, 'rb').read()
        if f.endswith('.html'): b = b.replace(b'<!--LOG-->', PRE.encode())
        self.reply(200, CT.get(os.path.splitext(f)[1], 'application/octet-stream'), b)
class S(socketserver.ThreadingMixIn, http.server.HTTPServer): daemon_threads = True
srv = S(('127.0.0.1', int(sys.argv[1])), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()

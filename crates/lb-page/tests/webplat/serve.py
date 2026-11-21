# Loopback server for the webplat cases (ADR 0047): /<case>/<file> from
# cases/; <!--LOG--> in HTML becomes prelude.html.
# Usage: python3 serve.py PORT (0 picks one; the port is printed first).
import http.server, os, sys, socketserver
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'cases')
PRE = open(os.path.join(ROOT, '..', 'prelude.html')).read().strip()
CT = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.json': 'application/json', '.txt': 'text/plain'}
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def do_GET(self):
        if self.path.startswith('/favicon.ico'):
            self.send_response(204); self.send_header('Content-Length', '0'); self.end_headers(); return
        p = self.path.split('?')[0].lstrip('/')
        if p.endswith('/'): p += 'index.html'
        f = os.path.normpath(os.path.join(ROOT, p))
        if not f.startswith(ROOT) or not os.path.isfile(f):
            b, st, ct = b'not found', 404, 'text/plain'
        else:
            b = open(f, 'rb').read(); st = 200
            ct = CT.get(os.path.splitext(f)[1], 'application/octet-stream')
            if f.endswith('.html'): b = b.replace(b'<!--LOG-->', PRE.encode())
        self.send_response(st); self.send_header('Content-Type', ct)
        self.send_header('Content-Length', str(len(b))); self.send_header('Cache-Control', 'no-store')
        self.end_headers(); self.wfile.write(b)
class S(socketserver.ThreadingMixIn, http.server.HTTPServer): daemon_threads = True
srv = S(('127.0.0.1', int(sys.argv[1])), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()

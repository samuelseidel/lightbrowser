# Loopback server for the stylesheet cases: /<case>/<file>; files named
# slow-* wait 300 ms, slower-* 600 ms; <!--LOG--> is replaced by the prelude.
import http.server, os, sys, time, socketserver
ROOT = sys.argv[1]; PORT = int(sys.argv[2])
PRE = open(os.path.join(ROOT, '..', 'prelude.html')).read().strip()
CT = {'.html': 'text/html; charset=utf-8', '.css': 'text/css', '.js': 'text/javascript', '.txt': 'text/plain', '.png': 'image/png', '.svg': 'image/svg+xml', '.gif': 'image/gif', '.jpg': 'image/jpeg', '.webp': 'image/webp'}
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def do_GET(self):
        p = self.path.split('?')[0].lstrip('/')
        if p.endswith('/'): p += 'index.html'
        f = os.path.join(ROOT, p)
        name = os.path.basename(f)
        if name.startswith('slower-'): time.sleep(0.6)
        elif name.startswith('slow-'): time.sleep(0.3)
        if not os.path.isfile(f):
            b = b'not found'; self.send_response(404); self.send_header('Content-Type', 'text/plain')
        else:
            b = open(f, 'rb').read()
            if f.endswith('.html'): b = b.replace(b'<!--LOG-->', PRE.encode())
            self.send_response(200); self.send_header('Content-Type', CT.get(os.path.splitext(f)[1], 'application/octet-stream'))
        self.send_header('Content-Length', str(len(b))); self.send_header('Cache-Control', 'no-store'); self.end_headers(); self.wfile.write(b)
class S(socketserver.ThreadingMixIn, http.server.HTTPServer): daemon_threads = True
S(('127.0.0.1', PORT), H).serve_forever()

# Loopback server for the time zone / locale / geolocation cases (ADR 0039):
# site/ as is, and /headers answering the request's headers as JSON (in
# order). Usage: python3 serve.py PORT (0 picks one; the port is printed first).
import http.server, json, os, sys
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'site')
CT = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript'}
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def reply(self, ct, b, status=200):
        self.send_response(status); self.send_header('Content-Type', ct)
        self.send_header('Content-Length', str(len(b))); self.send_header('Cache-Control', 'no-store'); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        p = self.path.split('?')[0]
        if p == '/headers':
            return self.reply('application/json', json.dumps([[k, v] for k, v in self.headers.items()]).encode())
        if p.endswith('/'): p += 'index.html'
        f = os.path.normpath(os.path.join(ROOT, p.lstrip('/')))
        if not f.startswith(ROOT) or not os.path.isfile(f):
            return self.reply('text/plain', b'not found', 404)
        self.reply(CT.get(os.path.splitext(f)[1], 'application/octet-stream'), open(f, 'rb').read())
class S(http.server.ThreadingHTTPServer):
    daemon_threads = True
s = S(('127.0.0.1', int(sys.argv[1])), H)
print(s.server_address[1], flush=True)
s.serve_forever()

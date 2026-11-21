# Loopback server for the service worker cases: /<case>/<file>.
# <!--LOG--> in HTML becomes prelude.html; //PRELUDE in JS becomes
# sw_prelude.js; {{v}} in JS becomes the `v` query parameter. Files named
# allowed-* send `Service-Worker-Allowed: /`; /<case>/hits answers how
# often each path of the case was requested (JSON).
import http.server, json, os, sys, time, socketserver, urllib.parse, collections, threading
ROOT = sys.argv[1]; PORT = int(sys.argv[2])
PRE = open(os.path.join(ROOT, '..', 'prelude.html')).read().strip()
SWPRE = open(os.path.join(ROOT, '..', 'sw_prelude.js')).read().strip()
CT = {'.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.json': 'application/json', '.txt': 'text/plain', '.css': 'text/css'}
HITS = collections.Counter(); LOCK = threading.Lock()
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def do_GET(self):
        u = urllib.parse.urlsplit(self.path); p = u.path.lstrip('/'); q = urllib.parse.parse_qs(u.query)
        extra = []
        if p.endswith('/hits'):
            case = p[:-len('hits')]
            with LOCK: b = json.dumps({k[len(case):]: v for k, v in sorted(HITS.items()) if k.startswith(case)}).encode()
            self.send_response(200); ct = 'application/json'
        else:
            with LOCK: HITS[p] += 1
            if p.endswith('/') or p == '': p += 'index.html'
            f = os.path.join(ROOT, p)
            if not os.path.isfile(f):
                b = b'not found'; self.send_response(404); ct = 'text/plain'
            else:
                b = open(f, 'rb').read()
                if f.endswith('.html'): b = b.replace(b'<!--LOG-->', PRE.encode())
                if f.endswith('.js'): b = b.replace(b'//PRELUDE', SWPRE.encode()).replace(b'{{v}}', q.get('v', [''])[0].encode())
                if os.path.basename(f).startswith('allowed-'): extra.append(('Service-Worker-Allowed', '/'))
                self.send_response(200); ct = CT.get(os.path.splitext(f)[1], 'application/octet-stream')
        self.send_header('Content-Type', ct)
        for k, v in extra: self.send_header(k, v)
        self.send_header('Content-Length', str(len(b))); self.send_header('Cache-Control', 'no-store'); self.end_headers(); self.wfile.write(b)
class S(socketserver.ThreadingMixIn, http.server.HTTPServer): daemon_threads = True
S(('127.0.0.1', PORT), H).serve_forever()

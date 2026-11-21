# Loopback server for the image cases: /<case>/ is cases/<case>.html with
# <!--LOG--> replaced by the prelude; /<case>/[slow-]a.png, b.png (slow-*
# wait 300 ms); anything else 404. GET /__hits/<case> answers the request
# targets seen under /<case>/ with their counts (JSON).
import http.server, json, os, sys, time, socketserver, collections, threading
ROOT = os.path.dirname(os.path.abspath(__file__)); PORT = int(sys.argv[1])
PRE = open(os.path.join(ROOT, 'prelude.html')).read().strip()
HITS = collections.Counter(); LOCK = threading.Lock()
class H(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, *a): pass
    def send(self, status, ct, b):
        self.send_response(status); self.send_header('Content-Type', ct); self.send_header('Content-Length', str(len(b)))
        self.send_header('Cache-Control', 'no-store'); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        if self.path.startswith('/__hits/'):
            case = self.path[len('/__hits/'):]
            with LOCK: h = {k.split('/', 2)[2]: v for k, v in HITS.items() if k.startswith('/' + case + '/')}
            return self.send(200, 'application/json', json.dumps(h, sort_keys=True).encode())
        with LOCK: HITS[self.path] += 1
        parts = self.path.split('?')[0].split('/')
        case, name = parts[1], parts[2] if len(parts) > 2 else ''
        if name in ('', 'index.html'):
            f = os.path.join(ROOT, 'cases', case + '.html')
            if os.path.isfile(f):
                return self.send(200, 'text/html; charset=utf-8', open(f).read().replace('<!--LOG-->', PRE).encode())
        if name.startswith('slow-'): time.sleep(0.3); name = name[5:]
        if name in ('a.png', 'b.png'):
            return self.send(200, 'image/png', open(os.path.join(ROOT, name), 'rb').read())
        self.send(404, 'text/plain', b'not found')
class S(socketserver.ThreadingMixIn, http.server.HTTPServer): daemon_threads = True
S(('127.0.0.1', PORT), H).serve_forever()

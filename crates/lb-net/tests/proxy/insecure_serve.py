#!/usr/bin/env python3
"""Loopback page server that logs request heads (one JSON line each) for
recording Chrome's headers to a non-trustworthy http: origin.

  python3 insecure_serve.py PORT

The page loads a script, a module script, a style sheet, an image, a font,
an iframe and a fetch(); Chrome reaches it as http://insecure.test:PORT/
(--host-resolver-rules) so the origin is not potentially trustworthy.
"""
import json
import socketserver
import sys
from http.server import BaseHTTPRequestHandler

PAGE = b"""<!doctype html><html><head>
<link rel=stylesheet href=/style.css>
<script src=/script.js></script>
<script type=module src=/module.js></script>
<style>@font-face{font-family:f;src:url(/font.woff2)} body{font-family:f}</style>
</head><body>x<img src=/img.png><iframe src=/frame.html></iframe>
<script>fetch('/api').then(r=>r.text()).then(()=>{document.title='done'})</script>
</body></html>"""

BODIES = {
    "/": ("text/html", PAGE),
    "/frame.html": ("text/html", b"<p>frame"),
    "/style.css": ("text/css", b"p{color:red}"),
    "/script.js": ("text/javascript", b"1"),
    "/module.js": ("text/javascript", b"export {}"),
    "/api": ("text/plain", b"ok"),
}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        print(json.dumps({"line": self.requestline, "headers": list(self.headers.items())}), flush=True)
        ctype, body = BODIES.get(self.path, ("application/octet-stream", b""))
        code = 200 if self.path in BODIES else 404
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


class S(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True
    daemon_threads = True


S(("127.0.0.1", int(sys.argv[1])), H).serve_forever()

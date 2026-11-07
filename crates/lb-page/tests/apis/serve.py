# Loopback server for the API cases (crypto, XHR, modules, WebSocket):
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
        if self.path.split('?')[0].endswith('/ws'):
            return self.websocket()
        p = self.path.split('?')[0].lstrip('/')
        if p.endswith('/'): p += 'index.html'
        f = os.path.normpath(os.path.join(ROOT, p))
        if not f.startswith(ROOT) or not os.path.isfile(f):
            return self.reply(404, 'text/plain', b'not found')
        if os.path.basename(f).startswith('slow-'): time.sleep(0.3)
        b = open(f, 'rb').read()
        if f.endswith('.html'): b = b.replace(b'<!--LOG-->', PRE.encode())
        self.reply(200, CT.get(os.path.splitext(f)[1], 'application/octet-stream'), b)
    # WebSocket (RFC 6455, permessage-deflate with ?deflate=1). Messages:
    # 'headers' answers the handshake request's header lines (JSON, in
    # order); 'close:CODE:REASON' makes the server close; 'drop' closes the
    # TCP connection without a closing handshake; 'ping' makes the server
    # ping; anything else is echoed (text as text, binary as binary).
    # ?proto=X selects subprotocol X; ?cookie=1 sets a cookie on the 101.
    def websocket(self):
        q = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
        if q.get('status'):
            return self.reply(int(q['status'][0]), 'text/plain', b'no')
        key = self.headers.get('Sec-WebSocket-Key', '')
        accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        lines = [self.requestline] + ['%s: %s' % (k, v) for k, v in self.headers.items()]
        offered = self.headers.get('Sec-WebSocket-Extensions', '')
        deflate = q.get('deflate') == ['1'] and 'permessage-deflate' in offered
        self.send_response(101, 'Switching Protocols')
        self.send_header('Upgrade', 'websocket'); self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', q['accept'][0] if q.get('accept') else accept)
        if q.get('proto'): self.send_header('Sec-WebSocket-Protocol', q['proto'][0])
        if deflate: self.send_header('Sec-WebSocket-Extensions', 'permessage-deflate')
        if q.get('cookie'): self.send_header('Set-Cookie', 'wscookie=1; Path=/')
        self.end_headers(); self.wfile.flush()
        sock = self.connection
        def recv_exact(n):
            b = b''
            while len(b) < n:
                c = sock.recv(n - len(b))
                if not c: raise EOFError
                b += c
            return b
        def send(op, payload, compress=False):
            rsv = 0
            if compress and deflate and op in (1, 2):
                c = zlib.compressobj(wbits=-15); payload = c.compress(payload) + c.flush(zlib.Z_SYNC_FLUSH)
                payload = payload[:-4]; rsv = 0x40
            h = bytes([0x80 | rsv | op])
            n = len(payload)
            if n < 126: h += bytes([n])
            elif n < 65536: h += bytes([126]) + struct.pack('>H', n)
            else: h += bytes([127]) + struct.pack('>Q', n)
            sock.sendall(h + payload)
        msg, mop, mcomp, closing = b'', 0, False, False
        try:
            while True:
                b0, b1 = recv_exact(2)
                fin, op, comp = b0 & 0x80, b0 & 0x0f, bool(b0 & 0x40)
                n = b1 & 0x7f
                if n == 126: n = struct.unpack('>H', recv_exact(2))[0]
                elif n == 127: n = struct.unpack('>Q', recv_exact(8))[0]
                mask = recv_exact(4) if b1 & 0x80 else b'\0\0\0\0'
                data = bytes(x ^ mask[i % 4] for i, x in enumerate(recv_exact(n)))
                if op == 8:
                    if not closing: send(8, data[:2])
                    break
                if op == 9: send(10, data); continue
                if op == 10: continue
                if op != 0: mop, mcomp, msg = op, comp, b''
                msg += data
                if not fin: continue
                if mcomp: msg = zlib.decompressobj(wbits=-15).decompress(msg + b'\x00\x00\xff\xff')
                if mop == 1 and msg == b'headers': send(1, json.dumps(lines).encode(), True)
                elif mop == 1 and msg.startswith(b'close:'):
                    _, code, reason = msg.decode().split(':', 2)
                    send(8, struct.pack('>H', int(code)) + reason.encode()); closing = True
                elif mop == 1 and msg == b'drop': break
                elif mop == 1 and msg == b'ping': send(9, b'p')
                elif mop == 1 and msg == b'raw': send(1, b'compressed? ' + str(mcomp).encode())
                else: send(mop, msg, True)
        except (EOFError, ConnectionError, OSError):
            pass
        self.close_connection = True
        try: sock.shutdown(2)
        except OSError: pass

class S(socketserver.ThreadingMixIn, http.server.HTTPServer): daemon_threads = True
srv = S(('127.0.0.1', int(sys.argv[1])), H)
print(srv.server_address[1], flush=True)
srv.serve_forever()

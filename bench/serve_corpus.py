#!/usr/bin/env python3
"""Serve bench/corpus on 127.0.0.1 only (plain HTTP/1.1, no caching).

Prints `listening 127.0.0.1:PORT` once ready. Port 0 picks a free port.
It is a separate process so its work never competes with the runner's GIL.
"""

import argparse
import functools
import http.server
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent / "corpus"


class Handler(http.server.SimpleHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, *a):
        pass


class Server(http.server.ThreadingHTTPServer):
    def handle_error(self, request, client_address):
        pass  # clients that hang up mid-response are normal here

    def do_GET(self):
        if self.path == "/favicon.ico":
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        super().do_GET()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=0)
    ap.add_argument("--root", default=str(ROOT))
    args = ap.parse_args()
    handler = functools.partial(Handler, directory=args.root)
    srv = Server(("127.0.0.1", args.port), handler)  # loopback only
    srv.daemon_threads = True
    print(f"listening 127.0.0.1:{srv.server_address[1]}", flush=True)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())

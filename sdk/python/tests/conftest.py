"""Shared fixtures. Unit tests need nothing; integration tests need a built binary."""

from __future__ import annotations

import socket
import subprocess
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

from lightbrowser import BinaryNotFoundError, Browser, find_binary
from lightbrowser.errors import HumanInputError

REPO = Path(__file__).resolve().parents[3]

PAGE = b"""<!doctype html>
<html><head><meta charset="utf-8"><title>SDK test page</title>
<style>
 body { margin: 0; }
 #btn { position: absolute; left: 100px; top: 120px; width: 160px; height: 48px; }
 #q   { position: absolute; left: 100px; top: 220px; width: 240px; height: 32px; }
 #tall { height: 4000px; }
 #far { position: absolute; left: 100px; top: 3000px; width: 160px; height: 48px; }
</style></head>
<body>
<button id="btn">Go</button>
<input id="q" type="text" autocomplete="off">
<div id="tall"></div>
<button id="far">Far away</button>
<script>
 window.log = [];
 for (const t of ['mousemove', 'mousedown', 'mouseup', 'click', 'wheel', 'keydown', 'keyup', 'input'])
   document.addEventListener(t, (e) => log.push({
     t, trusted: e.isTrusted, x: e.clientX, y: e.clientY, id: e.target && e.target.id, key: e.key,
   }), true);
</script>
</body></html>
"""


class _Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802
        if self.path.split("?")[0] in ("/", "/page"):
            body, ctype = PAGE, "text/html; charset=utf-8"
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args: object) -> None:
        pass


@pytest.fixture(scope="session")
def page_url():
    """A throwaway test page on 127.0.0.1."""
    srv = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    yield f"http://127.0.0.1:{srv.server_address[1]}/page"
    srv.shutdown()
    srv.server_close()


@pytest.fixture(scope="session")
def binary() -> str:
    try:
        return find_binary(family="chrome")
    except BinaryNotFoundError as e:
        pytest.skip(f"no chrome-family lightbrowser binary: {e}")


@pytest.fixture(scope="module")
def lb(binary):
    """One LightBrowser per test module."""
    with Browser(executable_path=binary) as b:
        yield b


@pytest.fixture
def page(lb, page_url):
    p = lb.new_page()
    p.goto(page_url)
    yield p
    p.context.close()


@pytest.fixture(scope="module")
def human_ready(lb):
    """xfail the human_* tests while the browser answers 'not implemented'."""
    p = lb.new_page()
    try:
        p.human_move(10, 10)
    except HumanInputError as e:
        if "not implemented" in str(e) or "not supported" in str(e) or "wasn't found" in str(e):
            pytest.xfail(f"LightBrowser.human* not available in this binary: {e}")
        raise
    finally:
        p.context.close()


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@pytest.fixture(scope="session")
def bench_url(tmp_path_factory):
    """tests/detect-bench/server.py on a free loopback port, results in a temp dir."""
    server = REPO / "tests" / "detect-bench" / "server.py"
    if not server.is_file():
        pytest.skip("tests/detect-bench not found")
    port = free_port()
    results = tmp_path_factory.mktemp("bench-results")
    proc = subprocess.Popen(
        [sys.executable, str(server), "--port", str(port), "--results-dir", str(results), "--quiet"],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    url = f"http://127.0.0.1:{port}"
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        for _ in range(50):
            try:
                opener.open(f"{url}/api/results", timeout=1).read()
                break
            except OSError:
                time.sleep(0.1)
        else:
            pytest.skip("bench server did not start")
        yield url
    finally:
        proc.terminate()
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill()

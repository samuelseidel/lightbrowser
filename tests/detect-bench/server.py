#!/usr/bin/env python3
"""LightBrowser detect-bench server.

Serves local test sites with the collector script, records server-side
signals per session and scores sessions with `scoring.py`.

Loopback only: it binds 127.0.0.1 (or ::1) and refuses anything else.
Standard library only. Optional `--tls` makes a throwaway self-signed cert
with the `openssl` CLI in a temp dir that is removed on exit.

    python3 server.py --port 8765            # http://127.0.0.1:8765/shop
    python3 server.py --port 8766 --tls      # https://localhost:8766/shop
"""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import secrets
import shutil
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib
from http import cookies
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

import scoring

HERE = Path(__file__).resolve().parent
SITES = HERE / "sites"
LOOPBACK_HOSTS = {"127.0.0.1", "::1", "localhost"}
COOKIE = "lb_sid"
SID_RE = re.compile(r"^[0-9a-f]{16}$")
MAX_BODY = 2 * 1024 * 1024
MAX_REQUESTS_PER_SESSION = 500
MAX_TRACE = 20000
MAX_HEADER_VALUE = 600
# Signed-agent headers (RFC 9421, see sigagent.py): kept in full on the first
# document and on every request that carries them.
AGENT_HEADERS = ("signature-agent", "signature-input", "signature")

# Product catalogue used by the shop and search pages.
PRODUCTS = [
    ("Trail Runner 3", "Shoes", 89.0),
    ("City Backpack 22L", "Bags", 64.5),
    ("Merino Crew Sock", "Socks", 14.0),
    ("Rain Shell Jacket", "Jackets", 149.0),
    ("Canvas Tote", "Bags", 19.9),
    ("Wool Beanie", "Hats", 24.0),
    ("Down Vest", "Jackets", 119.0),
    ("Leather Belt", "Accessories", 39.0),
    ("Linen Shirt", "Shirts", 54.0),
    ("Chino Shorts", "Shorts", 44.0),
    ("Insulated Bottle", "Accessories", 29.0),
    ("Hiking Boot Pro", "Shoes", 179.0),
]
# High-entropy client hints the bench asks for on every document (Accept-CH),
# and the one it insists on (Critical-CH: the browser retries the navigation
# once with it). Browsers only honour these on secure origins; loopback
# http counts. `ch_high_entropy` in scoring.py checks the later requests.
ACCEPT_CH = [
    "Sec-CH-UA-Platform-Version", "Sec-CH-UA-Arch", "Sec-CH-UA-Bitness", "Sec-CH-UA-Full-Version-List",
    "Sec-CH-UA-Model", "Sec-CH-UA-WoW64", "Sec-CH-UA-Form-Factors",
]
CRITICAL_CH = ["Sec-CH-UA-Platform-Version"]
PALETTE = ["#d1495b", "#edae49", "#00798c", "#30638e", "#003d5b", "#6a4c93"]


def now() -> float:
    return time.time()


STUN_MAGIC = 0x2112A442


class StunResponder:
    """A loopback-only UDP STUN server (RFC 5389): it answers Binding requests
    with the sender's address as XOR-MAPPED-ADDRESS and nothing else.

    The collector's `webrtc_candidates` probe gathers ICE candidates against
    it. It binds 127.0.0.1 or ::1 only (never a public address), counts the
    requests it answered and ignores anything that is not a well-formed
    Binding request. `requests` keeps the last 100 senders and times.
    """

    def __init__(self, host: str, port: int = 0):
        if host not in ("127.0.0.1", "::1"):
            raise ValueError(f"STUN responder binds loopback only, not {host!r}")
        family = socket.AF_INET6 if ":" in host else socket.AF_INET
        self.sock = socket.socket(family, socket.SOCK_DGRAM)
        self.sock.bind((host, port))
        self.sock.settimeout(0.2)
        self.host = host
        self.port = self.sock.getsockname()[1]
        self.answered = 0
        self.ignored = 0
        self.requests: list[dict] = []
        self.lock = threading.Lock()
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    @staticmethod
    def parse(data: bytes) -> bytes | None:
        """The transaction id of a well-formed Binding request, else None."""
        if len(data) < 20 or data[0] & 0xC0:
            return None
        mtype, length, cookie = struct.unpack(">HHI", data[:8])
        if mtype != 0x0001 or cookie != STUN_MAGIC or length % 4 or len(data) != 20 + length:
            return None
        # Attributes must be well formed TLVs that fill the message.
        i = 20
        while i < len(data):
            if i + 4 > len(data):
                return None
            alen = struct.unpack(">H", data[i + 2:i + 4])[0]
            i += 4 + alen + (-alen % 4)
        return data[8:20] if i == len(data) else None

    @staticmethod
    def response(txid: bytes, addr: tuple) -> bytes:
        """Binding success response with XOR-MAPPED-ADDRESS for `addr`."""
        port = addr[1] ^ (STUN_MAGIC >> 16)
        if ":" in addr[0]:
            raw = socket.inet_pton(socket.AF_INET6, addr[0].split("%")[0])
            key = struct.pack(">I", STUN_MAGIC) + txid
            body = struct.pack(">BBH", 0, 2, port) + bytes(a ^ b for a, b in zip(raw, key))
        else:
            raw = socket.inet_aton(addr[0])
            body = struct.pack(">BBH", 0, 1, port) + bytes(a ^ b for a, b in zip(raw, struct.pack(">I", STUN_MAGIC)))
        attr = struct.pack(">HH", 0x0020, len(body)) + body
        return struct.pack(">HHI", 0x0101, len(attr), STUN_MAGIC) + txid + attr

    def _run(self) -> None:
        while not self._stop.is_set():
            try:
                data, addr = self.sock.recvfrom(2048)
            except socket.timeout:
                continue
            except OSError:
                return
            txid = self.parse(data)
            with self.lock:
                if txid is None:
                    self.ignored += 1
                    continue
                self.answered += 1
                self.requests.append({"t": now(), "from": f"{addr[0]}:{addr[1]}"})
                del self.requests[:-100]
            try:
                self.sock.sendto(self.response(txid, addr), addr)
            except OSError:
                pass

    def start(self) -> "StunResponder":
        self._thread.start()
        return self

    def close(self) -> None:
        self._stop.set()
        if self._thread.is_alive():
            self._thread.join(1)
        self.sock.close()

    def address(self) -> str:
        return f"[{self.host}]:{self.port}" if ":" in self.host else f"{self.host}:{self.port}"

    def stats(self) -> dict:
        with self.lock:
            return {"address": self.address(), "answered": self.answered, "ignored": self.ignored,
                    "requests": list(self.requests)}


class Store:
    """In-memory sessions, mirrored to one JSON line per session on disk."""

    def __init__(self, results_dir: Path):
        self.lock = threading.Lock()
        self.sessions: dict[str, dict] = {}
        self.results_dir = results_dir
        results_dir.mkdir(parents=True, exist_ok=True)
        stamp = time.strftime("%Y%m%d-%H%M%S")
        self.path = results_dir / f"sessions-{stamp}.jsonl"

    def new_session(self) -> dict:
        sid = secrets.token_hex(8)
        s = {
            "sid": sid,
            "created": now(),
            "documents": [],
            "requests": [],
            "pages": {},
            "beacons": 0,
            "user_agents": [],
            "tls": [],
        }
        self.sessions[sid] = s
        return s

    def get(self, sid: str | None) -> dict | None:
        if sid and SID_RE.match(sid):
            return self.sessions.get(sid)
        return None

    def flush(self) -> None:
        """Rewrite the results file: one JSON line per session."""
        tmp = self.path.with_suffix(".tmp")
        with tmp.open("w") as f:
            for s in self.sessions.values():
                f.write(json.dumps(s, separators=(",", ":")) + "\n")
        os.replace(tmp, self.path)


def resource_kind(path: str) -> str:
    if path.startswith("/favicon"):
        return "favicon"
    if path.endswith(".css"):
        return "css"
    if path.endswith(".js"):
        return "js"
    if path.startswith("/img/"):
        return "img"
    if path.startswith("/api/report"):
        return "beacon"
    return "other"


def product_svg(n: int) -> bytes:
    name, cat, price = PRODUCTS[n % len(PRODUCTS)]
    c1, c2 = PALETTE[n % len(PALETTE)], PALETTE[(n + 2) % len(PALETTE)]
    svg = (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="320" height="220" viewBox="0 0 320 220">'
        f'<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{c1}"/>'
        f'<stop offset="1" stop-color="{c2}"/></linearGradient></defs>'
        f'<rect width="320" height="220" fill="url(#g)"/>'
        f'<circle cx="{60 + 17 * n % 200}" cy="110" r="{40 + n * 3 % 30}" fill="#fff" opacity=".25"/>'
        f'<text x="20" y="200" font-family="sans-serif" font-size="18" fill="#fff">{html.escape(cat)}</text></svg>'
    )
    return svg.encode()


def favicon_png() -> bytes:
    """A 16x16 PNG built by hand, served as the favicon."""
    w = h = 16
    raw = b"".join(b"\x00" + bytes((0x30, 0x63, 0x8E, 0xFF)) * w for _ in range(h))
    def chunk(t: bytes, d: bytes) -> bytes:
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


FAVICON = favicon_png()


def render_products(items, sid: str) -> str:
    out = []
    for i, (name, cat, price) in items:
        out.append(
            f'<article class="card" data-id="{i}">'
            f'<img src="/img/p{i}.svg?s={sid}" width="320" height="220" alt="{html.escape(name)}">'
            f'<h3>{html.escape(name)}</h3><p class="cat">{html.escape(cat)}</p>'
            f'<p class="price">${price:.2f}</p>'
            f'<button type="button" class="add" id="add-{i}" data-id="{i}">Add to cart</button></article>'
        )
    return "\n".join(out)


class Handler(BaseHTTPRequestHandler):
    server_version = "nginx"
    protocol_version = "HTTP/1.1"
    store: Store
    config: dict
    quiet = False
    stun: StunResponder | None = None

    # ---- helpers -------------------------------------------------------

    def version_string(self):
        return self.server_version

    def log_message(self, fmt, *args):  # noqa: D401 - stdlib signature
        if not self.quiet:
            sys.stderr.write("%s %s\n" % (self.log_date_time_string(), fmt % args))

    def cookie_sid(self) -> str | None:
        raw = self.headers.get("Cookie")
        if not raw:
            return None
        c = cookies.SimpleCookie()
        try:
            c.load(raw)
        except cookies.CookieError:
            return None
        m = c.get(COOKIE)
        return m.value if m else None

    def tls_info(self) -> dict | None:
        sock = self.connection
        if not isinstance(sock, ssl.SSLSocket):
            return None
        shared = sock.shared_ciphers() or []
        return {
            "version": sock.version(),
            "cipher": (sock.cipher() or [None])[0],
            "alpn": sock.selected_alpn_protocol(),
            "shared_ciphers": len(shared),
        }

    def send_body(self, code: int, body: bytes, ctype: str, extra: dict | None = None) -> None:
        headers = {"Content-Type": ctype, "Content-Length": str(len(body)), "Cache-Control": "no-cache"}
        headers.update(extra or {})
        self.send_response(code)
        for k, v in headers.items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def send_json(self, code: int, obj) -> None:
        body = json.dumps(obj, indent=1).encode()
        self.send_body(code, body, "application/json", {"Cache-Control": "no-store"})

    def fetch_context(self) -> dict:
        """The values (not only the names) of Referer, Sec-Fetch-* and Origin,
        and the request's own origin: what `referer_context_mismatch` judges."""
        h = self.headers

        def val(name: str) -> str | None:
            v = h.get(name)
            return v[:MAX_HEADER_VALUE] if v is not None else None

        return {
            "host": val("Host"),
            "scheme": "https" if self.tls_info() else "http",
            "referer": val("Referer"),
            "origin": val("Origin"),
            "sec_fetch_site": val("Sec-Fetch-Site"),
            "sec_fetch_mode": val("Sec-Fetch-Mode"),
            "sec_fetch_dest": val("Sec-Fetch-Dest"),
            "sec_fetch_user": val("Sec-Fetch-User"),
        }

    def agent_headers(self) -> dict:
        """Signature-Agent / Signature-Input / Signature as sent (repeated header
        lines joined with ", " as Structured Fields require); only those present."""
        out = {}
        for name in AGENT_HEADERS:
            vals = self.headers.get_all(name)
            if vals:
                out[name] = ", ".join(v.strip() for v in vals)[:4096]
        return out

    def record_request(self, s: dict | None, path: str, kind: str) -> None:
        if s is None or len(s["requests"]) >= MAX_REQUESTS_PER_SESSION:
            return
        t = now()
        docs = s["documents"]
        since = round((t - docs[-1]["t"]) * 1000, 1) if docs else None
        req = {
            "t": t,
            "path": path,
            "kind": kind,
            "since_doc_ms": since,
            "cookie": self.cookie_sid() == s["sid"],
            "http_version": self.request_version,
            "ua": self.headers.get("User-Agent"),
            "header_names": list(self.headers.keys()),
            **self.fetch_context(),
        }
        agent = self.agent_headers()
        if agent:
            req["agent"] = agent
        s["requests"].append(req)
        self.note_ua(s)

    def note_ua(self, s: dict) -> None:
        ua = self.headers.get("User-Agent") or ""
        if ua not in s["user_agents"]:
            s["user_agents"].append(ua)

    # ---- documents ----------------------------------------------------

    def serve_document(self, name: str, query: dict) -> None:
        store = self.store
        with store.lock:
            s = store.get(self.cookie_sid())
            fresh = s is None
            if fresh:
                s = store.new_session()
            hdrs = self.headers
            doc = {
                "t": now(),
                "path": self.path,
                "method": self.command,
                "http_version": self.request_version,
                "header_names": list(hdrs.keys()),
                "headers": {k.lower(): v for k, v in hdrs.items() if k.lower() != "cookie"},
                "cookie": not fresh,
                "scheme": "https" if self.tls_info() else "http",
                **{k: v for k, v in self.fetch_context().items() if k not in ("host", "scheme")},
            }
            agent = self.agent_headers()
            if agent:
                doc["headers"].update(agent)  # in full: the headers dict above keeps the last line only
            tls = self.tls_info()
            if tls:
                doc["tls"] = tls
            s["documents"].append(doc)
            s.setdefault("accept_ch", ACCEPT_CH)
            self.note_ua(s)
            store.flush()
        sid = s["sid"]
        ctx = {
            "sid": sid,
            "page": name,
            "ua": hdrs.get("User-Agent", ""),
            "acceptLanguage": hdrs.get("Accept-Language", ""),
            "secChUa": hdrs.get("sec-ch-ua", ""),
            "secChUaPlatform": hdrs.get("sec-ch-ua-platform", ""),
            "stun": self.stun.address() if self.stun else "",
        }
        tpl = (SITES / f"{name}.html").read_text()
        q = query.get("q", [""])[0][:200]
        if name == "search":
            ql = q.lower()
            hits = [(i, p) for i, p in enumerate(PRODUCTS)
                    if ql and (ql in p[0].lower() or ql in p[1].lower())]
            results = render_products(hits, sid) if hits else '<p class="empty">No products found.</p>'
        else:
            results = render_products(list(enumerate(PRODUCTS)), sid)
        body = (
            tpl.replace("{{CTX}}", json.dumps(ctx).replace("<", "\\u003c").replace(">", "\\u003e")
                        .replace("&", "\\u0026"))
            .replace("{{SID}}", sid)
            .replace("{{Q}}", html.escape(q))
            .replace("{{PRODUCTS}}", results)
            .replace("{{COUNT}}", str(len(PRODUCTS)))
        ).encode()
        extra = {"Cache-Control": "no-store", "Accept-CH": ", ".join(ACCEPT_CH),
                 "Critical-CH": ", ".join(CRITICAL_CH)}
        if fresh:
            extra["Set-Cookie"] = f"{COOKIE}={sid}; Path=/; SameSite=Lax; HttpOnly"
        self.send_body(200, body, "text/html; charset=utf-8", extra)

    # ---- routing ------------------------------------------------------

    PAGES = {"/": "shop", "/shop": "shop", "/news": "news", "/login": "login", "/search": "search"}

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        url = urlsplit(self.path)
        path, query = url.path, parse_qs(url.query)
        if path in self.PAGES:
            return self.serve_document(self.PAGES[path], query)
        if path.startswith("/api/"):
            return self.api_get(path, query)
        sid = query.get("s", [None])[0] or self.cookie_sid()
        with self.store.lock:
            s = self.store.get(sid)
            self.record_request(s, path, resource_kind(path))
        if path == "/favicon.ico":
            return self.send_body(200, FAVICON, "image/png")
        m = re.fullmatch(r"/img/p(\d+)\.svg", path)
        if m:
            return self.send_body(200, product_svg(int(m.group(1))), "image/svg+xml")
        if path == "/collector.js":
            return self.send_body(200, (SITES / "collector.js").read_bytes(),
                                  "text/javascript; charset=utf-8")
        m = re.fullmatch(r"/static/([a-z0-9_-]+\.(css|js))", path)
        if m:
            f = SITES / "static" / m.group(1)
            if f.is_file():
                ctype = "text/css" if m.group(2) == "css" else "text/javascript"
                return self.send_body(200, f.read_bytes(), ctype + "; charset=utf-8")
        self.send_body(404, b"not found", "text/plain")

    def do_POST(self):
        url = urlsplit(self.path)
        length = int(self.headers.get("Content-Length") or 0)
        if length > MAX_BODY:
            return self.send_json(413, {"error": "too large"})
        raw = self.rfile.read(length) if length else b""
        if url.path == "/api/report":
            return self.api_report(raw)
        if url.path in ("/login", "/cart"):
            # Forms post here; record it and show a plain result page.
            with self.store.lock:
                self.record_request(self.store.get(self.cookie_sid()), url.path, "form")
            msg = b"<!doctype html><title>Done</title><p>Thanks. <a href=/shop>Back to shop</a></p>"
            if url.path == "/login":
                msg = b"<!doctype html><title>Sign in</title><p>Invalid credentials. <a href=/login>Try again</a></p>"
            return self.send_body(200, msg, "text/html; charset=utf-8")
        self.send_json(404, {"error": "not found"})

    # ---- API ----------------------------------------------------------

    def api_report(self, raw: bytes) -> None:
        try:
            rep = json.loads(raw)
            sid = rep["sid"]
            page = rep["page"]
            pid = str(page["id"])[:40]
        except (ValueError, KeyError, TypeError):
            return self.send_json(400, {"error": "bad report"})
        with self.store.lock:
            s = self.store.get(sid)
            if s is None:
                return self.send_json(404, {"error": "unknown session"})
            self.record_request(s, "/api/report", "beacon")
            s["beacons"] += 1
            pg = s["pages"].setdefault(pid, {
                "url": str(page.get("url", ""))[:300], "seq": -1, "reason": "", "received": now(),
                "t": None, "probes": {}, "trace": [],
            })
            seq = int(page.get("seq", 0))
            newer = seq > pg["seq"]
            if newer:
                pg.update(seq=seq, reason=str(page.get("reason", ""))[:20], received=now(), t=page.get("t"))
            # Probes merge by id; an older beacon never overwrites a newer one.
            for p in rep.get("probes", [])[:200]:
                if isinstance(p, dict) and isinstance(p.get("id"), str):
                    if newer or p["id"] not in pg["probes"]:
                        pg["probes"][p["id"][:60]] = p
            # The trace arrives in slices; `traceFrom` is the slice's offset.
            start = int(rep.get("traceFrom", 0) or 0)
            events = rep.get("trace", [])
            if isinstance(events, list) and 0 <= start <= len(pg["trace"]):
                merged = pg["trace"][:start] + events
                if len(merged) >= len(pg["trace"]):
                    pg["trace"] = merged[:MAX_TRACE]
            self.store.flush()
        self.send_json(200, {"ok": True})

    def fleet(self) -> dict:
        """Cross-session groups over every retained session (call with the store lock held)."""
        return scoring.fleet_index(self.store.sessions.values(), self.config)

    def clusters(self) -> dict:
        """Sessions grouped by a hash of their stable device probes and by the
        shape of their pointer strokes. Groups of one are left out; a group the
        size of the threshold or more is what `fleet_fingerprint_reuse` fires on."""
        idx = self.fleet()
        thr = idx["threshold"]
        fps = [{"hash": h, "size": len(sids), "fires": len(sids) >= thr, "sessions": sids,
                "parts": idx["parts"].get(h)}
               for h, sids in sorted(idx["fingerprint"].items(), key=lambda kv: -len(kv[1])) if len(sids) >= 2]
        shs = [{"hash": h, "size": len(sids), "sessions": sids}
               for h, sids in sorted(idx["shape"].items(), key=lambda kv: -len(kv[1])) if len(sids) >= 2]
        return {"sessions_retained": idx["sessions"], "threshold": thr, "window_s": self.config.get("fleet_window_s", 0),
                "fingerprint": fps, "path_shape": shs}

    def api_get(self, path: str, query: dict) -> None:
        if path == "/api/clusters":
            with self.store.lock:
                return self.send_json(200, self.clusters())
        if path == "/api/stun":
            return self.send_json(200, self.stun.stats() if self.stun else {"address": None})
        if path == "/api/results":
            with self.store.lock:
                rows = []
                fleet = self.fleet()
                for s in self.store.sessions.values():
                    r = scoring.score_session(s, self.config, fleet)
                    rows.append({
                        "sid": s["sid"], "created": s["created"], "beacons": s["beacons"],
                        "ua": (s["user_agents"] or [""])[0], "score": r["score"],
                        "verdict": r["verdict"], "fired": [f["id"] for f in r["fired"]],
                        "agent_declared": bool(r["agent_declared"]),
                    })
            return self.send_json(200, {"sessions": rows})
        m = re.fullmatch(r"/api/(score|session|trace)/([0-9a-f]{16})", path)
        if not m:
            return self.send_json(404, {"error": "not found"})
        kind, sid = m.groups()
        with self.store.lock:
            s = self.store.get(sid)
            if s is None:
                return self.send_json(404, {"error": "unknown session"})
            if kind == "score":
                return self.send_json(200, scoring.score_session(s, self.config, self.fleet()))
            if kind == "session":
                return self.send_json(200, s)
            pages = sorted(s["pages"].values(), key=lambda p: p["received"])
            return self.send_json(200, {"sid": sid, "pages": [
                {"url": p["url"], "events": p["trace"]} for p in pages]})


def make_tls_context(tmp: Path) -> ssl.SSLContext:
    """Self-signed cert for localhost/127.0.0.1, made fresh in a temp dir."""
    key, crt = tmp / "key.pem", tmp / "cert.pem"
    subprocess.run(
        ["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1",
         "-nodes", "-days", "2", "-subj", "/CN=localhost", "-keyout", str(key), "-out", str(crt),
         "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1"],
        check=True, capture_output=True,
    )
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(crt, key)
    # The stdlib speaks HTTP/1.1 only. Compare H2 with lb-fptest instead.
    ctx.set_alpn_protocols(["http/1.1"])
    return ctx


class LoopbackServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, addr, handler):
        if ":" in addr[0]:
            import socket
            self.address_family = socket.AF_INET6
        super().__init__(addr, handler)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default="127.0.0.1", help="127.0.0.1 or ::1 only")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--tls", action="store_true", help="serve HTTPS with a throwaway self-signed cert")
    ap.add_argument("--results-dir", default=str(HERE / "results"))
    ap.add_argument("--config", help="JSON file overriding thresholds/weights (see scoring.py)")
    ap.add_argument("--quiet", action="store_true")
    ap.add_argument("--stun-port", type=int, default=0,
                    help="UDP port of the loopback STUN responder (0: any free port)")
    ap.add_argument("--no-stun", action="store_true", help="do not start the STUN responder")
    a = ap.parse_args(argv)

    if a.host not in ("127.0.0.1", "::1"):
        print(f"refusing to bind {a.host!r}: loopback only (127.0.0.1 or ::1)", file=sys.stderr)
        return 2
    Handler.store = Store(Path(a.results_dir))
    Handler.config = scoring.load_config(a.config)
    Handler.quiet = a.quiet
    Handler.stun = None if a.no_stun else StunResponder(a.host, a.stun_port).start()
    httpd = LoopbackServer((a.host, a.port), Handler)
    scheme = "http"
    if a.tls:
        tmp = Path(tempfile.mkdtemp(prefix="lb-detect-bench-"))
        try:
            ctx = make_tls_context(tmp)
        finally:
            shutil.rmtree(tmp, ignore_errors=True)  # the cert is loaded; drop the files
        httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
        scheme = "https"
    host = "localhost" if a.tls else a.host
    print(f"detect-bench on {scheme}://{host}:{httpd.server_address[1]}/shop "
          f"(results: {Handler.store.path}; STUN udp {Handler.stun.address() if Handler.stun else 'off'})", flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        httpd.server_close()
        if Handler.stun:
            Handler.stun.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())

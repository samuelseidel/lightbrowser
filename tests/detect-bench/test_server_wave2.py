"""In-process tests for the wave-2 server signals: recorded Referer and
Sec-Fetch-* values, signed-agent headers, /api/clusters and the loopback STUN
responder (ADR 0043).

    cd tests/detect-bench && python3 -m unittest -v test_server_wave2
"""

from __future__ import annotations

import copy
import http.client
import json
import socket
import struct
import tempfile
import threading
import unittest
from pathlib import Path

import scoring
import server
from test_rules_wave2 import GOOD, chrome_browse, clone, moves, arc, transform, trace_session
from test_scoring import os_session

UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
      "Chrome/153.0.0.0 Safari/537.36")
NAV = {"User-Agent": UA, "Accept-Language": "en-US,en;q=0.9", "Sec-Fetch-Site": "none", "Sec-Fetch-Mode": "navigate",
       "Sec-Fetch-Dest": "document", "Sec-Fetch-User": "?1"}


class Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        server.Handler.store = server.Store(Path(cls.tmp.name))
        server.Handler.config = scoring.load_config()
        server.Handler.quiet = True
        server.Handler.stun = server.StunResponder("127.0.0.1", 0).start()
        cls.httpd = server.LoopbackServer(("127.0.0.1", 0), server.Handler)
        cls.port = cls.httpd.server_address[1]
        threading.Thread(target=cls.httpd.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()
        cls.httpd.server_close()
        server.Handler.stun.close()
        server.Handler.stun = None
        cls.tmp.cleanup()

    def req(self, method, path, body=None, headers=None, extra=()):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        c.putrequest(method, path, skip_accept_encoding=True)
        for k, v in (headers or {}).items():
            c.putheader(k, v)
        for k, v in extra:  # repeated header lines
            c.putheader(k, v)
        if isinstance(body, str):
            body = body.encode()
        if body is not None:
            c.putheader("Content-Length", str(len(body)))
        c.endheaders(body)
        r = c.getresponse()
        data = r.read()
        c.close()
        return r, data

    def start_session(self, headers=None, extra=()):
        r, body = self.req("GET", "/shop", headers=dict(NAV, **(headers or {})), extra=extra)
        cookie = r.getheader("Set-Cookie").split(";")[0]
        return cookie, cookie.split("=")[1], body

    def session(self, sid):
        _, b = self.req("GET", f"/api/session/{sid}")
        return json.loads(b)

    def score(self, sid):
        _, b = self.req("GET", f"/api/score/{sid}")
        return json.loads(b)

    def sub(self, cookie, sid, path, kind_headers):
        return self.req("GET", f"{path}?s={sid}", headers=dict({"Cookie": cookie, "User-Agent": UA}, **kind_headers))


SAME = {"Sec-Fetch-Site": "same-origin", "Sec-Fetch-Mode": "no-cors", "Sec-Fetch-Dest": "style"}


class RecordedContext(Base):
    def test_values_are_kept_for_documents_and_subresources(self):
        cookie, sid, _ = self.start_session()
        page = f"http://127.0.0.1:{self.port}/shop"
        self.sub(cookie, sid, "/static/style.css", dict(SAME, Referer=page))
        self.req("POST", "/api/report", json.dumps({"sid": sid, "page": {"id": "p1", "url": "/shop", "seq": 1, "reason": "load", "t": 5}}),
                 {"Cookie": cookie, "User-Agent": UA, "Referer": page, "Origin": f"http://127.0.0.1:{self.port}",
                  "Sec-Fetch-Site": "same-origin", "Sec-Fetch-Mode": "cors", "Sec-Fetch-Dest": "empty"})
        s = self.session(sid)
        d = s["documents"][0]
        self.assertEqual((d["sec_fetch_site"], d["sec_fetch_mode"], d["sec_fetch_dest"], d["sec_fetch_user"]),
                         ("none", "navigate", "document", "?1"))
        self.assertIsNone(d["referer"])
        css, beacon = s["requests"][0], s["requests"][1]
        self.assertEqual((css["referer"], css["sec_fetch_site"], css["sec_fetch_mode"], css["sec_fetch_dest"]),
                         (page, "same-origin", "no-cors", "style"))
        self.assertEqual((css["host"], css["scheme"], css["origin"]), (f"127.0.0.1:{self.port}", "http", None))
        self.assertEqual((beacon["origin"], beacon["sec_fetch_mode"], beacon["sec_fetch_dest"]),
                         (f"http://127.0.0.1:{self.port}", "cors", "empty"))

    def test_a_consistent_session_does_not_fire(self):
        cookie, sid, _ = self.start_session()
        page = f"http://127.0.0.1:{self.port}/shop"
        for path, dest in (("/static/style.css", "style"), ("/static/app.js", "script"), ("/img/p1.svg", "image")):
            self.sub(cookie, sid, path, dict(SAME, **{"Sec-Fetch-Dest": dest, "Referer": page}))
        # A link click to another page: same-origin navigation with the first page as Referer.
        self.req("GET", "/news", headers={"Cookie": cookie, "User-Agent": UA, "Sec-Fetch-Site": "same-origin",
                                          "Sec-Fetch-Mode": "navigate", "Sec-Fetch-Dest": "document", "Referer": page})
        self.sub(cookie, sid, "/static/style.css", dict(SAME, Referer=f"http://127.0.0.1:{self.port}/news"))
        self.assertNotIn("referer_context_mismatch", [f["id"] for f in self.score(sid)["fired"]])

    def fires_for(self, sub_headers, doc_headers=None):
        cookie, sid, _ = self.start_session(doc_headers)
        self.sub(cookie, sid, "/static/style.css", sub_headers)
        return {f["id"]: f for f in self.score(sid)["fired"]}.get("referer_context_mismatch")

    def test_each_contradiction_fires_through_the_api(self):
        page = f"http://127.0.0.1:{self.port}/shop"
        f = self.fires_for(dict(SAME, **{"Sec-Fetch-Site": "none", "Referer": page}))
        self.assertIn("Sec-Fetch-Site none on a subresource", f["detail"])
        f = self.fires_for(dict(SAME, Referer=f"http://127.0.0.1:{self.port}/other"))
        self.assertIn("is not a page of this session", f["detail"])
        f = self.fires_for(dict(SAME, Referer="https://example.net/shop"))
        self.assertIn("same-origin with Referer https://example.net/shop", f["detail"])
        f = self.fires_for(dict(SAME))
        self.assertIn("same-origin without Referer", f["detail"])
        f = self.fires_for(dict(SAME, Referer=page), doc_headers={"Referer": "http://127.0.0.1:1/x"})
        self.assertIn("Sec-Fetch-Site none with Referer http://127.0.0.1:1/x", f["detail"])
        self.assertEqual(f["weight"], 35)


class SignedAgents(Base):
    def test_declared_agent_is_reported_with_its_fields(self):
        cookie, sid, _ = self.start_session(GOOD)
        r = self.score(sid)
        self.assertEqual(r["verdict"], "declared_agent")
        a = r["agent_declared"]
        self.assertEqual((a["agent"], a["keyid"], a["created"], a["expires"], a["verified"]),
                         ("https://signature-agent.example.com", "ba3e64==", 1735689600, 1735693200, False))
        _, rows = self.req("GET", "/api/results")
        row = next(x for x in json.loads(rows)["sessions"] if x["sid"] == sid)
        self.assertEqual((row["verdict"], row["agent_declared"]), ("declared_agent", True))

    def test_headers_on_subresources_are_recorded_and_counted(self):
        cookie, sid, _ = self.start_session(GOOD)
        self.sub(cookie, sid, "/static/app.js", dict(SAME, **GOOD))
        s = self.session(sid)
        self.assertEqual(s["requests"][0]["agent"]["signature-agent"], GOOD["signature-agent"])
        self.assertEqual(self.score(sid)["agent_declared"]["requests"], 2)

    def test_repeated_header_lines_are_joined(self):
        halves = GOOD["signature-input"].split(";")
        cookie, sid, _ = self.start_session(
            {"Signature-Agent": GOOD["signature-agent"], "Signature": GOOD["signature"]},
            extra=[("Signature-Input", ";".join(halves[:3])), ("Signature-Input", "sig2=(\"@method\");created=1;keyid=\"x\"")])
        # Two lines make one dictionary: sig1=(...);created=...;keyid=..., sig2=(...)
        r = self.score(sid)
        self.assertEqual(r["agent_declared"]["label"], "sig1")

    def test_malformed_headers_are_not_accepted(self):
        for why, hdrs in {
            "bad signature": dict(GOOD, Signature="sig1=:!!!:"),
            "agent not a url": {"Signature-Agent": '"gopher://x"', "Signature-Input": GOOD["signature-input"], "Signature": GOOD["signature"]},
            "only the agent": {"Signature-Agent": GOOD["signature-agent"]},
            "unbalanced": {"Signature-Agent": GOOD["signature-agent"], "Signature-Input": 'sig1=("a"', "Signature": GOOD["signature"]},
        }.items():
            with self.subTest(why=why):
                h = {k.title() if k.islower() else k: v for k, v in hdrs.items()}
                cookie, sid, _ = self.start_session(h)
                r = self.score(sid)
                self.assertIsNone(r["agent_declared"])
                self.assertNotEqual(r["verdict"], "declared_agent")

    def test_plain_browsers_have_no_agent_fields(self):
        cookie, sid, _ = self.start_session()
        s = self.session(sid)
        self.assertNotIn("agent", s["requests"][0] if s["requests"] else {})
        self.assertIsNone(self.score(sid)["agent_declared"])


def put_session(sess: dict) -> str:
    server.Handler.store.sessions[sess["sid"]] = sess
    return sess["sid"]


class Clusters(Base):
    def setUp(self):
        server.Handler.store.sessions.clear()

    def api(self, path):
        _, b = self.req("GET", path)
        return json.loads(b)

    def test_three_sessions_of_one_fingerprint_are_a_fleet(self):
        base = chrome_browse()
        sids = [put_session(clone(base, f"{i:016x}")) for i in range(1, 4)]
        put_session(clone(os_session("mac"), "9" * 16))
        c = self.api("/api/clusters")
        self.assertEqual((c["sessions_retained"], c["threshold"]), (4, 3))
        self.assertEqual(len(c["fingerprint"]), 1)
        g = c["fingerprint"][0]
        self.assertEqual((g["size"], g["fires"], sorted(g["sessions"])), (3, True, sids))
        self.assertEqual(set(g["parts"]), {"ua", "canvas", "webgl", "audio", "fonts", "screen", "tz", "languages"})
        for sid in sids:
            self.assertIn("fleet_fingerprint_reuse", [f["id"] for f in self.score(sid)["fired"]])
        self.assertNotIn("fleet_fingerprint_reuse", [f["id"] for f in self.score("9" * 16)["fired"]])
        _, rows = self.req("GET", "/api/results")
        fired = {x["sid"]: x["fired"] for x in json.loads(rows)["sessions"]}
        self.assertTrue(all("fleet_fingerprint_reuse" in fired[s] for s in sids))

    def test_two_sessions_group_but_do_not_fire(self):
        base = chrome_browse()
        sids = [put_session(clone(base, f"{i:016x}")) for i in (1, 2)]
        c = self.api("/api/clusters")
        self.assertEqual([(g["size"], g["fires"]) for g in c["fingerprint"]], [(2, False)])
        for sid in sids:
            self.assertNotIn("fleet_fingerprint_reuse", [f["id"] for f in self.score(sid)["fired"]])

    def test_a_lone_session_scores_as_before(self):
        sid = put_session(clone(chrome_browse(), "1" * 16))
        alone = scoring.score_session(chrome_browse())
        r = self.score(sid)
        self.assertEqual((r["score"], [f["id"] for f in r["fired"]]), (alone["score"], [f["id"] for f in alone["fired"]]))
        self.assertEqual(self.api("/api/clusters")["fingerprint"], [])

    def test_sessions_sharing_a_stroke_shape_cluster(self):
        a = arc()
        sids = [put_session(trace_session([moves(transform(a, dx=40 * i), t0=500)], sid=f"{i:016x}")) for i in (1, 2)]
        put_session(trace_session([moves(transform(arc(bend=-30, phase=1.0)), t0=500)], sid="3" * 16))
        c = self.api("/api/clusters")
        self.assertEqual(len(c["path_shape"]), 1)
        self.assertEqual(sorted(c["path_shape"][0]["sessions"]), sids)


class Stun(Base):
    def ask(self, payload: bytes, timeout=1.0):
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.settimeout(timeout)
        s.bind(("127.0.0.1", 0))
        try:
            s.sendto(payload, ("127.0.0.1", server.Handler.stun.port))
            return s.recvfrom(2048)[0], s.getsockname()
        except socket.timeout:
            return None, s.getsockname()
        finally:
            s.close()

    @staticmethod
    def binding(txid=b"0123456789ab", attrs=b""):
        return struct.pack(">HHI", 1, len(attrs), server.STUN_MAGIC) + txid + attrs

    def test_binding_response_carries_the_xor_mapped_sender_address(self):
        txid = bytes(range(12))
        data, me = self.ask(self.binding(txid))
        mtype, length, cookie = struct.unpack(">HHI", data[:8])
        self.assertEqual((mtype, cookie, data[8:20], length, len(data)), (0x0101, server.STUN_MAGIC, txid, 12, 32))
        atype, alen, _, fam, xport = struct.unpack(">HHBBH", data[20:28])
        self.assertEqual((atype, alen, fam), (0x0020, 8, 1))
        port = xport ^ (server.STUN_MAGIC >> 16)
        ip = socket.inet_ntoa(bytes(a ^ b for a, b in zip(data[28:32], struct.pack(">I", server.STUN_MAGIC))))
        self.assertEqual((ip, port), me)

    def test_attributes_in_a_request_are_accepted(self):
        attrs = struct.pack(">HH", 0x8022, 5) + b"lb-ok\x00\x00\x00"  # SOFTWARE with padding
        data, _ = self.ask(self.binding(attrs=attrs))
        self.assertIsNotNone(data)

    def test_malformed_requests_are_ignored(self):
        before = server.Handler.stun.stats()
        bad = [
            b"",
            b"hello world, this is not stun!",
            struct.pack(">HHI", 1, 0, 0xDEADBEEF) + b"0123456789ab",           # wrong cookie
            struct.pack(">HHI", 0x0101, 0, server.STUN_MAGIC) + b"0123456789ab",  # a response, not a request
            struct.pack(">HHI", 1, 4, server.STUN_MAGIC) + b"0123456789ab",     # length says 4, no body
            struct.pack(">HHI", 1, 3, server.STUN_MAGIC) + b"0123456789ab" + b"abc",  # length not a multiple of 4
            struct.pack(">HHI", 1, 4, server.STUN_MAGIC) + b"0123456789ab" + struct.pack(">HH", 0x8022, 99),  # attribute overruns
            self.binding()[:19],
            b"\xc0" + self.binding()[1:],                                          # top bits set
        ]
        for payload in bad:
            data, _ = self.ask(payload, timeout=0.3)
            self.assertIsNone(data, payload[:12])
        after = server.Handler.stun.stats()
        self.assertEqual(after["answered"], before["answered"])
        self.assertGreaterEqual(after["ignored"] - before["ignored"], len(bad) - 1)  # the empty datagram may not arrive

    def test_ipv6_xor_math(self):
        txid = bytes(range(12))
        data = server.StunResponder.response(txid, ("::1", 4242))
        atype, alen, _, fam, xport = struct.unpack(">HHBBH", data[20:28])
        self.assertEqual((atype, alen, fam, xport ^ (server.STUN_MAGIC >> 16)), (0x0020, 20, 2, 4242))
        key = struct.pack(">I", server.STUN_MAGIC) + txid
        addr = socket.inet_ntop(socket.AF_INET6, bytes(a ^ b for a, b in zip(data[28:44], key)))
        self.assertEqual(addr, "::1")

    def test_loopback_only(self):
        for host in ("0.0.0.0", "192.168.1.5", "::", "example.com", ""):
            with self.assertRaises(ValueError):
                server.StunResponder(host, 0)
        self.assertEqual(server.Handler.stun.host, "127.0.0.1")
        self.assertEqual(server.main(["--host", "0.0.0.0", "--stun-port", "0", "--results-dir", self.tmp.name]), 2)

    def test_pages_and_api_expose_the_address(self):
        cookie, sid, body = self.start_session()
        ctx = json.loads(body.decode().split('id="lb-ctx">')[1].split("</script>")[0])
        self.assertEqual(ctx["stun"], f"127.0.0.1:{server.Handler.stun.port}")
        self.ask(self.binding())
        _, b = self.req("GET", "/api/stun")
        st = json.loads(b)
        self.assertEqual(st["address"], ctx["stun"])
        self.assertGreaterEqual(st["answered"], 1)
        self.assertTrue(st["requests"][-1]["from"].startswith("127.0.0.1:"))

    def test_page_without_responder_has_empty_stun(self):
        saved, server.Handler.stun = server.Handler.stun, None
        try:
            _, body = self.req("GET", "/shop", headers=NAV)
            ctx = json.loads(body.decode().split('id="lb-ctx">')[1].split("</script>")[0])
            self.assertEqual(ctx["stun"], "")
            _, b = self.req("GET", "/api/stun")
            self.assertIsNone(json.loads(b)["address"])
        finally:
            server.Handler.stun = saved


if __name__ == "__main__":
    unittest.main()

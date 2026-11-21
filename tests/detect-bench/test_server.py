"""In-process tests for server.py: sessions, cookies, reports, scoring API.

    cd tests/detect-bench && python3 -m unittest -v
"""

from __future__ import annotations

import http.client
import json
import tempfile
import threading
import unittest
from pathlib import Path

import scoring
import server

UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
      "Chrome/153.0.0.0 Safari/537.36")


class ServerTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        server.Handler.store = server.Store(Path(cls.tmp.name))
        server.Handler.config = scoring.load_config()
        server.Handler.quiet = True
        cls.httpd = server.LoopbackServer(("127.0.0.1", 0), server.Handler)
        cls.port = cls.httpd.server_address[1]
        threading.Thread(target=cls.httpd.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()
        cls.httpd.server_close()
        cls.tmp.cleanup()

    def req(self, method, path, body=None, headers=None):
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        c.request(method, path, body=body, headers=headers or {})
        r = c.getresponse()
        data = r.read()
        c.close()
        return r, data

    def test_refuses_non_loopback_bind(self):
        self.assertEqual(server.main(["--host", "0.0.0.0", "--results-dir", self.tmp.name]), 2)

    def test_session_flow(self):
        r, body = self.req("GET", "/shop", headers={"User-Agent": UA, "Accept-Language": "en-US,en;q=0.9"})
        self.assertEqual(r.status, 200)
        cookie = r.getheader("Set-Cookie").split(";")[0]
        sid = cookie.split("=")[1]
        ctx = json.loads(body.decode().split('id="lb-ctx">')[1].split("</script>")[0])
        self.assertEqual(ctx["sid"], sid)
        self.assertEqual(ctx["acceptLanguage"], "en-US,en;q=0.9")

        # Subresource with the cookie, and a second page in the same session.
        self.req("GET", f"/static/style.css?s={sid}", headers={"Cookie": cookie, "User-Agent": UA})
        r2, _ = self.req("GET", "/search?q=bag", headers={"Cookie": cookie, "User-Agent": UA})
        self.assertIsNone(r2.getheader("Set-Cookie"))

        rep = {"sid": sid, "page": {"id": "p1", "url": "/shop", "seq": 1, "reason": "load", "t": 900},
               "probes": [{"id": "webdriver", "cat": "automation", "value": True, "suspicious": True, "weight": 90}],
               "traceFrom": 0, "trace": [{"t": 1, "type": "mousemove", "tr": 1, "x": 1, "y": 1}]}
        r3, _ = self.req("POST", "/api/report", json.dumps(rep), {"Content-Type": "application/json", "Cookie": cookie})
        self.assertEqual(r3.status, 200)
        # A second slice of the trace appends; a stale duplicate is ignored.
        rep2 = dict(rep, page=dict(rep["page"], seq=2), probes=[], traceFrom=1,
                    trace=[{"t": 2, "type": "mousedown", "tr": 1, "x": 1, "y": 1}])
        self.req("POST", "/api/report", json.dumps(rep2), {"Cookie": cookie})
        self.req("POST", "/api/report", json.dumps(rep), {"Cookie": cookie})

        _, sbody = self.req("GET", f"/api/session/{sid}")
        s = json.loads(sbody)
        self.assertEqual(len(s["documents"]), 2)
        self.assertEqual(s["beacons"], 3)
        self.assertEqual([e["type"] for e in s["pages"]["p1"]["trace"]], ["mousemove", "mousedown"])
        self.assertTrue(all(r["cookie"] for r in s["requests"]))

        _, score = self.req("GET", f"/api/score/{sid}")
        res = json.loads(score)
        self.assertIn("webdriver", [f["id"] for f in res["fired"]])

        _, results = self.req("GET", "/api/results")
        self.assertIn(sid, [x["sid"] for x in json.loads(results)["sessions"]])
        lines = server.Handler.store.path.read_text().splitlines()
        self.assertIn(sid, [json.loads(line)["sid"] for line in lines])

    def test_bad_reports(self):
        r, _ = self.req("POST", "/api/report", b"not json")
        self.assertEqual(r.status, 400)
        r, _ = self.req("POST", "/api/report", json.dumps({"sid": "f" * 16, "page": {"id": "x"}}))
        self.assertEqual(r.status, 404)
        r, _ = self.req("GET", "/api/score/nothex")
        self.assertEqual(r.status, 404)


if __name__ == "__main__":
    unittest.main()

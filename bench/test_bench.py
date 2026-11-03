"""Unit tests for the bench tooling (no browser needed): python3 -m unittest discover -s bench"""

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import gen_corpus  # noqa: E402
import run  # noqa: E402


class Corpus(unittest.TestCase):
    def test_generator_is_deterministic_and_matches_tree(self):
        a, b = gen_corpus.build(), gen_corpus.build()
        self.assertEqual(a, b)
        self.assertTrue(gen_corpus.tree_matches(a), "bench/corpus is stale: run bench/gen_corpus.py")

    def test_size_and_shape(self):
        files = gen_corpus.build()
        self.assertEqual(sum(1 for n in files if n.startswith("pages/")), gen_corpus.PAGES)
        self.assertLess(sum(len(v) for v in files.values()), 3_000_000)
        m = json.loads(files["manifest.json"])
        self.assertEqual(len(m["pages"]), gen_corpus.PAGES)
        for p in m["pages"]:
            self.assertGreater(p["expected_rendered"], 0)
            self.assertIn("bench.mark(", files[p["path"].lstrip("/")].decode())

    def test_disk_digest_matches_manifest(self):
        m = json.loads((gen_corpus.CORPUS / "manifest.json").read_text())
        self.assertEqual(gen_corpus.disk_digest(), m["sha256"])


class Guard(unittest.TestCase):
    def test_loopback_only(self):
        for ok in ("http://127.0.0.1:1/", "ws://localhost:9/x", "http://[::1]:80/"):
            self.assertEqual(run.assert_loopback(ok), ok)
        for bad in ("http://example.com/", "http://127.0.0.1.evil.test/", "http://10.0.0.1/", "http://0.0.0.0/", "ws://192.168.1.2:9222/"):
            with self.assertRaises(SystemExit) as cm:
                run.assert_loopback(bad)
            self.assertEqual(cm.exception.code, 2)


class Stats(unittest.TestCase):
    def test_percentile_and_spread(self):
        self.assertEqual(run.pctl(list(range(1, 11)), 90), 9)
        self.assertEqual(run.pctl([5], 90), 5)
        s = run.spread([3, 1, 2, None])
        self.assertEqual((s["median"], s["min"], s["max"], s["n"]), (2, 1, 3, 3))

    def test_stat_parsing_survives_odd_comm(self):
        f = run._stat_fields(os.getpid())
        self.assertIsNotNone(f)
        self.assertEqual(int(f[1]), os.getppid())

    def test_tree_includes_children(self):
        p = subprocess.Popen(["sleep", "5"])
        try:
            self.assertIn(p.pid, run.proc_tree(os.getpid()))
            self.assertGreater(run.tree_rss_mb(os.getpid()), 0)
        finally:
            p.kill()
            p.wait()


class Report(unittest.TestCase):
    def test_realm_line_parser(self):
        out = "snapshot: engine 2.90 ms, registry+define 5.30 ms, first realm 2.40 ms, next realm 2.10 ms\n  Restored"
        self.assertEqual(run.REALM_LINE.search(out).groups(), ("2.90", "5.30", "2.40", "2.10"))

    def test_budgets_skip_without_lightpanda(self):
        s = {"cdp:lightbrowser": {"idle_rss_mb": {"median": 10.0}, "disk_files_created": {"max": 0}, "disk_write_bytes": {"max": 0},
                                 "crawl_total_s": {"median": 1}, "peak_rss_mb": {"median": 1}, "cpu_s": {"median": 1}}}
        bs = run.budgets(s, 39.2, {"first_realm_ms": {"median": 2.4}, "next_realm_ms": {"median": 2.1}})
        by = {b["budget"]: b["status"] for b in bs}
        self.assertEqual(by["binary size (stripped)"], "pass")
        self.assertEqual(by["idle RSS (serve, no client, 1 s after listen)"], "pass")
        self.assertTrue(all(v == "skip" for k, v in by.items() if "Lightpanda" in k))
        bs = run.budgets(s, 40.1, None)
        self.assertEqual({b["budget"]: b["status"] for b in bs}["binary size (stripped)"], "fail")

    def test_budgets_compare_with_lightpanda(self):
        lb = {"crawl_total_s": {"median": 1.0}, "peak_rss_mb": {"median": 50.0}, "cpu_s": {"median": 2.0}}
        lp = {"crawl_total_s": {"median": 2.0}, "peak_rss_mb": {"median": 40.0}, "cpu_s": {"median": 2.0}}
        s = {"fetch:lightbrowser": lb, "fetch:lightpanda": lp}
        by = {b["budget"]: b["status"] for b in run.budgets(s, 39.0, None)}
        self.assertEqual(by["crawl wall time vs Lightpanda (fetch)"], "pass")
        self.assertEqual(by["peak RSS vs Lightpanda (fetch)"], "fail")
        self.assertEqual(by["CPU time vs Lightpanda (fetch)"], "pass")  # equal counts as <=
        self.assertEqual(by["CPU time vs Lightpanda (cdp)"], "skip")

    def test_ws_url_from_json_version(self):
        import http.server
        import threading

        class H(http.server.BaseHTTPRequestHandler):
            ws = "ws://127.0.0.1:1/"

            def do_GET(self):
                body = json.dumps({"webSocketDebuggerUrl": self.ws}).encode() if self.path == "/json/version" else b""
                self.send_response(200 if body else 404)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *a):
                pass

        srv = http.server.HTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=srv.serve_forever, daemon=True).start()

        class Fake:  # the parts of `Spawned` the helper reads
            proc = type("P", (), {"poll": lambda self: None, "returncode": None})()
            tail: list = []

        try:
            port = srv.server_address[1]
            self.assertEqual(run.wait_json_version(Fake(), port, timeout=5), "ws://127.0.0.1:1/")
            H.ws = "ws://example.com:9222/"
            with self.assertRaises(SystemExit) as cm:
                run.wait_json_version(Fake(), port, timeout=5)
            self.assertEqual(cm.exception.code, 2)
        finally:
            srv.shutdown()

    def test_corpus_server_serves_loopback(self):
        srv = run.Server()
        try:
            self.assertTrue(srv.base.startswith("http://127.0.0.1:"))
            import urllib.request

            with urllib.request.urlopen(srv.base + "/pages/p00.html", timeout=5) as r:
                self.assertEqual(r.status, 200)
                self.assertEqual(r.headers["Cache-Control"], "no-store")
        finally:
            srv.close()


if __name__ == "__main__":
    unittest.main()

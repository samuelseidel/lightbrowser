"""Unit tests for scoring.py, using sessions recorded by run.js.

    cd tests/detect-bench && python3 -m unittest -v

Fixtures were recorded with:
    node run.js --scenario T-001,T-003,T-020 \
        --trace fixtures/synthetic-human-trace.json --save-fixtures fixtures
"""

from __future__ import annotations

import copy
import json
import re
import tempfile
import time
import unittest
from pathlib import Path

import scoring

HERE = Path(__file__).resolve().parent
FIX = HERE / "fixtures"
CHROME_UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
             "Chrome/153.0.0.0 Safari/537.36")


def load(name: str) -> dict:
    return json.loads((FIX / f"{name}.json").read_text())


def fired(result: dict) -> set[str]:
    return {f["id"] for f in result["fired"]}


def part(result: dict, behavior: bool) -> float:
    return scoring.combine(f["weight"] for f in result["fired"] if (f["cat"] == "behavior") == behavior)


def clean_session() -> dict:
    """T-020 (human-like input) with every client probe and request made clean."""
    s = copy.deepcopy(load("T-020"))
    for pg in s["pages"].values():
        for p in pg["probes"].values():
            p["suspicious"] = False
        pg["probes"]["ua_headless"]["value"] = CHROME_UA
    for i, d in enumerate(s["documents"]):
        h = d["headers"]
        h["user-agent"] = CHROME_UA
        h["sec-ch-ua"] = '"Chromium";v="153", "Google Chrome";v="153", "Not_A Brand";v="99"'
        h["accept-language"] = "en-US,en;q=0.9"
        names = [n for n in d["header_names"] if n not in ("Accept-Language", "Cookie")]
        names.append("Accept-Language")
        if i:
            names.append("Cookie")
        d["header_names"] = names
    s["user_agents"] = [CHROME_UA]
    for r in s["requests"]:
        r["cookie"] = True
        r["ua"] = CHROME_UA
    s["requests"].append(dict(s["requests"][0], kind="favicon", path="/favicon.ico"))
    return s


class Catalogue(unittest.TestCase):
    def test_shape(self):
        ids = [r["id"] for r in scoring.RULES]
        self.assertEqual(len(ids), len(set(ids)), "duplicate rule ids")
        for r in scoring.RULES:
            with self.subTest(rule=r["id"]):
                self.assertIn(r["cat"], scoring.CATEGORIES)
                self.assertTrue(0 <= r["weight"] <= 99)
                self.assertTrue(("probe" in r) != ("check" in r))
                self.assertTrue(r["why"])

    def test_collector_and_catalogue_agree(self):
        src = (HERE / "sites" / "collector.js").read_text()
        emitted = set(re.findall(r"\b(?:probe|add)\('([a-z0-9_]+)'", src))
        catalogued = {r["probe"] for r in scoring.RULES if "probe" in r}
        self.assertEqual(emitted - catalogued, set(), "probes missing from scoring.RULES")
        self.assertEqual(catalogued - emitted, set(), "rules for probes the collector never sends")

    def test_readme_lists_every_rule(self):
        readme = (HERE / "README.md").read_text()
        for r in scoring.RULES:
            with self.subTest(rule=r["id"]):
                self.assertIn(f"| `{r['id']}` | {r['weight']} | {r['cat']} |", readme)

    def test_combine(self):
        self.assertEqual(scoring.combine([]), 0)
        self.assertEqual(scoring.combine([50, 50]), 75)
        self.assertEqual(scoring.combine([100]), 99)


class Fixtures(unittest.TestCase):
    def test_t001_stock_headless(self):
        r = scoring.score_session(load("T-001"))
        self.assertGreaterEqual(r["score"], 90)
        self.assertEqual(r["verdict"], "bot")
        self.assertLessEqual({"webdriver", "ua_headless", "ua_headless_header", "beh_typing_no_keys",
                              "beh_click_center", "beh_click_hold"}, fired(r))

    def test_t003_scripted_slow_caught_by_fingerprint(self):
        r = scoring.score_session(load("T-003"))
        self.assertEqual(r["verdict"], "bot")
        self.assertGreaterEqual(part(r, behavior=False), 70)
        f = fired(r)
        self.assertLessEqual({"beh_mouse_linear", "beh_key_rhythm", "beh_key_dwell", "beh_click_center"}, f)
        self.assertNotIn("beh_typing_no_keys", f)  # it typed key by key

    def test_t020_humanlike_input_has_no_behavior_hits(self):
        r = scoring.score_session(load("T-020"))
        self.assertEqual([x["id"] for x in r["fired"] if x["cat"] == "behavior"], [])
        self.assertEqual(r["verdict"], "bot")  # still headless Chromium

    def test_clean_session_scores_human(self):
        r = scoring.score_session(clean_session())
        self.assertEqual(r["fired"], [], [f["id"] for f in r["fired"]])
        self.assertEqual(r["verdict"], "human")


class ServerRules(unittest.TestCase):
    def test_html_only_client(self):
        s = {
            "sid": "0" * 16, "created": time.time() - 60, "beacons": 0, "pages": {}, "requests": [],
            "user_agents": ["python-requests/2.32"], "tls": [],
            "documents": [{"t": time.time() - 60, "path": "/shop", "http_version": "HTTP/1.1",
                           "header_names": ["Host", "User-Agent", "Accept-Encoding", "Accept", "Connection"],
                           "headers": {"user-agent": "python-requests/2.32", "accept": "*/*"},
                           "cookie": False, "scheme": "http"}],
        }
        r = scoring.score_session(s)
        self.assertLessEqual({"no_beacon", "no_subresources", "non_browser_ua"}, fired(r))
        self.assertGreaterEqual(r["score"], 90)

    def test_beacon_grace(self):
        s = copy.deepcopy(load("T-001"))
        s["beacons"] = 0
        s["created"] = time.time()
        self.assertNotIn("no_beacon", fired(scoring.score_session(s)))
        s["created"] = time.time() - 30
        self.assertIn("no_beacon", fired(scoring.score_session(s)))

    def test_header_order(self):
        s = clean_session()
        self.assertIsNone(scoring.chk_header_order(s, scoring.load_config()))
        names = s["documents"][0]["header_names"]
        i, j = names.index("User-Agent"), names.index("Accept")
        names[i], names[j] = names[j], names[i]
        self.assertIn("order", scoring.chk_header_order(s, scoring.load_config()))
        s["documents"][0]["header_names"] = [n.lower() for n in names]
        self.assertIn("case", scoring.chk_header_order(s, scoring.load_config()))

    def test_cookie_not_echoed(self):
        s = clean_session()
        self.assertNotIn("cookie_not_echoed", fired(scoring.score_session(s)))
        s["requests"][0]["cookie"] = False
        self.assertIn("cookie_not_echoed", fired(scoring.score_session(s)))

    def test_probe_fires_if_any_page_suspicious(self):
        s = clean_session()
        pages = list(s["pages"].values())
        pages[-1]["probes"]["webdriver"]["suspicious"] = True
        self.assertIn("webdriver", fired(scoring.score_session(s)))

    def test_uncatalogued_probe_counts_capped(self):
        s = clean_session()
        pg = next(iter(s["pages"].values()))
        pg["probes"]["new_probe"] = {"id": "new_probe", "cat": "automation", "value": 1,
                                     "suspicious": True, "weight": 90}
        f = {x["id"]: x for x in scoring.score_session(s)["fired"]}
        self.assertEqual(f["new_probe"]["weight"], 50)


class Config(unittest.TestCase):
    def test_overrides(self):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
            json.dump({"thresholds": {"bot": 99.9}, "weights": {"ua_headless": 10},
                       "disabled": ["webdriver"]}, f)
        cfg = scoring.load_config(f.name)
        Path(f.name).unlink()
        self.assertEqual(cfg["thresholds"], {"bot": 99.9, "suspect": 40})
        r = scoring.score_session(load("T-001"), cfg)
        f = {x["id"]: x for x in r["fired"]}
        self.assertNotIn("webdriver", f)
        self.assertEqual(f["ua_headless"]["weight"], 10)
        self.assertEqual(r["thresholds"]["bot"], 99.9)


if __name__ == "__main__":
    unittest.main()

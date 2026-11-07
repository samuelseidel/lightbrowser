"""Tests for the wave-2 rules (ADR 0043): referer_context_mismatch, declared
agents, fleet_fingerprint_reuse, beh_path_repeat, beh_click_offset_repeat,
worker_measure_mismatch, fonts_overclaim and webrtc_candidates.

    cd tests/detect-bench && python3 -m unittest -v test_rules_wave2

Ground truth for "must not fire": the real-Chrome-shaped sessions in
fixtures/ (os_*_chrome.json, T-0xx) and three sessions recorded from stock
Chrome 153 (headless=new over CDP, loopback, rtx-dev): chrome153_browse.json
(normal browsing), chrome153_t010.json (the T-010 flow) and
chrome153_referer_case.json (the one contradiction stock Chrome sends: a
driver-set Referer on a user-initiated navigation).
"""

from __future__ import annotations

import copy
import itertools
import json
import math
import time
import unittest

import scoring
import shapes
import sigagent
from test_scoring import FIX, clean_session, fired, load, os_session


def chrome_browse() -> dict:
    s = load("chrome153_browse")
    s["created"] = time.time() - 60
    return s


def real_chrome_sessions():
    """(name, session) of every real-Chrome-shaped session the rules must stay quiet on."""
    out = [("chrome153_browse", chrome_browse()), ("chrome153_t010", load("chrome153_t010"))]
    out += [(f"os_{n}", os_session(n)) for n in ("linux", "windows", "mac")]
    out += [(n, load(n)) for n in ("T-001", "T-003", "T-020")]
    out.append(("clean_session", clean_session()))
    return out


def rule_detail(rule: str, s: dict, cfg=None, fleet=None):
    cfg = cfg or scoring.load_config()
    if fleet is not None:
        cfg = dict(cfg, _fleet=fleet)
    r = next(x for x in scoring.RULES if x["id"] == rule)
    return r["check"](s, cfg)


def trace_session(pages: list[list[dict]], sid: str = "a" * 16) -> dict:
    """A session whose pages carry the given event traces."""
    return {"sid": sid, "created": time.time() - 60, "documents": [], "requests": [], "beacons": 1,
            "user_agents": [], "tls": [],
            "pages": {f"p{i}": {"url": f"/p{i}", "received": i, "probes": {}, "trace": ev}
                      for i, ev in enumerate(pages)}}


def rule_in_catalogue(rule: str, weight: int, cat: str):
    r = next(x for x in scoring.RULES if x["id"] == rule)
    return (r["weight"], r["cat"]) == (weight, cat)


class Catalogue(unittest.TestCase):
    def test_new_rules_are_catalogued(self):
        for rule, w, cat in [("referer_context_mismatch", 35, "server"), ("fleet_fingerprint_reuse", 30, "server"),
                             ("beh_path_repeat", 30, "behavior"), ("beh_click_offset_repeat", 30, "behavior"),
                             ("worker_measure_mismatch", 40, "automation"), ("fonts_overclaim", 25, "consistency"),
                             ("webrtc_candidates", 0, "consistency"), ("worker_measure", 0, "automation"),
                             ("webrtc_gather", 0, "consistency")]:
            with self.subTest(rule=rule):
                self.assertTrue(rule_in_catalogue(rule, w, cat))

    def test_real_chrome_fires_no_new_rule(self):
        cfg = scoring.load_config()
        cfg["weights"]["webrtc_candidates"] = 15
        for name, s in real_chrome_sessions():
            with self.subTest(session=name):
                got = {f["id"] for f in scoring.score_session(s, cfg)["fired"]}
                new = {"referer_context_mismatch", "fleet_fingerprint_reuse", "beh_path_repeat",
                       "beh_click_offset_repeat", "worker_measure_mismatch", "fonts_overclaim", "webrtc_candidates"}
                self.assertEqual(got & new, set())


# ---- referer_context_mismatch -------------------------------------------------

class RefererContext(unittest.TestCase):
    def detail(self, s):
        return rule_detail("referer_context_mismatch", s)

    def test_stock_chrome_browsing_is_quiet(self):
        s = chrome_browse()
        # The recording has typed navigation, link clicks, a GET form, reload, back,
        # forward, a POST form, a script navigation, service worker script fetches
        # and favicons: all with Referer and Sec-Fetch-* as Chrome sends them.
        self.assertGreater(len(s["documents"]), 8)
        self.assertTrue(all("sec_fetch_site" in r for r in s["requests"]))
        self.assertIn("serviceworker", {r["sec_fetch_dest"] for r in s["requests"]})
        self.assertIn("none", {d["headers"]["sec-fetch-site"] for d in s["documents"]})
        self.assertIsNone(self.detail(s))
        self.assertIsNone(self.detail(load("chrome153_t010")))

    def test_driver_set_referer_on_a_typed_navigation(self):
        d = self.detail(load("chrome153_referer_case"))
        self.assertIn("Sec-Fetch-Site none with Referer", d)
        self.assertIn("document /shop", d)

    def test_old_sessions_without_the_values_are_quiet(self):
        for name in ("T-001", "T-003", "T-020"):
            self.assertIsNone(self.detail(load(name)))
        for n in ("linux", "windows", "mac"):
            self.assertIsNone(self.detail(os_session(n)))

    def edited(self, edit):
        s = chrome_browse()
        edit(s)
        return self.detail(s)

    def req(self, s, kind, nth=0):
        return [r for r in s["requests"] if r["kind"] == kind][nth]

    def test_subresource_with_site_none(self):
        d = self.edited(lambda s: self.req(s, "css").update(sec_fetch_site="none"))
        self.assertIn("Sec-Fetch-Site none on a subresource", d)
        d = self.edited(lambda s: self.req(s, "beacon").update(sec_fetch_site="none"))
        self.assertIn("beacon /api/report", d)

    def test_same_origin_subresource_with_a_foreign_page_as_referer(self):
        # An origin-correct Referer that no page of the session has.
        d = self.edited(lambda s: self.req(s, "js").update(referer="http://127.0.0.1:19200/checkout"))
        self.assertIn("is not a page of this session", d)
        # The right page but a different query.
        d = self.edited(lambda s: self.req(s, "img").update(referer="http://127.0.0.1:19200/search?q=boots"))
        self.assertIn("is not a page of this session", d)

    def test_same_origin_subresource_without_referer(self):
        for kind in ("css", "js", "img", "favicon", "beacon", "form"):
            with self.subTest(kind=kind):
                d = self.edited(lambda s: self.req(s, kind).update(referer=None))
                self.assertIn("same-origin without Referer", d)

    def test_same_origin_request_with_another_origin_referer(self):
        d = self.edited(lambda s: self.req(s, "img").update(referer="https://shop.example.net/shop"))
        self.assertIn("same-origin with Referer https://shop.example.net/shop", d)
        d = self.edited(lambda s: s["documents"][2]["headers"].update(referer="http://127.0.0.1:19201/shop"))
        self.assertIn("document /news", d)

    def test_none_document_with_referer(self):
        d = self.edited(lambda s: s["documents"][0]["headers"].update(referer="http://127.0.0.1:19200/shop"))
        self.assertIn("document /shop: Sec-Fetch-Site none with Referer", d)

    def test_cross_site_requests_are_not_judged_here(self):
        """Only same-origin and none claims are checked: a cross-site request may carry any Referer."""
        def edit(s):
            self.req(s, "img").update(sec_fetch_site="cross-site", referer="https://elsewhere.example/page")
        self.assertIsNone(self.edited(edit))

    def test_rule_scores_server_category(self):
        s = load("chrome153_referer_case")
        r = scoring.score_session(s)
        f = {x["id"]: x for x in r["fired"]}
        self.assertEqual((f["referer_context_mismatch"]["weight"], f["referer_context_mismatch"]["cat"]), (35, "server"))


# ---- declared agents ----------------------------------------------------------

SIG_AGENT = '"https://signature-agent.example.com"'
SIG_INPUT = ('sig1=("@authority" "signature-agent");created=1735689600;keyid="ba3e64==";alg="ed25519";'
             'expires=1735693200;nonce="Zm9v";tag="web-bot-auth"')
SIG = "sig1=:dGVzdHNpZ25hdHVyZQ==:"
GOOD = {"signature-agent": SIG_AGENT, "signature-input": SIG_INPUT, "signature": SIG}


class SigAgent(unittest.TestCase):
    def test_well_formed(self):
        d, problems = sigagent.parse(GOOD)
        self.assertEqual(problems, [])
        self.assertEqual(d["agent"], "https://signature-agent.example.com")
        self.assertEqual((d["label"], d["keyid"], d["created"], d["expires"]), ("sig1", "ba3e64==", 1735689600, 1735693200))
        self.assertEqual((d["alg"], d["tag"], d["nonce"]), ("ed25519", "web-bot-auth", "Zm9v"))
        self.assertEqual(d["components"], ["@authority", "signature-agent"])
        self.assertTrue(d["covers_agent"])
        self.assertIs(d["verified"], False)

    def test_forms_of_signature_agent(self):
        for value in ('"https://a.example/dir"', "https://a.example", 'sig1="https://a.example/x"',
                      'other="https://b.example", sig1="https://a.example"'):
            with self.subTest(value=value):
                d, _ = sigagent.parse(dict(GOOD, **{"signature-agent": value}))
                self.assertIsNotNone(d)
                self.assertTrue(d["agent"].startswith("https://a.example"))
        d, _ = sigagent.parse(dict(GOOD, **{"signature-agent": '"http://127.0.0.1:8080"'}))
        self.assertEqual(d["agent"], "http://127.0.0.1:8080")

    def test_optional_and_multiple_labels(self):
        no_expires = SIG_INPUT.replace(";expires=1735693200", "")
        d, _ = sigagent.parse(dict(GOOD, **{"signature-input": no_expires}))
        self.assertIsNone(d["expires"])
        # The first label has no created and is skipped; the second is the declaration.
        d, _ = sigagent.parse(dict(GOOD, **{"signature-input": 'bad=("@method");keyid="k", ' + SIG_INPUT.replace("sig1", "sig2"),
                                            "signature": "bad=:YQ==:, sig2=:YWJj:"}))
        self.assertEqual(d["label"], "sig2")

    def test_absent_is_not_a_declaration(self):
        self.assertEqual(sigagent.parse({}), (None, []))
        self.assertEqual(sigagent.parse({"user-agent": "x"}), (None, []))

    def test_malformed_is_not_accepted(self):
        bad = {
            "missing signature": {"signature-agent": SIG_AGENT, "signature-input": SIG_INPUT},
            "missing agent": {"signature-input": SIG_INPUT, "signature": SIG},
            "missing input": {"signature-agent": SIG_AGENT, "signature": SIG},
            "unbalanced input": dict(GOOD, **{"signature-input": 'sig1=("@authority";created=1;keyid="k"'}),
            "input not a dictionary": dict(GOOD, **{"signature-input": "garbage"}),
            "input member not a list": dict(GOOD, **{"signature-input": 'sig1="abc";created=1;keyid="k"'}),
            "no keyid": dict(GOOD, **{"signature-input": 'sig1=("@authority");created=1735689600'}),
            "keyid not a string": dict(GOOD, **{"signature-input": 'sig1=("@authority");created=1;keyid=abc'}),
            "created not an integer": dict(GOOD, **{"signature-input": 'sig1=("@authority");created="1";keyid="k"'}),
            "created decimal": dict(GOOD, **{"signature-input": 'sig1=("@authority");created=1.5;keyid="k"'}),
            "no created": dict(GOOD, **{"signature-input": 'sig1=("@authority");keyid="k"'}),
            "expires before created": dict(GOOD, **{"signature-input": 'sig1=("@authority");created=9;keyid="k";expires=3'}),
            "component not a string": dict(GOOD, **{"signature-input": "sig1=(authority);created=1;keyid=\"k\""}),
            "signature not base64": dict(GOOD, **{"signature": "sig1=:!!!!:"}),
            "signature not a byte sequence": dict(GOOD, **{"signature": 'sig1="abc"'}),
            "signature unterminated": dict(GOOD, **{"signature": "sig1=:YWJj"}),
            "signature empty": dict(GOOD, **{"signature": "sig1=::"}),
            "label mismatch": dict(GOOD, **{"signature": "sig2=:YWJj:"}),
            "agent not a url": dict(GOOD, **{"signature-agent": '"not a url"'}),
            "agent ftp": dict(GOOD, **{"signature-agent": '"ftp://a.example"'}),
            "agent javascript": dict(GOOD, **{"signature-agent": '"javascript:alert(1)"'}),
            "agent with credentials": dict(GOOD, **{"signature-agent": '"https://u:p@a.example"'}),
            "agent unquoted with query": dict(GOOD, **{"signature-agent": "https://a.example/?x=1"}),
            "agent integer": dict(GOOD, **{"signature-agent": "42"}),
            "agent empty string": dict(GOOD, **{"signature-agent": '""'}),
            "agent dictionary of two, none for the label": dict(GOOD, **{"signature-agent": 'a="https://a.example", b="https://b.example"'}),
            "trailing comma": dict(GOOD, **{"signature": SIG + ","}),
            "header too long": dict(GOOD, **{"signature-input": SIG_INPUT + ";x=\"" + "a" * 5000 + '"'}),
            "non-printable in string": dict(GOOD, **{"signature-agent": '"https://a.example/\x07"'}),
            "bad escape": dict(GOOD, **{"signature-agent": '"https://a.example/\\n"'}),
        }
        for why, headers in bad.items():
            with self.subTest(why=why):
                d, problems = sigagent.parse(headers)
                self.assertIsNone(d)
                self.assertTrue(problems)


def with_agent_headers(s: dict, headers: dict) -> dict:
    s = copy.deepcopy(s)
    s["documents"][0]["headers"].update(headers)
    return s


class DeclaredAgentVerdict(unittest.TestCase):
    def test_declared_agent_replaces_the_bot_verdict(self):
        base = scoring.score_session(load("T-001"))
        self.assertEqual(base["verdict"], "bot")
        self.assertIsNone(base["agent_declared"])
        r = scoring.score_session(with_agent_headers(load("T-001"), GOOD))
        self.assertEqual(r["verdict"], "declared_agent")
        self.assertEqual(r["score_verdict"], "bot")
        self.assertEqual(r["score"], base["score"])  # a class, not a score
        a = r["agent_declared"]
        self.assertEqual((a["agent"], a["keyid"], a["created"], a["expires"]),
                         ("https://signature-agent.example.com", "ba3e64==", 1735689600, 1735693200))
        self.assertIs(a["verified"], False)
        self.assertEqual(a["requests"], 1)

    def test_it_also_applies_to_a_session_that_scores_human(self):
        r = scoring.score_session(with_agent_headers(clean_session(), GOOD))
        self.assertEqual((r["verdict"], r["score_verdict"], r["score"]), ("declared_agent", "human", 0))

    def test_malformed_headers_leave_the_verdict_alone(self):
        s = with_agent_headers(load("T-001"), dict(GOOD, **{"signature": "sig1=:!!!:"}))
        r = scoring.score_session(s)
        self.assertEqual(r["verdict"], "bot")
        self.assertIsNone(r["agent_declared"])
        self.assertTrue(any("base64" in p for p in r["agent_rejected"]), r["agent_rejected"])

    def test_headers_on_subresources_count_too(self):
        s = clean_session()
        s["requests"][0]["agent"] = dict(GOOD)
        s["requests"][1]["agent"] = dict(GOOD)
        s["requests"][2]["agent"] = {"signature": SIG}  # incomplete: not counted
        a = scoring.declared_agent(s)
        self.assertEqual(a["requests"], 2)

    def test_sessions_without_headers_have_no_agent(self):
        for name, s in real_chrome_sessions():
            with self.subTest(session=name):
                r = scoring.score_session(s)
                self.assertIsNone(r["agent_declared"])
                self.assertNotEqual(r["verdict"], "declared_agent")
                self.assertNotIn("agent_rejected", r)


# ---- fleet_fingerprint_reuse --------------------------------------------------

def clone(s: dict, sid: str, created: float | None = None) -> dict:
    c = copy.deepcopy(s)
    c["sid"] = sid
    c["created"] = created if created is not None else s["created"]
    return c


def probe_of(s: dict, pid: str) -> dict:
    return next(pg["probes"][pid] for pg in sorted(s["pages"].values(), key=lambda p: p["received"]) if pid in pg["probes"])


class FleetReuse(unittest.TestCase):
    def fleet_of(self, n: int, base=None, cfg=None):
        base = base or os_session("windows")
        sessions = [clone(base, f"{i:016x}") for i in range(1, n + 1)]
        return sessions, scoring.fleet_index(sessions, cfg or scoring.load_config())

    def test_three_sessions_with_one_fingerprint_fire(self):
        sessions, idx = self.fleet_of(3)
        self.assertEqual(len(idx["fingerprint"]), 1)
        self.assertEqual(len(next(iter(idx["fingerprint"].values()))), 3)
        for s in sessions:
            r = scoring.score_session(s, fleet=idx)
            f = {x["id"]: x for x in r["fired"]}
            self.assertIn("fleet_fingerprint_reuse", f)
            self.assertEqual((f["fleet_fingerprint_reuse"]["weight"], f["fleet_fingerprint_reuse"]["cat"]), (30, "server"))
            self.assertIn("3 retained sessions share device fingerprint", f["fleet_fingerprint_reuse"]["detail"])

    def test_fewer_than_the_threshold_never_fires(self):
        for n in (1, 2):
            sessions, idx = self.fleet_of(n)
            for s in sessions:
                self.assertNotIn("fleet_fingerprint_reuse", fired(scoring.score_session(s, fleet=idx)))

    def test_single_session_scoring_is_unchanged(self):
        for name, s in real_chrome_sessions():
            with self.subTest(session=name):
                alone = scoring.score_session(s)
                with_fleet = scoring.score_session(s, fleet=scoring.fleet_index([s]))
                self.assertEqual(alone["score"], with_fleet["score"])
                self.assertEqual(alone["fired"], with_fleet["fired"])
        # without a fleet the rule is quiet even if the caller has many sessions
        sessions, _ = self.fleet_of(5)
        self.assertNotIn("fleet_fingerprint_reuse", fired(scoring.score_session(sessions[0])))

    def test_threshold_is_configurable(self):
        cfg = scoring.load_config()
        cfg["fleet_threshold"] = 2
        sessions, idx = self.fleet_of(2, cfg=cfg)
        self.assertIn("fleet_fingerprint_reuse", fired(scoring.score_session(sessions[0], cfg, idx)))
        cfg["fleet_threshold"] = 4
        sessions, idx = self.fleet_of(3, cfg=cfg)
        self.assertNotIn("fleet_fingerprint_reuse", fired(scoring.score_session(sessions[0], cfg, idx)))

    def test_different_devices_do_not_group(self):
        sessions = [clone(os_session(n), f"{i:016x}") for i, n in enumerate(("linux", "windows", "mac"), 1)]
        sessions += [clone(os_session("windows"), "f" * 16)]
        idx = scoring.fleet_index(sessions)
        self.assertEqual(sorted(len(v) for v in idx["fingerprint"].values()), [1, 1, 2])
        for s in sessions:
            self.assertNotIn("fleet_fingerprint_reuse", fired(scoring.score_session(s, fleet=idx)))

    def test_each_stable_probe_changes_the_hash(self):
        base = chrome_browse()  # has every stable probe, WebGL included
        h0 = scoring.fleet_hash(scoring.fleet_parts(base))

        def probe_edit(pid, fn):
            def edit(s):
                for pg in s["pages"].values():
                    if pid in pg["probes"]:
                        fn(pg["probes"][pid]["value"])
            return edit

        edits = {
            "canvas": probe_edit("canvas", lambda v: v.update(hash="deadbeef")),
            "webgl": probe_edit("webgl_params", lambda v: v.update(paramHash="deadbeef")),
            "audio": probe_edit("audio", lambda v: v.update(hash="deadbeef")),
            "fonts": probe_edit("fonts", lambda v: v.update(hash="deadbeef")),
            "screen": probe_edit("os_surface", lambda v: v["screen"].update(width=v["screen"]["width"] + 1)),
            "tz": lambda s: [pg["probes"]["timezone_utc"].update(value="Asia/Tokyo") for pg in s["pages"].values()
                             if "timezone_utc" in pg["probes"]],
            "languages": probe_edit("languages", lambda v: v.update(languages=["de-DE"])),
            "ua": lambda s: s["documents"][0]["headers"].update({"user-agent": s["documents"][0]["headers"]["user-agent"] + " X"}),
        }
        for what, edit in edits.items():
            with self.subTest(changed=what):
                s = copy.deepcopy(base)
                edit(s)
                self.assertNotEqual(scoring.fleet_hash(scoring.fleet_parts(s)), h0)
        # Things that vary per session do not: session id, timing, window geometry.
        s = clone(base, "9" * 16, created=1.0)
        for pg in s["pages"].values():
            if "os_surface" in pg["probes"]:
                pg["probes"]["os_surface"]["value"]["screen"].update(innerWidth=1, innerHeight=2, screenX=9, screenY=9)
        self.assertEqual(scoring.fleet_hash(scoring.fleet_parts(s)), h0)

    def test_sessions_with_too_few_probes_have_no_fingerprint(self):
        s = trace_session([[]])
        self.assertIsNone(scoring.fleet_parts(s))
        t1 = load("T-001")
        self.assertIsNotNone(scoring.fleet_parts(t1))
        self.assertNotIn("fleet_fingerprint_reuse", fired(scoring.score_session(s, fleet=scoring.fleet_index([s] * 3))))

    def test_window_counts_only_sessions_close_in_time(self):
        cfg = scoring.load_config()
        cfg["fleet_window_s"] = 60
        base = os_session("windows")
        spread = [clone(base, f"{i:016x}", created=1000.0 + i * 600) for i in range(1, 4)]
        idx = scoring.fleet_index(spread, cfg)
        self.assertNotIn("fleet_fingerprint_reuse", fired(scoring.score_session(spread[0], cfg, idx)))
        close = [clone(base, f"{i:016x}", created=1000.0 + i * 10) for i in range(1, 4)]
        idx = scoring.fleet_index(close, cfg)
        self.assertIn("fleet_fingerprint_reuse", fired(scoring.score_session(close[0], cfg, idx)))

    def test_the_recorded_sessions_group_only_where_one_device_made_them(self):
        """T-001, T-003 and T-020 were recorded with the same browser on the same
        machine, so they are three sessions of one device and do group (a real
        user who starts a fresh session three times looks the same: see the
        README's caveat). Every other fixture is a different device."""
        named = real_chrome_sessions()
        sessions = [clone(s, f"{i:016x}") for i, (_, s) in enumerate(named, 1)]
        idx = scoring.fleet_index(sessions)
        hits = {name for (name, _), s in zip(named, sessions)
                if "fleet_fingerprint_reuse" in fired(scoring.score_session(s, fleet=idx))}
        self.assertEqual(hits, {"T-001", "T-003", "T-020"})
        without_them = [s for (name, _), s in zip(named, sessions) if name not in hits]
        idx = scoring.fleet_index(without_them)
        for s in without_them:
            self.assertNotIn("fleet_fingerprint_reuse", fired(scoring.score_session(s, fleet=idx)))


# ---- pointer strokes ----------------------------------------------------------

def synthetic_pages() -> list[list[dict]]:
    t = json.loads((FIX / "synthetic-human-trace.json").read_text())
    return [copy.deepcopy(p["events"]) for p in t["pages"]]


def moves(points, t0=0.0, dt=16.0, trusted=1):
    return [{"t": round(t0 + i * dt, 1), "type": "mousemove", "tr": trusted, "x": x, "y": y}
            for i, (x, y) in enumerate(points)]


def arc(n=40, length=300.0, bend=24.0, phase=0.0):
    """A smooth curved stroke from (100, 400) going right: a sine bow plus a skew."""
    pts = []
    for i in range(n):
        u = i / (n - 1)
        pts.append((100 + length * u, 400 + bend * math.sin(math.pi * u + phase) + 0.2 * bend * math.sin(3 * math.pi * u)))
    return pts


def transform(points, dx=0.0, dy=0.0, scale=1.0, angle=0.0, noise=0.0, seed=1):
    import random
    rnd = random.Random(seed)
    x0, y0 = points[0]
    c, s = math.cos(angle), math.sin(angle)
    out = []
    for x, y in points:
        vx, vy = (x - x0) * scale, (y - y0) * scale
        px = x0 + dx + c * vx - s * vy + rnd.uniform(-noise, noise)
        py = y0 + dy + s * vx + c * vy + rnd.uniform(-noise, noise)
        out.append((round(px), round(py)))
    return out


class PathRepeat(unittest.TestCase):
    def detail(self, s):
        return rule_detail("beh_path_repeat", s)

    def test_catalogue_entry(self):
        self.assertTrue(rule_in_catalogue("beh_path_repeat", 30, "behavior"))

    def test_synthetic_human_trace_is_quiet(self):
        s = trace_session(synthetic_pages())
        self.assertGreaterEqual(len([n for n in map(shapes.normalise, shapes.pointer_strokes(s)) if n and n["informative"]]), 3)
        self.assertIsNone(self.detail(s))

    def test_lightbrowser_human_runs_are_quiet(self):
        runs = json.loads((FIX / "lb_human_traces.json").read_text())["runs"]
        for name, run in runs.items():
            with self.subTest(run=name):
                self.assertIsNone(self.detail(trace_session([p["events"] for p in run["pages"]])))

    def test_recorded_fixtures_are_quiet(self):
        for name, s in real_chrome_sessions():
            with self.subTest(session=name):
                self.assertIsNone(self.detail(s))

    def test_replayed_stroke_fires(self):
        pages = synthetic_pages()
        strokes = [st for st in shapes.pointer_strokes(trace_session(pages))
                   if (n := shapes.normalise(st)) and n["informative"]]
        self.assertTrue(strokes)
        original = max(strokes, key=lambda st: shapes.normalise(st)["bend"])
        # The same stroke again, 5 s later, 220 px right and 90 px down, on the last page.
        last = pages[-1]
        t_end = max(e["t"] for e in last)
        replay = [{"t": round(t_end + 5000 + (t - original[0][2]), 1), "type": "mousemove", "tr": 1,
                   "x": x + 220, "y": y + 90} for x, y, t in original]
        last.extend(replay)
        d = self.detail(trace_session(pages))
        self.assertIsNotNone(d)
        self.assertIn("repeated stroke pair", d)

    def test_replay_across_pages_scale_rotation_and_rounding(self):
        a = arc()
        for label, kw in {"shifted": dict(dx=300, dy=-120), "scaled": dict(scale=1.5, dx=40),
                          "rotated": dict(angle=0.5, dy=60), "scaled+rotated+noise": dict(scale=0.8, angle=-0.4, noise=0.4)}.items():
            with self.subTest(replay=label):
                first = moves(transform(a), t0=1000)
                again = moves(transform(a, **kw), t0=1000)
                self.assertIsNotNone(self.detail(trace_session([first, again])), label)

    def test_two_takes_of_the_same_gesture_by_a_person_are_not_a_replay(self):
        a = arc()
        # Second take: the same plan, but the bend is 25 % deeper and the path wobbles 3 px.
        second = [(x, y + 0.25 * (y - 400) + 3 * math.sin(i / 3.0)) for i, (x, y) in enumerate(a)]
        s = trace_session([moves(transform(a), t0=1000), moves(transform(second, dx=50, dy=80), t0=1000)])
        self.assertIsNone(self.detail(s))

    def test_straight_strokes_carry_no_shape(self):
        line = [(100 + 7.5 * i, 400 + 0.1 * i) for i in range(40)]
        s = trace_session([moves(transform(line), t0=1000), moves(transform(line, dy=200), t0=1000)])
        self.assertIsNone(self.detail(s))

    def test_short_untrusted_and_looped_strokes_are_ignored(self):
        small = [(100 + i, 400 + 3 * math.sin(i / 4.0)) for i in range(30)]  # 30 px
        self.assertIsNone(self.detail(trace_session([moves(transform(small), t0=0), moves(transform(small, dx=90), t0=0)])))
        a = arc()
        s = trace_session([moves(transform(a), t0=0), moves(transform(a, dx=300), t0=0, trusted=0)])
        self.assertIsNone(self.detail(s))
        loop = [(300 + 80 * math.cos(2 * math.pi * i / 39), 400 + 80 * math.sin(2 * math.pi * i / 39)) for i in range(40)]
        self.assertIsNone(self.detail(trace_session([moves(transform(loop), t0=0), moves(transform(loop, dx=500), t0=0)])))

    def test_pause_and_press_split_strokes(self):
        a = arc(n=60)
        one = moves(transform(a), t0=0)
        # One long stroke, then the same shape as two halves of 30 events would not match the whole.
        self.assertEqual(len(shapes.pointer_strokes(trace_session([one]))), 1)
        paused = moves(transform(a)[:30], t0=0) + moves(transform(a)[30:], t0=5000)
        self.assertEqual(len(shapes.pointer_strokes(trace_session([paused]))), 2)
        pressed = moves(transform(a)[:30], t0=0) + [{"t": 500.0, "type": "mousedown", "tr": 1, "x": 0, "y": 0}] \
            + moves(transform(a)[30:], t0=530)
        self.assertEqual(len(shapes.pointer_strokes(trace_session([pressed]))), 2)

    def test_shape_hash_groups_a_replayed_stroke(self):
        a = arc()
        h1 = shapes.session_shape_hashes(trace_session([moves(transform(a), t0=0)]))
        h2 = shapes.session_shape_hashes(trace_session([moves(transform(a, dx=500, dy=-60), t0=0)]))
        h3 = shapes.session_shape_hashes(trace_session([moves(transform(arc(bend=-30, phase=1.0)), t0=0)]))
        self.assertEqual(h1, h2)
        self.assertTrue(h1)
        self.assertNotEqual(h1, h3)


# ---- beh_click_offset_repeat ----------------------------------------------------

def click(tg, cx, cy, w=100.0, h=30.0, ti=0, tr=1, d=1):
    return {"t": 1000.0, "type": "click", "tr": tr, "x": 0, "y": 0, "b": 0, "d": d, "tg": tg, "ti": ti,
            "cx": cx, "cy": cy, "w": w, "h": h, "f": 1}


def corner(tg, ox, oy, w, h, **kw):
    """A click `ox, oy` px from the target's top-left corner."""
    return click(tg, ox - w / 2, oy - h / 2, w, h, **kw)


class ClickOffsetRepeat(unittest.TestCase):
    def detail(self, s):
        return rule_detail("beh_click_offset_repeat", s)

    def test_catalogue_entry(self):
        self.assertTrue(rule_in_catalogue("beh_click_offset_repeat", 30, "behavior"))

    def test_fixed_offset_from_the_corner_on_different_targets(self):
        clicks = [corner("#a", 5, 5, 120, 40), corner("#b", 5.4, 4.8, 90, 30), corner("a", 5.2, 5.3, 60, 24, ti=2),
                  corner("a", 4.9, 5.6, 60, 24, ti=7)]
        d = self.detail(trace_session([clicks]))
        self.assertIn("4 clicks on 4 different targets", d)
        self.assertIn("top-left corner", d)

    def test_fixed_offset_from_the_centre(self):
        clicks = [click("#a", 3, -2, 120, 40), click("#b", 3.3, -2.2, 90, 30), click("#c", 2.8, -1.9, 70, 28),
                  click("#d", 3.1, -2.4, 300, 38)]
        d = self.detail(trace_session([clicks]))
        self.assertIn("from the target's centre", d)

    def test_targets_on_different_pages_count_as_different(self):
        pages = [[corner("#go", 7, 9, 100, 30)], [corner("#go", 7.2, 9.4, 100, 30)],
                 [corner("#go", 6.9, 9.1, 100, 30)], [corner("#go", 7.3, 9.3, 100, 30)]]
        self.assertIsNotNone(self.detail(trace_session(pages)))

    def test_three_targets_are_not_enough(self):
        clicks = [corner("#a", 5, 5, 120, 40), corner("#b", 5, 5, 90, 30), corner("#c", 5, 5, 60, 24)]
        self.assertIsNone(self.detail(trace_session([clicks])))

    def test_the_same_target_clicked_again_is_one_target(self):
        clicks = [corner("#a", 5, 5, 120, 40)] * 6
        self.assertIsNone(self.detail(trace_session([clicks])))

    def test_offsets_more_than_a_pixel_apart(self):
        clicks = [corner("#a", 5, 5, 120, 40), corner("#b", 8, 5, 90, 30), corner("#c", 5, 9, 60, 24),
                  corner("#d", 12, 14, 80, 30)]
        self.assertIsNone(self.detail(trace_session([clicks])))

    def test_the_centre_is_left_to_beh_click_center(self):
        clicks = [click(f"#t{i}", 0, 0, 100 + 10 * i, 30) for i in range(6)]
        self.assertIsNone(self.detail(trace_session([clicks])))
        clicks = [click(f"#t{i}", 0.4, -0.3, 100 + 10 * i, 30) for i in range(6)]
        self.assertIsNone(self.detail(trace_session([clicks])))

    def test_tiny_untrusted_and_keyboard_clicks_do_not_count(self):
        clicks = [corner("#a", 5, 5, 120, 40), corner("#b", 5, 5, 90, 30), corner("#c", 5, 5, 60, 24)]
        for extra in (corner("#d", 5, 5, 8, 8), corner("#d", 5, 5, 80, 30, tr=0), corner("#d", 5, 5, 80, 30, d=0)):
            self.assertIsNone(self.detail(trace_session([clicks + [extra]])))
        self.assertIsNotNone(self.detail(trace_session([clicks + [corner("#d", 5, 5, 80, 30)]])))

    def test_real_and_synthetic_traces_are_quiet(self):
        for name, s in real_chrome_sessions():
            with self.subTest(session=name):
                self.assertIsNone(self.detail(s))
        self.assertIsNone(self.detail(trace_session(synthetic_pages())))
        runs = json.loads((FIX / "lb_human_traces.json").read_text())["runs"]
        for name, run in runs.items():
            with self.subTest(run=name):
                s = trace_session([p["events"] for p in run["pages"]])
                n = sum(1 for p in run["pages"] for e in p["events"] if e["type"] == "click" and e.get("tr"))
                self.assertGreaterEqual(n, 6)  # a real flow with 6+ clicks on different targets
                self.assertIsNone(self.detail(s))


# ---- fonts_overclaim ------------------------------------------------------------

def add_fonts(s: dict, n: int) -> dict:
    for pg in s["pages"].values():
        p = pg["probes"].get("os_surface")
        if p:
            det = p["value"]["fonts"]["detected"]
            for i in range(n):
                det[f"Invented Face {i}"] = [900 + i, 80]
    return s


class FontsOverclaim(unittest.TestCase):
    def detail(self, s):
        return rule_detail("fonts_overclaim", s)

    def test_catalogue_entry(self):
        self.assertTrue(rule_in_catalogue("fonts_overclaim", 25, "consistency"))

    def test_ceilings(self):
        self.assertEqual(scoring.FONT_CEILING, {"windows": 18, "mac": 19, "linux": 22})

    def detected(self, s):
        return len(scoring.probe_value(s, "os_surface")["fonts"]["detected"])

    def test_real_installs_are_far_below_the_ceiling(self):
        counts = {n: self.detected(os_session(n)) for n in ("linux", "windows", "mac")}
        self.assertEqual(counts, {"linux": 16, "windows": 9, "mac": 13})
        for n, c in counts.items():
            self.assertLess(c, scoring.FONT_CEILING[n])
            self.assertIsNone(self.detail(os_session(n)))
        stock = chrome_browse()
        self.assertLessEqual(self.detected(stock), scoring.FONT_CEILING["linux"])
        self.assertIsNone(self.detail(stock))

    def test_overclaim_fires_just_past_the_ceiling(self):
        for n in ("linux", "windows", "mac"):
            with self.subTest(os=n):
                s = os_session(n)
                have = self.detected(s)
                at = scoring.FONT_CEILING[n]
                self.assertIsNone(self.detail(add_fonts(copy.deepcopy(s), at - have)))
                d = self.detail(add_fonts(copy.deepcopy(s), at - have + 1))
                self.assertIn(f"{at + 1} candidate fonts detected on {n}", d)

    def test_it_scores_as_consistency_25(self):
        s = add_fonts(os_session("windows"), 20)
        f = {x["id"]: x for x in scoring.score_session(s)["fired"]}
        self.assertEqual((f["fonts_overclaim"]["weight"], f["fonts_overclaim"]["cat"]), (25, "consistency"))

    def test_sessions_without_the_font_probe_are_quiet(self):
        for name in ("T-001", "T-003", "T-020"):
            self.assertIsNone(self.detail(load(name)))

    def test_derivation_matches_the_recorded_data(self):
        """The README derives the ceilings from the catalogue and the captures; check the inputs."""
        src = (FIX.parent / "sites" / "collector.js").read_text()
        import re
        cand = re.findall(r"'([^']+)'", re.search(r"const OS_FONTS = \[(.*?)\];", src, re.S).group(1))
        self.assertEqual(len(cand), 36)
        catalog = json.loads((FIX.parents[2] / "profiles" / "catalog" / "fonts.json").read_text())["sets"]
        per = {k: len([c for c in cand if c in v]) for k, v in catalog.items()}
        self.assertEqual(max(per["windows10"], per["windows11"]), 12)
        self.assertEqual(per["macos"], 13)
        self.assertEqual(max(per[k] for k in per if k not in ("windows10", "windows11", "macos")), 9)
        captures = sorted((FIX.parents[1] / "fixtures" / "os").glob("linux_*.json"))
        self.assertEqual(len(captures), 4)
        seen = []
        for path in captures:
            c = json.loads(path.read_text())["result"]["fonts"]
            seen.append(len([f for f in cand if f in c["detected"]]))
            # The captures probed 30 of the 36 candidates; the 6 left out are four fonts only
            # Windows or macOS ship and two portable ones (Roboto, Open Sans).
            self.assertEqual([f for f in cand if f not in c["against"]],
                             ["Segoe UI Variable Display", "Segoe Fluent Icons", "Lucida Grande", "Geneva", "Roboto", "Open Sans"])
        self.assertEqual(max(seen), 16)
        # 12 + 6, 13 + 6, 16 + 6
        self.assertEqual(scoring.FONT_CEILING, {"windows": 12 + 6, "mac": 13 + 6, "linux": 16 + 6})


# ---- worker_measure_mismatch ------------------------------------------------------

class WorkerMeasure(unittest.TestCase):
    def detail(self, s):
        return rule_detail("worker_measure_mismatch", s)

    def edited(self, edit, base=None):
        s = base or chrome_browse()
        for pg in s["pages"].values():
            p = pg["probes"].get("worker_measure")
            if p:
                edit(p["value"])
        return s

    def test_catalogue_entry(self):
        self.assertTrue(rule_in_catalogue("worker_measure_mismatch", 40, "automation"))

    def test_stock_chrome_shows_no_difference(self):
        s = chrome_browse()
        probes = [pg["probes"]["worker_measure"]["value"] for pg in s["pages"].values() if "worker_measure" in pg["probes"]]
        self.assertGreaterEqual(len(probes), 1)
        v = probes[0]
        self.assertEqual(len(v["window"]["measure"]), 8 * 5)
        self.assertEqual(set(v["window"]["measure"]), set(v["worker"]["measure"]))
        self.assertIsNone(v["worker"]["dpr"])  # Chrome workers have no devicePixelRatio
        self.assertIsNone(self.detail(s))
        self.assertIsNone(self.detail(load("chrome153_t010")))

    def test_each_kind_of_difference_fires(self):
        key = "sans-serif|1"
        edits = {
            "width": lambda v: v["worker"]["measure"][key].__setitem__(0, v["worker"]["measure"][key][0] + 0.5),
            "ascent": lambda v: v["worker"]["measure"][key].__setitem__(3, v["worker"]["measure"][key][3] + 1),
            "font box": lambda v: v["worker"]["measure"]["monospace|0"].__setitem__(6, 99),
            "missing key": lambda v: v["worker"]["measure"].pop("serif|2"),
            "language": lambda v: v["worker"].update(language="de-DE"),
            "languages": lambda v: v["worker"].update(languages=["en-US", "de"]),
            "locale": lambda v: v["worker"].update(locale="fr-FR"),
            "timeZone": lambda v: v["worker"].update(timeZone="Asia/Tokyo"),
            "dpr": lambda v: v["worker"].update(dpr=2),
        }
        for what, edit in edits.items():
            with self.subTest(difference=what):
                d = self.detail(self.edited(edit))
                self.assertIsNotNone(d)
                self.assertTrue(d.startswith("worker differs:"))

    def test_rounding_is_not_a_difference(self):
        def jitter(v):
            for k, m in v["worker"]["measure"].items():
                v["worker"]["measure"][k] = [x + 0.004 for x in m]
        self.assertIsNone(self.detail(self.edited(jitter)))

    def test_a_worker_dpr_equal_to_the_pages_is_fine(self):
        self.assertIsNone(self.detail(self.edited(lambda v: v["worker"].update(dpr=v["window"]["dpr"]))))

    def test_unsupported_timeout_and_missing_probe_are_quiet(self):
        self.assertIsNone(self.detail(self.edited(lambda v: v["worker"].pop("measure"))))
        for value in ({"supported": False}, {"timeout": True}, {"error": "worker error"}):
            s = self.edited(lambda v: None)
            for pg in s["pages"].values():
                if "worker_measure" in pg["probes"]:
                    pg["probes"]["worker_measure"]["value"] = value
            self.assertIsNone(self.detail(s))
        self.assertIsNone(self.detail(os_session("linux")))

    def test_it_scores_as_automation_40(self):
        s = self.edited(lambda v: v["worker"].update(language="de-DE"))
        f = {x["id"]: x for x in scoring.score_session(s)["fired"]}
        self.assertEqual((f["worker_measure_mismatch"]["weight"], f["worker_measure_mismatch"]["cat"]), (40, "automation"))

    def test_real_chrome_shapes_with_agreeing_worker_stay_quiet(self):
        for name in ("linux", "windows", "mac"):
            s = os_session(name)
            pg = list(s["pages"].values())[0]
            measure = {f"sans-serif|{i}": [10.0 + i, 0, 10.0 + i, 11, 3, 14, 4] for i in range(3)}
            env = {"language": "en-US", "languages": ["en-US", "en"], "locale": "en-US", "timeZone": "UTC", "supported": True}
            pg["probes"]["worker_measure"] = {"id": "worker_measure", "cat": "automation", "weight": 0, "suspicious": False,
                                              "value": {"window": dict(env, dpr=1, measure=measure),
                                                        "worker": dict(env, dpr=None, measure=copy.deepcopy(measure))}}
            with self.subTest(os=name):
                self.assertIsNone(self.detail(s))


# ---- webrtc_candidates ------------------------------------------------------------

class WebrtcCandidates(unittest.TestCase):
    def cfg(self):
        cfg = scoring.load_config()
        cfg["weights"]["webrtc_candidates"] = 15  # info until LightBrowser gathers srflx
        return cfg

    def detail(self, s):
        return rule_detail("webrtc_candidates", s, self.cfg())

    def edited(self, edit, base=None):
        s = base or chrome_browse()
        for pg in s["pages"].values():
            p = pg["probes"].get("webrtc_gather")
            if p:
                edit(p["value"])
        return s

    def test_catalogue_entry_is_info_for_now(self):
        self.assertTrue(rule_in_catalogue("webrtc_candidates", 0, "consistency"))

    def test_reference_is_stock_chrome(self):
        s = chrome_browse()
        v = next(pg["probes"]["webrtc_gather"]["value"] for pg in s["pages"].values() if "webrtc_gather" in pg["probes"])
        self.assertTrue(v["complete"])
        self.assertEqual(sorted(c["type"] for c in v["candidates"]), ["host", "srflx"])
        host = next(c for c in v["candidates"] if c["type"] == "host")
        srflx = next(c for c in v["candidates"] if c["type"] == "srflx")
        self.assertTrue(host["mdns"])
        self.assertEqual(srflx["address"], "127.0.0.1")
        self.assertEqual(srflx["port"], host["port"])
        self.assertLess(v["firstMs"], 100)
        self.assertLess(v["gatherMs"], 1000)
        self.assertIsNone(self.detail(s))
        self.assertIsNone(self.detail(load("chrome153_t010")))

    def test_each_departure_fires(self):
        def drop(t):
            return lambda v: v.update(candidates=[c for c in v["candidates"] if c["type"] != t])
        edits = {
            "RTCPeerConnection missing": (lambda v: v.update(supported=False, candidates=[]), "missing"),
            "no host": (drop("host"), "no host candidate"),
            "no srflx": (drop("srflx"), "no server-reflexive candidate"),
            "raw host address": (lambda v: [c.update(address="192.168.1.20", mdns=False) for c in v["candidates"] if c["type"] == "host"],
                                 "exposes 192.168.1.20"),
            "wrong srflx address": (lambda v: [c.update(address="203.0.113.7") for c in v["candidates"] if c["type"] == "srflx"],
                                    "not the responder's view"),
        }
        for what, (edit, text) in edits.items():
            with self.subTest(departure=what):
                d = self.detail(self.edited(edit))
                self.assertIsNotNone(d)
                self.assertIn(text, d)

    def test_without_a_stun_server_no_reflexive_candidate_is_expected(self):
        def edit(v):
            v.update(stun=None, candidates=[c for c in v["candidates"] if c["type"] == "host"])
        self.assertIsNone(self.detail(self.edited(edit)))

    def test_loopback_host_candidates_and_incomplete_gathering_are_quiet(self):
        def local(v):
            [c.update(address="127.0.0.1", mdns=False) for c in v["candidates"] if c["type"] == "host"]
        self.assertIsNone(self.detail(self.edited(local)))
        self.assertIsNone(self.detail(self.edited(lambda v: v.update(complete=False, timedOut=True, candidates=[]))))
        self.assertIsNone(self.detail(self.edited(lambda v: v.update(error="NotSupportedError", candidates=[]))))

    def test_old_sessions_are_quiet(self):
        for name in ("T-001", "T-003", "T-020"):
            self.assertIsNone(self.detail(load(name)))
        for n in ("linux", "windows", "mac"):
            self.assertIsNone(self.detail(os_session(n)))

    def test_info_rule_never_scores_and_is_listed(self):
        s = self.edited(lambda v: v.update(supported=False, candidates=[]))
        r = scoring.score_session(s)
        self.assertNotIn("webrtc_candidates", fired(r))
        self.assertIn("webrtc_candidates", [x["id"] for x in r["info_fired"]])
        r = scoring.score_session(s, self.cfg())
        self.assertIn("webrtc_candidates", fired(r))


if __name__ == "__main__":
    unittest.main()

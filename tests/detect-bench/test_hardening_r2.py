"""Round-2 fixes for the wave-2 rules (ADR 0043): hostile header and beacon
values must not stop the scoring of other sessions, the click-offset rule must
not read a parked pointer as a replay, the repeat searches are bounded, and the
WebRTC rule reads IPv6 addresses the way Chrome writes them.

    cd tests/detect-bench && python3 -m unittest -v test_hardening_r2
"""

from __future__ import annotations

import copy
import json
import unittest
from unittest import mock

import scoring
import shapes
import sigagent
from test_rules_wave2 import (GOOD, SIG, SIG_INPUT, arc, chrome_browse, clone, corner, click, moves, rule_detail,
                              transform, trace_session)
from test_scoring import fired, load
from test_server_wave2 import Base, put_session
import server


# ---- sigagent: digits that are not digits -----------------------------------------

# str.isdigit() is true for these, and int() on them raises a plain ValueError.
# They arrive as latin-1 header bytes (0xB2, 0xB3, 0xB9).
NOT_DIGITS = ["²", "³", "¹"]


class SigAgentHostile(unittest.TestCase):
    def inputs(self, created, extra=""):
        return dict(GOOD, **{"signature-input": f'sig1=("@method");keyid="k";created={created}{extra}'})

    def test_superscript_digits_are_a_rejection_not_an_exception(self):
        for ch in NOT_DIGITS:
            for why, created, extra in (("created", ch, ""), ("decimal", f"1.{ch}", ""), ("integer part", f"1{ch}", ""),
                                        ("expires", "1", f";expires={ch}"), ("other parameter", "1", f";n={ch}")):
                with self.subTest(char=ascii(ch), where=why):
                    d, problems = sigagent.parse(self.inputs(created, extra))
                    self.assertIsNone(d)
                    self.assertTrue(problems)
                    with self.assertRaises(sigagent.SFError) as cm:
                        sigagent.parse_dictionary(self.inputs(created, extra)["signature-input"])
                    self.assertIs(type(cm.exception), sigagent.SFError)

    def test_other_unicode_digits_are_not_numbers_either(self):
        # Not reachable from latin-1 bytes, but the parser is strict about it all the same:
        # int("１") is 1.
        for ch in ("１", "١", "①"):
            with self.subTest(char=ascii(ch)):
                d, problems = sigagent.parse(self.inputs(ch))
                self.assertIsNone(d)
                self.assertTrue(problems)

    def test_parse_never_raises(self):
        # Whatever a parser did not foresee is still a rejection.
        with mock.patch.object(sigagent, "_parse", side_effect=ValueError("boom")):
            d, problems = sigagent.parse(GOOD)
        self.assertIsNone(d)
        self.assertEqual(problems, ["unparseable: ValueError"])

    def test_implausible_times_are_not_accepted(self):
        for why, created, extra in (("negative created", "-5", ""), ("created past year 9999", "999999999999", ""),
                                    ("expires past year 9999", "1735689600", ";expires=999999999999"),
                                    ("negative expires", "1735689600", ";expires=-1")):
            with self.subTest(why=why):
                d, problems = sigagent.parse(self.inputs(created, extra))
                self.assertIsNone(d)
                self.assertIn("plausible" if "created" in why and "expires" not in why else "expires", " ".join(problems))
        d, _ = sigagent.parse(self.inputs("0", ";expires=253402300799"))
        self.assertEqual((d["created"], d["expires"]), (0, 253402300799))

    def test_the_declaration_is_recognised_not_trusted(self):
        # ADR 0043: a client that merely adds three well-formed headers gets the verdict
        # declared_agent, verified is false and score_verdict keeps what the score says.
        s = copy.deepcopy(load("T-001"))
        s["documents"][0]["headers"].update({"signature-agent": GOOD["signature-agent"],
                                             "signature-input": SIG_INPUT, "signature": SIG})
        r = scoring.score_session(s)
        self.assertEqual(r["verdict"], "declared_agent")
        self.assertIs(r["agent_declared"]["verified"], False)
        self.assertEqual(r["score_verdict"], "bot")


class SigAgentServer(Base):
    def test_a_latin1_superscript_in_signature_input_does_not_break_the_endpoints(self):
        victim = self.start_session()[1]
        bad = 'sig1=("@method");keyid="k";created=²'
        cookie, sid, _ = self.start_session({"Signature-Agent": GOOD["signature-agent"], "Signature": GOOD["signature"],
                                             "Signature-Input": bad})
        for path in (f"/api/score/{sid}", f"/api/score/{victim}", "/api/results", "/api/clusters"):
            with self.subTest(path=path):
                r, body = self.req("GET", path)
                self.assertEqual(r.status, 200)
                json.loads(body)
        r = self.score(sid)
        self.assertIsNone(r["agent_declared"])
        self.assertTrue(r["agent_rejected"])
        self.assertNotEqual(r["verdict"], "declared_agent")


# ---- hostile beacon values ---------------------------------------------------------

def with_probe(s: dict, pid: str, fn) -> dict:
    s = copy.deepcopy(s)
    for pg in s["pages"].values():
        p = pg["probes"].get(pid)
        if p:
            fn(p["value"])
    return s


def hostile_fonts(value):
    """The os_surface probe a hostile client sends instead of the collector's."""
    def edit(v):
        v["fonts"]["detected"] = {"Arial": value}
    return edit


class HostileValues(unittest.TestCase):
    VALUES = [5, None, "x", 1.5, True, {"a": 1}, [[1], 2]]

    def test_fleet_parts_survive_odd_font_values(self):
        for v in self.VALUES:
            with self.subTest(value=v):
                s = with_probe(chrome_browse(), "os_surface", hostile_fonts(v))
                scoring.fleet_index([s, clone(s, "b" * 16)])  # must not raise
                scoring.score_session(s, fleet=scoring.fleet_index([s]))

    def test_fleet_parts_survive_odd_os_surface_shapes(self):
        for odd in (None, 5, "x", [], {"fonts": 5}, {"fonts": None}, {"fonts": {"detected": 5}}, {"fonts": {"detected": []}},
                    {"screen": 7}):
            with self.subTest(os_surface=odd):
                s = copy.deepcopy(chrome_browse())
                for pg in s["pages"].values():
                    if "os_surface" in pg["probes"]:
                        pg["probes"]["os_surface"]["value"] = odd
                scoring.safe_fleet_parts(s)
                scoring.fleet_index([s])

    def test_an_unreadable_session_has_no_parts_and_does_not_hide_the_others(self):
        good = chrome_browse()
        bad = clone(good, "c" * 16)
        for pg in bad["pages"].values():
            if "canvas" in pg["probes"]:
                pg["probes"]["canvas"]["value"] = {"hash": object()}  # not JSON: unhashable into parts
        self.assertIsNone(scoring.safe_fleet_parts(bad))
        idx = scoring.fleet_index([good, bad, clone(good, "d" * 16), clone(good, "e" * 16)])
        self.assertEqual([len(v) for v in idx["fingerprint"].values()], [3])

    def test_fonts_unchanged_for_real_values(self):
        # The fix keeps real sessions' hashes: fleets formed before it still group.
        s = chrome_browse()
        parts = scoring.fleet_parts(s)
        det = next(pg["probes"]["os_surface"]["value"]["fonts"]["detected"] for pg in s["pages"].values()
                   if "os_surface" in pg["probes"])
        self.assertEqual(parts["fonts"][1], sorted((k, list(v)) for k, v in det.items()))

    def test_click_checks_ignore_values_of_the_wrong_type(self):
        for key, v in (("w", "x"), ("h", None), ("d", "x"), ("cx", [1]), ("cy", {}), ("x", "a"), ("y", "b"),
                       ("w", float("nan")), ("cx", float("inf"))):
            with self.subTest(key=key, value=v):
                ev = [corner(f"#t{i}", 5, 5, 100 + i, 30) for i in range(5)]
                ev[2][key] = v
                scoring.click_offset_groups(trace_session([ev]))
                rule_detail("beh_click_offset_repeat", trace_session([ev]))

    def test_trace_events_that_are_not_events(self):
        s = trace_session([[1, None, "x", [], {"type": 5, "tr": 1}, {"type": "click", "tr": 1}]])
        scoring.event_counts(s)
        scoring.score_session(s)
        scoring.fleet_index([s])

    def test_webrtc_ignores_values_of_the_wrong_type(self):
        for key, v in (("stun", 7), ("stun", ["a"]), ("candidates", 5), ("candidates", [1, None, "x"])):
            with self.subTest(key=key, value=v):
                s = with_probe(chrome_browse(), "webrtc_gather", lambda val, k=key, x=v: val.update({k: x}))
                rule_detail("webrtc_candidates", s)
        s = with_probe(chrome_browse(), "webrtc_gather",
                       lambda val: [c.update(address=7) for c in val["candidates"]] and val.update(stun="127.0.0.1:9"))
        rule_detail("webrtc_candidates", s)  # address an int, srflx present

    def test_a_rule_that_raises_is_reported_and_the_rest_still_score(self):
        def boom(s, cfg):
            raise TypeError("hostile")
        rules = [{"id": "boom_rule", "weight": 50, "cat": "server", "check": boom, "why": "test"}] + list(scoring.RULES)
        with mock.patch.object(scoring, "RULES", rules):
            r = scoring.score_session(load("T-001"))
        self.assertEqual(r["check_errors"], ["boom_rule"])
        self.assertEqual(r["verdict"], "bot")
        self.assertNotIn("check_errors", scoring.score_session(load("T-001")))


class HostileServer(Base):
    def setUp(self):
        server.Handler.store.sessions.clear()

    def api(self, path):
        r, b = self.req("GET", path)
        self.assertEqual(r.status, 200, path)
        return json.loads(b)

    def test_one_session_with_a_hostile_probe_does_not_break_the_others(self):
        victim = put_session(clone(chrome_browse(), "1" * 16))
        for i, v in enumerate((5, None, "x")):
            put_session(clone(with_probe(chrome_browse(), "os_surface", hostile_fonts(v)), f"{i + 2:016x}"))
        self.assertEqual(self.api(f"/api/score/{victim}")["sid"], victim)
        self.assertEqual(len(self.api("/api/results")["sessions"]), 4)
        self.api("/api/clusters")

    def test_a_session_nothing_can_score_is_listed_as_unscorable(self):
        victim = put_session(clone(chrome_browse(), "1" * 16))
        bad = clone(chrome_browse(), "2" * 16)
        bad["pages"] = 5  # no probe reader copes with this
        put_session(bad)
        rows = {x["sid"]: x for x in self.api("/api/results")["sessions"]}
        self.assertEqual(rows["2" * 16]["verdict"], "unscorable")
        self.assertNotEqual(rows[victim]["verdict"], "unscorable")
        self.assertEqual(self.api("/api/score/" + "2" * 16)["error"], "AttributeError")
        self.api(f"/api/score/{victim}")
        self.api("/api/clusters")


# ---- click offsets: a parked pointer ----------------------------------------------

class ParkedPointer(unittest.TestCase):
    def test_paging_with_the_cursor_on_the_same_link_is_silent(self):
        # One trusted click per page on the same "Next" link (tag a, index 5), the pointer
        # not moved between page loads: five URLs, one place.
        pages = [[dict(corner("a", 13.4, 4.1, 92, 34, ti=5, x=612.0, y=288.0), t=1000.0 * i)] for i in range(5)]
        s = trace_session(pages)
        for i, p in enumerate(s["pages"].values()):
            p["url"] = f"/shop?page={i + 1}"
        self.assertIsNone(rule_detail("beh_click_offset_repeat", s))
        self.assertNotIn("beh_click_offset_repeat", fired(scoring.score_session(s)))

    def test_stock_chrome_with_a_parked_pointer_recorded_on_rtx_dev(self):
        # chrome153_parked_pointer.json: five typed navigations to /shop?page=N, one real
        # click at the same screen position on the nav "News" link of each page.
        s = load("chrome153_parked_pointer")
        clicks = [(pg["url"], e["x"], e["y"], e["cx"], e["cy"], e["tg"], e["ti"]) for pg in s["pages"].values()
                  for e in pg["trace"] if e["type"] == "click" and e["tr"]]
        self.assertEqual(len(clicks), 5)
        self.assertEqual(len({c[0] for c in clicks}), 5)   # five URLs ...
        self.assertEqual(len({c[1:] for c in clicks}), 1)  # ... one pointer position, one link, one offset
        self.assertNotEqual(clicks[0][3:5], (0, 0))        # and not the centre
        self.assertIsNone(rule_detail("beh_click_offset_repeat", s))
        self.assertIsNone(scoring.click_offset_groups(s))
        self.assertNotIn("beh_click_offset_repeat", fired(scoring.score_session(s)))

    def test_distinct_pointer_positions_still_fire(self):
        pages = [[corner("a", 13.4, 4.1, 92, 34, ti=5, x=612.0 + 40 * i, y=288.0)] for i in range(5)]
        self.assertIsNotNone(rule_detail("beh_click_offset_repeat", trace_session(pages)))

    def test_real_sessions_stay_quiet(self):
        for name in ("T-001", "T-003", "T-020", "chrome153_browse", "chrome153_t010"):
            with self.subTest(name=name):
                self.assertIsNone(rule_detail("beh_click_offset_repeat", load(name)))


# ---- bounded work -----------------------------------------------------------------

class Bounded(unittest.TestCase):
    def bendy(self, n):
        """n informative strokes, each of its own shape, a second apart."""
        out = []
        for i in range(n):
            out += moves(transform(arc(bend=14 + 0.37 * i, phase=0.011 * i), dx=3 * i), t0=1000.0 * i)
        return out

    def test_stroke_pairs_compared_are_capped(self):
        s = trace_session([self.bendy(shapes.MAX_STROKES + 40)])
        calls = []
        real = shapes.same_stroke
        with mock.patch.object(shapes, "same_stroke", side_effect=lambda a, b: calls.append(1) or real(a, b)):
            shapes.repeated(s)
        n = shapes.MAX_STROKES
        self.assertLessEqual(len(calls), n * (n - 1) // 2)
        self.assertGreater(len(calls), 0)

    def test_a_replay_within_the_cap_is_still_found(self):
        a = arc(bend=30)
        ev = self.bendy(20) + moves(transform(a), t0=60000.0) + moves(transform(a, dx=15, dy=9), t0=61000.0)
        self.assertTrue(shapes.repeated(trace_session([ev])))

    def test_clicks_compared_are_capped(self):
        clicks = [corner(f"#t{i}", 3 + 7 * (i % 11), 4 + 5 * (i // 11), 100 + i, 30) for i in range(40)]
        repeat = [corner(f"#r{i}", 21, 21, 140 + i, 40) for i in range(4)]
        with mock.patch.object(scoring, "MAX_CLICKS_COMPARED", 10):
            self.assertIsNone(scoring.click_offset_groups(trace_session([clicks + repeat])))
        self.assertIsNotNone(scoring.click_offset_groups(trace_session([clicks + repeat])))

    def test_hostile_sizes_finish_quickly(self):
        import time
        s = trace_session([self.bendy(shapes.MAX_STROKES * 4)])
        t = time.time()
        scoring.score_session(s)
        self.assertLess(time.time() - t, 20)
        clicks = [corner(f"#t{i}", 5, 5.4, 100 + i, 30) for i in range(400)]
        t = time.time()
        scoring.click_offset_groups(trace_session([clicks]))
        self.assertLess(time.time() - t, 20)


# ---- WebRTC on IPv6 loopback -------------------------------------------------------

class WebrtcIPv6(unittest.TestCase):
    def cfg(self):
        return scoring.load_config()

    def v6(self, address="[::1]", stun="[::1]:19219", host="[::1]"):
        def edit(v):
            v["stun"] = stun
            for c in v["candidates"]:
                if c["type"] == "srflx":
                    c["address"] = address
        return with_probe(chrome_browse(), "webrtc_gather", edit)

    def test_bracketed_ipv6_srflx_is_the_responders_view(self):
        self.assertIsNone(rule_detail("webrtc_candidates", self.v6(), self.cfg()))
        self.assertIsNone(rule_detail("webrtc_candidates", self.v6("::1"), self.cfg()))  # without brackets too

    def test_a_different_ipv6_address_still_fires(self):
        d = rule_detail("webrtc_candidates", self.v6("[::2]"), self.cfg())
        self.assertIn("not the responder's view", d)
        self.assertIn("::2", d)

    def test_bracketed_loopback_host_candidate_is_not_exposure(self):
        def edit(v):
            for c in v["candidates"]:
                if c["type"] == "host":
                    c.update(address="[::1]", mdns=False)
        self.assertIsNone(rule_detail("webrtc_candidates", with_probe(chrome_browse(), "webrtc_gather", edit), self.cfg()))

    def test_stock_chrome_recorded_on_ipv6_loopback_is_quiet(self):
        s = load("chrome153_ipv6")
        v = next(pg["probes"]["webrtc_gather"]["value"] for pg in s["pages"].values() if "webrtc_gather" in pg["probes"])
        self.assertTrue(v["stun"].startswith("[::1]:"))
        self.assertTrue(any(c["address"].startswith("[") for c in v["candidates"] if c["type"] == "srflx"))
        self.assertIsNone(rule_detail("webrtc_candidates", s, self.cfg()))
        r = scoring.score_session(s)
        self.assertNotIn("webrtc_candidates", [x["id"] for x in r["info_fired"]])
        for rule in ("referer_context_mismatch", "worker_measure_mismatch", "fonts_overclaim", "beh_path_repeat",
                     "beh_click_offset_repeat"):
            self.assertNotIn(rule, fired(r))


if __name__ == "__main__":
    unittest.main()

"""Unit tests for scoring.py, using sessions recorded by run.js.

    cd tests/detect-bench && python3 -m unittest -v

Fixtures were recorded with:
    node run.js --scenario T-001,T-003,T-020 \
        --trace fixtures/synthetic-human-trace.json --save-fixtures fixtures

fixtures/os_{linux,windows,mac}_chrome.json (OS rules) come from
tools/os_truth/bench_fixtures.py: Linux is a captured stock Chrome session,
Windows and macOS are not captured (see their `provenance`).
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
        names.insert(names.index("sec-ch-ua-platform") + 1, "Accept-Language")
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


OS_RULES = ["uach_platform_version", "uach_arch_gpu", "ch_high_entropy", "api_throws", "webgl_backend_os",
            "fonts_os_markers", "os_api_surface", "generic_fonts_os", "voices_os", "scrollbar_os", "emoji_render",
            "webgpu_os", "os_bluetooth", "codecs_os", "screen_os", "window_work_area", "wheel_os"]
OS_TRUTH = HERE.parent / "fixtures" / "os"


def os_cfg() -> dict:
    """Default config with the info-only OS rules at their target weights."""
    cfg = scoring.load_config()
    cfg["weights"].update({"codecs_os": 15, "screen_os": 15, "window_work_area": 15, "wheel_os": 20})
    return cfg


def os_session(name: str) -> dict:
    s = load(f"os_{name}_chrome")
    s["created"] = time.time() - 60
    return s


def set_probe(s: dict, pid: str, fn) -> dict:
    """Apply fn to the value of probe `pid` on every page that has it."""
    for pg in s["pages"].values():
        if pid in pg["probes"]:
            fn(pg["probes"][pid]["value"])
    return s


def os_fired(s: dict) -> dict:
    return {f["id"]: f["detail"] for f in scoring.score_session(s, os_cfg())["fired"] if f["id"] in OS_RULES}


class OsRules(unittest.TestCase):
    def fires(self, rule, os_name, *edits):
        s = os_session(os_name)
        for pid, fn in edits:
            if pid == "session":
                fn(s)
            else:
                set_probe(s, pid, fn)
        self.assertIn(rule, os_fired(s), f"{rule} did not fire on edited {os_name}")
        return s

    def quiet(self, rule, os_name, *edits):
        s = os_session(os_name)
        for pid, fn in edits:
            set_probe(s, pid, fn)
        self.assertNotIn(rule, os_fired(s))

    def test_catalogue_has_the_os_rules(self):
        self.assertLessEqual(set(OS_RULES), scoring.RULE_IDS)

    def test_real_chrome_shapes_fire_no_os_rule(self):
        for name in ("linux", "windows", "mac"):
            with self.subTest(os=name):
                s = os_session(name)
                self.assertEqual(os_fired(s), {})
                self.assertEqual(scoring.ua_os(scoring.doc_ua(s)), name)
                self.assertTrue(s["accept_ch"])
                self.assertIn("sec-ch-ua-platform-version", s["documents"][-1]["headers"])

    def test_linux_capture_backs_the_rules(self):
        """The Linux ground truth (tools/os_truth) agrees with what the rules expect."""
        caps = sorted(OS_TRUTH.glob("linux_*.json"))
        self.assertEqual(len(caps), 4)
        for path in caps:
            with self.subTest(capture=path.name):
                c = json.loads(path.read_text())
                r = c["result"]
                self.assertTrue(c["provenance"]["captured"] and c["provenance"]["loopbackOnly"])
                self.assertEqual(r["identity"]["uaHigh"]["platformVersion"], "")
                self.assertEqual(r["identity"]["platform"], "Linux x86_64")
                self.assertEqual(r["scrollbar"]["div"], 15)
                self.assertFalse(r["osApis"]["navigator"]["share"] or r["osApis"]["navigator"]["canShare"])
                self.assertFalse(r["osApis"]["navigator"]["bluetooth"] or r["osApis"]["window"]["Bluetooth"])
                self.assertTrue(r["osApis"]["navigator"]["keyboard"] and r["osApis"]["navigator"]["gpu"])
                self.assertGreater(r["emoji"]["colored"], 0)
                self.assertIn("Arial", r["fonts"]["resolves"]["sans-serif"])
                self.assertIn("Times New Roman", r["fonts"]["resolves"]["serif"])
                self.assertEqual(r["errors"], {})
                self.assertGreaterEqual(r["navigator"]["deviceMemory"], 8)
                if "headless" not in path.name:
                    pairs = {(e["deltaY"], e["wheelDeltaY"]) for e in c["wheel"]["events"]}
                    self.assertEqual(pairs, {(120, -120), (-120, 120)})

    def test_uach_platform_version(self):
        def pv(v):
            return ("os_media", lambda m: m["high"].update(platformVersion=v))
        self.fires("uach_platform_version", "linux", pv("6.18.44"))
        self.fires("uach_platform_version", "windows", pv("11.0.0"))
        self.fires("uach_platform_version", "windows", pv("10"))
        self.fires("uach_platform_version", "mac", pv("16.0.0"))
        self.fires("uach_platform_version", "mac", pv("10.15.7"))
        self.quiet("uach_platform_version", "windows", pv("19.0.0"))
        self.quiet("uach_platform_version", "mac", pv("26.0.1"))

    def test_uach_arch_gpu(self):
        intel = ("os_surface", lambda v: v["gl"].update(
            renderer="ANGLE (Intel, ANGLE Metal Renderer: Intel(R) Iris(TM) Plus Graphics, Unspecified Version)"))
        self.fires("uach_arch_gpu", "mac", intel)
        self.fires("uach_arch_gpu", "mac", ("os_media", lambda m: m["high"].update(architecture="x86")))
        self.fires("uach_arch_gpu", "mac", intel, ("os_media", lambda m: m["high"].update(
            architecture="x86", platformVersion="27.0.0")))
        self.quiet("uach_arch_gpu", "mac", intel, ("os_media", lambda m: m["high"].update(architecture="x86")))
        self.fires("uach_arch_gpu", "linux", ("os_media", lambda m: m["high"].update(wow64=True)))
        self.fires("uach_arch_gpu", "windows", ("os_media", lambda m: m["high"].update(bitness="128")))

    def test_ch_high_entropy(self):
        def drop(s):
            del s["documents"][-1]["headers"]["sec-ch-ua-arch"]
        def differ(s):
            s["documents"][-1]["headers"]["sec-ch-ua-platform-version"] = '"6.8.0"'
        def lb_like(s):  # no Accept-CH support at all
            for d in s["documents"][1:]:
                for k in list(d["headers"]):
                    if k in scoring.CH_HINTS:
                        del d["headers"][k]
        self.fires("ch_high_entropy", "linux", ("session", drop))
        self.fires("ch_high_entropy", "linux", ("session", differ))
        self.fires("ch_high_entropy", "windows", ("session", lb_like))
        s = os_session("windows")
        lb_like(s)
        del s["accept_ch"]  # sessions recorded before the server sent Accept-CH
        self.assertNotIn("ch_high_entropy", os_fired(s))

    def test_os_api_surface_and_bluetooth(self):
        on = ("os_surface", lambda v: v["apis"].update(share=True, canShare=True, bluetooth=True, Bluetooth=True))
        off = ("os_surface", lambda v: v["apis"].update(share=False, canShare=False, bluetooth=False, Bluetooth=False))
        self.fires("os_api_surface", "linux", on)
        self.fires("os_bluetooth", "linux", on)
        self.fires("os_api_surface", "windows", off)
        self.fires("os_bluetooth", "mac", off)

    def test_api_throws(self):
        self.fires("api_throws", "linux", ("os_media", lambda m: m["throws"].update(canPlayType="NotSupportedError")))
        self.fires("api_throws", "windows", ("os_media", lambda m: m["present"].update(gpu=False)))
        self.quiet("api_throws", "linux", ("os_media", lambda m: m["throws"].update(getLayoutMap="InvalidStateError")))

    def test_webgl_backend_os(self):
        def gl(**kw):
            return ("os_surface", lambda v: v["gl"].update(**kw))
        self.fires("webgl_backend_os", "windows", gl(renderer="ANGLE (NVIDIA, NVIDIA GeForce RTX 3060/PCIe/SSE2, OpenGL 4.5.0 NVIDIA 550.54)"))
        self.fires("webgl_backend_os", "windows", gl(lineWidth=[1, 7.99]))
        self.fires("webgl_backend_os", "windows", gl(viewport=[8192, 8192]))
        self.fires("webgl_backend_os", "mac", gl(viewport=[32767, 32767]))
        self.fires("webgl_backend_os", "linux", ("os_surface", lambda v: v.update(gl=dict(
            vendor="Google Inc. (NVIDIA)", renderer="ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 (0x00002504) Direct3D11 vs_5_0 ps_5_0, D3D11)",
            viewport=[32767, 32767], lineWidth=[1, 1], pointSize=[1, 1024]))))

    def test_fonts_os_markers(self):
        def add(name, dims):
            return ("os_surface", lambda v: v["fonts"]["detected"].update({name: dims}))
        self.fires("fonts_os_markers", "windows", add("DejaVu Sans", [1112, 108]))
        self.fires("fonts_os_markers", "mac", add("Cantarell", [990, 101]))
        self.fires("fonts_os_markers", "linux", add("Consolas", [752, 85]))
        self.fires("fonts_os_markers", "windows", add("Segoe UI Variable Text", [960, 96]))
        # Windows 11 has Segoe UI Variable.
        self.quiet("fonts_os_markers", "windows", add("Segoe UI Variable Text", [960, 96]),
                   ("os_media", lambda m: m["high"].update(platformVersion="19.0.0")))
        # fontconfig aliases (same size as another font) are not the real font:
        # the captured Linux session has "Segoe UI" and "Helvetica Neue" mapped to Inter.
        det = set_probe(os_session("linux"), "os_surface", lambda v: None)
        d = scoring.probe_value(det, "os_surface")["fonts"]["detected"]
        self.assertEqual(d["Segoe UI"], d["Inter"])

    def test_generic_fonts_os(self):
        def gen(**kw):
            return ("os_surface", lambda v: v["fonts"]["generic"].update(
                {k.replace("_", "-"): x for k, x in kw.items()}))
        self.fires("generic_fonts_os", "windows", gen(system_ui=[965, 82]))
        self.fires("generic_fonts_os", "linux", gen(sans_serif=[1112, 108]))
        self.fires("generic_fonts_os", "mac", gen(sans_serif=[965, 82]))
        self.fires("generic_fonts_os", "linux", ("os_surface", lambda v: v["fonts"]["detected"].pop("Arial")))
        # A real (non-alias) Segoe UI as system-ui on Linux.
        self.fires("generic_fonts_os", "linux", ("os_surface", lambda v: (
            v["fonts"]["detected"].update({"Segoe UI": [962, 96]}),
            v["fonts"]["generic"].update({"system-ui": [962, 96]}))))

    def test_scrollbar_os(self):
        def sb(**kw):
            return ("os_surface", lambda v: v["scrollbar"].update(**kw))
        self.fires("scrollbar_os", "linux", sb(div=17, root=17))
        self.fires("scrollbar_os", "linux", sb(div=0, root=0))
        self.fires("scrollbar_os", "mac", sb(div=17, root=17))
        self.fires("scrollbar_os", "windows", sb(div=0, root=0))
        self.fires("scrollbar_os", "windows", sb(root=0))
        self.quiet("scrollbar_os", "windows", sb(div=0, root=0),
                   ("os_media", lambda m: m["high"].update(platformVersion="15.0.0")))
        self.quiet("scrollbar_os", "mac", sb(div=15, root=15))

    def test_emoji_voices_webgpu(self):
        self.fires("emoji_render", "linux", ("os_surface", lambda v: v["emoji"].update(colored=0)))
        self.fires("voices_os", "linux", ("os_voices", lambda m: m.update(
            count=3, local=1, localNames=["Microsoft Zira - English (United States)"])))
        self.fires("voices_os", "windows", ("os_voices", lambda m: m["localNames"].append("Samantha")))
        self.fires("voices_os", "mac", ("os_voices", lambda m: m.update(local=0, localNames=[])))
        self.fires("webgpu_os", "windows", ("os_media", lambda m: m["gpu"]["adapter"].update(vendor="intel")))

    def test_info_rules(self):
        self.fires("codecs_os", "windows", ("os_media", lambda m: m["codecs"]["h264"].update(powerEfficient=False)))
        self.fires("codecs_os", "linux", ("os_media", lambda m: m["codecs"].update(hevcCanPlay="probably")),
                   ("os_surface", lambda v: v.update(gl={"renderer": "ANGLE (Intel, Mesa Intel(R) UHD Graphics 630 (CFL GT2), OpenGL 4.6)"})))
        self.fires("screen_os", "mac", ("os_surface", lambda v: v["screen"].update(availTop=0)))
        self.fires("screen_os", "windows", ("os_surface", lambda v: v["screen"].update(availHeight=1020)))
        # Linux window outside the work area (Wayland geometry with X11 bars).
        self.fires("window_work_area", "linux", ("os_surface", lambda v: v["screen"].update(
            availTop=32, availLeft=0, screenX=0, screenY=0)))
        self.fires("window_work_area", "linux", ("os_surface", lambda v: v["screen"].update(
            availTop=0, availLeft=64, screenX=0, screenY=0)))
        self.quiet("window_work_area", "linux", ("os_surface", lambda v: v["screen"].update(
            availTop=32, availLeft=64, screenX=64, screenY=32)))
        self.quiet("window_work_area", "linux", ("os_surface", lambda v: v["screen"].update(
            availTop=0, availLeft=0, screenX=0, screenY=0)))
        self.quiet("window_work_area", "windows", ("os_surface", lambda v: v["screen"].update(
            availTop=32, screenX=-8, screenY=0)))
        wheel = lambda pairs: ("beh_wheel_deltas", lambda w: w.update(pairs=pairs) if w["pairs"] else None)  # noqa: E731
        self.fires("wheel_os", "linux", wheel([[100, -120, 0]] * 4))
        self.fires("wheel_os", "mac", wheel([[120, -120, 0]] * 4))
        self.quiet("wheel_os", "windows", wheel([[100, -120, 0]] * 4))
        # Info rules count for nothing at their catalogue weight.
        s = self.fires("screen_os", "mac", ("os_surface", lambda v: v["screen"].update(availTop=0)))
        self.assertNotIn("screen_os", fired(scoring.score_session(s)))

    def test_old_sessions_fire_no_os_rule(self):
        for name in ("T-001", "T-003", "T-020"):
            with self.subTest(fixture=name):
                self.assertEqual(os_fired(load(name)), {})
        self.assertEqual(os_fired(clean_session()), {})


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


def with_worker_navigator(s: dict, edit=None) -> dict:
    """Add a `worker_navigator` probe (stock Chrome 153 shape: page and
    worker agree) to the first page; `edit(worker_values)` may change the
    worker side."""
    high = list(s["pages"].values())[0]["probes"]["os_media"]["value"]["high"]
    nav = {"userAgent": scoring.doc_ua(s), "appVersion": scoring.doc_ua(s)[len("Mozilla/"):],
           "platform": "Linux x86_64", "hardwareConcurrency": 8, "deviceMemory": 8,
           "language": "en-US", "languages": ["en-US", "en"], "gpu": True,
           "uaData": {"brands": high["brands"], "mobile": False, "platform": "Linux"},
           "high": dict(high, uaFullVersion="153.0.8010.12")}
    worker = json.loads(json.dumps(nav))
    if edit:
        edit(worker)
    pg = list(s["pages"].values())[0]
    pg["probes"]["worker_navigator"] = {"id": "worker_navigator", "cat": "automation", "weight": 0,
                                        "suspicious": False, "value": {"window": nav, "worker": worker}}
    return s


class WorkerNavigatorRule(unittest.TestCase):
    def detail(self, s):
        return {f["id"]: f["detail"] for f in scoring.score_session(s)["fired"]}.get("worker_navigator_mismatch")

    def test_in_catalogue_with_weight(self):
        rule = next(r for r in scoring.RULES if r["id"] == "worker_navigator_mismatch")
        self.assertEqual((rule["weight"], rule["cat"]), (45, "automation"))

    def test_agreeing_worker_is_quiet(self):
        self.assertIsNone(self.detail(with_worker_navigator(os_session("linux"))))

    def test_sessions_without_the_probe_are_quiet(self):
        self.assertIsNone(self.detail(os_session("linux")))
        for v in ({"supported": False}, {"timeout": True}, {"error": "worker error"}):
            s = with_worker_navigator(os_session("linux"))
            list(s["pages"].values())[0]["probes"]["worker_navigator"]["value"] = v
            self.assertIsNone(self.detail(s))

    def test_each_field_fires(self):
        edits = {
            "userAgent": lambda w: w.update(userAgent=w["userAgent"].replace("Linux", "Windows NT 10.0")),
            "platform": lambda w: w.update(platform="Win32"),
            "hardwareConcurrency": lambda w: w.update(hardwareConcurrency=4),
            "deviceMemory": lambda w: w.update(deviceMemory=4),
            "languages": lambda w: w.update(languages=["en-US"]),
            "gpu": lambda w: w.update(gpu=False),
            "uaData": lambda w: w["uaData"].update(platform="Windows"),
            "high.platformVersion": lambda w: w["high"].update(platformVersion="10.0.0"),
            "high.architecture": lambda w: w["high"].update(architecture="arm"),
            "high.fullVersionList": lambda w: w["high"]["fullVersionList"].pop(),
            "high.uaFullVersion": lambda w: w["high"].pop("uaFullVersion"),
        }
        for field, edit in edits.items():
            with self.subTest(field=field):
                d = self.detail(with_worker_navigator(os_session("linux"), edit))
                self.assertIsNotNone(d)
                self.assertIn(field, d)

    def test_real_chrome_shapes_stay_quiet(self):
        for name in ("linux", "windows", "mac"):
            with self.subTest(os=name):
                s = with_worker_navigator(os_session(name))
                self.assertNotIn("worker_navigator_mismatch", fired(scoring.score_session(s)))

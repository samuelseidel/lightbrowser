"""detect-bench scoring: rule catalogue -> bot score 0-100 + fired rules.

Rules are data. Each entry in `RULES` is one dict:

    {"id": ..., "weight": 0-99, "cat": ..., "why": ..., and either
     "probe": "<collector probe id>"            # fires when the probe is suspicious
     "check": fn(session, cfg) -> detail|None}  # server-side or session-wide rule

Weights combine as a noisy-OR: score = 100 * (1 - prod(1 - w/100)). A weight
is roughly "how sure this rule alone makes us that the client is a bot".
Thresholds and per-rule weights can be overridden with a JSON config:

    {"thresholds": {"bot": 70, "suspect": 40},
     "weights": {"timezone_utc": 0}, "disabled": ["favicon_missing"],
     "beacon_grace_s": 4}

Run `python3 -m unittest test_scoring` for the tests.
"""

from __future__ import annotations

import json
import re
import time
from pathlib import Path

DEFAULT_CONFIG = {
    "thresholds": {"bot": 70, "suspect": 40},
    "weights": {},
    "disabled": [],
    # How long after the first document a session may stay beacon-less
    # before "no_beacon" fires.
    "beacon_grace_s": 4,
}

# ---- expected request shapes per family (HTTP/1.1 navigations) -------------
# Chrome values were captured from Chrome for Testing 153 on loopback (this
# bench, T-001). T-001's headless shell sends no Accept-Language; the
# Accept-Language slot comes from `chromium --headless=new` 153.0.8010.12 on
# loopback, which sends it right after sec-ch-ua-platform (Chrome 131 sent it
# after Accept-Encoding). Firefox values are Firefox 128 ESR; re-capture
# before relying on them.
EXPECTED = {
    "chrome": {
        "doc_header_order": [
            "Host", "Connection", "sec-ch-ua", "sec-ch-ua-mobile", "sec-ch-ua-platform",
            "Accept-Language", "Upgrade-Insecure-Requests", "User-Agent", "Accept", "Sec-Fetch-Site",
            "Sec-Fetch-Mode", "Sec-Fetch-User", "Sec-Fetch-Dest", "Accept-Encoding", "Cookie",
        ],
        "accept": ("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,"
                   "image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"),
        "accept_encoding": {"http": "gzip, deflate, br, zstd", "https": "gzip, deflate, br, zstd"},
    },
    "firefox": {
        "doc_header_order": [
            "Host", "User-Agent", "Accept", "Accept-Language", "Accept-Encoding", "Connection",
            "Cookie", "Upgrade-Insecure-Requests", "Sec-Fetch-Dest", "Sec-Fetch-Mode",
            "Sec-Fetch-Site", "Sec-Fetch-User", "Priority",
        ],
        "accept": ("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,"
                   "image/png,image/svg+xml,*/*;q=0.8"),
        "accept_encoding": {"http": "gzip, deflate", "https": "gzip, deflate, br, zstd"},
    },
}

INTERACTION_TYPES = ("mousemove", "mousedown", "keydown", "wheel", "touchstart")


# ---- session helpers ------------------------------------------------------

def ua_family(ua: str) -> str:
    if "Firefox/" in ua:
        return "firefox"
    if "Chrome/" in ua:
        return "chrome"
    if "Safari/" in ua and "Mozilla/" in ua:
        return "safari"
    return "other"


def first_doc(s: dict) -> dict | None:
    return s["documents"][0] if s.get("documents") else None


def doc_ua(s: dict) -> str:
    d = first_doc(s)
    return (d or {}).get("headers", {}).get("user-agent", "")


def merged_probes(s: dict) -> dict:
    """Probe id -> {suspicious, value, page} over all pages.

    A probe counts as suspicious if any page reported it so. Probes only mark
    themselves suspicious on positive evidence, so pages without data do not
    fire them.
    """
    out: dict[str, dict] = {}
    pages = sorted(s.get("pages", {}).values(), key=lambda p: p.get("received", 0))
    for pg in pages:
        probes = pg.get("probes", {})
        items = probes.values() if isinstance(probes, dict) else probes
        for p in items:
            pid = p.get("id")
            cur = out.get(pid)
            if cur is None or (p.get("suspicious") and not cur["suspicious"]):
                out[pid] = {"suspicious": bool(p.get("suspicious")), "value": p.get("value"),
                            "weight": p.get("weight", 0), "cat": p.get("cat"), "page": pg.get("url")}
    return out


def event_counts(s: dict) -> dict:
    counts: dict[str, int] = {}
    for pg in s.get("pages", {}).values():
        for e in pg.get("trace", []):
            if e.get("tr"):
                counts[e["type"]] = counts.get(e["type"], 0) + 1
    return counts


def age_s(s: dict) -> float:
    return time.time() - s.get("created", time.time())


def kinds(s: dict) -> list[str]:
    return [r["kind"] for r in s.get("requests", [])]


def is_browser(s: dict) -> bool:
    return ua_family(doc_ua(s)) in ("chrome", "firefox")


def chrome_major(ua: str) -> str | None:
    m = re.search(r"Chrome/(\d+)", ua)
    return m.group(1) if m else None


# ---- server-side and session-wide checks ------------------------------------
# Each returns a short detail (truthy) when the rule fires, else None.

def chk_no_beacon(s, cfg):
    if s.get("documents") and not s.get("beacons") and age_s(s) > cfg["beacon_grace_s"]:
        return f"{len(s['documents'])} document(s), 0 beacons"
    return None


def chk_no_subresources(s, cfg):
    if s.get("documents") and not {"css", "js", "img"} & set(kinds(s)):
        if age_s(s) > cfg["beacon_grace_s"] or s.get("beacons"):
            return "no css/js/img requested"
    return None


def chk_images_blocked(s, cfg):
    k = set(kinds(s))
    if {"css", "js"} & k and "img" not in k and s.get("beacons"):
        return "css/js fetched, images never requested"
    return None


def chk_favicon_missing(s, cfg):
    if s.get("beacons") and "favicon" not in kinds(s):
        return "favicon never requested"
    return None


def chk_non_browser_ua(s, cfg):
    ua = doc_ua(s)
    if first_doc(s) and ua_family(ua) == "other":
        return ua[:80] or "(no User-Agent)"
    return None


def chk_ua_headless_header(s, cfg):
    ua = doc_ua(s)
    return ua[:120] if "Headless" in ua else None


def chk_ua_changed(s, cfg):
    uas = [u for u in s.get("user_agents", []) if u]
    return f"{len(uas)} different User-Agents" if len(uas) > 1 else None


def chk_js_ua_mismatch(s, cfg):
    p = merged_probes(s).get("ua_headless")
    ua = doc_ua(s)
    if p and isinstance(p.get("value"), str) and ua and p["value"] != ua:
        return f"header {ua[:60]!r} vs navigator {p['value'][:60]!r}"
    return None


def _doc_headers(s):
    d = first_doc(s)
    return (d or {}).get("headers", {}), d


def chk_missing_client_hints(s, cfg):
    h, d = _doc_headers(s)
    ua = doc_ua(s)
    major = chrome_major(ua)
    if d and ua_family(ua) == "chrome" and major and int(major) >= 90 and "sec-ch-ua" not in h:
        return "Chrome UA without sec-ch-ua"
    return None


def chk_ch_header_mismatch(s, cfg):
    h, d = _doc_headers(s)
    ua = doc_ua(s)
    ch = h.get("sec-ch-ua")
    if not d or not ch or ua_family(ua) != "chrome":
        return None
    major = chrome_major(ua)
    brands = re.findall(r'"([^"]*)";\s*v="([^"]*)"', ch)
    if not any(v == major and re.fullmatch(r"Google Chrome|Chromium|Microsoft Edge|Opera|Brave", b)
               for b, v in brands):
        return f"sec-ch-ua {ch[:80]} vs UA major {major}"
    if any("Headless" in b for b, _ in brands):
        return "HeadlessChrome brand in sec-ch-ua"
    return None


def chk_missing_fetch_metadata(s, cfg):
    h, d = _doc_headers(s)
    if d and is_browser(s):
        missing = [k for k in ("sec-fetch-site", "sec-fetch-mode", "sec-fetch-dest") if k not in h]
        if missing:
            return "missing " + ",".join(missing)
    return None


def chk_accept_language_missing(s, cfg):
    h, d = _doc_headers(s)
    if d and is_browser(s) and not h.get("accept-language"):
        return "no Accept-Language"
    return None


def chk_accept_mismatch(s, cfg):
    h, d = _doc_headers(s)
    fam = ua_family(doc_ua(s))
    if d and fam in EXPECTED and h.get("accept") != EXPECTED[fam]["accept"]:
        return f"Accept {h.get('accept', '')[:80]!r}"
    return None


def chk_accept_encoding_mismatch(s, cfg):
    h, d = _doc_headers(s)
    fam = ua_family(doc_ua(s))
    if d and fam in EXPECTED:
        want = EXPECTED[fam]["accept_encoding"][d.get("scheme", "http")]
        if h.get("accept-encoding") != want:
            return f"Accept-Encoding {h.get('accept-encoding')!r}, want {want!r}"
    return None


def chk_header_order(s, cfg):
    """Header names (with case) must follow the family's order."""
    d = first_doc(s)
    fam = ua_family(doc_ua(s))
    if not d or fam not in EXPECTED:
        return None
    want = EXPECTED[fam]["doc_header_order"]
    by_lower = {w.lower(): w for w in want}
    names = d.get("header_names", [])
    unknown = [n for n in names if n.lower() not in by_lower]
    wrong_case = [n for n in names if n.lower() in by_lower and by_lower[n.lower()] != n]
    pos = [want.index(by_lower[n.lower()]) for n in names if n.lower() in by_lower]
    out_of_order = any(a > b for a, b in zip(pos, pos[1:]))
    reasons = []
    if out_of_order:
        reasons.append("order " + ",".join(names))
    if wrong_case:
        reasons.append("case " + ",".join(wrong_case))
    if unknown:
        reasons.append("extra " + ",".join(unknown))
    return "; ".join(reasons) or None


def chk_http_version(s, cfg):
    d = first_doc(s)
    if d and d.get("http_version") not in ("HTTP/1.1", "HTTP/2", "HTTP/2.0"):
        return d.get("http_version")
    return None


def chk_cookie_not_echoed(s, cfg):
    reqs = [r for r in s.get("requests", []) if r["kind"] in ("css", "js", "img", "beacon", "favicon")]
    missing = [r for r in reqs if not r.get("cookie")]
    if missing:
        return f"{len(missing)}/{len(reqs)} follow-up requests without the session cookie"
    if len(s.get("documents", [])) > 1 and not all(d.get("cookie") for d in s["documents"][1:]):
        return "later document without cookie"
    return None


def chk_no_interaction(s, cfg):
    if not s.get("beacons") or age_s(s) <= cfg["beacon_grace_s"]:
        return None
    c = event_counts(s)
    if not any(c.get(t) for t in INTERACTION_TYPES):
        return "no trusted mouse, key, wheel or touch input in the whole session"
    return None


def chk_page_dwell_short(s, cfg):
    short = []
    for pg in s.get("pages", {}).values():
        clicks = sum(1 for e in pg.get("trace", []) if e.get("type") == "click" and e.get("tr"))
        if pg.get("reason") in ("pagehide", "hidden") and clicks and (pg.get("t") or 1e9) < 1500:
            short.append(f"{pg.get('url')} {pg.get('t')}ms")
    return "left after " + ", ".join(short) if short else None


# ---- OS consistency (M7) ------------------------------------------------------
# These read the info probes `os_surface`, `os_media` and `beh_wheel_deltas`
# (collector.js) and the document headers. Ground truth: Linux values were
# captured from the local Chrome for Testing 153 (tests/fixtures/os/, made by
# tools/os_truth); Windows and macOS values come from the surface reference
# in docs/ROADMAP.md ("M7 status") and are not captured yet. The per-OS
# real-Chrome-shaped sessions are fixtures/os_{windows,mac,linux}_chrome.json.

def ua_os(ua: str) -> str:
    if "Windows NT" in ua:
        return "windows"
    if "Android" in ua:
        return "android"
    if "CrOS" in ua:
        return "chromeos"
    if "Macintosh" in ua or "Mac OS X" in ua:
        return "mac"
    if "Linux" in ua or "X11" in ua:
        return "linux"
    return "other"


def probe_value(s: dict, pid: str) -> dict | None:
    """First usable value of an info probe, pages in arrival order."""
    for pg in sorted(s.get("pages", {}).values(), key=lambda p: p.get("received", 0)):
        p = pg.get("probes", {}).get(pid)
        v = p.get("value") if isinstance(p, dict) else None
        if isinstance(v, dict) and not v.get("timeout") and "error" not in v:
            return v
    return None


def _chrome_os(s):
    """(os, ua) when the session claims desktop Chrome, else (None, ua)."""
    ua = doc_ua(s)
    if ua_family(ua) != "chrome":
        return None, ua
    o = ua_os(ua)
    return (o if o in ("windows", "mac", "linux") else None), ua


def _version(v):
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", v or "")
    return tuple(int(x) for x in m.groups()) if m else None


# macOS majors Chrome 153 can run on (12 Monterey .. 15 Sequoia, then 26 Tahoe;
# 27 is the next one). macOS 27 drops Intel Macs.
MAC_MAJORS = {12, 13, 14, 15, 26, 27}


def chk_uach_platform_version(s, cfg):
    o, _ = _chrome_os(s)
    m = probe_value(s, "os_media")
    if not o or not m or not isinstance(m.get("high"), dict) or "platformVersion" not in m["high"]:
        return None
    pv = m["high"]["platformVersion"]
    if o == "linux":
        # Chrome 153 on Linux reports "" (captured, headed and headless).
        return f"Linux platformVersion {pv!r}, Chrome sends \"\"" if pv != "" else None
    v = _version(pv)
    if v is None:
        return f"platformVersion {pv!r} is not N.N.N"
    if o == "windows":
        # Windows 10: 1.0.0 .. 10.0.0; Windows 11: 13.0.0 and up.
        if v[1:] != (0, 0) or not (1 <= v[0] <= 10 or 13 <= v[0] <= 30):
            return f"Windows platformVersion {pv}"
    if o == "mac" and v[0] not in MAC_MAJORS:
        return f"macOS platformVersion {pv}"
    return None


APPLE_GPU = re.compile(r"Apple (M\d|GPU)|Apple, ANGLE Metal Renderer: Apple", re.I)


def chk_uach_arch_gpu(s, cfg):
    o, _ = _chrome_os(s)
    m = probe_value(s, "os_media")
    if not o or not m or not isinstance(m.get("high"), dict):
        return None
    h = m["high"]
    arch, bits, wow = h.get("architecture"), h.get("bitness"), h.get("wow64")
    reasons = []
    if arch is not None and arch not in ("x86", "arm"):
        reasons.append(f"architecture {arch!r}")
    if bits is not None and bits not in ("32", "64"):
        reasons.append(f"bitness {bits!r}")
    if o != "windows" and wow:
        reasons.append(f"wow64 on {o}")
    if o != "windows" and bits == "32":
        reasons.append(f"32-bit Chrome on {o}")
    if o == "mac":
        gl = (probe_value(s, "os_surface") or {}).get("gl") or {}
        r = gl.get("renderer") or ""
        if arch == "arm" and r and not APPLE_GPU.search(r):
            reasons.append("arm Mac with a non-Apple GPU")
        if arch == "x86" and APPLE_GPU.search(r):
            reasons.append("Apple GPU with x86")
        v = _version(h.get("platformVersion"))
        if arch == "x86" and v and v[0] >= 27:
            reasons.append(f"Intel Mac on macOS {v[0]}")
    return "; ".join(reasons) or None


# Header -> getHighEntropyValues key, for the hints server.py asks for.
CH_HINTS = {
    "sec-ch-ua-platform-version": "platformVersion", "sec-ch-ua-arch": "architecture",
    "sec-ch-ua-bitness": "bitness", "sec-ch-ua-full-version-list": "fullVersionList",
    "sec-ch-ua-model": "model", "sec-ch-ua-wow64": "wow64", "sec-ch-ua-form-factors": "formFactors",
}


def _sf_value(key, v):
    """A structured-field header value in getHighEntropyValues' shape."""
    v = v.strip()
    if key == "wow64":
        return v == "?1"
    if key == "fullVersionList":
        return sorted(re.findall(r'"([^"]*)";\s*v="([^"]*)"', v))
    if key == "formFactors":
        return re.findall(r'"([^"]*)"', v)
    return v[1:-1] if len(v) >= 2 and v[0] == v[-1] == '"' else v


def chk_ch_high_entropy(s, cfg):
    """After Accept-CH, later documents must carry the hints, equal to JS."""
    o, _ = _chrome_os(s)
    asked = [h.lower() for h in s.get("accept_ch", [])]
    docs = s.get("documents", [])
    if not o or not asked or len(docs) < 2:
        return None
    m = probe_value(s, "os_media") or {}
    js = m.get("high") if isinstance(m.get("high"), dict) else {}
    for d in docs[1:]:
        h = d.get("headers", {})
        missing = [n for n in asked if n in CH_HINTS and n not in h]
        if missing:
            return f"{d.get('path')}: no {','.join(missing)}"
        for n in asked:
            key = CH_HINTS.get(n)
            if not key or key not in js:
                continue
            want = js[key]
            if key == "fullVersionList":
                want = sorted((b.get("brand"), b.get("version")) for b in want)
                want = [list(x) for x in want]
                got = [list(x) for x in _sf_value(key, h[n])]
            else:
                got = _sf_value(key, h[n])
            if got != want:
                return f"{n} {h[n]!r} vs JS {js[key]!r}"
    return None


def chk_os_api_surface(s, cfg):
    o, _ = _chrome_os(s)
    v = probe_value(s, "os_surface")
    if not o or not v or not isinstance(v.get("apis"), dict):
        return None
    a = v["apis"]
    want = o in ("windows", "mac")  # Web Share: Windows and macOS, not Linux
    bad = [k for k in ("share", "canShare") if k in a and a[k] != want]
    return f"navigator.{'/'.join(bad)} {'missing' if want else 'present'} on {o}" if bad else None


def chk_os_bluetooth(s, cfg):
    o, _ = _chrome_os(s)
    v = probe_value(s, "os_surface")
    if not o or not v or not isinstance(v.get("apis"), dict):
        return None
    a = v["apis"]
    want = o in ("windows", "mac")  # Web Bluetooth: Windows and macOS; off on Linux
    bad = [k for k in ("bluetooth", "Bluetooth") if k in a and a[k] != want]
    return f"{'/'.join(bad)} {'missing' if want else 'present'} on {o}" if bad else None


# Errors real Chrome can give for these calls in a normal page.
API_OK_ERRORS = {"getLayoutMap": {"InvalidStateError", "SecurityError", "NotAllowedError"}}


# APIs desktop Chrome 153 has in a secure context on every OS (captured on
# Linux headed and headless; navigator.gpu is there even without a GPU).
CHROME_SECURE_APIS = ("mediaCapabilities", "MediaSource", "MediaRecorder", "keyboard", "gpu", "speechSynthesis")


def chk_api_throws(s, cfg):
    o, _ = _chrome_os(s)
    m = probe_value(s, "os_media")
    if not o or not m or not isinstance(m.get("throws"), dict):
        return None
    bad = [f"{k}: {e}" for k, e in m["throws"].items() if e and e not in API_OK_ERRORS.get(k, ())]
    present = m.get("present") or {}
    if m.get("secure"):
        bad += [f"{k} missing" for k in CHROME_SECURE_APIS if present.get(k) is False]
    return "; ".join(bad) or None


def chk_codecs_os(s, cfg):
    o, _ = _chrome_os(s)
    m = probe_value(s, "os_media")
    if not o or not m or not isinstance(m.get("codecs"), dict):
        return None
    c = m["codecs"]
    r = ((probe_value(s, "os_surface") or {}).get("gl") or {}).get("renderer") or ""
    if o == "linux" and c.get("hevcCanPlay") == "probably" and "Mesa" in r:
        return "HEVC 'probably' on Linux with a Mesa GPU"
    h264 = c.get("h264") or {}
    hw = r and not re.search(r"SwiftShader|llvmpipe|Basic Render", r)
    if o in ("windows", "mac") and hw and h264.get("supported") and h264.get("powerEfficient") is False:
        return f"H.264 1080p not power efficient on {o} with {r[:60]}"
    return None


def chk_scrollbar_os(s, cfg):
    o, _ = _chrome_os(s)
    v = probe_value(s, "os_surface")
    sb = (v or {}).get("scrollbar")
    if not o or not isinstance(sb, dict) or not isinstance(sb.get("div"), (int, float)):
        return None
    div = sb["div"]
    # Windows 10: 17 (classic; 16 after rounding at some scales). Windows 11
    # (platformVersion >= 13) is not captured yet: Fluent scrollbars may be
    # thinner or overlay, so 0 and 15-17 pass. macOS: 0 (overlay) or 15
    # (legacy, mouse attached). Linux: 15 (captured, headed and headless).
    pv = _version(((probe_value(s, "os_media") or {}).get("high") or {}).get("platformVersion"))
    win11 = pv is not None and pv[0] >= 13
    ok = {"windows": {0, 15, 16, 17} if win11 else {16, 17}, "mac": {0, 15}, "linux": {14, 15, 16}}[o]
    if div not in ok:
        return f"scrollbar {div}px on {o}"
    if (div > 0 and sb.get("overflows") and sb.get("root") == 0
            and sb.get("rootOverflowY") in ("visible", "auto", "scroll")
            and sb.get("bodyOverflowY") in ("visible", "auto", "scroll", None)):
        return f"{div}px scrollbars, but the overflowing root has none"
    return None


FONT_MARKERS = {
    "windows": ["Segoe UI", "Segoe UI Variable Text", "Segoe UI Variable Display", "Segoe Fluent Icons",
                "Consolas", "Segoe UI Emoji"],
    "mac": ["Helvetica Neue", "Menlo", "Apple Color Emoji", "Lucida Grande", "Geneva", "SF Pro"],
    "linux": ["DejaVu Sans", "DejaVu Sans Mono", "Cantarell", "Ubuntu", "Ubuntu Sans", "Adwaita Sans",
              "Noto Color Emoji"],
}
# Markers that count against each OS. Office for Mac brings Consolas and
# friends to macOS, so only Windows 11 UI fonts count there.
FOREIGN_MARKERS = {
    "windows": FONT_MARKERS["mac"] + FONT_MARKERS["linux"],
    "mac": FONT_MARKERS["linux"] + ["Segoe UI Variable Text", "Segoe UI Variable Display", "Segoe Fluent Icons"],
    "linux": FONT_MARKERS["windows"] + FONT_MARKERS["mac"],
}


def _real_fonts(fonts: dict) -> dict:
    """Detected fonts that are not aliases: no other detected font has the
    same size (fontconfig maps e.g. "Segoe UI" or "Helvetica Neue" to the
    local sans on some Linux setups; that is not the real font)."""
    det = fonts.get("detected") or {}
    out = {}
    for f, d in det.items():
        if not any(g != f and list(e) == list(d) for g, e in det.items()):
            out[f] = d
    return out


def chk_fonts_os_markers(s, cfg):
    o, _ = _chrome_os(s)
    fonts = ((probe_value(s, "os_surface") or {}).get("fonts")) or {}
    if not o or not isinstance(fonts.get("detected"), dict):
        return None
    det = fonts["detected"]
    real = _real_fonts(fonts) if o == "linux" else det
    foreign = [f for f in FOREIGN_MARKERS[o] if f in real]
    reasons = []
    if foreign:
        reasons.append(f"{o} with {', '.join(foreign)}")
    if o == "windows" and ("Segoe UI Variable Text" in det or "Segoe UI Variable Display" in det):
        m = probe_value(s, "os_media") or {}
        v = _version((m.get("high") or {}).get("platformVersion"))
        if v and v[0] < 13:
            reasons.append("Windows 11 font Segoe UI Variable with a Windows 10 platformVersion")
    return "; ".join(reasons) or None


def chk_generic_fonts_os(s, cfg):
    o, _ = _chrome_os(s)
    fonts = ((probe_value(s, "os_surface") or {}).get("fonts")) or {}
    gen, det = fonts.get("generic") or {}, fonts.get("detected")
    if not o or not isinstance(det, dict) or not gen:
        return None
    same = lambda a, b: a is not None and b is not None and list(a) == list(b)  # noqa: E731
    reasons = []
    segoe = det.get("Segoe UI")
    if o == "windows" and segoe and not same(gen.get("system-ui"), segoe):
        reasons.append("system-ui is not Segoe UI on Windows")
    if o != "windows" and "Segoe UI" in _real_fonts(fonts) and same(gen.get("system-ui"), segoe):
        reasons.append(f"system-ui is Segoe UI on {o}")
    # Chrome's default sans-serif/serif fonts: Arial / Times New Roman on
    # Windows and Linux (fontconfig maps them to Liberation), Helvetica /
    # Times on macOS.
    sans, serif = ("Helvetica", "Times") if o == "mac" else ("Arial", "Times New Roman")
    if det.get(sans) and not same(gen.get("sans-serif"), det[sans]):
        reasons.append(f"sans-serif is not {sans}")
    if det.get(serif) and not same(gen.get("serif"), det[serif]):
        reasons.append(f"serif is not {serif}")
    if o == "linux" and "Arial" not in det:
        # Chrome's Linux packages depend on Liberation fonts, which stand in for Arial.
        reasons.append("no Arial (Liberation Sans) on Linux")
    return "; ".join(reasons) or None


def chk_emoji_render(s, cfg):
    o, _ = _chrome_os(s)
    e = (probe_value(s, "os_surface") or {}).get("emoji")
    if not o or not isinstance(e, dict) or "colored" not in e:
        return None
    if e["colored"] == 0:
        return f"emoji drawn without colour ({e.get('inked')} inked px)"
    return None


APPLE_VOICES = {"Samantha", "Alex", "Daniel", "Karen", "Moira", "Fred", "Albert", "Bad News", "Bahh", "Bells",
                "Boing", "Bubbles", "Cellos", "Good News", "Jester", "Organ", "Superstar", "Trinoids", "Whisper",
                "Wobble", "Zarvox", "Junior", "Ralph", "Kathy", "Rishi", "Tessa", "Veena", "Amélie", "Thomas",
                "Anna", "Alice", "Yuna", "Kyoko", "Tingting", "Milena", "Monica", "Paulina"}


def chk_voices_os(s, cfg):
    o, _ = _chrome_os(s)
    # The fullest list any beacon saw (voices load after the page starts).
    lists = [(pg.get("probes", {}).get("os_voices") or {}).get("value") for pg in s.get("pages", {}).values()]
    lists = [v for v in lists if isinstance(v, dict) and v.get("count")]
    vo = max(lists, key=lambda v: v["count"]) if lists else None
    if not o or not vo:
        return None
    names = vo.get("localNames") or []
    reasons = []
    ms = [n for n in names if n.startswith("Microsoft ")]
    if ms and o != "windows":
        reasons.append(f"Microsoft voices on {o}: {', '.join(ms[:3])}")
    apple = [n for n in names if n.split(" (")[0] in APPLE_VOICES]
    if apple and o != "mac":
        reasons.append(f"Apple voices on {o}: {', '.join(apple[:3])}")
    if o in ("windows", "mac") and vo.get("local") == 0:
        reasons.append(f"no local voices on {o}")
    return "; ".join(reasons) or None


def chk_webgl_backend_os(s, cfg):
    o, _ = _chrome_os(s)
    gl = (probe_value(s, "os_surface") or {}).get("gl")
    if not o or not isinstance(gl, dict) or not gl.get("renderer"):
        return None
    r = gl["renderer"]
    reasons = []
    if o in ("windows", "mac") and re.search(r"OpenGL", r) and "SwiftShader" not in r:
        reasons.append(f"OpenGL ANGLE backend on {o}")
    if o == "linux" and re.search(r"Direct3D|D3D11|Metal", r):
        reasons.append("Direct3D/Metal backend on Linux")
    lw = gl.get("lineWidth") or []
    vp = gl.get("viewport") or []
    if re.search(r"Direct3D11|D3D11", r):
        if len(lw) == 2 and lw[1] > 1:
            reasons.append(f"D3D11 with line width {lw}")
        if vp and vp[0] < 16384:
            reasons.append(f"D3D11 with MAX_VIEWPORT_DIMS {vp}")
    if "Metal" in r:
        if len(lw) == 2 and lw[1] > 1:
            reasons.append(f"Metal with line width {lw}")
        if vp and vp[0] > 16384:
            reasons.append(f"Metal with MAX_VIEWPORT_DIMS {vp}")
    return "; ".join(reasons) or None


GPU_VENDORS = {"nvidia": r"NVIDIA|GeForce|Quadro", "amd": r"AMD|Radeon|ATI", "intel": r"Intel",
               "apple": r"Apple", "qualcomm": r"Qualcomm|Adreno"}


def chk_webgpu_os(s, cfg):
    o, _ = _chrome_os(s)
    m = probe_value(s, "os_media") or {}
    ad = (m.get("gpu") or {}).get("adapter")
    gl = (probe_value(s, "os_surface") or {}).get("gl") or {}
    if not o or not isinstance(ad, dict) or not ad.get("vendor") or not gl.get("renderer"):
        return None
    pat = GPU_VENDORS.get(ad["vendor"].lower())
    if pat and not re.search(pat, f"{gl.get('vendor')} {gl['renderer']}", re.I):
        return f"WebGPU adapter {ad['vendor']} vs WebGL {gl['renderer'][:60]}"
    return None


def chk_wheel_os(s, cfg):
    """wheelDeltaY / deltaY per notch: Linux -1 (captured: deltaY 120,
    wheelDeltaY -120), Windows -1.2 at 100% (deltaY 100, 120), macOS -3."""
    o, _ = _chrome_os(s)
    if not o:
        return None
    pairs = []
    for pg in s.get("pages", {}).values():
        v = (pg.get("probes", {}).get("beh_wheel_deltas") or {}).get("value") or {}
        pairs += [p for p in v.get("pairs") or [] if isinstance(p, list) and len(p) == 3 and p[0] and p[2] == 0]
    if len(pairs) < 3:
        return None
    ratios = sorted({round(-p[1] / p[0], 2) for p in pairs if p[1]})
    want = {"linux": 1.0, "windows": 1.2, "mac": 3.0}[o]
    if ratios and all(abs(r - want) > 0.15 * want for r in ratios):
        return f"wheelDeltaY/deltaY {ratios[:4]} on {o} (want about -{want})"
    return None


def chk_screen_os(s, cfg):
    o, _ = _chrome_os(s)
    sc = (probe_value(s, "os_surface") or {}).get("screen")
    if not o or not isinstance(sc, dict) or not sc.get("height"):
        return None
    band_h = sc["height"] - sc.get("availHeight", sc["height"])
    top = sc.get("availTop") or 0
    if o == "mac" and band_h > 0 and not (23 <= top <= 25 or 30 <= top <= 39):
        return f"macOS menu bar {top}px"
    if o == "windows" and band_h > 0 and top == 0:
        dpr = sc.get("dpr") or 1
        if not any(abs(band_h - b / dpr) <= 1 for b in (40, 48)):
            return f"Windows taskbar {band_h}px at DPR {dpr}"
    return None


def chk_window_work_area(s, cfg):
    """Linux: a window sits inside the work area. Window managers keep a
    window below a top bar and right of a dock (struts), so screenY < availTop
    or screenX < availLeft cannot happen in Chrome; it is what a Wayland
    persona (window at 0,0, no work-area offset) given X11 bars looks like.
    Quiet when there are no bars (availTop and availLeft 0: stock Chrome on
    Xvfb, Wayland). Windows (taskbar at the bottom, maximized windows overhang
    by 8 px) and macOS are not checked."""
    o, _ = _chrome_os(s)
    sc = (probe_value(s, "os_surface") or {}).get("screen")
    if o != "linux" or not isinstance(sc, dict):
        return None
    top, left = sc.get("availTop") or 0, sc.get("availLeft") or 0
    x, y = sc.get("screenX"), sc.get("screenY")
    if not isinstance(x, (int, float)) or not isinstance(y, (int, float)):
        return None
    if (top > 0 and y < top) or (left > 0 and x < left):
        return f"window at {x},{y} outside the work area starting at {left},{top}"
    return None


# ---- the catalogue ----------------------------------------------------------

def P(id_, weight, cat, why, probe=None):
    return {"id": id_, "weight": weight, "cat": cat, "probe": probe or id_, "why": why}


def S(id_, weight, cat, check, why):
    return {"id": id_, "weight": weight, "cat": cat, "check": check, "why": why}


RULES: list[dict] = [
    # Automation tells: direct evidence of a driver, a CDP client or patching.
    P("webdriver", 95, "automation", "navigator.webdriver is true: the browser was started for automation."),
    P("ua_headless", 95, "automation", "navigator.userAgent says HeadlessChrome."),
    P("ua_ch_headless", 90, "automation", "UA-CH brands (JS or sec-ch-ua) include HeadlessChrome."),
    P("automation_keys", 90, "automation", "Driver globals or attributes (cdc_, __playwright, __pw_, _selenium, ...)."),
    P("cdp_runtime", 80, "automation", "Console argument getters ran: a CDP client has Runtime enabled."),
    P("beh_untrusted", 70, "behavior", "Input events with isTrusted=false: events dispatched by script."),
    P("outer_size_zero", 60, "automation", "outerWidth/outerHeight is 0: no real window."),
    P("native_functions", 50, "automation", "A native getter or method has visible source, a prototype or a wrapper frame."),
    P("iframe_mismatch", 45, "automation", "A fresh iframe sees different navigator values: patching missed frames."),
    P("worker_mismatch", 45, "automation", "A worker sees different navigator values: patching missed workers."),
    P("webdriver_tamper", 40, "automation", "navigator.webdriver was deleted, moved or redefined."),
    P("extra_globals", 30, "automation", "The page window has globals a clean iframe does not: injected script."),
    P("property_order", 30, "automation", "navigator enumerates keys that are not on its prototype chain."),
    P("stack_hooked", 25, "automation", "Error.prepareStackTrace set or stackTraceLimit changed: stack hiding."),
    P("has_focus", 10, "automation", "Visible document without focus at load (common for headless)."),

    # Consistency: values that contradict the claimed browser/OS.
    P("stack_engine", 60, "consistency", "Error.stack format does not match the engine the UA claims."),
    P("ua_ch_mismatch", 50, "consistency", "userAgentData brands/platform/mobile disagree with the UA or sec-ch-ua."),
    P("notification_permission", 50, "consistency", "Notification.permission disagrees with permissions.query()."),
    P("webgl_os_mismatch", 50, "consistency", "WebGL renderer belongs to a different OS than the UA."),
    P("plugins", 45, "consistency", "plugins/mimeTypes count, names or prototypes differ from Chrome/Firefox."),
    P("chrome_object", 40, "consistency", "window.chrome missing (or present on Firefox) for the claimed UA."),
    P("languages", 40, "consistency", "navigator.languages disagrees with the Accept-Language header."),
    P("timezone_mismatch", 40, "consistency", "Intl time zone offset differs from Date.getTimezoneOffset()."),
    P("screen_sanity", 35, "consistency", "Screen and viewport sizes that no real desktop has (viewport == screen, 800x600, ...)."),
    P("fonts", 35, "consistency", "Installed fonts do not fit the UA's OS."),
    P("window_frame_absent", 25, "consistency", "outer == inner size: no tab strip or toolbar."),
    P("hardware", 20, "consistency", "hardwareConcurrency/deviceMemory missing or impossible."),
    P("locale_mismatch", 15, "consistency", "Intl locale language differs from navigator.language."),
    P("perf_now_resolution", 15, "consistency", "performance.now() resolution differs from the claimed browser's."),
    P("timezone_utc", 10, "consistency", "Time zone is UTC: typical of servers and containers, rare on desktops."),
    P("screen_no_taskbar", 10, "consistency", "Windows/macOS screen without taskbar or menu bar."),
    P("connection", 10, "consistency", "navigator.connection missing on Chrome or present on Firefox."),
    P("battery", 10, "consistency", "getBattery missing on Chrome."),

    # Device fingerprint.
    P("webgl_software", 45, "fingerprint", "Software WebGL renderer (SwiftShader, llvmpipe): no GPU."),
    P("canvas", 30, "fingerprint", "Canvas output changes between identical renders or solid fills: noise injection."),
    P("audio", 25, "fingerprint", "OfflineAudioContext output unstable or missing."),
    P("webgl_params", 0, "fingerprint", "Info only: WebGL parameter and extension hashes."),

    # Behaviour (client-side, per page).
    P("beh_typing_no_keys", 45, "behavior", "Text appeared in a field with no key events (CDP insertText, fill())."),
    P("beh_click_center", 40, "behavior", "Clicks land within 1 px of the target's exact centre."),
    P("beh_click_no_path", 35, "behavior", "Clicks with fewer than 3 mouse moves before them: the pointer teleported."),
    P("beh_mouse_linear", 35, "behavior", "85%+ of the mouse path is long dead-straight pieces (within 1 px of a line)."),
    P("beh_key_rhythm", 35, "behavior", "Inter-key intervals almost constant (MAD/median < 0.1)."),
    P("beh_key_dwell", 35, "behavior", "Key hold times under 10 ms, or all the same (MAD/median < 0.08)."),
    P("beh_click_hold", 30, "behavior", "Mouse button held under 8 ms."),
    P("beh_movement_zero", 30, "behavior", "Pointer moves but movementX/Y stay 0."),
    P("beh_input_without_focus", 30, "behavior", "Key or button input while document.hasFocus() is false."),
    P("beh_mouse_velocity", 25, "behavior", "Flat speed profile: stroke middles no faster than their ends."),
    P("beh_ttfi", 25, "behavior", "First click/key/wheel < 300 ms after navigation start or < 150 ms after load."),
    P("beh_scroll_no_input", 25, "behavior", "Page scrolled with no wheel, key, touch or press before it."),
    P("beh_wheel_pattern", 20, "behavior", "One huge wheel delta, or a perfectly regular wheel stream."),
    P("beh_counts", 0, "behavior", "Info only: event counts and page dwell."),

    # Server-side and session-wide.
    S("no_beacon", 95, "server", chk_no_beacon, "The page loaded but no beacon came back: no JavaScript ran."),
    S("non_browser_ua", 90, "server", chk_non_browser_ua, "User-Agent is not a browser (curl, python-requests, ...)."),
    S("ua_headless_header", 95, "server", chk_ua_headless_header, "User-Agent header says HeadlessChrome."),
    S("no_subresources", 60, "server", chk_no_subresources, "No CSS, JS or images fetched: an HTML-only client."),
    S("js_ua_mismatch", 60, "server", chk_js_ua_mismatch, "navigator.userAgent differs from the User-Agent header."),
    S("ua_changed", 50, "server", chk_ua_changed, "The User-Agent changed within one session."),
    S("ch_header_mismatch", 50, "server", chk_ch_header_mismatch, "sec-ch-ua brands do not match the UA major."),
    S("missing_client_hints", 40, "server", chk_missing_client_hints, "Chrome UA without sec-ch-ua."),
    S("missing_fetch_metadata", 40, "server", chk_missing_fetch_metadata, "Browser UA without Sec-Fetch-* headers."),
    S("accept_language_missing", 40, "server", chk_accept_language_missing, "Browser UA without Accept-Language."),
    S("cookie_not_echoed", 40, "server", chk_cookie_not_echoed, "The session cookie was not sent back."),
    S("http_version", 40, "server", chk_http_version, "HTTP/1.0 or another version no current browser uses."),
    S("header_order", 30, "server", chk_header_order, "Header names, case or order differ from the claimed browser."),
    S("no_interaction", 30, "behavior", chk_no_interaction, "No trusted mouse, key, wheel or touch input in the session."),
    S("images_blocked", 30, "server", chk_images_blocked, "CSS/JS fetched but no images: image blocking."),
    S("accept_encoding_mismatch", 25, "server", chk_accept_encoding_mismatch, "Accept-Encoding differs from the claimed browser."),
    S("accept_mismatch", 20, "server", chk_accept_mismatch, "Navigation Accept header differs from the claimed browser."),
    S("page_dwell_short", 15, "behavior", chk_page_dwell_short, "Left a page within 1.5 s of navigation after clicking."),
    S("favicon_missing", 10, "server", chk_favicon_missing, "No favicon request (headless Chrome skips it)."),

    # OS consistency (M7): the claimed OS against OS-specific surfaces. Info
    # probes carry the raw values; the checks above judge them.
    P("os_surface", 0, "consistency", "Info only: scrollbars, OS-gated APIs, fonts, emoji, WebGL limits, screen."),
    P("os_media", 0, "consistency", "Info only: high-entropy UA-CH, codecs, WebGPU, keyboard map, API errors."),
    P("beh_wheel_deltas", 0, "behavior", "Info only: deltaY and wheelDeltaY of trusted wheel events."),
    P("os_voices", 0, "consistency", "Info only: speech voices (count, local names), read at every beacon."),
    S("uach_platform_version", 40, "consistency", chk_uach_platform_version,
      "UA-CH platformVersion impossible for the OS (Linux Chrome sends \"\")."),
    S("uach_arch_gpu", 40, "consistency", chk_uach_arch_gpu,
      "UA-CH architecture/bitness/wow64 contradict the OS or GPU (arm Mac with Intel GPU, ...)."),
    S("ch_high_entropy", 40, "server", chk_ch_high_entropy,
      "High-entropy Sec-CH-UA-* headers missing after Accept-CH, or different from JS."),
    S("api_throws", 40, "consistency", chk_api_throws,
      "canPlayType, isTypeSupported, decodingInfo, getLayoutMap or requestAdapter threw."),
    S("webgl_backend_os", 35, "consistency", chk_webgl_backend_os,
      "WebGL backend or its limits do not fit the OS (OpenGL on Windows, D3D11 limits off)."),
    S("fonts_os_markers", 35, "consistency", chk_fonts_os_markers,
      "Fonts only another OS has (DejaVu on Windows, Segoe UI on Linux, Win11 fonts on Win10)."),
    S("os_api_surface", 30, "consistency", chk_os_api_surface,
      "navigator.share/canShare present on Linux or missing on Windows/macOS."),
    S("generic_fonts_os", 30, "consistency", chk_generic_fonts_os,
      "system-ui, sans-serif or serif resolve to fonts Chrome does not use on that OS."),
    S("voices_os", 30, "consistency", chk_voices_os,
      "Speech voices of another OS, or no local voices on Windows/macOS."),
    S("scrollbar_os", 25, "consistency", chk_scrollbar_os,
      "Scrollbar width of another OS, or an overflowing root without a scrollbar."),
    S("emoji_render", 25, "fingerprint", chk_emoji_render,
      "Emoji drawn without colour: no colour emoji font."),
    S("webgpu_os", 25, "consistency", chk_webgpu_os, "WebGPU adapter vendor differs from the WebGL GPU."),
    S("os_bluetooth", 15, "consistency", chk_os_bluetooth,
      "navigator.bluetooth present on Linux or missing on Windows/macOS."),
    S("codecs_os", 0, "consistency", chk_codecs_os,
      "Info until Windows/macOS captures (target 15): HEVC or H.264 power efficiency contradicts OS and GPU."),
    S("screen_os", 0, "consistency", chk_screen_os,
      "Info until captures (target 15): taskbar or menu bar size impossible for the OS."),
    S("window_work_area", 0, "consistency", chk_window_work_area,
      "Info until a GNOME capture (target 15): Linux window above or left of the work area."),
    S("wheel_os", 0, "behavior", chk_wheel_os,
      "Info until captures (target 20): wheelDeltaY/deltaY ratio of another OS."),
]

RULE_IDS = {r["id"] for r in RULES}
CATEGORIES = ("automation", "consistency", "fingerprint", "behavior", "server")


def load_config(path: str | None = None) -> dict:
    cfg = json.loads(json.dumps(DEFAULT_CONFIG))
    if path:
        user = json.loads(Path(path).read_text())
        for k, v in user.items():
            if isinstance(v, dict) and isinstance(cfg.get(k), dict):
                cfg[k].update(v)
            else:
                cfg[k] = v
    return cfg


def combine(weights) -> float:
    keep = 1.0
    for w in weights:
        keep *= 1 - min(max(w, 0), 99) / 100
    return round(100 * (1 - keep), 1)


def server_signals(s: dict) -> dict:
    """Summary of the server-side record, for reports."""
    d = first_doc(s) or {}
    t0 = d.get("t")
    reqs = s.get("requests", [])
    first_page = [r for r in reqs if r.get("since_doc_ms") is not None][:12]
    return {
        "http_version": d.get("http_version"),
        "scheme": d.get("scheme"),
        "tls": d.get("tls"),
        "header_names": d.get("header_names"),
        "accept": d.get("headers", {}).get("accept"),
        "accept_language": d.get("headers", {}).get("accept-language"),
        "accept_encoding": d.get("headers", {}).get("accept-encoding"),
        "sec_ch_ua": d.get("headers", {}).get("sec-ch-ua"),
        "documents": len(s.get("documents", [])),
        "subresource_order": [f"{r['kind']}:{r['path']}@{r['since_doc_ms']}ms" for r in first_page],
        "first_subresource_ms": (round((reqs[0]["t"] - t0) * 1000, 1) if reqs and t0 else None),
        "cookie_echo": all(r.get("cookie") for r in reqs) if reqs else None,
        "beacons": s.get("beacons", 0),
    }


def score_session(s: dict, cfg: dict | None = None) -> dict:
    cfg = cfg or load_config()
    weights = cfg.get("weights", {})
    disabled = set(cfg.get("disabled", []))
    probes = merged_probes(s)
    fired = []
    for r in RULES:
        if r["id"] in disabled:
            continue
        w = weights.get(r["id"], r["weight"])
        if "probe" in r:
            p = probes.get(r["probe"])
            if not p or not p["suspicious"]:
                continue
            detail = p["value"]
        else:
            detail = r["check"](s, cfg)
            if not detail:
                continue
        fired.append({"id": r["id"], "cat": r["cat"], "weight": w, "detail": detail, "why": r["why"]})
    # Suspicious probes the catalogue does not know yet count with the
    # collector's default weight, capped, so a new probe is visible at once.
    known_probes = {r.get("probe") for r in RULES}
    for pid, p in probes.items():
        if p["suspicious"] and pid not in known_probes and pid not in disabled:
            fired.append({"id": pid, "cat": p.get("cat") or "uncatalogued",
                          "weight": min(int(p.get("weight") or 0), 50), "detail": p["value"],
                          "why": "uncatalogued probe (add it to scoring.RULES)"})
    fired = [f for f in fired if f["weight"] > 0]
    fired.sort(key=lambda f: -f["weight"])
    score = combine(f["weight"] for f in fired)
    by_cat = {c: combine(f["weight"] for f in fired if f["cat"] == c) for c in CATEGORIES}
    th = cfg["thresholds"]
    verdict = "bot" if score >= th["bot"] else "suspect" if score >= th["suspect"] else "human"
    return {
        "sid": s.get("sid"),
        "score": score,
        "verdict": verdict,
        "thresholds": th,
        "by_category": by_cat,
        "fired": fired,
        "pages": len(s.get("pages", {})),
        "events": event_counts(s),
        "signals": server_signals(s),
    }

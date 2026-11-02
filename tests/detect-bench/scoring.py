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
# bench, T-001). Firefox values are Firefox 128 ESR; re-capture before relying
# on them.
EXPECTED = {
    "chrome": {
        "doc_header_order": [
            "Host", "Connection", "sec-ch-ua", "sec-ch-ua-mobile", "sec-ch-ua-platform",
            "Upgrade-Insecure-Requests", "User-Agent", "Accept", "Sec-Fetch-Site", "Sec-Fetch-Mode",
            "Sec-Fetch-User", "Sec-Fetch-Dest", "Accept-Encoding", "Accept-Language", "Cookie",
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

"""Scenario runner: ports of the GeckoPhantom T-001..T-007 scenarios to LightBrowser.

    python -m lightbrowser.scenarios --bench http://127.0.0.1:8765 [--scenario T-004,T-005 | all]
                                     [--persona FILE] [--binary PATH] [--json]

The target is our own detect-bench (`tests/detect-bench/server.py`, ADR 0007),
which must already be running on loopback. Any `--bench` URL that is not
127.0.0.1, ::1 or localhost is refused, and a scenario fails if its browser
requests anything off loopback. The mapping to the GeckoPhantom scenarios is
in README.md.
"""

from __future__ import annotations

import argparse
import json
import random
import sys
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any, Callable

from .bench import Bench, BenchError, Score
from .errors import LaunchError, LightBrowserError, NonLoopbackError, PersonaValidationError
from .expect import Expectation
from .loopback import assert_loopback
from .persona import Persona
from .sync_api import Browser

DEFAULT_WAIT_S = 3.5  # the collector's last beacon goes out 3 s after load


@dataclass
class Options:
    bench: Bench
    persona: Persona
    binary: str | None = None
    wait_s: float = DEFAULT_WAIT_S
    seed: int = 1


@dataclass
class ScenarioResult:
    id: str
    name: str
    status: str  # pass | fail | skip | error
    expect: str = ""
    detail: str = ""
    score: float | None = None
    fp: float | None = None
    beh: float | None = None
    verdict: str = ""
    rules: str = ""
    duration_s: float = 0.0
    notes: list[str] = field(default_factory=list)

    @property
    def passed(self) -> bool:
        return self.status == "pass"


# -- flows (the same as tests/detect-bench/run.js) ---------------------------------


def flow_instant(page: Any, base: str, **_: Any) -> None:
    """Shop, scroll, click a product, search, submit: with Playwright's own input."""
    page.goto(f"{base}/shop")
    page.mouse.wheel(0, 900)
    page.click("#add-3")
    page.fill("#q", "jacket")
    with page.expect_navigation():
        page.click("#search-btn")


def flow_scripted_slow(page: Any, base: str, **_: Any) -> None:
    """The same flow with pauses, stepped mouse moves and per-key delays."""
    page.goto(f"{base}/shop")
    page.wait_for_timeout(800)
    for _i in range(6):
        page.mouse.wheel(0, 120)
        page.wait_for_timeout(150)
    page.locator("#add-3").scroll_into_view_if_needed()

    def center(sel: str) -> tuple[float, float]:
        b = page.locator(sel).bounding_box()
        return b["x"] + b["width"] / 2, b["y"] + b["height"] / 2

    ax, ay = center("#add-3")
    page.mouse.move(ax - 140, ay - 90, steps=10)
    page.mouse.move(ax, ay, steps=25)
    page.wait_for_timeout(300)
    page.click("#add-3", delay=80)
    page.wait_for_timeout(500)
    qx, qy = center("#q")
    page.mouse.move(qx, qy, steps=20)
    page.click("#q", delay=60)
    page.locator("#q").press_sequentially("jacket", delay=120)
    page.wait_for_timeout(400)
    with page.expect_navigation():
        page.keyboard.press("Enter")


def reading_pause(page: Any, rnd: random.Random) -> None:
    """Time spent looking at a page before acting: part of its words at 600-700 wpm."""
    try:
        text = page.locator("body").inner_text()
    except Exception:  # noqa: BLE001 - a pause estimate must never fail a scenario
        text = ""
    words = len(text.split())
    wpm = 600 + 100 * rnd.random()
    share = 0.33 + 0.33 * rnd.random()
    ms = min(6000.0, max(1200.0, words * share / wpm * 60000))
    page.wait_for_timeout(int(ms))


def flow_human(page: Any, base: str, seed: int = 1, **_: Any) -> None:
    """The flow through `page.human_*`, with a reading pause per page."""
    rnd = random.Random(seed)
    page.goto(f"{base}/shop")
    reading_pause(page, rnd)
    page.human_scroll(900)
    page.human_click("#add-3")
    page.human_type("#q", "jacket")
    with page.expect_navigation():
        page.human_click("#search-btn")
    page.wait_for_load_state("load")
    reading_pause(page, rnd)


def flow_idle(page: Any, base: str, **_: Any) -> None:
    """Load two pages and look at them without any input at all."""
    page.goto(f"{base}/shop")
    page.wait_for_timeout(4000)
    page.goto(f"{base}/news")
    page.wait_for_timeout(4000)


# -- running a flow and scoring the session -----------------------------------------


def _session_id(context: Any, page: Any, base: str) -> str | None:
    for c in context.cookies(base):
        if c["name"] == "lb_sid":
            return c["value"]
    try:  # the page embeds its session id for the collector
        return json.loads(page.locator("#lb-ctx").text_content(timeout=2000))["sid"]
    except Exception:  # noqa: BLE001
        return None


def _watch_offsite(page: Any, offsite: list[str]) -> None:
    def on_request(req: Any) -> None:
        url = req.url
        if url.startswith(("http:", "https:", "ws:", "wss:")):
            from .loopback import is_loopback_url

            if not is_loopback_url(url):
                offsite.append(url)

    page.on("request", on_request)


def _finish(context: Any, page: Any, base: str, bench: Bench, wait_s: float) -> Score:
    sid = _session_id(context, page, base)
    try:
        page.close(run_before_unload=True)
    except Exception:  # noqa: BLE001
        pass
    time.sleep(0.3)  # pagehide beacon
    if not sid:
        raise BenchError("no session cookie: the page never reached the bench")
    return bench.score(sid)


def run_on_lightbrowser(opts: Options, flow: Callable[..., None]) -> Score:
    """Fresh `serve` with the persona, run `flow`, return the bench's score for the session."""
    base = opts.bench.base
    offsite: list[str] = []
    with Browser(persona=opts.persona, executable_path=opts.binary) as browser:
        page = browser.new_page(ignore_https_errors=True)
        _watch_offsite(page, offsite)
        context = page.context
        flow(page, base, seed=opts.seed)
        time.sleep(opts.wait_s)
        score = _finish(context, page, base, opts.bench, opts.wait_s)
    if offsite:
        raise NonLoopbackError(f"the browser requested non-loopback URLs: {offsite[:3]}")
    return score


def run_on_stock_chromium(opts: Options, flow: Callable[..., None]) -> Score:
    """Playwright's own headless Chromium (the control). Raises LaunchError if it is not installed."""
    from playwright.sync_api import Error as PlaywrightError, sync_playwright

    base = opts.bench.base
    offsite: list[str] = []
    with sync_playwright() as pw:
        try:
            browser = pw.chromium.launch(headless=True)
        except PlaywrightError as e:
            raise LaunchError(f"stock Chromium is not available: {str(e).splitlines()[0]}") from e
        try:
            context = browser.new_context(ignore_https_errors=True)
            page = context.new_page()
            _watch_offsite(page, offsite)
            flow(page, base, seed=opts.seed)
            time.sleep(opts.wait_s)
            score = _finish(context, page, base, opts.bench, opts.wait_s)
            context.close()
        finally:
            browser.close()
    if offsite:
        raise NonLoopbackError(f"the browser requested non-loopback URLs: {offsite[:3]}")
    return score


# -- scenarios ------------------------------------------------------------------------


@dataclass(frozen=True)
class Scenario:
    id: str
    name: str
    summary: str
    expect: str  # "" when the scenario checks something other than a score
    run: Callable[[Options, "Scenario"], ScenarioResult]


def _scored(opts: Options, sc: Scenario, runner: Callable[..., Score], flow: Callable[..., None]) -> ScenarioResult:
    score = runner(opts, flow)
    exp = Expectation.parse(sc.expect)
    ok = exp.check(score)
    return ScenarioResult(
        id=sc.id, name=sc.name, status="pass" if ok else "fail", expect=sc.expect,
        detail=exp.describe(score), score=score.score, fp=score.fp, beh=score.beh,
        verdict=score.verdict, rules=score.top_rules(),
    )


def t001(opts: Options, sc: Scenario) -> ScenarioResult:
    return _scored(opts, sc, run_on_stock_chromium, flow_instant)


def t002(opts: Options, sc: Scenario) -> ScenarioResult:
    return _scored(opts, sc, run_on_lightbrowser, flow_instant)


def t003(opts: Options, sc: Scenario) -> ScenarioResult:
    return _scored(opts, sc, run_on_stock_chromium, flow_scripted_slow)


def t004(opts: Options, sc: Scenario) -> ScenarioResult:
    return _scored(opts, sc, run_on_lightbrowser, flow_human)


def t006(opts: Options, sc: Scenario) -> ScenarioResult:
    return _scored(opts, sc, run_on_lightbrowser, flow_idle)


def t007(opts: Options, sc: Scenario) -> ScenarioResult:
    res = _scored(opts, sc, run_on_lightbrowser, flow_instant)
    res.notes.append("TLS/HTTP2 (JA3, JA4, Akamai H2) are checked by `cargo test -p lb-net --test fingerprint`; the bench speaks plain HTTP/1.1")
    return res


def inconsistent_persona(base: Persona) -> Persona:
    """`base` with the platform changed so that it contradicts the User-Agent (rule R01)."""
    data = json.loads(json.dumps(base.data))
    ua = data.get("browser", {}).get("user_agent", "")
    data.setdefault("os", {})["platform"] = "MacIntel" if "Windows" in ua else "Win32"
    data["profile_name"] = "SDK scenario: deliberately inconsistent persona"
    return Persona(data)


def t005(opts: Options, sc: Scenario) -> ScenarioResult:
    bad = inconsistent_persona(opts.persona)
    rep = bad.check(opts.binary)
    if rep.ok or "R01" not in rep.output:
        return ScenarioResult(sc.id, sc.name, "fail", detail=f"validate accepted the inconsistent persona: {rep.output[:200]}")
    try:
        with Browser(persona=bad, executable_path=opts.binary, validate=False, startup_timeout=20):
            return ScenarioResult(sc.id, sc.name, "fail", detail="serve started with an inconsistent persona")
    except LaunchError as e:
        if "inconsistent" not in str(e):
            return ScenarioResult(sc.id, sc.name, "fail", detail=f"serve failed, but not on the consistency rule: {str(e)[:200]}")
    return ScenarioResult(
        sc.id, sc.name, "pass", detail="validate exits 1 with R01_UA_PLATFORM; serve refuses to start",
        notes=[l.strip() for l in rep.output.splitlines() if "VIOLATION" in l],
    )


SCENARIOS: dict[str, Scenario] = {
    s.id: s
    for s in [
        Scenario("T-001", "baseline-stock-chromium", "Stock headless Chromium (control): the bench must call it a bot.", ">=90", t001),
        Scenario("T-002", "naive-playwright-input", "LightBrowser with Playwright's own input: fingerprint clean, input flagged.", "fp<=30&beh>=70", t002),
        Scenario("T-003", "scripted-slow-stock", "Stock Chromium, slowed-down script: still a bot on fingerprint.", ">=70&fp>=70", t003),
        Scenario("T-004", "full-persona-human-input", "LightBrowser + persona + page.human_*: must score as human.", "fp<=30&beh<=30", t004),
        Scenario("T-005", "consistency-violation", "An inconsistent persona is rejected by validate and by serve.", "", t005),
        Scenario("T-006", "behaviour-only", "Clean fingerprint, no input at all: behaviour rules fire.", "fp<=30&beh>=30", t006),
        Scenario("T-007", "network-only", "Server-side signals (headers, HTTP, cookies) of a LightBrowser session are clean.", "server<=10", t007),
    ]
}


def run_scenario(opts: Options, sc: Scenario) -> ScenarioResult:
    t0 = time.monotonic()
    try:
        res = sc.run(opts, sc)
    except LaunchError as e:
        # Missing stock Chromium is a skipped control, not a LightBrowser failure.
        status = "skip" if "stock Chromium" in str(e) else "error"
        res = ScenarioResult(sc.id, sc.name, status, expect=sc.expect, detail=str(e).splitlines()[0])
    except (LightBrowserError, OSError) as e:
        res = ScenarioResult(sc.id, sc.name, "error", expect=sc.expect, detail=f"{type(e).__name__}: {e}"[:300])
    res.duration_s = round(time.monotonic() - t0, 1)
    return res


def format_table(results: list[ScenarioResult]) -> str:
    rows = [["scenario", "score", "fp", "beh", "expect", "result", "detail"]]
    for r in results:
        def n(v: float | None) -> str:
            return "-" if v is None else f"{v:g}"

        detail = r.detail if r.status != "pass" or not r.rules else f"{r.detail}; {r.rules}"
        rows.append([f"{r.id} {r.name}", n(r.score), n(r.fp), n(r.beh), r.expect or "-", r.status.upper(), detail])
    widths = [max(len(row[i]) for row in rows) for i in range(len(rows[0]) - 1)]
    return "\n".join("  ".join(c.ljust(w) for c, w in zip(row, widths)) + "  " + row[-1] for row in rows)


def default_persona_path() -> Path | None:
    refs = Persona.references("chrome")
    for p in refs:
        if "win10" in p.name:
            return p
    return refs[0] if refs else None


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    ap = argparse.ArgumentParser(
        prog="python -m lightbrowser.scenarios",
        description="Run the LightBrowser scenarios against the local detect-bench (loopback only).",
    )
    ap.add_argument("--bench", required=True, help="bench base URL, for example http://127.0.0.1:8765")
    ap.add_argument("--scenario", default="all", help="comma-separated ids (T-001 ... T-007) or 'all'")
    ap.add_argument("--persona", help="persona file (default: the Chrome 153 Windows reference persona)")
    ap.add_argument("--binary", help="path to the lightbrowser executable (default: $LIGHTBROWSER_BIN or target/)")
    ap.add_argument("--wait", type=float, default=DEFAULT_WAIT_S, help="seconds to wait for the last beacon (default 3.5)")
    ap.add_argument("--seed", type=int, default=1, help="seed for the reading pauses")
    ap.add_argument("--json", action="store_true", help="print results as JSON")
    ap.add_argument("--list", action="store_true", help="list the scenarios and exit")
    return ap.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.list:
        for s in SCENARIOS.values():
            print(f"{s.id}  {s.name:<26} {s.expect or '(custom check)':<16} {s.summary}")
        return 0
    try:
        assert_loopback(args.bench, "--bench URL")
    except NonLoopbackError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    ids = list(SCENARIOS) if args.scenario.lower() == "all" else [s.strip().upper() for s in args.scenario.split(",") if s.strip()]
    unknown = [i for i in ids if i not in SCENARIOS]
    if unknown:
        print(f"error: unknown scenario {', '.join(unknown)} (have {', '.join(SCENARIOS)})", file=sys.stderr)
        return 2
    path = Path(args.persona) if args.persona else default_persona_path()
    if path is None:
        print("error: no persona given and no profiles/reference/chrome found", file=sys.stderr)
        return 2
    try:
        persona = Persona.load(path)
        bench = Bench(args.bench)
        bench.check_reachable()
    except (PersonaValidationError, BenchError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    opts = Options(bench=bench, persona=persona, binary=args.binary, wait_s=args.wait, seed=args.seed)
    results = []
    for i in ids:
        print(f"running {i} {SCENARIOS[i].name} ...", file=sys.stderr, flush=True)
        results.append(run_scenario(opts, SCENARIOS[i]))
    if args.json:
        print(json.dumps([asdict(r) for r in results], indent=1))
    print(format_table(results))
    return 1 if any(r.status in ("fail", "error") for r in results) else 0


if __name__ == "__main__":
    sys.exit(main())

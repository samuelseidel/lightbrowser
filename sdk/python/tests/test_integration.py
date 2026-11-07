"""Integration tests: launch the real binary and drive it over CDP.

Run with `pytest -m integration`. They need `target/debug/lightbrowser` (or
release, or $LIGHTBROWSER_BIN), built for the chrome family. Everything runs
on 127.0.0.1 against a page served by the test.
"""

from __future__ import annotations

import os

import pytest

from lightbrowser import Browser, HumanInputError, LaunchError, Persona, PersonaValidationError, attach
from lightbrowser._common import leaky_playwright

pytestmark = pytest.mark.integration

LEAKY = leaky_playwright()


def events(page, kind=None):
    log = page.evaluate("window.log")
    return [e for e in log if kind is None or e["t"] == kind]


# -- launch and stock Playwright ---------------------------------------------------------


def test_launch_returns_stock_playwright_objects(lb, page_url):
    from playwright.sync_api import BrowserContext, Page

    assert lb.ws_endpoint.startswith("ws://127.0.0.1:")
    assert lb.pid and lb.is_connected()
    ctx = lb.new_context()
    page = ctx.new_page()
    assert type(page) is Page and type(ctx) is BrowserContext
    page.goto(page_url)
    assert page.title() == "SDK test page"
    assert page.evaluate("navigator.webdriver") is False
    # Stock input still works next to the helpers.
    page.click("#btn")
    assert events(page, "click")[0]["id"] == "btn"
    ctx.close()


def test_new_page_keeps_the_persona_window(lb, page_url):
    page = lb.new_page()
    page.goto(page_url)
    # The default persona is 1920x1080; no viewport override was applied.
    assert page.evaluate("screen.width") == 1920
    assert page.evaluate("innerWidth") == 1920
    page.context.close()


def test_helpers_are_attached_to_every_page(lb, page_url):
    ctx = lb.new_context()
    first = ctx.new_page()
    second = ctx.new_page()
    for p in (first, second):
        for name in ("human_move", "human_click", "human_type", "human_scroll", "verify_fingerprint"):
            assert callable(getattr(p, name)), name
    assert attach(first) is attach(first)
    ctx.close()


def test_close_stops_the_process(binary, lb):
    b = Browser(executable_path=binary, playwright=lb.playwright)
    with b:
        proc = b._server.proc
        tmp = b._plan
        assert proc.poll() is None
    assert proc.poll() is not None
    with pytest.raises(LaunchError):
        b.launch()
    assert tmp is not None


def test_two_browsers_get_different_ports(binary, lb):
    with Browser(executable_path=binary, playwright=lb.playwright) as a, Browser(executable_path=binary, playwright=lb.playwright) as b:
        assert a.ws_endpoint != b.ws_endpoint


# -- persona -----------------------------------------------------------------------------


@pytest.fixture(scope="module")
def reference_persona():
    refs = Persona.references("chrome")
    if not refs:
        pytest.skip("no reference personas found")
    return Persona.load(refs[0])


def test_reference_personas_validate(binary):
    refs = Persona.references("chrome")
    assert refs
    for path in refs:
        rep = Persona.load(path).validate(binary)
        assert rep.ok and rep.violations == 0


def test_inconsistent_persona_is_rejected(binary, reference_persona):
    from lightbrowser.scenarios import inconsistent_persona

    bad = inconsistent_persona(reference_persona)
    rep = bad.check(binary)
    assert not rep.ok and "R01" in rep.output
    with pytest.raises(PersonaValidationError, match="invalid"):
        bad.validate(binary)
    with pytest.raises(PersonaValidationError):
        Browser(persona=bad, executable_path=binary)
    # Even without the SDK's own check, serve refuses it.
    with pytest.raises(LaunchError, match="inconsistent"):
        with Browser(persona=bad, executable_path=binary, validate=False, startup_timeout=20):
            pass


def test_dict_persona_runs_and_temp_files_are_removed(binary, lb, reference_persona, page_url):
    b = Browser(persona=reference_persona.data, executable_path=binary, playwright=lb.playwright)
    tmp = b._plan.tmpdir.name
    assert os.path.isdir(tmp)
    with b:
        page = b.new_page()
        page.goto(page_url)
        assert page.evaluate("navigator.userAgent") == reference_persona.user_agent
    assert not os.path.exists(tmp)


# -- human input ----------------------------------------------------------------------------


def test_human_click_selector(page, human_ready):
    box = page.locator("#btn").bounding_box()
    x, y = page.human_click("#btn")
    assert box["x"] <= x <= box["x"] + box["width"]
    assert box["y"] <= y <= box["y"] + box["height"]
    clicks = events(page, "click")
    assert len(clicks) == 1 and clicks[0]["trusted"] and clicks[0]["id"] == "btn"
    assert abs(clicks[0]["x"] - x) <= 1 and abs(clicks[0]["y"] - y) <= 1
    # A real path leads to the click: several moves, then press and release.
    assert len(events(page, "mousemove")) >= 5
    assert [e["trusted"] for e in events(page, "mousedown") + events(page, "mouseup")] == [True, True]


def test_human_click_coordinates(page, human_ready):
    x, y = page.human_click(x=150, y=140)
    # The pointer lands near the requested point, not exactly on it.
    assert abs(x - 150) <= 25 and abs(y - 140) <= 25
    assert events(page, "click")[0]["id"] == "btn"


def test_human_click_positional_coordinates(page, human_ready):
    page.human_click(150, 140)
    assert events(page, "click")[0]["id"] == "btn"


def test_human_click_is_not_always_the_centre(page, human_ready):
    box = page.locator("#btn").bounding_box()
    cx, cy = box["x"] + box["width"] / 2, box["y"] + box["height"] / 2
    points = []
    for _ in range(4):
        points.append(page.human_click("#btn"))
    assert any(abs(x - cx) > 1 or abs(y - cy) > 1 for x, y in points)


def test_human_type(page, human_ready):
    text = "jacket and boots"
    page.human_type("#q", text)
    assert page.input_value("#q") == text
    downs = [e for e in events(page, "keydown") if e["trusted"]]
    # At least one keydown per character (typos add Backspace and retyped keys).
    assert len(downs) >= len(text)
    assert all(e["trusted"] for e in events(page, "keyup"))
    assert events(page, "click") and events(page, "click")[0]["id"] == "q"  # focused by a click


def test_human_type_into_focused_element(page, human_ready):
    page.focus("#q")
    page.human_type(None, "abc")
    assert page.input_value("#q") == "abc"


def test_human_scroll_by_distance(page, human_ready):
    assert page.evaluate("scrollY") == 0
    page.human_scroll(700)
    y = page.evaluate("scrollY")
    assert y > 100
    wheels = events(page, "wheel")
    assert wheels and all(w["trusted"] for w in wheels)
    page.human_scroll(-300)
    assert page.evaluate("scrollY") < y


def test_human_scroll_to_selector(page, human_ready):
    vh = page.evaluate("innerHeight")
    assert page.locator("#far").bounding_box()["y"] > vh
    page.human_scroll(selector="#far")
    box = page.locator("#far").bounding_box()
    assert 0 <= box["y"] and box["y"] + box["height"] <= vh


def test_human_click_scrolls_a_far_element_into_view(page, human_ready):
    page.human_click("#far")
    assert page.evaluate("scrollY") > 0
    assert events(page, "click")[-1]["id"] == "far"


def test_human_move(page, human_ready):
    page.human_move(400, 300)
    moves = events(page, "mousemove")
    assert moves and all(m["trusted"] for m in moves)
    assert abs(moves[-1]["x"] - 400) <= 1 and abs(moves[-1]["y"] - 300) <= 1


def test_human_errors(page, human_ready):
    with pytest.raises(HumanInputError, match="No node found for selector: #nope"):
        page.human_click("#nope")
    with pytest.raises(HumanInputError, match="No node found"):
        page.human_type("#nope", "x")
    with pytest.raises(HumanInputError, match="No node found"):
        page.human_scroll(selector="#nope")
    with pytest.raises(ValueError):
        page.human_click()


def test_nothing_automation_related_in_the_page(page, human_ready):
    page.human_click("#btn")
    page.verify_fingerprint()
    leaked = page.evaluate(
        "Object.getOwnPropertyNames(window).filter(n => /LightBrowser|human|cdc_|puppeteer|webdriver/i.test(n))"
    )
    assert leaked == []


# -- fingerprint report -----------------------------------------------------------------------


def test_verify_fingerprint(page):
    report = page.verify_fingerprint()
    assert report.family == "chrome"
    assert report.summary["ok"] > 50 and report.summary["unimplemented"] == 0
    names = [c.name for c in report.mismatches]
    if LEAKY:
        # Playwright itself defines the two globals; nothing else may differ.
        assert names in ([], ["noTells.mainWorldGlobals"]), names
    else:
        assert report.ok and names == [], str(report)
    assert any(c.name == "navigator.userAgent" and c.status == "ok" for c in report.checks)


def test_leaky_playwright_warns(binary, lb):
    if not LEAKY:
        pytest.skip("Playwright is recent enough")
    import lightbrowser._common as common

    common._warned = False
    with pytest.warns(UserWarning, match="__pwInitScripts"):
        with Browser(executable_path=binary, playwright=lb.playwright):
            pass



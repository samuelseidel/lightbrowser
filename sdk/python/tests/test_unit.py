"""Unit tests: no binary and no browser needed."""

from __future__ import annotations

import os
import stat
import sys
import textwrap

import pytest

from lightbrowser import (
    BinaryNotFoundError,
    FingerprintReport,
    LaunchError,
    NonLoopbackError,
    Persona,
    assert_loopback,
    is_loopback_url,
)
from lightbrowser import _human
from lightbrowser.bench import Bench, Score
from lightbrowser.expect import Expectation
from lightbrowser.launcher import (
    ServerProcess,
    build_serve_args,
    find_binary,
    parse_devtools_line,
)
from lightbrowser.persona import parse_validate_output
from lightbrowser import scenarios

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="uses /bin/sh fake binaries")


# -- loopback guard ------------------------------------------------------------------


@pytest.mark.parametrize(
    "url",
    [
        "http://127.0.0.1:8765/shop",
        "http://localhost:8765",
        "https://localhost/x",
        "http://[::1]:9/",
        "ws://127.0.0.1:9222/devtools/browser/abc",
    ],
)
def test_loopback_urls_allowed(url):
    assert is_loopback_url(url)
    assert assert_loopback(url) == url


@pytest.mark.parametrize(
    "url",
    [
        "http://example.com/",
        "https://bot.sannysoft.com",
        "http://127.0.0.1.example.com/",
        "http://localhost.evil.test/",
        "http://127.0.0.1@evil.test/",
        "http://evil.test#@127.0.0.1/",
        "http://10.0.0.1:8765",
        "http://0.0.0.0:8765",
        "http://[::2]:1/",
        "127.0.0.1:8765",
        "",
        "http://127.0.0.1:notaport/",
    ],
)
def test_non_loopback_urls_refused(url):
    assert not is_loopback_url(url)
    with pytest.raises(NonLoopbackError):
        assert_loopback(url)


def test_bench_client_refuses_remote():
    with pytest.raises(NonLoopbackError):
        Bench("https://example.com")
    assert Bench("http://127.0.0.1:1/").base == "http://127.0.0.1:1"


def test_scenarios_cli_refuses_remote(capsys):
    assert scenarios.main(["--bench", "http://example.com:8765"]) == 2
    assert "refusing" in capsys.readouterr().err


def test_scenarios_cli_unknown_scenario(capsys):
    assert scenarios.main(["--bench", "http://127.0.0.1:1", "--scenario", "T-099"]) == 2
    assert "unknown scenario" in capsys.readouterr().err


def test_scenarios_cli_bench_down(capsys):
    assert scenarios.main(["--bench", "http://127.0.0.1:1", "--scenario", "T-005"]) == 2
    assert "error:" in capsys.readouterr().err


def test_scenarios_list(capsys):
    assert scenarios.main(["--bench", "http://127.0.0.1:1", "--list"]) == 0
    out = capsys.readouterr().out
    assert all(f"T-00{i}" in out for i in range(1, 8))


# -- ws line parsing and command line -----------------------------------------------


def test_parse_devtools_line():
    line = "\nDevTools listening on ws://127.0.0.1:35907/devtools/browser/eedb322e-2689\n"
    assert parse_devtools_line(line) == "ws://127.0.0.1:35907/devtools/browser/eedb322e-2689"
    assert parse_devtools_line("persona c1530a10") is None
    assert parse_devtools_line("DevTools listening on nothing") is None
    assert parse_devtools_line("") is None


def test_build_serve_args_minimal():
    assert build_serve_args("/x/lb") == ["/x/lb", "serve", "--port", "0"]


def test_build_serve_args_full(tmp_path):
    cmd = build_serve_args(
        "lb", persona_path=tmp_path / "p.json", port=9333, insecure=True,
        profiles_dir=tmp_path / "tls", workers=3, extra_args=["--remote-allow-origins", "*"],
    )
    assert cmd == [
        "lb", "serve", "--port", "9333", "--persona", str(tmp_path / "p.json"),
        "--profiles-dir", str(tmp_path / "tls"), "--insecure", "--workers", "3",
        "--remote-allow-origins", "*",
    ]


@pytest.mark.parametrize("kw", [{"port": -1}, {"port": 70000}, {"workers": 0}])
def test_build_serve_args_rejects_bad_values(kw):
    with pytest.raises(ValueError):
        build_serve_args("lb", **kw)


def test_serve_args_never_choose_a_bind_address():
    # The server binds 127.0.0.1 itself; the SDK has no way to pass another host.
    assert "--host" not in build_serve_args("lb", port=1)


# -- binary lookup --------------------------------------------------------------------


def _fake_binary(path, body: str):
    path.write_text("#!/bin/sh\n" + textwrap.dedent(body))
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def test_find_binary_explicit_and_missing(tmp_path):
    exe = _fake_binary(tmp_path / "lb", "exit 0\n")
    assert find_binary(exe) == str(exe)
    with pytest.raises(BinaryNotFoundError):
        find_binary(tmp_path / "nope")


def test_find_binary_env(tmp_path, monkeypatch):
    exe = _fake_binary(tmp_path / "lb", "exit 0\n")
    monkeypatch.setenv("LIGHTBROWSER_BIN", str(exe))
    assert find_binary() == str(exe)
    monkeypatch.setenv("LIGHTBROWSER_BIN", str(tmp_path / "gone"))
    with pytest.raises(BinaryNotFoundError, match="LIGHTBROWSER_BIN"):
        find_binary()


def test_find_binary_explicit_beats_env(tmp_path, monkeypatch):
    a = _fake_binary(tmp_path / "a", "exit 0\n")
    b = _fake_binary(tmp_path / "b", "exit 0\n")
    monkeypatch.setenv("LIGHTBROWSER_BIN", str(a))
    assert find_binary(b) == str(b)


def test_find_binary_skips_other_family(tmp_path, monkeypatch):
    monkeypatch.delenv("LIGHTBROWSER_BIN", raising=False)
    (tmp_path / "target" / "release").mkdir(parents=True)
    ff = _fake_binary(tmp_path / "target" / "release" / "lightbrowser", 'echo "lightbrowser 0.0.1 (firefox)"\n')
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr("lightbrowser.launcher._checkout_roots", lambda: [tmp_path])
    monkeypatch.setenv("PATH", "")
    with pytest.raises(BinaryNotFoundError, match="wrong family"):
        find_binary(family="chrome")
    assert find_binary(family="firefox") == str(ff)
    assert find_binary() == str(ff)  # no family: first candidate


def test_find_binary_prefers_release_over_debug(tmp_path, monkeypatch):
    monkeypatch.delenv("LIGHTBROWSER_BIN", raising=False)
    for prof in ("debug", "release"):
        (tmp_path / "target" / prof).mkdir(parents=True)
        _fake_binary(tmp_path / "target" / prof / "lightbrowser", 'echo "lightbrowser 0.0.1 (chrome)"\n')
    monkeypatch.setattr("lightbrowser.launcher._checkout_roots", lambda: [tmp_path])
    assert find_binary(family="chrome") == str(tmp_path / "target" / "release" / "lightbrowser")


# -- server process (fake binaries) --------------------------------------------------


def test_server_process_parses_endpoint_and_terminates(tmp_path):
    exe = _fake_binary(
        tmp_path / "lb",
        """
        echo "persona abc" >&2
        echo >&2
        echo "DevTools listening on ws://127.0.0.1:34567/devtools/browser/zzz" >&2
        exec sleep 60
        """,
    )
    sp = ServerProcess([str(exe), "serve"], startup_timeout=10)
    try:
        assert sp.ws_endpoint == "ws://127.0.0.1:34567/devtools/browser/zzz"
        assert sp.port == 34567
        assert sp.alive()
        assert "persona abc" in sp.stderr_tail()
    finally:
        sp.close()
    assert not sp.alive()
    sp.close()  # idempotent


def test_server_process_early_exit_reports_stderr(tmp_path):
    exe = _fake_binary(tmp_path / "lb", 'echo "error: loading p.json: persona is inconsistent" >&2\nexit 1\n')
    with pytest.raises(LaunchError, match="persona is inconsistent"):
        ServerProcess([str(exe)], startup_timeout=10)


def test_server_process_timeout_kills_child(tmp_path):
    exe = _fake_binary(tmp_path / "lb", "exec sleep 60\n")
    with pytest.raises(LaunchError, match="no 'DevTools listening on' line"):
        ServerProcess([str(exe)], startup_timeout=0.5)


def test_server_process_rejects_non_loopback_endpoint(tmp_path):
    exe = _fake_binary(tmp_path / "lb", 'echo "DevTools listening on ws://example.com:9222/devtools/browser/x" >&2\nexec sleep 60\n')
    with pytest.raises(NonLoopbackError):
        ServerProcess([str(exe)], startup_timeout=10)


def test_server_process_drains_stderr_after_ready(tmp_path):
    # More than a pipe buffer of output after the ready line must not block the child.
    exe = _fake_binary(
        tmp_path / "lb",
        """
        echo "DevTools listening on ws://127.0.0.1:1/devtools/browser/x" >&2
        i=0
        while [ $i -lt 4000 ]; do echo "log line $i padding padding padding padding padding" >&2; i=$((i+1)); done
        echo done > "$0.done"
        exec sleep 60
        """,
    )
    sp = ServerProcess([str(exe)], startup_timeout=10)
    try:
        done = tmp_path / "lb.done"
        for _ in range(100):
            if done.exists():
                break
            import time

            time.sleep(0.05)
        assert done.exists()
    finally:
        sp.close()


def test_server_process_missing_binary(tmp_path):
    with pytest.raises(LaunchError):
        ServerProcess([str(tmp_path / "does-not-exist")])


# -- persona --------------------------------------------------------------------------


def test_parse_validate_output_ok():
    out = "p.json\nChrome 153 on Windows 10 (chrome, schema 2.0, id)\nOK: 2 warning(s)\n"
    rep = parse_validate_output(out, 0)
    assert rep.ok and rep.warnings == 2 and rep.violations == 0


def test_parse_validate_output_violation():
    out = "p.json\nx\n  [VIOLATION] [R01_UA_PLATFORM] bad\nINVALID: 1 violation(s), 0 warning(s)\n"
    rep = parse_validate_output(out, 1)
    assert not rep.ok and rep.violations == 1 and "R01_UA_PLATFORM" in rep.output


def test_parse_validate_output_load_error():
    rep = parse_validate_output("/nope\nERROR: cannot read persona: No such file\n", 1)
    assert not rep.ok


def test_persona_fields_and_dict_roundtrip(tmp_path):
    data = {"profile_id": "id-1", "profile_name": "N", "browser": {"family": "firefox", "user_agent": "UA"}, "os": {"name": "Linux"}}
    p = Persona(data)
    assert (p.profile_id, p.name, p.family, p.os_name, p.user_agent) == ("id-1", "N", "firefox", "Linux", "UA")
    out = p.write_to(tmp_path)
    assert Persona.load(out).data == data
    assert Persona({}).family == "chrome"


def test_persona_load_errors(tmp_path):
    from lightbrowser import PersonaValidationError

    with pytest.raises(PersonaValidationError):
        Persona.load(tmp_path / "missing.json")
    bad = tmp_path / "bad.json"
    bad.write_text("[1]")
    with pytest.raises(PersonaValidationError):
        Persona.load(bad)


def test_persona_check_with_fake_binary(tmp_path):
    exe = _fake_binary(
        tmp_path / "lb",
        """
        echo "$2"
        echo "INVALID: 1 violation(s), 0 warning(s)"
        exit 1
        """,
    )
    from lightbrowser import PersonaValidationError

    p = Persona({"profile_id": "x"})
    rep = p.check(exe)
    assert not rep.ok and rep.violations == 1
    with pytest.raises(PersonaValidationError) as ei:
        p.validate(exe)
    assert "INVALID" in ei.value.report


def test_inconsistent_persona_helper():
    base = Persona({"browser": {"user_agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64)"}, "os": {"platform": "Win32"}})
    bad = scenarios.inconsistent_persona(base)
    assert bad.data["os"]["platform"] == "MacIntel"
    assert base.data["os"]["platform"] == "Win32"  # the original is untouched


# -- human parameter building -----------------------------------------------------------


def test_click_params_selector():
    assert _human.click_params("#a") == {"selector": "#a"}
    assert _human.click_params("#a", button="right", click_count=2) == {"selector": "#a", "button": "right", "clickCount": 2}


def test_click_params_coordinates():
    assert _human.click_params(x=10, y=20.5) == {"x": 10.0, "y": 20.5}
    assert _human.click_params(10, 20) == {"x": 10.0, "y": 20.0}  # human_click(10, 20)


@pytest.mark.parametrize(
    "args,kw,exc",
    [
        ((), {}, ValueError),
        (("#a",), {"x": 1, "y": 2}, ValueError),
        ((), {"x": 1}, ValueError),
        (("",), {}, ValueError),
        (("#a",), {"button": "back"}, ValueError),
        (("#a",), {"click_count": 0}, ValueError),
        (("#a",), {"click_count": 4}, ValueError),
        ((), {"x": "1", "y": 2}, TypeError),
        ((), {"x": float("nan"), "y": 2}, ValueError),
        ((5,), {}, TypeError),
    ],
)
def test_click_params_rejects(args, kw, exc):
    with pytest.raises(exc):
        _human.click_params(*args, **kw)


def test_type_params():
    assert _human.type_params("#q", "hi") == {"text": "hi", "selector": "#q"}
    assert _human.type_params(None, "hi") == {"text": "hi"}
    assert _human.type_params("#q", "") == {"text": "", "selector": "#q"}
    with pytest.raises(TypeError):
        _human.type_params("#q", 5)
    with pytest.raises(ValueError):
        _human.type_params("  ", "x")


def test_scroll_params():
    assert _human.scroll_params(600) == {"deltaY": 600.0}
    assert _human.scroll_params(-100, 20) == {"deltaY": -100.0, "deltaX": 20.0}
    assert _human.scroll_params(selector="#far") == {"selector": "#far"}
    assert _human.scroll_params(300, x=5, y=6) == {"deltaY": 300.0, "x": 5.0, "y": 6.0}
    with pytest.raises(ValueError):
        _human.scroll_params()
    with pytest.raises(ValueError):
        _human.scroll_params(10, x=5)


def test_move_params():
    assert _human.move_params(1, 2) == {"x": 1.0, "y": 2.0}
    with pytest.raises(TypeError):
        _human.move_params(True, 2)


def test_to_human_error_keeps_browser_text():
    class E(Exception):
        pass

    err = _human.to_human_error(
        "LightBrowser.humanClick",
        E("Protocol error (LightBrowser.humanClick): No node found for selector: #x\nCall log: ..."),
    )
    assert str(err) == "LightBrowser.humanClick: No node found for selector: #x"


def test_click_point():
    assert _human.click_point({"x": 1, "y": 2.5}) == (1.0, 2.5)
    from lightbrowser import HumanInputError

    with pytest.raises(HumanInputError):
        _human.click_point({})


# -- fingerprint report -----------------------------------------------------------------


def test_fingerprint_report_from_dict():
    raw = {
        "ok": False, "persona": "p", "family": "chrome", "url": "about:blank",
        "summary": {"ok": 1, "mismatch": 1, "info": 0, "unimplemented": 0},
        "checks": [
            {"name": "a", "status": "ok", "expected": 1, "actual": 1},
            {"name": "b", "status": "mismatch", "expected": "x", "actual": "y"},
        ],
        "surface": {"k": 1},
    }
    r = FingerprintReport.from_dict(raw)
    assert not r.ok and not r.passed
    assert [c.name for c in r.mismatches] == ["b"]
    assert "mismatch: b: expected 'x', got 'y'" in str(r)
    assert FingerprintReport.from_dict({"ok": True}).passed


# -- scores and expectations -------------------------------------------------------------


def _score(fired, by_cat=None, score=0.0):
    return Score("sid", score, "human", by_cat or {}, fired)


def test_score_parts_match_bench_formula():
    s = _score([
        {"id": "a", "cat": "behavior", "weight": 45},
        {"id": "b", "cat": "behavior", "weight": 40},
        {"id": "c", "cat": "server", "weight": 10},
    ])
    assert s.beh == round(1000 * (1 - 0.55 * 0.60)) / 10 == 67.0
    assert s.fp == 10.0
    assert _score([]).fp == 0.0


def test_expectation_grammar():
    s = _score([{"id": "a", "cat": "behavior", "weight": 40}], {"server": 5.0}, score=40.0)
    assert Expectation.parse(">=30").check(s)
    assert not Expectation.parse(">=50").check(s)
    assert Expectation.parse("fp<=30&beh>=30").check(s)
    assert not Expectation.parse("fp<=30&beh<=30").check(s)
    assert Expectation.parse("server<=10").check(s)
    for bad in ("", "fp<30", "beh=3", "&", "fp<=x"):
        with pytest.raises(ValueError):
            Expectation.parse(bad)


def test_every_scenario_expectation_parses():
    for sc in scenarios.SCENARIOS.values():
        if sc.expect:
            Expectation.parse(sc.expect)


def test_format_table_columns():
    r = scenarios.ScenarioResult("T-004", "n", "pass", expect="fp<=30", detail="fp=0", score=0.0, fp=0.0, beh=0.0)
    txt = scenarios.format_table([r])
    assert "T-004 n" in txt and "PASS" in txt

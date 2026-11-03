"""Scenario runner against a local detect-bench server (loopback)."""

from __future__ import annotations

import pytest

from lightbrowser import Persona, scenarios
from lightbrowser._common import leaky_playwright
from lightbrowser.bench import Bench

pytestmark = pytest.mark.integration

LEAKY = leaky_playwright()
LEAK_REASON = (
    "Playwright < 1.63 defines __pwInitScripts/__playwright_builtins__ in the main world, "
    "so the bench's automation_keys rule fires regardless of the browser"
)


@pytest.fixture(scope="module")
def reference_persona():
    # The Windows persona: the macOS one trips beh_key_dwell on the 6-key search
    # text with its own seed (docs/ROADMAP.md, "M5 status: human input").
    path = scenarios.default_persona_path()
    if path is None:
        pytest.skip("no reference personas found")
    return Persona.load(path)


def _opts(bench_url, binary, persona):
    return scenarios.Options(bench=Bench(bench_url), persona=persona, binary=binary, wait_s=3.5, seed=1)


def test_scenario_t005_consistency_violation(bench_url, binary, reference_persona):
    res = scenarios.run_scenario(_opts(bench_url, binary, reference_persona), scenarios.SCENARIOS["T-005"])
    assert res.status == "pass", res.detail


@pytest.mark.skipif(LEAKY, reason=LEAK_REASON)
@pytest.mark.parametrize("sid", ["T-002", "T-006", "T-007"])
def test_scenario_naive_and_idle(bench_url, binary, reference_persona, sid):
    res = scenarios.run_scenario(_opts(bench_url, binary, reference_persona), scenarios.SCENARIOS[sid])
    assert res.status == "pass", f"{res.detail} {res.rules}"


@pytest.mark.slow
@pytest.mark.skipif(LEAKY, reason=LEAK_REASON)
def test_scenario_t004_human_input_scores_as_human(bench_url, binary, reference_persona):
    res = scenarios.run_scenario(_opts(bench_url, binary, reference_persona), scenarios.SCENARIOS["T-004"])
    assert res.status == "pass", f"{res.detail} {res.rules}"
    assert res.fp <= 30 and res.beh <= 30

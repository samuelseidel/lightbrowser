# ADR 0007: Our own local detection bench is "our detection engine"

Status: Accepted (2026-11-03)

## Context

The M4 and M5 done-criteria in `ROADMAP.md` say "our detection engine
scores the Chrome personas as consistent" and "`page.human_click` produces
realistic event streams in the detection engine". Both CLAUDE.md files
forbid pointing the browser at third-party sites or public detection
services. We need a concrete, repeatable target that we own and that runs
anywhere a developer or CI job runs.

`tests/detect-bench` already exists: a Python stdlib server with test pages
(`/shop`, `/news`, `/login`, `/search`), a collector script with about 50
client probes plus an input-event trace, a rule catalogue with scoring,
and a Playwright scenario runner. Stock headless Chromium scores 100 (bot).

## Decision

- `tests/detect-bench` is "our detection engine" for the M4 and M5
  done-criteria.
- It runs on loopback only. The server binds `127.0.0.1` or `::1`, and
  `run.js` refuses any base, CDP or replay URL that is not loopback.
- Scenarios:
  - T-001: stock headless Chromium; must score as a bot (`>=90`).
  - T-003: scripted, slowed-down Chromium; must score as a bot
    (`>=70&fp>=70`).
  - T-010: any browser reached over CDP (`--cdp <ws>`); this is how
    LightBrowser is scored.
  - T-020: replay of a human-like input trace.
- M4 target: `node run.js --scenario T-010 --cdp <ws> --expect 'fp<=30'`
  (non-behaviour rules only).
- M5 target: the same scenario with `--expect 'beh<=30'` (behaviour rules
  only), with input from `LightBrowser.human*`.
- Until `lb-page` implements `lb_cdp::PageHost`, the baseline is taken
  with `lightbrowser fetch http://127.0.0.1:<port>/shop` and read from
  `GET /api/score/<sid>`. The baseline is recorded in `ROADMAP.md`.

## Alternatives considered

- **The production detection engine.** It is the real target, but it is
  not available offline or in CI, and its rules change on their own
  schedule. Results there are still welcome as an extra check.
- **Public fingerprinting and bot-test sites.** Forbidden by policy.
- **Unit tests per probe only.** They miss how signals combine into a score.

## Consequences

- Done-criteria for M4 and M5 are measurable on any machine, offline.
- The bench is only as good as its rules. Passing it does not prove the
  production engine agrees; new rules go into `scoring.py` with tests.
- Rule changes move the target, so a change to `scoring.py` should note
  the effect on the LightBrowser baseline.

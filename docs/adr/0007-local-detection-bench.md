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
  (non-behaviour rules only). For `--cdp`, `run.js` creates the context
  with `viewport: null` (ADR 0009), so the browser keeps its own window
  size. Met for the Chrome reference personas on 2026-11-03 (fp 0; see
  `ROADMAP.md`, "M4 status: detect-bench target and determinism").
- M5 target: the same scenario with `--expect 'beh<=30'` (behaviour rules
  only), with input from `LightBrowser.human*`.
- The M2 baseline was taken with `lightbrowser fetch
  http://127.0.0.1:<port>/shop` and read from `GET /api/score/<sid>`,
  before `lb-page` implemented `lb_cdp::PageHost`. Since M3, T-010 runs
  against `lightbrowser serve` over CDP. Baselines are recorded in
  `ROADMAP.md`.

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
- A rule may only be corrected when stock Chromium on loopback is misjudged
  the same way, with the stock evidence recorded. Example (2026-11-03):
  Chrome 153 sends `Accept-Language` right after `sec-ch-ua-platform`, not
  after `Accept-Encoding` as `EXPECTED` had it; stock `--headless=new`
  fired `header_order` as LightBrowser did. T-001/T-003 stay at 100.
- The scripted flow is part of the target too. On 2026-11-03 T-010 (and
  T-001) began typing a three-word query and a login form instead of one
  six-letter word, for both input modes. With six keys the per-page key
  spread rules fire by chance on human-like typing, about 26 % of the time.
  No rule changed. Results are in `ROADMAP.md`, "M5 status: human input".

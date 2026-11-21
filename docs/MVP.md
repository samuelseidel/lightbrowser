# MVP

The Chrome build is acceptable when one Playwright crawl, attached with
`connectOverCDP`, passes our own detection engine with **no scored flags**.

A scored flag is a rule in the session's `fired` list (weight above 0).
Weight-0 rules are reported as `info_fired` and do not fail this check.
Firefox (M4b) is after the MVP. The firefox build must still compile.

## The crawl

`tests/detect-bench/mvp.sh` is the single command. It only talks to loopback:

1. `tests/detect-bench/server.py` on `127.0.0.1` (the in-repo engine: catalogue,
   fp/beh probes, T-010).
2. `lightbrowser serve` (Chrome family) with
   `profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json` and
   `--egress loopback`.
3. Playwright `chromium.connectOverCDP`, then T-010 with `--input human`
   (`LightBrowser.humanScroll` / `humanClick` / `humanType` on the page's CDP
   session) and `--expect 'fp<=0&beh<=0'`.

```bash
cargo build -p lightbrowser
npm install --prefix tests/playwright   # once; Playwright 1.65, no browser download required
tests/detect-bench/mvp.sh
```

Playwright's own `click` / `fill` path is the scripted baseline. It is expected
to raise behaviour rules. The MVP crawl is the human-input path, because that
is the one the engine is supposed to accept.

## What is in and what is not

In, because the crawl or a green build needs it:

- the consolidated Chrome tree on `integrate/mvp` (network persona, V8, DOM,
  page, CDP, human input, detect-bench including the borrow-line rules)
- `mvp.sh` staying green

Out, until a scored flag or a broken build says otherwise:

- Firefox / SpiderMonkey (M4b)
- Lightpanda budget comparisons (informational; ADR 0041 on detection acceptance)
- TCP shim on `serve` (T10), Windows/macOS recaptures, and the other open
  items in `docs/ROADMAP.md`
- `wip/mac/claude/gaps-wave1-snapshot` and `public-bench-*` (not in this
  branch). Bring them in only if a scored flag needs them.

## Result on this tree

First run of `mvp.sh` after the consolidation merge, debug binary, seed 1,
default catalogue: **score 0, fp 0, beh 0, fired empty.** No scored flag to
fix. `info_fired` is whatever the engine records at weight 0; it does not
fail the command.

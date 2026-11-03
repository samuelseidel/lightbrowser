# WPT subset

A small vendored subset of [web-platform-tests](https://github.com/web-platform-tests/wpt)
(`dom/nodes`, `dom/events`, `html/dom`, `url`, `encoding`) plus
`resources/testharness.js`, taken from WPT `master` on 2026-11-03.
WPT is BSD-3-Clause licensed (`LICENSE.md`).

- `MANIFEST.txt` lists the tests that run. `.any.js` tests get a generated
  `.any.html` wrapper (window scope only).
- `resources/testharnessreport.js` is ours: it prints one
  `WPT_RESULT {json}` console line with every subtest result.
- Helper files (`*.js` without `.any`, `common/`, `dom/common.js`) are
  loaded by the tests.

Run (loopback only, a static server on 127.0.0.1:0):

```bash
cargo test -p lb-page --test wpt -- --nocapture
```

The runner prints per-file results and the totals, and fails if the
number of passing subtests drops below the baseline in
`crates/lb-page/tests/wpt.rs`. Tests that need iframes, XML documents,
shadow DOM or layout-dependent features fail today; that is expected and
tracked by the pass count, not hidden.

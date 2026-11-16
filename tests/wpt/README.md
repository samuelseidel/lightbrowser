# WPT subset

A small vendored subset of [web-platform-tests](https://github.com/web-platform-tests/wpt)
(`dom/nodes`, `dom/events`, `html/dom`, `url`, `encoding`) plus
`resources/testharness.js`, taken from WPT `master` on 2026-11-03, and
`shadow-dom` / `custom-elements` testharness files without iframes,
testdriver, crash or tentative tests (with their `resources/`,
`common/gc.js`, `html/resources/common.js`), taken from WPT `master`
095dcead on 2026-11-04, and the `IndexedDB` `.any.js` testharness files
(without `idlharness`, `storage-buckets` and `.https` ones; with their
`resources/` helpers and `common/subset-tests.js`) from the same commit.
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

The runner prints per-file results, totals per group, and the files with no
subtests or a harness problem. `EXPECTATIONS.txt` has one line per
`MANIFEST.txt` entry (passes, total, status) and the run fails when a file
does worse than its line, or when the two files name different files; there
is no total floor in `crates/lb-page/tests/wpt.rs` any more. After a fix, or
when a test is added to the manifest:

```bash
LB_WPT_BLESS=1 cargo test --locked -p lb-page --test wpt -- --nocapture
```

then review and commit the diff of `EXPECTATIONS.txt`. Details are in
`tests/README.md` ("WPT subset"). Tests that need iframes, XML documents or
layout-dependent features fail today; that is expected and tracked per file,
not hidden.

# tests

Unit tests stay inside each crate. This file lists the cross-crate suites.
Run everything with `cargo test --workspace` plus `node --test
tests/playwright/` (build `lightbrowser` first).

## CI (`.github/workflows/ci.yml`)

Every push and pull request runs the jobs below on ubuntu-latest; every cargo
command uses `--locked`. Anything longer than a command or two is a script in
`tools/ci/`, so the same check runs locally from the repository root.

| Job | Runs | Locally |
|---|---|---|
| `rust` | fmt, clippy (`--workspace --all-targets -D warnings`), `cargo test --workspace` (WPT subset and page suites included), the bindings table, the personas | `cargo fmt --all --check`; `cargo clippy --locked --workspace --all-targets -- -D warnings`; `cargo test --locked --workspace`; `cargo run --locked -p lb-webidl-gen -- --family chrome --check`; `tools/ci/validate_personas.sh` |
| `playwright` | the debug binary, the pinned Playwright, the suite below | `cargo build --locked -p lightbrowser`; `npm install --prefix tests/playwright`; `node --test tests/playwright/` |
| `python-tools` | detect-bench and bench unit tests, the Python SDK suite (needs the debug binary, `pytest` and Playwright >= 1.63) | `tools/ci/python_tools.sh` |
| `firefox` | clippy and unit tests of `lightbrowser`, `lb-js`, `lb-webapi`, `lb-page` with `--no-default-features --features firefox` | the two firefox commands in `CLAUDE.md` "Commands", plus `--locked` |
| `release` | `cargo build --release --locked -p lightbrowser` and the size budget | `tools/ci/check_binary_size.sh` |
| `macos` | build, then the tests of `lb-net`, `lb-css`, `lb-dom`; **manual runs only** (`workflow_dispatch`) | `cargo test --locked -p lb-net -p lb-css -p lb-dom` |

The scripts:

- `tools/ci/validate_personas.sh [BIN]`: the 8 files in `profiles/reference/chrome/`
  must validate (`lightbrowser validate` exits 0), and each file in
  `profiles/generated/adversarial/` must be rejected (exit 1 with at least one
  `[VIOLATION]` line, so an unreadable file does not count). Which rule fires
  is not checked: one adversarial file breaks several. The binary is the
  argument, `$LIGHTBROWSER_BIN` or `target/debug/lightbrowser`.
- `tools/ci/python_tools.sh`: `python3 -m unittest discover -s tests/detect-bench`,
  `python3 -m unittest discover -s bench` and `python3 -m pytest -ra tests` in
  `sdk/python`. Every suite runs even if one fails. The SDK integration tests
  skip when there is no binary, which would hide a broken build, so the script
  fails without one (`SDK_UNIT_ONLY=1` runs only the unit tests). `PYTHON=...`
  picks an interpreter that has `pytest` and Playwright.
- `tools/ci/check_binary_size.sh [BIN]`: the release binary must be under the
  budget. The budget is **< 50 MB, decimal** (50,000,000 bytes, not 50 MiB):
  ADR 0013 revision 4, and `BUDGET_BINARY_MB = 50.0` with `st_size / 1e6` in
  `bench/run.py`, from which the script reads the number. The release job builds
  `-p lightbrowser` because that is the build the budget is measured on (a
  whole-workspace build unifies more features into the binary).

CI sets `CARGO_INCREMENTAL=0` and `CARGO_PROFILE_DEV_DEBUG=line-tables-only`
(smaller test binaries on a runner's disk). The `macos` job starts from the
Actions tab ("Run workflow") only. The `playwright` job also runs the raw CDP
replay (`tests/cdp-raw/replay.mjs compare`, 350 steps against Chrome's recorded
answers in `tests/cdp-raw/chrome.json`; no Chrome needed). `tests/cdp-raw/behavior.mjs`
is not in CI: it reports differences from Chrome on this tree (see its README).

## Reference Chrome for the recorder scripts

The scripts that record Chrome's answers or compare against a local Chrome read
the browser binary from `$LB_CHROME`: `crates/lb-gl/tests/chrome/*.mjs`,
`crates/lb-page/tests/*/chrome.mjs` (and `screenshots/chrome_shots.mjs`,
`perf/fulfill.mjs`, `console/record.sh`), `crates/lb-net/tests/proxy/*chrome.mjs`,
`tests/cdp-raw/{replay,behavior}.mjs`, `tests/playwright/workers/record*.mjs`,
`tests/header-capture/drive.js`, `tools/dump_worker_surface.js`,
`tools/header_capture/accept_ch.js`, `tools/os_truth/capture.js` and `bench/run.py`.
The old variables still work as the next choice (`$CHROME`; `$CHROME_BIN` for the
two cdp-raw scripts), then each script's old default (`/opt/pw-browsers/chromium`,
or Playwright's cache under `~/.cache/ms-playwright` for cdp-raw). A command-line
option (`--bin`, `--chromium`, the port arguments of `drive.js`) wins over all of
them. `tools/surface_diff/run.py` takes Chrome only from `--chromium` (the Chrome
dump is optional there). Use the Chrome for Testing build a fixture was recorded
with: 153.0.8010.12 for `chrome.json`, `behavior.chrome.json` and most of the others.

## Network fingerprint test (M1, in place)

`crates/lb-net/tests/fingerprint.rs` runs `lb_net::fetch` against
`lb-fptest`'s capture server on 127.0.0.1 and compares the wire with
`profiles/tls/`:

```bash
cargo test -p lb-net --test fingerprint -- --nocapture   # prints measured JA3/JA4/H2
```

- **Chrome** (`chrome_153_linux`): everything must match exactly:
  - JA3 (over the canonical order), JA4 and JA4_r;
  - ciphers, extension set, groups and sigalgs (GREASE aside);
  - ALPN, ALPS and cert compression;
  - the trust_anchors and server_padding payloads;
  - H2 SETTINGS, WINDOW_UPDATE, pseudo-header order, HEADERS priority, frame
    order and the Akamai text;
  - per-connection extension permutation.
  - `chrome_fingerprint_same_for_every_reference_persona`: every persona in
    `profiles/reference/chrome` (Windows, macOS, Linux) gives the same
    exact match (M7: TLS and H2 do not depend on the OS).
- **Firefox** (`firefox_128_win10`):
  - Must match: ciphers, sigalgs, ALPN, cert compression, groups (minus
    ffdhe) and all H2 values.
  - Pinned and printed as expected differences: the BoringSSL gaps listed in
    ADR 0003 and the profile's own `ja3_string` vs `extensions_order`
    mismatch.

## TCP profiles vs public data (in place, no root)

`crates/lb-net/tests/public_reference.rs` checks that every trait a
`profiles/tcp/` profile asserts (layout, TTL, window, scale, ECN, IP ID) is seen in
at least 5% of real sessions of that OS in `profiles/reference/tcp_public.json`.
Refresh the reference with `python3 scripts/tcp_public_reference.py` (it downloads
the public file to memory; nothing is saved). ADR 0033.

```bash
cargo test -p lb-net --test public_reference -- --nocapture   # prints each share
```

## TCP SYN fingerprint test (M1b, in place)

`crates/lb-net/tests/tcp_fingerprint.rs` (ADR 0031), loopback only. Needs root
for raw capture, the netfilter queue and iptables; without them each test
prints `LB-SKIP <test>: <reason>` and returns, which counts as a pass (see
"Skipped tests"; `LB_REQUIRE_NO_SKIPS=1` makes it fail).

```bash
cargo test -p lb-net --test tcp_fingerprint -- --nocapture --test-threads=1
```

For each `profiles/tcp/` profile it fetches from a local HTTP server with a
3 MiB body, captures the SYN on `lo` and asserts TTL, DF, window, MSS, scale,
option bytes and order, timestamps, JA4T and (for rewritten packets) both
checksums. It then checks the IP ID of every packet over two connections (macOS
all 0, Windows one +1 counter, Linux per-connection +1), and, with the server
ignoring SYNs (`iptables INPUT DROP`), that the SYNs arrive on the profile's
schedule (macOS 0/1/2/3 s, Windows 0/1/3/7 s) with the right options and that
no kernel retransmission leaks. The capture reports kernel packet drops, so a
missed packet cannot pass as a wrong ID. Linux runs with no shim; Windows and macOS run with the shim
in-process. Also checked: no profile shows the host stack, a UA/OS mismatch is
refused, a stopped shim drops SYNs, and a shim profile is refused when the rule
is missing.

### Verifying a Windows or macOS profile on a real device

The Windows and macOS profiles are from public signatures. Check them on one of
our own machines connecting to our own test host:

```bash
# on the Linux test host (root); the device connects to HOST:18446
./target/debug/lb-fptest tcp-capture --listen HOST_IP:18446 --iface eth0 \
    --allow-non-loopback --count 3
# on the device:  curl http://HOST_IP:18446/   (or any TCP connect), three times
```

Compare `ja4t`, `ttl`, `df`, `option_bytes` with `profiles/tcp/<os>.json`. If
they differ, update the profile (and `expected.ja4t`), set `provenance.status`
to `captured`, and run the test above.

For the traits after the SYN, add `--flow` and let the device connect and send a
request (`curl http://HOST_IP:18446/`). Run it once more with this host ignoring
SYNs for about 20 s to see the retransmissions:

```bash
iptables -I INPUT -p tcp --syn --dport 18446 -j DROP    # then curl from the device
iptables -D INPUT -p tcp --syn --dport 18446 -j DROP
```

The last JSON line summarises the stack: `syn_times_ms` (retransmit schedule),
`syn_option_kinds` (layout per attempt), `ip_id` (`zero`, `sequential`,
`random`), `ts_hz` and `window_fields`. Put them into `ip.id`, `syn_retransmit`
and the notes.
## Skipped tests (LB-SKIP)

A skipped Rust test counts as passed, so a test that cannot run here must say
so. In `lb-net` (the only crate with such tests) it calls `lb_skip!("reason")`
from `crates/lb-net/tests/common/mod.rs` and returns:

- By default one line, `LB-SKIP <binary>::<test>: <reason>`, goes to the real
  stderr. libtest does not capture that, so a plain `cargo test` log has it
  (no `--nocapture` needed); the test still passes.
- With `LB_REQUIRE_NO_SKIPS=1` the same call panics with that line, so the
  test fails. Set it on a machine that is meant to have the environment (a
  root CI job); a box without it should leave it unset.

`tools/ci/skip_summary.sh` reads a log (files or stdin) and prints the count
per reason, with the tests under each; it prints "no skipped tests" for a clean
log and always exits 0:

```bash
cargo test --locked -p lb-net 2>&1 | tee lb-net.log
tools/ci/skip_summary.sh lb-net.log
LB_REQUIRE_NO_SKIPS=1 cargo test --locked -p lb-net --test tcp_fingerprint   # as root: any skip fails
```

| Test binary | Skips when |
|---|---|
| `tcp_fingerprint` (6 of 7 tests; `syn_matches_each_profile` also skips the shim profiles alone) | it cannot capture on `lo` (`CAP_NET_RAW`), has no `iptables`, or cannot bind the netfilter queue or install the rule (`CAP_NET_ADMIN`) |
| `egress` (11 of 20 tests) | no non-loopback address on a default route (10 tests), no IPv6 loopback, or the default-route address is IPv6 |

On rtx-dev as a non-root user (2026-11-16): 6 of the 7 `tcp_fingerprint` tests
skip (5 with "cannot capture on lo: Operation not permitted (os error 1)",
`shim_profile_needs_the_rule` with "needs root and iptables"), and all 20
`egress` tests run. Tests that need `python3` (`proxy`, `egress`, the lb-page
suites that start `serve.py`) fail when it is missing instead of skipping.

## WPT subset (M2, in place)

`tests/wpt/` holds testharness.js and 450 upstream WPT files (dom, url,
encoding, html, shadow-dom, custom-elements, IndexedDB); see
`tests/wpt/README.md`. `crates/lb-page/tests/wpt.rs` serves them on loopback
and checks every file against `tests/wpt/EXPECTATIONS.txt` (11742/12472
subtests pass today; about 2 minutes in the debug profile).

```bash
cargo test -p lb-page --test wpt -- --nocapture   # WPT_VERBOSE=1, WPT_FILTER=x, WPT_TIMEOUT_SCALE=2
```

`EXPECTATIONS.txt` has one line for every `MANIFEST.txt` entry, in manifest
order: `<path> <passes> <total> <status>`, then an optional `timeout=<seconds>`
and a `# why`. The status is `ok`, `harness-error`, `harness-timeout` or
`no-result` (the page printed nothing in the file's timeout). The runner
writes the file; nobody types the numbers:

```bash
LB_WPT_BLESS=1 cargo test --locked -p lb-page --test wpt -- --nocapture   # whole manifest, not with WPT_FILTER
```

A run **fails** when

- a file passes fewer subtests than its line says;
- a file's status is worse: `ok`, then `harness-error` and `harness-timeout`
  (the same level), then `no-result`;
- a file that had subtests has none (0 of 5 becoming 0 of 0 would raise the
  ratio without a fix);
- `MANIFEST.txt` and `EXPECTATIONS.txt` name different files, a file is
  listed twice, or a manifest file is not on disk (checked before anything
  runs, and by the fast test `expectations_file_matches_the_manifest`);
- a page's thread is still busy 180 s after the file's timeout (a script
  stuck in a loop; the polls queue behind it, so without this the run would
  hang).

Better results (more passes, a better status, another total) **pass** and print
the files with the bless command. After a fix: bless, read the diff of
`EXPECTATIONS.txt`, commit it with the fix. Blessing a worse result is
possible and prints `blessed a regression`, so it shows in review. The total
floor is the sum of the expected passes; `wpt.rs` has no constant. The run
prints the totals, a table per group (first path component), the files with
no subtests (0/0) and the files with a harness error, a harness timeout or no
result, each with its reason, so the known problems are listed in every log
and in `EXPECTATIONS.txt` (12 files at 0/0 and 23 not `ok` today).

Files that print no result, measured on rtx-dev (2026-11-15):

- Four IndexedDB files (`event-dispatch-active-flag`, `idb-explicit-commit`,
  `transaction-deactivation-timing`, `upgrade-transaction-deactivation-timing`)
  print nothing in 60 s: the page thread spins at 100% CPU and still answers
  `evaluate`. The runner's context has a virtual clock. The page loop
  (`crates/lb-page/src/page.rs`) runs a due timer ahead of queued tasks only
  on a real clock (the 50-task streak and `timer_first`) and moves the virtual
  clock only when no task is queued, so a page that always has a task queued
  never runs a timer, not even testharness' own 10 s one. With a real clock the
  same pages print in 25 to 45 ms: 4/4, 11/12 (with a harness error), 4/5 and
  3/3. Which task keeps them busy was not traced (`idb-explicit-commit` has no
  `setTimeout` of its own). They get a 5 s timeout instead of 30 s each.
  Running them on a real clock would add 22 passing subtests; that is a change
  of the runner's context and is not done.
- `custom-elements/form-associated/ElementInternals-target-element-is-held-strongly.html`
  is not a hang. One script task builds 1e5 elements and takes about 65 s in
  the debug profile, during which every poll waits behind it; the old runner
  gave up after 30 s and recorded no result, but the result (1 of 1) follows
  the task. Its line has `timeout=300`.

`WPT_TIMEOUT_SCALE=2` doubles every timeout on a slow machine.

## Web API coverage (M2, in place)

`cargo test -p lb-webapi --test coverage` asks the page layer's registry
(`rt::Runtime::new`, plus the defaults `Bindings::define` adds) about every
member of the generated Chrome table, as the installer does, and counts what
is bound to host code and what is a stub (a getter returning a default, an
operation throwing `NotSupportedError`, `Illegal constructor`). `tests/install.rs`
only checks shapes, which a stub has too. The line goes to the real stderr, so
every `cargo test` log has it (`docs/PLAN.md`: track coverage as a number in CI):

```text
web api coverage: 1143/2458 operations, 2269/4504 attributes, 394/893 interfaces
```

Counted per (interface, member), so a mixin member counts once per interface
that includes it:

- an operation is real when the registry has its method (static: static
  method), or for `toString` of a `stringifier attribute` the attribute's
  getter; constants and the 28 operations that are `Array.prototype` functions
  are not counted;
- an attribute is real when the registry has its getter (static: static
  getter);
- an interface is real when any of its attribute getters, operations,
  constructor, legacy factory or indexed/named getter is; the 45 interfaces
  with nothing to implement (callback interfaces with constants, opaque
  handles such as `WebGLBuffer`) are not in the 893.

The test fails when a real count falls below its floor in `coverage.rs` (today's
numbers); more passes, and the floors go up with the change.
`LB_COVERAGE_DUMP=file` writes one line per interface (operations, attributes,
setters, constructors, collections) to see the gaps. Also printed: 1309 of 1933
writable attributes have a real setter, 160 of 379 constructors and factories
are real, 188 interfaces are complete, and 317 of the 394 have a real member
beyond event handlers (the rest are real through generic `on*` registrations
only); no namespace has a real member (`CSS.supports` and `CSS.escape` are
stubs).

This differs from a count of interface names in the source (337 of 938 in the
2026-11-14 audit; a word search of `lb-webapi/src` without `generated/` gives
349): 85 of the 394 real interfaces have no literal name in the source (75 are
real only through generic `on*` handler registrations, 10 through code that does
not name them: `LargestContentfulPaint`, `LayoutShift`, the `Performance*Timing`
family), and 40 names in the source have no real member (22 have nothing to
implement, 18 are stubs such as `CSS`, `Range` and `SVGRectElement`). The count
here is of members the installer binds, so it falls when an implementation is
removed.

## Playwright CDP suite (M3, in place)

`tests/playwright/` runs stock Playwright `connectOverCDP` against
`lightbrowser serve` (the real V8 page host) with `viewport: null` in every
context (ADR 0009). 58 tests (the 38 of the macOS port plus the CDP coverage, performance timeline and web-platform additions; among them one for shadow DOM, one for proxies per context with `Fetch.authRequired`, two for workers as CDP targets, five in `content.spec.mjs` for `setContent` and JavaScript dialogs, ADR 0036, eight for uploads, permissions, geolocation, drag and drop and PDF, ADR 0037, one for a plain `newContext()`), all passing (debug and release binaries);
`harness.mjs` starts `serve` and a local site. `tests/playwright/package.json`
pins `playwright` to 1.65.0, the version the suite is checked with; install it
with `npm install --prefix tests/playwright` (a Chromium download is not needed:
the browser is `lightbrowser`). The harness looks for the package in a local
`node_modules` (next to the harness or above it), then in `$NODE_PATH`, then in
`npm root -g`, and says where it looked when it finds none.

| Spec | Covers |
|---|---|
| `smoke.spec.mjs` | goto, evaluate, title, content, start-up wall time |
| `content.spec.mjs` | `page.setContent` (document.open/write/close and the end of parsing, as in Chrome), `document.write`, `alert`/`confirm`/`prompt` through `page.on('dialog')`, auto-dismiss, closing a page with an open dialog (ADR 0036) |
| `files.spec.mjs` | `File`, `FileList`, `DataTransfer`, `setInputFiles` with buffers and with local paths (`DOM.setFileInputFiles`), the multipart body a form posts |
| `permissions.spec.mjs` | `grantPermissions` / `clearPermissions` states (as recorded in Chrome), geolocation from context options, `setGeolocation`, `watchPosition`, the timeout |
| `dnd.spec.mjs` | `dragAndDrop`: accepted and rejected drops and a link, event logs equal to Chrome's; a drag on plain mouse input |
| `pdf.spec.mjs` | `page.pdf`: page count, paper sizes, landscape, page ranges, the page unchanged afterwards |
| `actions.spec.mjs` | click/dblclick event order and coordinates, fill/type/press, check/radio/selectOption, labels, getByRole/Text/Label, locator reads, waitForSelector/Function, link navigation, form submit |
| `network.spec.mjs` | `page.route` fulfill/abort (fetch and navigation), cookies, two pages in one context, two contexts |
| `stealth.spec.mjs` | no added or suspicious main-world globals, `navigator.webdriver` false, trusted events |
| `cdp.spec.mjs` | raw CDP: DOM, Accessibility, Emulation, console, page errors |
| `frames.spec.mjs` | `frameLocator`, `page.frame`, frame attach/detach events, `LightBrowser.fingerprintReport` leaves no trace in the main world |
| `workers.spec.mjs` | workers as CDP targets (ADR 0029): `page.workers()`, `worker.evaluate`, worker console/errors/requests, `close`, nested and frame workers, shared workers, `serviceworker`, `fromServiceWorker`, all compared with Chrome's results in `workers/chrome.json` (`workers/record.mjs` records either browser; `record_idle.mjs` the idle service worker stop); nothing added to the worker's scope |
| `screenshot.spec.mjs` | `page.screenshot()`: viewport, element (also scrolled into view), full page, jpeg quality and clip, `omitBackground`, raw CDP clip scale and `captureBeyondViewport`, un-noised canvas pixels, the 0-width error (ADR 0014) |

```bash
npm install --prefix tests/playwright      # once; or NODE_PATH=/dir/with/node_modules
cargo build -p lightbrowser && node --test tests/playwright/   # LIGHTBROWSER_BIN overrides
```

## Page-level Rust tests (lb-page, M2 to M4)

`crates/lb-page/tests/` drive real V8 pages against loopback servers (chrome
build only). Run one with `cargo test -p lb-page --test <name>`.

| Test | Covers |
|---|---|
| `pages` | scripts and DOM after load, errors, `unload_events_beacon_and_favicon`; includes `frames/` (iframes, `postMessage`, cross-origin `SecurityError`) and `report/` (fingerprint-report gate) |
| `cdp_host` | `CdpHost` through `lb_cdp::PageHost` |
| `canvas` | canvas 2D API, image loading, same seed gives the same `toDataURL` hash, another seed a different one |
| `navigator` | plugins, UA-CH, permissions, battery, devices, voices, time zone, `window.chrome`, screen, media queries, RTC, for both reference personas |
| `fingerprint_surfaces` | WebGL strings and pixels, fonts, Web Audio sum (about 124.043), determinism per seed |
| `wpt` | the WPT subset above |
| `human` | `LightBrowser.human*` replay: trusted events in Chrome's order, monotonic timestamps, holds, the click point inside the target, scrolling into view, typed value, Shift, error texts; under virtual time the same persona gives the same stream and the clock advances |

Fingerprint report gate: `cargo test -p lb-page --test pages report` checks
every `profiles/reference/chrome` persona (on `about:blank` and on a loopback
page) for zero mismatches and zero unimplemented surfaces. The same check
from the command line:

```bash
./target/debug/lightbrowser fingerprint-report \
    --persona profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json   # exit 1 on mismatch
```

Other new suites: `cargo test -p lb-behavior` (66: determinism per seed,
Fitts scaling, bell-shaped speed, click scatter, keystroke statistics,
wheel and trackpad totals, CDP conversion), `cargo test -p lb-js --test
snapshot` and `cargo test -p lb-webapi --test snapshot` (snapshot and
built realms expose the identical surface, secure and insecure; V8 allows
one snapshot per process, so these are separate test binaries), `cargo test -p lb-layout -p lb-paint` (paths, compositing,
gradients, codecs, noise, display list and page painting), `cargo test -p lb-webapi` (`tests/install.rs`: interface
surface vs Chrome 153), `cargo test -p lb-net` (79 unit tests, including the
header profile in `header_profile.rs` and `client_hints.rs`, `--test
fingerprint` and `--test accept_ch`).

## Client hints (M7, WP-E)

- `cargo test -p lb-net --test accept_ch`: the header golden test replays
  `profiles/headers/captures/chrome_153_linux_accept_ch.jsonl` (84 of 88
  requests, 75 with high-entropy hints, both `Critical-CH` retries)
  through `HeaderProfile::build` and compares names, case, order and
  values. Four pipeline tests run `lb_net::fetch` against a loopback
  server: the `Critical-CH` retry, later navigations, same-origin frames
  and subresources, preflights and cross-origin frames (no hints),
  `Accept-CH` on a redirect, an empty `Accept-CH`, Firefox (no hints, no
  retry) and the values of every reference persona.
- `cargo test -p lb-page --test client_hints`: a real page with a
  same-origin and a cross-origin frame.
- Re-capture (loopback only, local Chromium): `node
  tools/header_capture/accept_ch.js --out <file>` (add `--mode headed`
  under `xvfb-run -a`), then `python3 tools/header_capture/derive_groups.py
  <file>` for `renderer_group.observed_hints`. See
  `tools/header_capture/README.md`.

## Detection bench (ADR 0007)

`tests/detect-bench` is our detection engine for M4/M5, loopback only. See
its README. M4 target (met for both reference Chrome personas, fp 0):

```bash
python3 tests/detect-bench/server.py --port 18766 --results-dir "$(mktemp -d)" &
./target/debug/lightbrowser serve --port 19222 \
    --persona profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json &
curl -s http://127.0.0.1:19222/json/version      # webSocketDebuggerUrl
node tests/detect-bench/run.js --base http://127.0.0.1:18766 --scenario T-010 \
    --cdp ws://127.0.0.1:19222/devtools/browser/<id> --expect 'fp<=30'
# stop both servers afterwards
python3 -m unittest -v   # in tests/detect-bench: scoring and server tests (170)
```

Since M7 the bench also has OS rules (`scoring.py`, "OS rules (M7)" in its
README). With them all eight Chrome reference personas score fp 0 and,
with `--input human`, beh 0 (checked again at the M7 close-out); stock
Chrome 153, headed and `--headless=new`, fires none of them; see "M7 status" in
`docs/ROADMAP.md`. Rendering per OS is checked without the bench by `cargo
test -p lb-layout --test os` (scrollbars for every reference persona; Linux
font widths, scrollbars and emoji against the capture in
`tests/fixtures/os/`); web APIs and input per OS by `cargo test -p lb-page
--test os_apis` (Web Share and Web Bluetooth only on Windows/macOS, media
answers, keyboard, WebGPU, `screenX/Y`, audio) and `--test human`
(`wheel_and_shortcuts_follow_the_os`), and the media tables by `cargo test
-p lb-webapi media` (every captured Linux answer). The OS overlay of
Windows/macOS-only members is `tests/fixtures/chrome_153_os_overlay.json`
(not captured; see its `_meta`). To serve a
persona that breaks the consistency rules (adversarial bench tests only),
use the hidden `lightbrowser serve --unchecked-persona FILE`; the M7
adversarial personas and the bench rule each one fires are listed in
`profiles/README.md`.

Use a fresh `serve` per run. M5 target, the same flow through
`LightBrowser.human*` (`--seed N` fixes the reading pauses):

```bash
node tests/detect-bench/run.js --base http://127.0.0.1:18766 --scenario T-010 \
    --cdp ws://127.0.0.1:19222/devtools/browser/<id> --input human --expect 'beh<=30'
```

Results (2026-11-03, flow with a three-word query and a login form):
both reference personas score beh 0 on their own seeds. Seeds 1-20 pass
20/20 for Windows and 19/20 for macOS (seed 10: `beh_mouse_linear`); see
"M5 status" in `docs/ROADMAP.md`. With the default `--input playwright`
both score beh 92.3, and stock Chromium (T-001, T-003) scores 100, so the
bench still catches naive automation.

## OS ground truth (M7)

`tools/os_truth` captures the OS-dependent surface of the local Chrome for
Testing 153 on loopback (headed under `xvfb-run` and `--headless=new`) into
`tests/fixtures/os/*.json`, each with a `provenance` block and this
machine's limits (no GPU, audio server or voices; container fonts; no window
manager; X11 only). Windows and macOS cannot be captured here. See
`tools/os_truth/README.md`.

## Python SDK suite (M5)

`sdk/python/tests` (pytest). Run it from `sdk/python`, because the package is
not installed:

```bash
cd sdk/python
python3 -m pytest tests -m "not integration"   # unit tests, no binary, about a second
python3 -m pytest tests                        # also launches target/debug/lightbrowser
python3 -m lightbrowser.scenarios --bench http://127.0.0.1:PORT   # scenarios T-001..T-007
```

`sdk/python/pyproject.toml` requires `playwright>=1.63` (CI installs `pytest`
and that Playwright, then runs `tools/ci/python_tools.sh`).

93 pass and 4 skip with the installed Python Playwright 1.52: the bench
scenarios T-002, T-004, T-006 and T-007 skip, because Playwright older than
1.63 defines `__pwInitScripts` in the page's main world. With 1.63, 96 pass
and 1 is skipped, and all seven scenarios pass (T-004 0 with fp 0 and beh 0).

## Crawl benchmark (M6)

`bench/` (see its README): `python3 bench/run.py` crawls a generated 50-page
corpus on loopback with LightBrowser (`fetch` and CDP), headless Chromium and,
if installed, Lightpanda, then checks the budgets and writes
`bench/results/`. `python3 -m unittest discover -s bench` runs its tooling
tests (10, no browser). `cargo run --release -p lb-webapi --example
realm_bench` prints the realm-creation timings the runner reads.

## Planned suites

- **Header golden test for the first capture** (M4): replay `profiles/headers/captures/chrome_153_linux.jsonl` the same way as the client-hints and form POST captures (`cargo test -p lb-net --test accept_ch`, `--test form_post_headers`). The old file's h2 `cookie` position is a decoder artifact (ADR 0011 amendment), so a replay must skip it there.
- **Fingerprint tests for Firefox** (M4b): same checks as the Chrome personas, against OUR detection engine only.
- **Firefox network fingerprint** (M4b): close the ADR 0003 gaps and re-capture Firefox 128 with `lb-fptest`.

## Re-capturing Chrome request headers (`profiles/headers`)

`profiles/headers/chrome_153.json` was built from a capture of Chrome for
Testing 153.0.8010.12 (`--headless=new`) against a local logging server on
loopback: HTTP/1.1 raw bytes, and HTTPS h2 decoded. The 69 requests are in
`profiles/headers/captures/chrome_153_linux.jsonl` (document, iframe, scripts,
style, image, font, fetch/XHR, preflight, favicon, redirect). The capture
server script is not in the repo (it was a throwaway Python script using
the `h2` package, which moves `cookie` last on h2, and an `openssl` cert);
`tools/header_capture/accept_ch.js` now covers most kinds with and without
client hints. A full re-capture for a new Chrome major is still partly
manual: record the same request kinds from a local Chromium, compare
with the existing JSONL, and update the slot lists (ADR 0011).

Form POST navigations (`chrome_153_linux_forms.jsonl`, 52 requests) were
captured with the scripts in `tests/header-capture/`, which are in the repo.
They bind 127.0.0.1 only and drive the local Chromium over raw CDP (not
Playwright: its contexts set an `Accept-Language` override that moves the
header):

```bash
d=$(mktemp -d)
openssl req -x509 -newkey rsa:2048 -nodes -keyout "$d/key.pem" -out "$d/cert.pem" \
    -days 2 -subj "/CN=localhost"
node tests/header-capture/server.js "$d/forms.jsonl" 18490 18491 "$d" &
node tests/header-capture/drive.js 18490 18491
kill %1
```

Each request is labelled with its scenario in `page` (the `?s=` of the page
that started it); `/favicon.ico` is not logged. Copy the output to
`profiles/headers/captures/` and run `cargo test -p lb-net --test
form_post_headers`.

## Re-capturing a Chrome TLS/H2 profile (`lb-fptest`)

`crates/lb-fptest` is test support only (never shipped). Its local server
reads the raw ClientHello, finishes TLS with an in-memory self-signed cert,
answers one tiny HTTP/2 or HTTP/1.1 response and records the H2 SETTINGS,
first WINDOW_UPDATE, pseudo-header order and HEADERS priority. It binds loopback only. Point
only a local browser or our own client at it.

1. Build and get the browser version:
   ```bash
   cargo build -p lb-fptest
   /opt/pw-browsers/chromium --version     # e.g. Google Chrome for Testing 153.0.8010.12
   ```
2. Start the capture (two connections, to see extension permutation):
   ```bash
   ./target/debug/lb-fptest capture --out profiles/tls/chrome_<MAJOR>_linux.json \
       --listen 127.0.0.1:18445 --count 2 --browser-version <FULL_VERSION>
   ```
3. In another shell, load the page twice, each time with a fresh profile dir
   so the second run does not resume the TLS session:
   ```bash
   for i in 1 2; do
     d=$(mktemp -d)
     timeout 12 /opt/pw-browsers/chromium --headless=new --no-sandbox \
       --ignore-certificate-errors --user-data-dir="$d" --no-proxy-server \
       --host-resolver-rules="MAP fptest.localhost 127.0.0.1" \
       --disable-background-networking --disable-component-update --disable-sync \
       --no-first-run --dump-dom https://fptest.localhost:18445/ >/dev/null
     rm -rf "$d"
   done
   ```
   Use a host name, not `127.0.0.1`: Chrome sends no SNI for IP literals.
   `--no-proxy-server` keeps Chrome off any `HTTPS_PROXY` in the environment.
4. Check the notes printed on stderr, then update `Persona::default_chrome()`
   (UA major, `sec-ch-ua`, `tls_profile_id`) if the major version changed.
   Point the Chrome test in `crates/lb-net/tests/fingerprint.rs` at the new
   id and run it.
5. If the test reports a TLS gap (`TlsConnector::gaps()`) or a missing
   extension, BoringSSL in `boring-sys` may be too old for that Chrome, or
   `lb-net/src/tls.rs` needs a new knob. Check
   `~/.cargo/registry/src/*/boring-sys-*/deps/boringssl/include/openssl/`.

What the tool does with the captures:

- Chromium opens one or two speculative sockets per load and drops them
  mid-handshake. Capture mode skips connections that fail or resume a
  session (`pre_shared_key`), and keeps only full handshakes that sent a
  request.
- If the extension orders differ between captures but the sets match, it sets
  `permute_extensions: true` and writes Chrome's canonical (un-permuted) order:
  `0,23,65281,10,11,35,16,5,13,18,51,45,43,27,ALPS,51764,4832,65037`, keeping only the
  extensions it saw. Extensions not in that list are appended at the end with
  a note. GREASE is not listed; `grease: true` covers it (BoringSSL puts
  GREASE first and last).
- `server_padding_request` and `trust_anchor_ids` (hex, without the outer
  length) are taken from the 4832 and 0xCA34 payloads, and
  `http2_priority.headers` from the first HEADERS frame. `lb-net` sends
  exactly these.
- `ja3`/`ja3_string` use the canonical order, so they are stable. Real Chrome
  has a new JA3 on every connection. `ja4` (hashed) and `ja4_r` are
  order-independent. `http2_akamai_fingerprint` is the Akamai H2 text.
- Unknown or GREASE H2 settings are dropped from `http2_settings` with a note,
  because `lb-net` rejects unknown setting names.

`lb-fptest serve --listen 127.0.0.1:PORT [--alpn http/1.1]` prints one JSON
line per connection (JA3, JA4, extension order, Akamai H2, and with
`--alpn http/1.1` the clear-text request headers). Use it for checking our own
client or for reading Chrome's `sec-ch-ua` and header order.

## Connection reuse vs local Chrome (ADR 0018)

`lb-fptest site` serves a page with subresources (plain HTTP/1.1, or TLS
with h2/http1.1 by ALPN), keeps connections open and prints, per
connection, the ClientHello (JA4, resumed or not), the ALPN and the
requests with their HTTP/2 stream and priority. Drive local Chrome at it
through Playwright (its default flags keep background networking off), go
to `about:blank` first and wait 3 s: a fresh Chrome flushes its socket
pools once at start-up ("Cert verifier changed" in a netlog), which
otherwise shows up as extra connections. By hand, pass
`--disable-background-networking --disable-component-update` as above.
The numbers Chrome 153 gave are the expectations in
`crates/lb-net/tests/pool.rs`.

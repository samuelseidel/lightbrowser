# tests

Unit tests stay inside each crate. This file lists the cross-crate suites.
Run everything with `cargo test --workspace` plus `node --test
tests/playwright/` (build `lightbrowser` first).

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

## WPT subset (M2, in place)

`tests/wpt/` holds testharness.js and 83 upstream WPT files (dom/nodes,
dom/events, html/dom, url, encoding); see `tests/wpt/README.md`.
`crates/lb-page/tests/wpt.rs` serves them on loopback and fails below 2520
passing subtests (2552/2788 today).

```bash
cargo test -p lb-page --test wpt -- --nocapture   # WPT_VERBOSE=1, WPT_FILTER=x
```

## Playwright CDP suite (M3, in place)

`tests/playwright/` runs stock Playwright `connectOverCDP` against
`lightbrowser serve` (the real V8 page host) with `viewport: null` in every
context (ADR 0009). 22 tests (one for shadow DOM, one for proxies per context with `Fetch.authRequired`), all passing (debug and release binaries);
`harness.mjs` starts `serve` and a local site.

| Spec | Covers |
|---|---|
| `smoke.spec.mjs` | goto, evaluate, title, content, start-up wall time |
| `actions.spec.mjs` | click/dblclick event order and coordinates, fill/type/press, check/radio/selectOption, labels, getByRole/Text/Label, locator reads, waitForSelector/Function, link navigation, form submit |
| `network.spec.mjs` | `page.route` fulfill/abort (fetch and navigation), cookies, two pages in one context, two contexts |
| `stealth.spec.mjs` | no added or suspicious main-world globals, `navigator.webdriver` false, trusted events |
| `cdp.spec.mjs` | raw CDP: DOM, Accessibility, Emulation, console, page errors |
| `frames.spec.mjs` | `frameLocator`, `page.frame`, frame attach/detach events, `LightBrowser.fingerprintReport` leaves no trace in the main world |
| `screenshot.spec.mjs` | `page.screenshot()`: viewport, element (also scrolled into view), full page, jpeg quality and clip, `omitBackground`, raw CDP clip scale and `captureBeyondViewport`, un-noised canvas pixels, the 0-width error (ADR 0014) |

```bash
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
python3 -m unittest -v   # in tests/detect-bench: scoring and server tests (33)
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

- **Header golden test for the first capture** (M4): replay `profiles/headers/captures/chrome_153_linux.jsonl` the same way as the client-hints capture (its h2 `cookie` position is a decoder artifact, ADR 0011).
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

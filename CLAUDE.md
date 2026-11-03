# LightBrowser: CLAUDE.md

## Overview
A from-scratch headless browser in Rust for internal bot-detection
validation. Same purpose as `/home/user/stealth-browser` (GeckoPhantom). It is
driven over CDP, so stock Playwright `connectOverCDP` works. Presents a Chrome
or Firefox persona.

`docs/ARCHITECTURE.md` and `docs/ROADMAP.md` are the source of truth. Read them
first and do not contradict them. Current state (Chrome build): M1 (network),
M2 (DOM + V8), M3 (layout + CDP), M4 (Chrome fingerprints), M5 (human input
and Python SDK) and most of M6 are done. `lightbrowser serve` runs real V8
pages through `lb_page::CdpHost`; stock Playwright `connectOverCDP` passes
20 tests (including screenshots); the WPT subset passes 2552/2788. On our
detection engine (`tests/detect-bench`, ADR 0007) T-010 scores fp 0 for both
reference Chrome personas. With `--input human` (`LightBrowser.human*`)
both reference personas score beh 0 on their own seeds (seeds 1-20: 20/20
Windows, 19/20 macOS), see "M5 status" in `docs/ROADMAP.md`. M6: V8 startup
snapshot (realm 2.5 ms), idle RSS 10.2 MB, `bench/` runner (every corpus
page faster than headless Chromium's median), release binary 39.63 MB
against < 40 MB (ADR 0013; ICU data in zstd chunks paged in on first
touch, no unwind tables, V8 code only off-by-default flags reach not
linked, root crate and bindings at `opt-level = "s"`). Lightpanda
comparison (user-approved 1.1.0 nightly, loopback only, telemetry off;
"M6 status: Lightpanda comparison"): wall time wins in `fetch` and CDP
mode, 4 of 6 checks pass; `fetch` CPU (2.5 vs 1.4 s) and CDP peak RSS
(42 vs 31 MB) still lose. Open: Firefox (M4b: no SpiderMonkey backend,
TLS gaps in ADR 0003), workers and service workers, shader execution in
WebGL, those two Lightpanda gaps, HTTP keep-alive, stylesheets blocking
scripts, and CSS background images in screenshots. See "Where we stand"
and the status sections in `docs/ROADMAP.md`.

## Internal use and legal
- Internal only. Never push binaries to public repos or registries.
- Point it only at our own detection engine and our own test servers, never
  at third-party sites or detection services. `bench/` runs locally.

## Crate map
| Crate | Role | State |
|---|---|---|
| `lb-persona` | Persona model, `BrowserFamily`, consistency rules, `sfc32` | v2 schema + loader, rules R01–R16, sfc32 done; optional `preferences`, media devices, battery |
| `lb-js` | `JsEngine` trait; V8 (chrome) or SpiderMonkey (firefox) | V8 backend done (startup snapshot made in memory, ADR 0013; ICU data in 128 KiB zstd chunks paged in on first touch, `v8_engine/icu_data.rs`; code cache for large scripts compiled again; V8 objects only off-by-default flags reach are stubbed out, `v8_engine/unused_v8.rs`, so `init_v8` refuses those flags; `RealmOps`, native `inspect` for CDP); SpiderMonkey M4b |
| `lb-net` | BoringSSL, own h1 writer, forked h2 (`vendor/h2`), cookies, decoding, `fetch`, per-destination headers (`profiles/headers`) | Done for Chrome; cookies shared across clones; Firefox headers generic; cache, proxies, connection reuse open (`lb-page` caps 6 connections per origin) |
| `lb-fptest` | Test support only, never shipped: ClientHello parser, JA3/JA4, Akamai H2, loopback capture server, profile builder | M1 done |
| `lb-dom` | Arena DOM, html5ever sink, selectors | Done; no shadow DOM, form owner, custom elements |
| `lb-webidl-gen` | WebIDL to Rust codegen (build time) | Own parser + Chrome generator done; table in `lb-webapi/src/generated/` |
| `lb-webapi` | Bindings installer, DOM/events/window/network APIs, input pipeline, canvas 2D, images, navigator surface, WebGL, audio, fonts, frames | Done (chrome), including the HTMLTable* APIs (`rt/table.rs`). WebGL runs no shaders; no workers, crypto or WebSocket |
| `lb-css` | Lazy cascade | Done (~75 longhands; no pseudo-elements, animations) |
| `lb-layout` | taffy layout on demand, font metrics, hit testing, AX tree | Done (approximate inline layout); `display_list` for screenshots |
| `lb-paint` | tiny-skia canvas 2D, text, PNG/JPEG/WebP codecs, seeded readback noise, page screenshots (ADR 0014) | Done (no filters or shaping; screenshots lack background images, radius, shadows, transforms) |
| `lb-page` | Event loop, navigation, frames, workers, CDP page host, fingerprint report | Event loop, navigation, iframes, `CdpHost`, `fingerprint_report`, human-input replay (`cdp/human.rs`), `Page.captureScreenshot` done; no workers or popups |
| `lb-cdp` | CDP server | Done; real host is `lb_page::CdpHost`, `MockHost` is for lb-cdp tests and the firefox build |
| `lb-behavior` | Human input models (Fitts, minimum jerk, log-normal keys, wheel/trackpad), rewritten from the vendored stealth-browser crate | Done; replayed by `LightBrowser.human*` |
| `sdk/python` | `lightbrowser` package: launcher, `Persona`, sync/async Playwright wrappers with `human_*` and `verify_fingerprint`, scenarios T-001..T-007 | Done; needs Python Playwright >= 1.63 |
| `bench/` | Crawl benchmark (generated 50-page corpus, loopback only) vs headless Chromium and Lightpanda; results in `bench/results/` | Runner done; Lightpanda compared (4 of 6 checks pass, see ROADMAP) |
| `bin/lightbrowser` | CLI; picks family by feature; `fetch`, `validate`, `serve`, `fingerprint-report` | All done for chrome; the firefox build's `serve` uses `MockHost` and `fingerprint-report` needs the JS engine |

## Commands
```bash
cargo build --workspace                       # chrome (default)
cargo build --release                         # release, chrome
cargo build --release --no-default-features --features firefox   # release, firefox
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
# Firefox family checks
cargo test -p lightbrowser -p lb-js -p lb-webapi -p lb-page --no-default-features --features firefox
cargo clippy -p lightbrowser -p lb-js -p lb-webapi -p lb-page --no-default-features --features firefox -- -D warnings
./target/release/lightbrowser --version       # prints the compiled family
./target/debug/lightbrowser validate profiles/reference/chrome/*.json   # persona rules
# Network fingerprint check vs profiles/tls (loopback only, prints JA3/JA4/H2)
cargo test -p lb-net --test fingerprint -- --nocapture
# Fetch from a local test server (self-signed cert)
./target/debug/lightbrowser fetch https://127.0.0.1:PORT/ --insecure --dump text
# WPT subset (loopback) and the Playwright CDP smoke test
cargo test -p lb-page --test wpt -- --nocapture
cargo build -p lightbrowser && node --test tests/playwright/   # 20 tests; LIGHTBROWSER_BIN overrides
# Live surface vs persona (JSON on stdout, exit 1 on any mismatch)
./target/debug/lightbrowser fingerprint-report --persona profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json
cargo test -p lb-page --test pages report     # same check as a CI gate, all reference Chrome personas
# Detection bench T-010 over CDP (loopback only; see tests/detect-bench/README.md)
python3 tests/detect-bench/server.py --port 18766 --results-dir "$(mktemp -d)" &
./target/debug/lightbrowser serve --port 19222 --persona profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json &
node tests/detect-bench/run.js --base http://127.0.0.1:18766 --scenario T-010 \
    --cdp ws://127.0.0.1:19222/devtools/browser/<id> --expect 'fp<=30'   # <id> from curl 127.0.0.1:19222/json/version
# Same flow through LightBrowser.human* (M5; use a fresh serve per run; both reference personas pass)
node tests/detect-bench/run.js --base http://127.0.0.1:18766 --scenario T-010 \
    --cdp ws://127.0.0.1:19222/devtools/browser/<id> --input human --expect 'beh<=30'
# stop both servers afterwards
python3 -m unittest discover -s tests/detect-bench   # scoring and server tests (18)
# Python SDK tests (run from sdk/python; scenarios need Playwright >= 1.63, else skipped)
(cd sdk/python && python3 -m pytest tests)
# Crawl benchmark (release binary, loopback only); tooling tests need no browser
python3 bench/run.py                          # 5 runs x 50 pages -> bench/results/
# with Lightpanda: put its binary's directory first on PATH (loopback only;
# run.py sets LIGHTPANDA_DISABLE_TELEMETRY=true; set it yourself for manual runs)
PATH=/path/to/lightpanda-dir:$PATH python3 bench/run.py
python3 -m unittest discover -s bench
# Regenerate the Chrome bindings table (--check: fail if stale)
cargo run -p lb-webidl-gen -- --family chrome
```
Both families output to `target/release/lightbrowser`; the last build wins.
Run the chrome and firefox checks before finishing any change that touches
`lb-js`, `lb-persona`, `lb-webapi`, `lb-page` or the binary. Do not run
`cargo build --all-features`: it enables both engines.

### Re-capturing a TLS profile
For a new Chrome version, capture from the local Chromium. Full steps are in
`tests/README.md`.
```bash
cargo build -p lb-fptest
./target/debug/lb-fptest capture --out profiles/tls/chrome_<MAJOR>_linux.json \
    --listen 127.0.0.1:18445 --count 2 --browser-version <FULL_VERSION>
# other shell, twice, fresh --user-data-dir each time:
/opt/pw-browsers/chromium --headless=new --no-sandbox --ignore-certificate-errors \
    --user-data-dir="$(mktemp -d)" --no-proxy-server \
    --host-resolver-rules="MAP fptest.localhost 127.0.0.1" \
    --dump-dom https://fptest.localhost:18445/
```
Then update `Persona::default_chrome()` (UA, `sec-ch-ua`, `tls_profile_id`)
and run the fingerprint test. Only capture on loopback with a local browser.

## Conventions
- **One engine per binary.** Exactly one of `chrome` / `firefox` is enabled.
  Engine-specific code stays behind `lb-js::JsEngine`. Everything else is shared
  and picks per-family data from `persona.browser.family`.
- **Persona via `Arc`, never globals.** Every fingerprint value (UA, screen,
  GPU, fonts, seeds, TLS/H2 profile) comes from the context's immutable
  `Arc<Persona>`. No statics, env vars or `thread_local` for fingerprint data.
- **No disk writes by default.** Cache, cookies and storage live in memory.
  Disk is used only when an explicit flag is passed (e.g. `--cache-dir`).
- **SFC32 must stay bit-identical** to `geckophantom_config.h` in
  stealth-browser (`geckophantom_config/include/`). Same seed, same sequence.
  Do not change `lb-persona::sfc32` without re-checking against the C output.
- **Nothing automation-related is visible to the page world** (`webdriver`,
  CDP leaks). CDP helper code runs in an isolated world.
- **Keep docs current.** When a decision changes, update
  `docs/ARCHITECTURE.md`, `docs/ROADMAP.md` and the matching ADR in
  `docs/adr/` in the same change. Add a new ADR for new decisions.
- Rust edition 2024, stable toolchain, `unsafe` only where an engine binding
  needs it, with a comment explaining why.
- Docs: concise, plain English.

## Reuse from stealth-browser
Persona schema (v1 loads as Firefox), `consistency/` rules, `behavior/` crate
and TLS profiles are vendored in later milestones. See the "Reuse" table in
`docs/ARCHITECTURE.md`. ShadowChannel is not used; control is CDP only.

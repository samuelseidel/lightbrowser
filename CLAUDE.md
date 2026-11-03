# LightBrowser: CLAUDE.md

## Overview
A from-scratch headless browser in Rust for internal bot-detection
validation. Same purpose as `/home/user/stealth-browser` (GeckoPhantom). It is
driven over CDP, so stock Playwright `connectOverCDP` works. Presents a Chrome
or Firefox persona.

`docs/ARCHITECTURE.md` and `docs/ROADMAP.md` are the source of truth. Read them
first and do not contradict them. Current state (Chrome build): M1 (network),
M2 (DOM + V8), M3 (layout + CDP), M4 (Chrome fingerprints), M5 (human input
and Python SDK), most of M6 and M7 (OS personas) are done. `lightbrowser serve` runs real V8
pages through `lb_page::CdpHost`; stock Playwright `connectOverCDP` passes
20 tests (including screenshots); the WPT subset passes 2556/2788. On our
detection engine (`tests/detect-bench`, ADR 0007) T-010 scores fp 0 for all
8 reference Chrome personas, with the M7 OS rules at full weight. With
`--input human` (`LightBrowser.human*`) all 8 score beh 0 on their own
seeds; see "M5 status" and "M7 status" in `docs/ROADMAP.md`. M7 (ADR 0015):
desktop Windows 10/11, macOS (Intel, Apple Silicon) and Linux (Ubuntu X11,
Fedora Wayland) Chrome personas; persona OS axis, rules R17-R26, catalog and
generator; scrollbars, fonts and emoji, OS-gated APIs, codecs, WebGPU,
WebGL backends, audio, wheel and shortcuts per OS; `Accept-CH` /
`Critical-CH` client hints. Linux values are captured from local Chrome;
Windows and macOS values come from a reference, not a capture. TCP/IP-level
signals are always the host's. M6: lazy interface
objects (ADR 0016: lazy data properties of the global as in Blink,
templates on demand, no startup snapshot by default; realm 1.4 ms), idle
RSS 6.2 MB, `bench/` runner (every corpus page faster than headless
Chromium's median), release binary 40.39 MB against < 50 MB (the user
raised the budget from < 40 MB, ADR 0013 revision 4; ICU data in 4 KiB
zstd chunks paged in on first touch, no unwind tables, V8 code only
off-by-default flags reach not linked, root crate and bindings at
`opt-level = "s"`, hot code first in `.text` from
`bin/lightbrowser/hot-symbols.txt`). Lightpanda comparison (user-approved
1.1.0 nightly, loopback only, telemetry off; "M6 status: Lightpanda
comparison", `bench/results/2026-11-03-af2c264.json`): 6 of 6 checks
pass (`fetch` wall 1.08 vs 3.37 s, peak RSS 22.1 vs 24.5 MB, CPU 0.88 vs
1.23 s; CDP 0.99 vs 5.04 s, 27.0 vs 31.4 MB, 0.68 vs 0.95 s); the
`fetch` peak RSS margin is small and moves with the code layout and the
page cache (22.06 vs 24.48 MB in that run; 23.09 vs 23.20 MB in an
independent re-run on a clean tree, `2026-11-03-02ca87f.json`, which also
passes 6 of 6; binary 40.38 MB, idle RSS 7.1 MB; after the frames work
`2026-11-03-cf3ac89.json`: 6 of 6, 21.9 vs 23.2 MB, binary 40.41 MB), so
regenerate the hot-symbol list after code changes
(`tools/hot_symbols`). `tools/surface_diff` compares the page-visible
surface of two builds. Open: Firefox (M4b: no SpiderMonkey backend, TLS
gaps in ADR 0003), workers and service workers, shader execution in
WebGL, HTTP keep-alive, stylesheets blocking scripts, CSS background
images in screenshots. Frames (ADR 0017): node refs carry a runtime-wide
document key, so one frame's DOM methods act on another frame's nodes,
nodes keep one wrapper per world and adoption keeps identity; a removed
frame's window stays usable, detached, as in Chrome; `Image`/`Audio`/
`Option` share their interface's prototype (all checked against local
Chrome in `lb-page`'s `frames::cross_realm` tests). See "Where we stand"
and the status sections in `docs/ROADMAP.md`.

## Internal use and legal
- Internal only. Never push binaries to public repos or registries.
- Point it only at our own detection engine and our own test servers, never
  at third-party sites or detection services. `bench/` runs locally.

## Crate map
| Crate | Role | State |
|---|---|---|
| `lb-persona` | Persona model, `BrowserFamily`, consistency rules, `sfc32` | v2 schema + loader, rules R01–R26, sfc32 done; OS axis and per-OS accessors (M7), `profiles/catalog/` + `gen_personas` example; optional `preferences`, media devices, battery |
| `lb-js` | `JsEngine` trait; V8 (chrome) or SpiderMonkey (firefox) | V8 backend done (lazy interfaces and templates, ADR 0016; startup snapshot made in memory, optional, ADR 0013; ICU data in 4 KiB zstd chunks paged in on first touch, `v8_engine/icu_data.rs`, `LB_ICU_EAGER=1` for valgrind; code cache for large scripts compiled again; V8 objects only off-by-default flags reach are stubbed out, `v8_engine/unused_v8.rs`, so `init_v8` refuses those flags; `RealmOps`, native `inspect` for CDP); SpiderMonkey M4b |
| `lb-net` | BoringSSL, own h1 writer, forked h2 (`vendor/h2`), cookies, decoding, `fetch`, per-destination headers (`profiles/headers`), `Accept-CH` client hints | Done for Chrome; cookies shared across clones; Firefox headers generic; cache, proxies, connection reuse open (`lb-page` caps 6 connections per origin) |
| `lb-fptest` | Test support only, never shipped: ClientHello parser, JA3/JA4, Akamai H2, loopback capture server, profile builder | M1 done |
| `lb-dom` | Arena DOM, html5ever sink, selectors | Done (one arena per document; `import_node_mapped` for adoption across documents); no shadow DOM, form owner, custom elements |
| `lb-webidl-gen` | WebIDL to Rust codegen (build time) | Own parser + Chrome generator done; table in `lb-webapi/src/generated/` |
| `lb-webapi` | Bindings installer, DOM/events/window/network APIs, input pipeline, canvas 2D, images, navigator surface, WebGL, audio, fonts, frames | Done (chrome), including the HTMLTable* APIs (`rt/table.rs`) and the OS-gated surface (share, bluetooth, codecs, keyboard, gpu). WebGL runs no shaders; no workers, crypto or WebSocket |
| `lb-css` | Lazy cascade | Done (~75 longhands; no pseudo-elements, animations) |
| `lb-layout` | taffy layout on demand, font metrics, hit testing, AX tree | Done (approximate inline layout); per-persona scrollbars, generic fonts, Linux aliases; `display_list` for screenshots |
| `lb-paint` | tiny-skia canvas 2D, text, PNG/JPEG/WebP codecs, seeded readback noise, page screenshots (ADR 0014) | Done (no filters or shaping; screenshots lack background images, radius, shadows, transforms) |
| `lb-page` | Event loop, navigation, frames, workers, CDP page host, fingerprint report | Event loop, navigation, iframes, `CdpHost`, `fingerprint_report`, human-input replay (`cdp/human.rs`), `Page.captureScreenshot` done; no workers or popups |
| `lb-cdp` | CDP server | Done; real host is `lb_page::CdpHost`, `MockHost` is for lb-cdp tests and the firefox build |
| `lb-behavior` | Human input models (Fitts, minimum jerk, log-normal keys, wheel/trackpad), rewritten from the vendored stealth-browser crate | Done; replayed by `LightBrowser.human*` |
| `sdk/python` | `lightbrowser` package: launcher, `Persona`, sync/async Playwright wrappers with `human_*` and `verify_fingerprint`, scenarios T-001..T-007 | Done; needs Python Playwright >= 1.63 |
| `bench/` | Crawl benchmark (generated 50-page corpus, loopback only) vs headless Chromium and Lightpanda; results in `bench/results/` | Runner done; Lightpanda compared (6 of 6 checks pass, see ROADMAP) |
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
./target/debug/lightbrowser validate profiles/reference/chrome/*.json   # persona rules (8 personas)
./target/debug/lightbrowser validate profiles/generated/adversarial/*.json  # each must fail its one rule
cargo run -p lb-persona --example gen_personas -- --os linux --count 50 --seed 1 --out profiles/generated/linux
# M7 per-OS tests
cargo test -p lb-layout --test os
cargo test -p lb-page --test os_apis
cargo test -p lb-net --test accept_ch
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
python3 -m unittest discover -s tests/detect-bench   # scoring and server tests (33)
# Linux ground truth from local Chrome (loopback, headed under xvfb-run): see tools/os_truth/README.md
# Python SDK tests (run from sdk/python; scenarios need Playwright >= 1.63, else skipped)
(cd sdk/python && python3 -m pytest tests)
# Crawl benchmark (release binary, loopback only); tooling tests need no browser
python3 bench/run.py                          # 5 runs x 50 pages -> bench/results/
# with Lightpanda: put its binary's directory first on PATH (loopback only;
# run.py sets LIGHTPANDA_DISABLE_TELEMETRY=true; set it yourself for manual runs)
PATH=/path/to/lightpanda-dir:$PATH python3 bench/run.py
python3 -m unittest discover -s bench
# Hot-code order for the release link (after code changes; valgrind, ~10 min)
cargo build --release -p lightbrowser --config 'profile.release.package.lightbrowser.strip=false'
python3 tools/hot_symbols/gen.py --binary "$PWD/target/release/lightbrowser" && cargo build --release -p lightbrowser
# Page-visible surface of two builds (binaries below the repo; loopback only)
python3 tools/surface_diff/run.py --out "$(mktemp -d)" OLD_BIN NEW_BIN
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

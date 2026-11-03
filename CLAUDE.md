# LightBrowser: CLAUDE.md

## Overview
A from-scratch headless browser in Rust for internal bot-detection
validation. Same purpose as `/home/user/stealth-browser` (GeckoPhantom). It is
driven over CDP, so stock Playwright `connectOverCDP` works. Presents a Chrome
or Firefox persona.

`docs/ARCHITECTURE.md` and `docs/ROADMAP.md` are the source of truth. Read them
first and do not contradict them. Current state (Chrome build): M1 (network),
M2 (DOM + V8), M3 (layout + CDP) and M4 (Chrome fingerprints) are done.
`lightbrowser serve` runs real V8 pages through `lb_page::CdpHost`; stock
Playwright `connectOverCDP` passes 16 tests; the WPT subset passes
2552/2788. On our detection engine (`tests/detect-bench`, ADR 0007) T-010
over CDP scores fp 0 for both reference Chrome personas (target `fp<=30`);
only behaviour rules fire (M5, target `beh<=30`). Open: Firefox (M4b: no
SpiderMonkey backend, TLS gaps in ADR 0003), workers, shader execution in
WebGL, behaviour replay and the Python SDK (M5), and size/snapshot budgets
(M6: release binary 62.1 MB vs < 40 MB). See "M4 status: summary" in
`docs/ROADMAP.md` for surfaces and gaps.

## Internal use and legal
- Internal only. Never push binaries to public repos or registries.
- Point it only at our own detection engine and our own test servers, never
  at third-party sites or detection services. `bench/` runs locally.

## Crate map
| Crate | Role | State |
|---|---|---|
| `lb-persona` | Persona model, `BrowserFamily`, consistency rules, `sfc32` | v2 schema + loader, rules R01–R16, sfc32 done; optional `preferences`, media devices, battery |
| `lb-js` | `JsEngine` trait; V8 (chrome) or SpiderMonkey (firefox) | V8 backend done (no snapshot; `RealmOps`, native `inspect` for CDP); SpiderMonkey M4b |
| `lb-net` | BoringSSL, own h1 writer, forked h2 (`vendor/h2`), cookies, decoding, `fetch`, per-destination headers (`profiles/headers`) | Done for Chrome; cookies shared across clones; Firefox headers generic; cache, proxies, connection reuse open |
| `lb-fptest` | Test support only, never shipped: ClientHello parser, JA3/JA4, Akamai H2, loopback capture server, profile builder | M1 done |
| `lb-dom` | Arena DOM, html5ever sink, selectors | Done; no shadow DOM, form owner, custom elements |
| `lb-webidl-gen` | WebIDL to Rust codegen (build time) | Own parser + Chrome generator done; table in `lb-webapi/src/generated/` |
| `lb-webapi` | Bindings installer, DOM/events/window/network APIs, input pipeline, canvas 2D, images, navigator surface, WebGL, audio, fonts, frames | Done (chrome). WebGL runs no shaders; no workers, crypto or WebSocket |
| `lb-css` | Lazy cascade | Done (~75 longhands; no pseudo-elements, animations) |
| `lb-layout` | taffy layout on demand, font metrics, hit testing, AX tree | Done (approximate inline layout) |
| `lb-paint` | tiny-skia canvas 2D, text, PNG/JPEG/WebP codecs, seeded readback noise | Done (no filters or shaping) |
| `lb-page` | Event loop, navigation, frames, workers, CDP page host, fingerprint report | Event loop, navigation, iframes, `CdpHost`, `fingerprint_report` done; no workers or popups |
| `lb-cdp` | CDP server | Done; real host is `lb_page::CdpHost`, `MockHost` is for lb-cdp tests and the firefox build |
| `lb-behavior` | Human input simulation (vendored from stealth-browser) | Vendored + `cdp` event conversion; replay M5 |
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
cargo build -p lightbrowser && node --test tests/playwright/   # 16 tests; LIGHTBROWSER_BIN overrides
# Live surface vs persona (JSON on stdout, exit 1 on any mismatch)
./target/debug/lightbrowser fingerprint-report --persona profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json
cargo test -p lb-page --test pages report     # same check as a CI gate, all reference Chrome personas
# Detection bench T-010 over CDP (loopback only; see tests/detect-bench/README.md)
python3 tests/detect-bench/server.py --port 18766 --results-dir "$(mktemp -d)" &
./target/debug/lightbrowser serve --port 19222 --persona profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json &
node tests/detect-bench/run.js --base http://127.0.0.1:18766 --scenario T-010 \
    --cdp ws://127.0.0.1:19222/devtools/browser/<id> --expect 'fp<=30'   # <id> from curl 127.0.0.1:19222/json/version
# stop both servers afterwards
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

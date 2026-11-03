# LightBrowser: CLAUDE.md

## Overview
A from-scratch headless browser in Rust for internal bot-detection
validation. Same purpose as `/home/user/stealth-browser` (GeckoPhantom). It is
driven over CDP, so stock Playwright `connectOverCDP` works. Presents a Chrome
or Firefox persona.

`docs/ARCHITECTURE.md` and `docs/ROADMAP.md` are the source of truth. Read them
first and do not contradict them. Current state: M1 (network) done for
Chrome; Firefox TLS gaps are listed in ADR 0003. M2 (DOM + V8) is done for
Chrome: `fetch --dump html` runs scripts and the WPT subset passes 2540/2788.
M3 is in progress: `lb-css`, `lb-layout` and `lb-cdp` have landed, and
`serve` runs on a mock page host until `lb-page` implements
`lb_cdp::PageHost`. `lb-paint` is still an empty skeleton. See "M2 status"
and "M3 status" in `docs/ROADMAP.md`. Our detection engine for M4/M5 is
`tests/detect-bench` on loopback (ADR 0007).

## Internal use and legal
- Internal only. Never push binaries to public repos or registries.
- Point it only at our own detection engine and our own test servers, never
  at third-party sites or detection services. `bench/` runs locally.

## Crate map
| Crate | Role | State |
|---|---|---|
| `lb-persona` | Persona model, `BrowserFamily`, consistency rules, `sfc32` | v2 schema + loader, rules R01–R16, sfc32 done |
| `lb-js` | `JsEngine` trait; V8 (chrome) or SpiderMonkey (firefox) | V8 backend done (no snapshot); SpiderMonkey M4b |
| `lb-net` | BoringSSL, hyper h1, forked h2 (`vendor/h2`), cookies, decoding, `fetch` | M1 done; cookies shared across clones; cache, proxies, connection reuse open |
| `lb-fptest` | Test support only, never shipped: ClientHello parser, JA3/JA4, Akamai H2, loopback capture server, profile builder | M1 done |
| `lb-dom` | Arena DOM, html5ever sink, selectors | Done; no shadow DOM, form owner, custom elements |
| `lb-webidl-gen` | WebIDL to Rust codegen (build time) | Own parser + Chrome generator done; table in `lb-webapi/src/generated/` |
| `lb-webapi` | Bindings installer, DOM/events/window/network APIs; canvas/WebGL/audio later | Installer + core APIs done (chrome); canvas/WebGL/audio M4 |
| `lb-css` | Lazy cascade | Done (~75 longhands; no pseudo-elements, animations) |
| `lb-layout` | taffy layout on demand, font metrics, hit testing, AX tree | Done (approximate inline layout) |
| `lb-paint` | tiny-skia canvas | M4 |
| `lb-page` | Event loop, navigation, frames, workers | Event loop + navigation done; no iframes/workers; `PageHost` for CDP M3 |
| `lb-cdp` | CDP server | Done against `MockHost`; real page host pending |
| `lb-behavior` | Human input simulation (vendored from stealth-browser) | Vendored + `cdp` event conversion; replay M5 |
| `bin/lightbrowser` | CLI; picks family by feature; `fetch`, `validate`, `serve` | `fetch` (runs scripts), `validate` done; `serve` on mock host |

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
cargo build -p lightbrowser && node --test tests/playwright/
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

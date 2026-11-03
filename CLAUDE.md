# LightBrowser: CLAUDE.md

## Overview
A from-scratch headless browser in Rust for internal bot-detection
validation. Same purpose as `/home/user/stealth-browser` (GeckoPhantom). It is
driven over CDP, so stock Playwright `connectOverCDP` works. Presents a Chrome
or Firefox persona.

`docs/ARCHITECTURE.md` and `docs/ROADMAP.md` are the source of truth. Read them
first and do not contradict them. Current state: M1 (network) done for
Chrome; Firefox TLS gaps are listed in ADR 0003. Crates from M2 on are still
empty skeletons.

## Internal use and legal
- Internal only. Never push binaries to public repos or registries.
- Point it only at our own detection engine and our own test servers, never
  at third-party sites or detection services. `bench/` runs locally.

## Crate map
| Crate | Role | State |
|---|---|---|
| `lb-persona` | Persona model, `BrowserFamily`, consistency rules, `sfc32` | sfc32 done |
| `lb-js` | `JsEngine` trait; V8 (chrome) or SpiderMonkey (firefox) | trait + `COMPILED_FAMILY` |
| `lb-net` | BoringSSL, hyper h1, forked h2 (`vendor/h2`), cookies, decoding, `fetch`, `tcpfp` (TCP SYN profiles, shim) | M1, M1b done; cache, proxies M2 |
| `lb-fptest` | Test support only, never shipped: ClientHello parser, JA3/JA4, Akamai H2, loopback capture server, profile builder, TCP SYN capture and JA4T | M1, M1b done |
| `lb-dom` | Arena DOM, html5ever sink, selectors | M2 |
| `lb-webidl-gen` | WebIDL to Rust codegen (build time) | M2 |
| `lb-webapi` | fetch, XHR, timers, storage, canvas/WebGL/audio APIs | M2+ |
| `lb-css` | Lazy cascade | M3 |
| `lb-layout` | taffy layout on demand, font metrics | M3 |
| `lb-paint` | tiny-skia canvas | M4 |
| `lb-page` | Event loop, navigation, frames, workers | M2/M3 |
| `lb-cdp` | CDP server | M3 |
| `lb-behavior` | Human input simulation (vendored from stealth-browser) | M5 |
| `bin/lightbrowser` | CLI; picks family by feature; `fetch`, `tcp-shim` | `fetch`, `tcp-shim` done, `serve` stub |

## Commands
```bash
cargo build --workspace                       # chrome (default)
cargo build --release                         # release, chrome
cargo build --release --no-default-features --features firefox   # release, firefox
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
# Firefox family checks
cargo test -p lightbrowser -p lb-js --no-default-features --features firefox
cargo clippy -p lightbrowser -p lb-js --no-default-features --features firefox -- -D warnings
./target/release/lightbrowser --version       # prints the compiled family
# Network fingerprint check vs profiles/tls (loopback only, prints JA3/JA4/H2)
cargo test -p lb-net --test fingerprint -- --nocapture
# TCP SYN check (Windows/macOS/Linux stack) on loopback; needs root, skips without it
cargo test -p lb-net --test tcp_fingerprint -- --nocapture --test-threads=1
# Run the packet shim, then fetch with a persona's TCP stack (ADR 0007)
sudo ./target/debug/lightbrowser tcp-shim &
./target/debug/lightbrowser fetch http://127.0.0.1:PORT/ --tcp-profile persona
# Fetch from a local test server (self-signed cert)
./target/debug/lightbrowser fetch https://127.0.0.1:PORT/ --insecure --dump text
```
Both families output to `target/release/lightbrowser`; the last build wins.
Run the chrome and firefox checks before finishing any change that touches
`lb-js`, `lb-persona` or the binary. Do not run
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
- **TCP profile matches the UA's OS.** `persona.tcp_profile_id` names a
  `profiles/tcp/` profile for the same OS as the User-Agent. Windows and macOS
  profiles need `tcp-shim` and fail closed without it; never send the host's SYN
  by accident (ADR 0007). The shim also sets the IP ID and the SYN retransmit
  schedule of every packet (ADR 0008).
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

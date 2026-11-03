# LightBrowser

A headless browser written from scratch in Rust. It has no GUI. Stock
Playwright and Puppeteer drive it over the Chrome DevTools Protocol (CDP).

It presents one coherent persona, either Chrome or Firefox: network
fingerprint (TLS, HTTP/2, header order), JavaScript surface, and device
fingerprints (canvas, WebGL, audio, fonts, screen). It aims to use less CPU,
RAM and disk than Lightpanda.

**Status:** M1 (network), M2 (DOM + V8), M3 (layout + CDP) and M4 (Chrome
fingerprints) are done for the Chrome build. `lightbrowser serve` runs real
V8 pages, and stock Playwright `connectOverCDP` drives them (click, fill,
getByRole, frames, network routing). On our own detection bench, fingerprint
rules score 0 for both reference Chrome personas; only behaviour rules fire
(M5). Not done: Firefox (M4b, no SpiderMonkey backend yet), workers,
behaviour replay and the Python SDK (M5), size and startup budgets (M6; the
release binary is 62.1 MB against < 40 MB). See
[docs/ROADMAP.md](docs/ROADMAP.md).

## Internal use only

This is a tool for validating our own bot-detection engine. It sits alongside
GeckoPhantom (`stealth-browser`). Point it only at our own detection engine and
our own test servers, never at third-party sites or detection services.
Performance benchmarks (`bench/`) run locally on our own machines. Do not
distribute binaries or push them to public repos or registries.

## Goals and budgets

| Metric | Target |
|---|---|
| Lightpanda crawl benchmark (wall time, peak RSS, CPU time) | <= Lightpanda on the same machine |
| Idle process RSS | < 15 MB |
| Binary size (per family, stripped) | < 40 MB |
| Disk writes during a session | none by default |
| Isolate/realm creation | < 5 ms (startup snapshot) |

## Families

One binary per family, chosen at build time with a cargo feature.

| | Chrome (default) | Firefox |
|---|---|---|
| Feature | `chrome` | `firefox` |
| JS engine | V8 | SpiderMonkey (Servo's `mozjs`) |
| TLS / H2 | BoringSSL, Chrome profile | BoringSSL, Firefox profile |

A binary refuses a persona whose family does not match its engine. The V8
backend has landed; SpiderMonkey arrives in M4b. Until then the firefox build
loads and parses pages but runs no scripts, and its `serve` uses a mock page
host.

## Build

Requires the stable Rust toolchain (see `rust-toolchain.toml`).

```bash
# Chrome family (default)
cargo build --release
./target/release/lightbrowser --version      # lightbrowser 0.0.1 (chrome)

# Firefox family
cargo build --release --no-default-features --features firefox
./target/release/lightbrowser --version      # lightbrowser 0.0.1 (firefox)
```

Both write to `target/release/lightbrowser`, so the second build replaces the
first. Use a separate `--target-dir` to keep both.

Test and lint:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
# Firefox family
cargo test -p lightbrowser -p lb-js -p lb-webapi -p lb-page --no-default-features --features firefox
cargo clippy -p lightbrowser -p lb-js -p lb-webapi -p lb-page --no-default-features --features firefox -- -D warnings
```

Subcommands: `fetch <url> --dump html|text` (runs scripts), `validate
<persona.json>`, `serve --port 9222` (CDP for Playwright `connectOverCDP`;
use `viewport: null`, because viewports that contradict the persona are
rejected), and `fingerprint-report [--persona FILE] [--url URL]` (compares the
live JS surface with the persona; JSON on stdout, exit 1 on a mismatch).
`fetch`, `serve` and `fingerprint-report` take `--persona FILE` and default to
the Windows Chrome 153 reference persona. Point them only at loopback test
servers.

## Layout

```
bin/lightbrowser/   CLI binary; selects the family feature
crates/             lb-persona, lb-js, lb-net, lb-dom, lb-css, lb-layout,
                    lb-paint, lb-webapi, lb-webidl-gen, lb-page, lb-cdp,
                    lb-behavior (see CLAUDE.md for the crate map)
sdk/python/         Python launcher + persona + human_* helpers (planned)
profiles/           personas and TLS profiles (profiles/tls/)
bench/              benchmarks vs Lightpanda and headless Chrome
tests/              integration tests
docs/               architecture, roadmap, ADRs
```

## Docs

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): layers, stack, fingerprint surface, CDP surface
- [docs/ROADMAP.md](docs/ROADMAP.md): milestones M0 to M6 and risks
- [docs/adr/](docs/adr/): architecture decision records

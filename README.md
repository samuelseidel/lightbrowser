# LightBrowser

A headless browser written from scratch in Rust. It has no GUI. Stock
Playwright and Puppeteer drive it over the Chrome DevTools Protocol (CDP).

It presents one coherent persona, either Chrome or Firefox: network
fingerprint (TLS, HTTP/2, header order), JavaScript surface, and device
fingerprints (canvas, WebGL, audio, fonts, screen). It aims to use less CPU,
RAM and disk than Lightpanda.

**Status:** M1 (network), M2 (DOM + V8), M3 (layout + CDP), M4 (Chrome
fingerprints) and M5 (human input, Python SDK) are done for the Chrome build,
and most of M6 (snapshot, memory, binary size, `bench/` runner, screenshots).
`lightbrowser serve` runs real V8 pages, and stock Playwright
`connectOverCDP` drives them (click, fill, getByRole, frames, network
routing, screenshots). On our own detection bench, fingerprint rules score 0
for both reference Chrome personas. With `LightBrowser.human*` input both
personas score 0 on behaviour rules on their own seeds (seed sweeps: 20/20
Windows, 19/20 macOS).
Idle RSS is 10.3 MB, realm creation 2.1 ms, and the release binary 44.4 MB
(budget < 56 MB with the prebuilt V8, ADR 0013; the original < 40 MB is
still missed). On the `bench/` corpus every LightBrowser page is faster
than headless Chromium's median (see `bench/results/README.md`).
Not done: Firefox (M4b, no SpiderMonkey backend yet), workers and service
workers, and the comparison with Lightpanda (no binary on the bench machine).
See [docs/ROADMAP.md](docs/ROADMAP.md).

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
| Binary size (per family, stripped) | < 56 MB for Chrome with the prebuilt V8 (was < 40 MB, ADR 0013); measured 44.4 MB |
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
rejected; `--workers N`, `--insecure`, `--profiles-dir`), and `fingerprint-report [--persona FILE] [--url URL]` (compares the
live JS surface with the persona; JSON on stdout, exit 1 on a mismatch).
`fetch`, `serve` and `fingerprint-report` take `--persona FILE` and default to
the Windows Chrome 153 reference persona. Point them only at loopback test
servers.

## Python SDK

`sdk/python` starts `lightbrowser serve` with a persona and returns stock
Playwright objects with human-like input and a fingerprint check bound on.
Needs Python Playwright >= 1.63 (older versions leak `__pwInitScripts` into
the page). Point it only at loopback servers. See
[sdk/python/README.md](sdk/python/README.md).

```python
from lightbrowser import Browser, Persona

persona = Persona.load("profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json")
with Browser(persona=persona) as browser:        # runs `lightbrowser serve --port 0`
    page = browser.new_page()                    # stock Playwright Page
    page.goto("http://127.0.0.1:8765/shop")      # your own local server
    page.human_scroll(900)
    x, y = page.human_click("#add-3")            # the point actually clicked
    page.human_type("#q", "jacket")
    assert page.verify_fingerprint().ok
```

Detection bench check (a bench server on 127.0.0.1 and a fresh `serve` per run):
`node tests/detect-bench/run.js --scenario T-010 --cdp <ws> --input human
--expect 'beh<=30'`. Crawl benchmark: `python3 bench/run.py`.

## Layout

```
bin/lightbrowser/   CLI binary; selects the family feature
crates/             lb-persona, lb-js, lb-net, lb-dom, lb-css, lb-layout,
                    lb-paint, lb-webapi, lb-webidl-gen, lb-page, lb-cdp,
                    lb-behavior (see CLAUDE.md for the crate map)
sdk/python/         Python launcher + persona + human_* helpers
profiles/           personas and TLS profiles (profiles/tls/)
bench/              crawl benchmark vs headless Chromium (and Lightpanda if installed)
tests/              Playwright, WPT and detection-bench suites
docs/               architecture, roadmap, ADRs
```

## Docs

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): layers, stack, fingerprint surface, CDP surface
- [docs/ROADMAP.md](docs/ROADMAP.md): milestones M0 to M6 and risks
- [docs/adr/](docs/adr/): architecture decision records

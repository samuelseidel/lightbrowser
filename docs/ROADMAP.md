# Roadmap

Each milestone ends with something we can run and measure.

| Milestone | Scope | Done when |
|---|---|---|
| **M0 Foundations** | Architecture docs, ADRs, workspace skeleton, CI, SFC32 port | `cargo build/test/clippy` green for both families ✅ |
| **M1 Network** | `lb-net`: BoringSSL ClientHello profiles, h1 + forked h2, cookies, decompression, proxy. `lightbrowser fetch <url>` | JA3/JA4 and Akamai H2 hash match real Chrome on our local TLS/H2 echo server |
| **M2 DOM + V8** | Arena DOM, html5ever sink, selectors, WebIDL codegen (Chromium set), V8 backend + snapshot, event loop, timers, fetch/XHR | `fetch --dump html` prints the DOM *after* scripts run. A WPT DOM subset passes. |
| **M3 Layout + CDP core** | Lazy cascade, taffy layout, hit-testing, AX tree, CDP server with the domains in ARCHITECTURE.md | Playwright `connectOverCDP`: goto, evaluate, click, fill, getByRole, content all pass |
| **M4 Chrome fingerprints** | Persona v2, vendored consistency crate + Chrome rules, navigator/UA-CH, screen, canvas, WebGL, audio, fonts, Intl/timezone, WebRTC, permissions | Our detection engine scores the Chrome personas as consistent. Same seed gives the same fingerprint. |
| **M4b Firefox family** | SpiderMonkey backend (`mozjs`), Gecko IDL set, Firefox TLS/H2/header profiles | Existing Firefox 128 personas pass the same checks as on GeckoPhantom |
| **M5 Behavior + SDK** | Vendored behavior crate, `LightBrowser.human*`, Python SDK | `page.human_click` produces realistic event streams in the detection engine |
| **M6 Performance** | `bench/` vs Lightpanda and headless Chrome, memory tuning, optional screenshots, service workers | Meets the budgets in ARCHITECTURE.md. Results published in `bench/results/`. |

## Risks

- **WebIDL breadth.** Chrome exposes 1,000+ interfaces. We generate all of them
  from IDL, but each needs an implementation or a spec-shaped stub. Plan: stub
  first, implement in order of what the detection engine and real pages touch.
- **SpiderMonkey embedding.** `mozjs` builds are slow and pin a SpiderMonkey
  ESR. Keeping it in its own feature/binary stops it from slowing Chrome work.
- **Firefox ClientHello on BoringSSL.** Most of it is reproducible, but a few
  NSS-only details may need small BoringSSL patches. M4b checks this against
  the captured `firefox_128_*.json`.
- **Layout fidelity.** Element sizes from a simplified layout won't match
  Chrome pixel-for-pixel. Detection relies mostly on font metrics and a few
  probes, which we cover with persona metric tables.

# Roadmap

Each milestone ends with something we can run and measure.

| Milestone | Scope | Done when |
|---|---|---|
| **M0 Foundations** | Architecture docs, ADRs, workspace skeleton, CI, SFC32 port | `cargo build/test/clippy` green for both families ✅ |
| **M1 Network** | `lb-net`: BoringSSL ClientHello profiles, h1 + forked h2, cookies, decompression, proxy. `lightbrowser fetch <url>` | JA3/JA4 and Akamai H2 hash match real Chrome on our local TLS/H2 echo server ✅ (Chrome). Proxies moved to M2. |
| **M2 DOM + V8** | Arena DOM, html5ever sink, selectors, WebIDL codegen (Chromium set), V8 backend + snapshot, event loop, timers, fetch/XHR | `fetch --dump html` prints the DOM *after* scripts run. A WPT DOM subset passes. ✅ (Chrome; 2540/2788 WPT subtests) |
| **M3 Layout + CDP core** | Lazy cascade, taffy layout, hit-testing, AX tree, CDP server with the domains in ARCHITECTURE.md | Playwright `connectOverCDP`: goto, evaluate, click, fill, getByRole, content all pass |
| **M4 Chrome fingerprints** | Persona v2, vendored consistency crate + Chrome rules, navigator/UA-CH, screen, canvas, WebGL, audio, fonts, Intl/timezone, WebRTC, permissions | Our detection engine (`tests/detect-bench`, ADR 0007) scores the Chrome personas as consistent: `run.js --scenario T-010 --cdp <ws> --expect 'fp<=30'`. Same seed gives the same fingerprint. |
| **M4b Firefox family** | SpiderMonkey backend (`mozjs`), Gecko IDL set, Firefox TLS/H2/header profiles | Existing Firefox 128 personas pass the same checks as on GeckoPhantom |
| **M5 Behavior + SDK** | Vendored behavior crate, `LightBrowser.human*`, Python SDK | `page.human_click` produces realistic event streams in the detection engine (`tests/detect-bench`, T-010 with `--expect 'beh<=30'`) |
| **M6 Performance** | `bench/` vs Lightpanda and headless Chrome, memory tuning, optional screenshots, service workers | Meets the budgets in ARCHITECTURE.md. Results published in `bench/results/`. |

## M1 status (2026-11-02)

Done:

- `lb-net`: TLS from profiles, h1 (hyper), h2 (fork in `vendor/h2`), cookies,
  gzip/deflate/br/zstd, redirects.
- `lightbrowser fetch <url> [--dump html|text] [--persona chrome|firefox]
  [--insecure]`.
- `lb-fptest`, a local capture server, and the `chrome_153_linux` profile
  captured from Chromium 153.

Verified by `cargo test -p lb-net --test fingerprint` (loopback only):

- **Chrome:** exact match with Chromium 153 on every connection: JA3 (over
  the canonical extension order), JA4, JA4_r, every ClientHello list, the
  Akamai H2 text, HEADERS priority, and the frame order SETTINGS,
  WINDOW_UPDATE, HEADERS. Extension order is permuted per connection.
- **Firefox 128:** ciphers, sigalgs, ALPN, cert compression, groups (minus
  ffdhe) and all H2 values match. JA3 and JA4 do not match yet. The
  documented gaps are in ADR 0003 and pinned by the test.

Not done, moved:

- HTTP and SOCKS5 proxies → M2.
- Firefox TLS gaps and a Firefox 128 re-capture → M4b.
- Exact HTTP/1.1 header-name case (hyper title-cases `sec-ch-ua`) → M2.
- Connection reuse and a TLS session cache → M2. Today every fetch is a new
  connection, so there is no `pre_shared_key`.

## M2 status (2026-11-03)

The done-criteria are met for the Chrome build: `lightbrowser fetch --dump
html` prints the DOM after scripts ran, and a vendored WPT subset runs
against a loopback server with **2540 of 2788 subtests passing** (48 of 83
files fully, 8 files end in a harness error or timeout). The runner fails
below 2520. The leftovers below carry into later milestones.

Landed:

- `lb-js`: V8 backend (`v8` =152.2.0) behind `chrome`. One isolate per
  `Engine`, one realm per frame or isolated world, host objects bound from
  `InterfaceSpec`, promises, static ES modules through a loader, heap cap,
  terminate from another thread, named setter/deleter/enumerator
  interceptors. A panic in a host function becomes a JS error. Chrome's
  error text, stack format, `Illegal invocation` and native `toString` are
  pinned by tests. V8 crashes above about 1,020 template properties, so
  instance member lists over 960 are copied onto each new instance.
- `lb-dom`: arena DOM (48-byte nodes, u32 ids), html5ever 0.40.1 sink,
  streaming parse that yields at every `</script>` and handles
  `document.write`, fragment parsing, serialization, Servo `selectors` 0.42,
  and `render_generation()` for style and layout caches.
- `lb-webidl-gen`: own tolerant WebIDL parser (all 2,229 vendored Chromium
  153 files parse; replaces the planned `weedle2`), partial/mixin merge,
  and a generator that orders members by a surface fixture from the local
  Chromium. Output: `lb-webapi/src/generated/chromium.rs` (909 interfaces,
  `CSS`, 1,225 window props), checked in, with a staleness test.
  - Two fixtures: `tests/fixtures/chromium_153_surface_secure.json`
    (loopback http page, a secure context; the default) and
    `chromium_153_surface.json` (`about:blank`, not secure). The second marks
    secure-context-only members (about 225 interfaces, 243 window props),
    which insecure realms leave out.
- `lb-webapi` bindings: registry and installer. In a secure realm all 900
  checked interfaces match Chrome 153 (prototype and static names, symbols,
  `length`); insecure realms match 675 of 675. Window own properties come
  out in Chrome's order; only `chrome` is missing (M4). Unregistered members
  are spec-shaped stubs. Defining all bindings plus the first realm takes
  about 34 ms in release.
- `lb-webapi` implementations (`rt`): Node/Element/Document and friends,
  live collections, about 140 reflected attributes, events with
  capture/bubble and listener options, timers, rAF, Location, History,
  Storage, Performance (100 µs), Screen, navigator (from the persona,
  `webdriver` false), XHR, fetch, Headers/Request/Response, AbortSignal,
  URL, TextEncoder/Decoder, DOMParser, Blob, MutationObserver,
  getComputedStyle and geometry through `lb-layout`.
- `lb-page`: `Browser` with worker threads (one engine each),
  `BrowserContext` (persona, shared cookie jar, localStorage), `Page`, the
  HTML event loop (task sources, microtasks, timer clamping, rAF, virtual
  time), navigation with encoding sniffing, classic/defer/async/module
  scripts, `document.write`, stylesheets, load and network-idle lifecycle.
  The firefox build loads and parses documents without running scripts.
- `lightbrowser fetch [--dump html|text] [--wait-until W] [--timeout S]`
  runs scripts. The tag stripper is gone.
- `lb-net`: `CookieJar` clones share one jar; `document.cookie` helpers.
- Pulled forward: `lb-persona` v2 with rules R01–R16 and Chrome 153
  reference personas (M4 task 1); `lb-behavior` vendoring plus CDP event
  conversion (M5 task 1); `tests/detect-bench`.

Detect-bench baseline (ADR 0007). Release chrome binary, `lightbrowser
fetch http://127.0.0.1:<port>/shop --dump html`, default network-only
persona. The collector ran and sent its load beacon. Score **98.8, bot**
(fp 98.2, beh 30.0). Fired rules: `ua_ch_mismatch`, `iframe_mismatch`,
`plugins`, `webgl_software`, `chrome_object`, `header_order` (h1 header
name case), `no_interaction`, `images_blocked`, `timezone_utc`,
`connection`, `favicon_missing`. `webdriver` and the headless-UA rules did
not fire. Reducing this is M4 (target `fp<=30`) and M5 (`beh<=30`).

Measured (release, chrome): stripped binary 58.9 MB (budget < 40 MB);
`fetch` of the bench `/shop` page about 0.2 s wall time.

Verified: `cargo fmt --all --check`, `cargo build --workspace`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo test --workspace`, firefox
clippy and tests for `lightbrowser`, `lb-js`, `lb-webapi`, `lb-page`, `cargo
test -p lb-net --test fingerprint` (network fingerprint unchanged), the
bench's Python tests and `node --test tests/playwright/`.

Still open from M2:

- Bindings: `Image`/`Option`/`Audio` are separate interface objects; no
  `WindowProperties` object; `NodeFilter` has a `prototype`; no weak
  wrappers (host objects live until the window drops); no `DOMException`
  `stack`; namespace and worker-only member order is from the IDL, not a
  fixture.
- Pages: no nested browsing contexts (`iframe.contentWindow` is null), no
  workers, no cross-document history, form POST sent as GET.
- Scripts: no dynamic `import()` or `import.meta`; script-inserted module
  scripts are skipped; dynamic `async = false` scripts are not ordered;
  scripts written by `document.write` run after the writer returns; no
  unhandled-rejection reporting.
- Network: sync XHR runs async; no CORS preflight; no `ReadableStream`
  bodies or FormData; subresources send navigation headers
  (`upgrade-insecure-requests`, `sec-fetch-user`); M1 leftovers (proxies,
  exact h1 header-name case, connection reuse, TLS session cache).
- DOM: no shadow DOM, custom elements, Range, Selection, TreeWalker, CDATA
  or XML parsing; cross-document adopt copies the node.
- CLI: `fetch` uses the network-only persona, so navigator values are
  derived from the UA; `--persona FILE` answers "M4".
- Realm creation is about 30 ms per navigation (budget < 5 ms with the M6
  snapshot).
- `lb-persona`: rule R04 rejects the upstream Apple Silicon profiles; the
  UA-CH GREASE logic is not checked against Chromium source.

## M3 status (2026-11-03)

In progress. `lb-css`, `lb-layout` and `lb-cdp` have landed and are tested
against a mock page host. Wiring them to real pages is pending.

- `lb-css`: lazy cascade with cssparser 0.38 and the lb-dom selectors,
  UA sheet modelled on Chromium's html.css, media queries from the persona,
  `var()`/`calc()`, about 75 longhands, Chrome's `getComputedStyle`
  serialization, cache keyed on `render_generation`.
- `lb-layout`: taffy 0.15.1 on our own box tree (so `calc()` works),
  inline content approximated as wrapping flex rows, tables as flex rows,
  Chrome-on-Windows form control sizes, text widths from font tables in
  `profiles/fonts/` (exact for the Liberation/Carlito metric-compatible
  fonts, approximate for Segoe UI, Verdana, Tahoma, Georgia, Helvetica and
  macOS `system-ui`), hit testing, scrolling and the accessibility tree.
- `lb-cdp`: CDP server on loopback (HTTP discovery + WebSocket, flat
  sessions, Host and Origin checks), the domains in ARCHITECTURE.md,
  Emulation checked against the persona, `LightBrowser.setPersona`.
  `lightbrowser serve` runs it on `MockHost` (lb-dom + lb-layout, no JS
  engine, loopback URLs only). Decisions: `Runtime.addBinding` only reaches
  isolated worlds; a `setDeviceMetricsOverride` that contradicts the persona
  screen (including Playwright's default 1280x720) is rejected, so clients
  pass `viewport: null`.
- `tests/playwright/`: stock Playwright 1.63 `connectOverCDP` → newContext →
  newPage → goto → evaluate → title → content passes on the mock.

Open: `lb-page` must implement `lb_cdp::PageHost` and replace `MockHost`
in `serve`; then click, fill and getByRole can run end to end. No child
frames, workers or popups in CDP; no screenshots. Layout has no real line
boxes, shaping, bidi or table layout; scrollbar width is 0. The persona has
no `prefers-color-scheme` or scrollbar field yet.

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

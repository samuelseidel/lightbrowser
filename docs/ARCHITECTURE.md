# LightBrowser Architecture

LightBrowser is a headless browser written from scratch in Rust. It has no GUI.
Stock Playwright and Puppeteer drive it over the Chrome DevTools Protocol (CDP).

It presents a coherent persona, either **Chrome** or **Firefox**. The persona covers:
- the network fingerprint: TLS ClientHello, HTTP/2 settings and header order
- the JavaScript surface
- device fingerprints: canvas, WebGL, audio, fonts and screen

It does this with less CPU, RAM and disk than Lightpanda.

It exists for **internal bot-detection validation**, the same purpose as
GeckoPhantom (`stealth-browser`). Test runs target our own detection engine.

## Goals and budgets

| Metric | Target |
|---|---|
| Lightpanda crawl benchmark (wall time, peak RSS, CPU time) | ≤ Lightpanda on the same machine |
| Idle process RSS | < 15 MB |
| Binary size (per family, stripped) | < 40 MB (most of it is the JS engine) |
| Disk writes during a session | none by default |
| Isolate/realm creation | < 5 ms (startup snapshot) |

`bench/` measures these against Lightpanda and headless Chrome.

## Decisions (see `docs/adr/`)

| # | Decision |
|---|---|
| 0001 | Rust for everything we write |
| 0002 | Embed the JS engine instead of writing one: V8 for Chrome, SpiderMonkey for Firefox |
| 0003 | BoringSSL + forked `h2` for network fingerprints |
| 0004 | Lazy style/layout, CPU canvas, no painting by default |
| 0005 | CDP server as the control API |
| 0006 | Two browser families, one binary per family |

## Layers

```
┌───────────────────────────────────────────────────────────────┐
│ Control      lb-cdp: CDP over WebSocket (Browser, Target,      │
│              Page, Runtime, DOM, Input, Network, Fetch,        │
│              Emulation, Accessibility, LightBrowser.*)         │
│              sdk/python: launcher + persona + human_* helpers  │
├───────────────────────────────────────────────────────────────┤
│ Page         lb-page: event loop, navigation, frames, workers, │
│              virtual time            lb-behavior: human input  │
├───────────────────────────────────────────────────────────────┤
│ Web APIs     lb-webapi: fetch, XHR, timers, URL, storage,      │
│              crypto, WebSocket, audio, WebGL stub              │
│ Bindings     lb-webidl-gen (build time) → generated glue       │
│ JS           lb-js: JsEngine trait → V8 | SpiderMonkey         │
├───────────────────────────────────────────────────────────────┤
│ Document     lb-dom (arena DOM, html5ever, selectors)          │
│              lb-css (lazy cascade)  lb-layout (taffy, on demand)│
│              lb-paint (tiny-skia canvas)                       │
├───────────────────────────────────────────────────────────────┤
│ Network      lb-net: BoringSSL, hyper h1, forked h2, cookies,  │
│              in-memory cache, proxies                          │
├───────────────────────────────────────────────────────────────┤
│ Persona      lb-persona: schema, consistency rules, SFC32,     │
│              per-family TLS/H2/IDL profile selection           │
└───────────────────────────────────────────────────────────────┘
```

Every layer reads the persona from an immutable `Arc<Persona>` owned by the
browser context. Nothing reads a global.

## Stack

| Area | Choice | Why |
|---|---|---|
| Language | Rust, edition 2024 | Memory safety without GC. Reuses the stealth-browser crates. |
| JS (Chrome) | `v8` crate (rusty_v8): startup snapshot of all bindings, pointer compression, per-isolate heap cap, optional `--jitless` | Same engine as Chrome, so engine quirks match (error text, stack format, `Function.prototype.toString`, Math precision, timing). |
| JS (Firefox) | Servo's `mozjs` (SpiderMonkey), per-realm heap cap | Same engine as Firefox, with a maintained Rust embedding. |
| Bindings | Own WebIDL → Rust codegen (`weedle2`), using the Chromium IDL set for Chrome and the Gecko IDL set for Firefox | Interface list, prototype chains, property order and descriptors all match the claimed browser. This is a large fingerprint surface. |
| TLS | BoringSSL (`boring`, `tokio-boring`, `boring-sys` FFI for unwrapped knobs) | Chrome uses BoringSSL, so the ClientHello matches exactly: GREASE, extension permutation, ALPS, brotli cert compression, ML-KEM, ML-DSA, trust_anchors, server_padding. Firefox's ciphers, sigalgs, groups and ALPN match by configuration. delegated_credentials, record_size_limit, ffdhe and Firefox's extension order need small BoringSSL patches (M4b, ADR 0003). rustls can do neither. |
| HTTP | hyper 1.x for h1. A fork of `h2` (`vendor/h2`) exposes SETTINGS order/values, the initial WINDOW_UPDATE, pseudo-header order and the HEADERS priority block, and writes the preface before reading. h3 via quiche later. | Akamai H2 fingerprint. Pseudo-header order: Chrome `m,a,s,p`, Firefox `m,p,a,s`. |
| Net misc | hickory-resolver, per-context cookie jar, in-memory LRU cache, gzip/br/zstd, HTTP and SOCKS5 proxies | No disk use. |
| HTML | `html5ever` tokenizer/tree builder → our own tree sink | Parses exactly like a browser. |
| DOM | Arena of compact nodes (u32 ids), interned atoms (`string_cache`). Each document is one arena, freed in one step on navigation. | Low RAM, cache-friendly, cheap teardown. |
| Selectors | Servo `selectors` + `cssparser` | `querySelector`, `matches`, cascade. |
| Style | Own cascade, resolved lazily per element | Pages that never ask for style pay nothing. |
| Layout | `taffy` (block/flex/grid). Text is measured with `skrifa` metrics of the persona's fonts. | Only runs when geometry is requested. |
| Canvas 2D | `tiny-skia` + glyph outlines from `skrifa`. Seeded SFC32 noise is added on readback (`toDataURL`, `getImageData`). | Real pixels, deterministic per persona. |
| WebGL | Stub context: parameters, extensions and shader precision come from the persona. `readPixels` returns seeded deterministic output. | No GPU, consistent answers. |
| Audio | Minimal OfflineAudioContext DSP (oscillator, dynamics compressor, analyser) + seeded noise | Covers the standard audio fingerprint script. |
| Runtime | Thread per core. Each thread runs a tokio `current_thread` runtime and owns its JS engine instances. | JS engines are single-threaded, so there's no locking. |
| Allocator | mimalloc. Release build: fat LTO, `codegen-units=1`, `panic=abort`, stripped | Speed and size. |
| Control | Own CDP server (`tokio-tungstenite`) | Playwright `connectOverCDP` works unchanged for both families. |

## Browser families

| | Chrome build | Firefox build |
|---|---|---|
| Binary | `lightbrowser` (default feature `chrome`) | `--no-default-features --features firefox` |
| JS engine | V8 | SpiderMonkey |
| IDL set | Chromium | Gecko (adds `InstallTrigger`, `mozInnerScreenX`, …; no `userAgentData`) |
| TLS | Chrome ClientHello (GREASE, permuted extensions) | Firefox ClientHello (`profiles/tls/firefox_*.json`) |
| H2 | Chrome SETTINGS, `m,a,s,p` | Firefox SETTINGS + PRIORITY frames, `m,p,a,s` |
| Headers | Chrome order, `sec-ch-ua*` client hints | Firefox order, no client hints |

`lb-js::JsEngine` is the only engine-specific boundary. DOM, network, layout
and CDP are shared code. They pick per-family data (IDL set, TLS/H2 profile,
header order) from `persona.browser.family`. When it starts, a binary refuses
any persona whose family doesn't match the engine compiled into it.

The existing GeckoPhantom personas (`persona_v1`, Firefox 128) load as
`family = firefox` without changes.

## Process and threading model

- **One process.** It has N worker threads (default: number of cores) plus one
  network/CDP I/O thread.
- **One CDP connection per client.** A Playwright `BrowserContext` maps to a
  LightBrowser context, which holds a persona, cookie jar, storage and cache
  partition. Many contexts with different personas run in one process.
- **Pages.** Each page is pinned to one worker. Each frame is its own JS realm.
  CDP utility code (Playwright's injected script) runs in an isolated world, so
  nothing is ever installed in the page's main world.
- **Workers.** Dedicated workers get their own engine instance on the same
  thread pool. Service workers are out of scope until M6.
- **Crash isolation.** `--process-per-context` is an option for later. The
  default is single-process for the RAM budget.

## Page lifecycle

1. `Page.navigate` → `lb-net` request. The persona's family picks the ClientHello, H2 settings and header order.
2. The response body streams into `html5ever`, which builds the DOM arena incrementally.
3. Scripts run in the realm (classic, `defer`, `async`, modules) through the HTML event loop: tasks, microtasks and timers.
4. Style and layout stay dirty. They are computed only when JS or CDP asks for geometry (`getBoundingClientRect`, `offsetWidth`, `elementFromPoint`, Playwright actionability, IntersectionObserver).
5. Load events fire. Lifecycle events go to CDP (`Page.lifecycleEvent`).
6. Navigating away drops the document arena and the realm in one free.

**Virtual time.** Timers and `requestAnimationFrame` can run on a virtual clock
to skip idle waiting. It is off when the persona needs realistic timing.

## Fingerprint surface → module

| Surface | Module | Source of truth |
|---|---|---|
| JA3/JA4, ALPN, cert compression | lb-net | `profiles/tls/<family>_*.json` |
| H2 SETTINGS, WINDOW_UPDATE, PRIORITY, pseudo-header order | lb-net (h2 fork) | TLS profile `http2_*` |
| Header order, Accept, Accept-Language, Sec-CH-UA* | lb-net | persona `browser`, `locale`, `client_hints` |
| `navigator.*`, `userAgentData`, plugins, mimeTypes | lb-webapi + generated bindings | persona `browser`, `hardware` |
| Interface set, prototype/property order, `toString` | lb-webidl-gen | family IDL set |
| Engine quirks (errors, stacks, Math) | lb-js | real engine |
| `screen`, `devicePixelRatio`, window sizes, media queries | lb-webapi, lb-css | persona `screen` |
| Canvas 2D | lb-paint | real raster + `canvas_seed` |
| WebGL params, extensions, precision, readPixels | lb-webapi | persona `webgl`, `hardware.gpu`, `webgl_noise_seed` |
| Audio | lb-webapi | DSP + `audio_seed` |
| Fonts (detection by width measurement) | lb-layout | persona `fonts` → bundled metric tables |
| Timezone, `Intl`, locale | lb-js (ICU data in the engine) | persona `locale` |
| WebRTC (no local IP leak) | lb-webapi | stubbed candidates |
| Permissions, battery, media devices, speech voices | lb-webapi | persona |
| `performance.now` resolution | lb-page | Chrome 100 µs / Firefox 1 ms (RFP off: 20 µs) |
| Automation tells (`webdriver`, CDP leaks) | lb-cdp, lb-page | never exposed to the page world |
| Mouse/keyboard dynamics | lb-behavior | persona behavior profile |

## Network fingerprint testing (`lb-fptest`)

`crates/lb-fptest` is test support only and is never shipped. It holds:

- a ClientHello parser;
- JA3, JA4 and JA4_r, plus the Akamai H2 text;
- a minimal H2 frame and HPACK reader;
- a capture server: TLS with an in-memory self-signed cert (rustls), loopback
  only, one `Capture` per connection.

It is used in two ways:

```
capture:  local Chromium ──TLS/H2──▶ lb-fptest capture ──▶ profiles/tls/chrome_<N>_linux.json
check:    lb_net::fetch  ──TLS/H2──▶ CaptureServer (in-process) ──▶ compare with the profile
```

- **Capture.** `lb-fptest capture` records two or more full handshakes from a
  local Chromium. It then writes the profile:
  - the ClientHello lists;
  - the canonical extension order, if the browser permutes;
  - the `trust_anchors` and `server_padding` payloads;
  - the H2 SETTINGS, WINDOW_UPDATE, pseudo-header order and HEADERS priority;
  - the expected JA3, JA4 and Akamai values.
- **Check.** `crates/lb-net/tests/fingerprint.rs` runs `lb_net::fetch` for
  each family against an in-process `CaptureServer` and asserts the values
  against the profile (ADR 0003 lists the Firefox gaps).
- **Spot checks.** `lb-fptest serve` prints one JSON line per connection,
  e.g. to compare header order with `--alpn http/1.1`.

Steps are in `tests/README.md`. Both modes use our own local server only.

## CDP surface (what Playwright needs)

- **Browser:** getVersion, close, setDownloadBehavior
- **Target:** setAutoAttach (flatten), createBrowserContext, disposeBrowserContext, createTarget, attachToTarget, closeTarget, getTargets, setDiscoverTargets
- **Page:** enable, navigate, reload, getFrameTree, createIsolatedWorld, addScriptToEvaluateOnNewDocument, lifecycleEvent, setLifecycleEventsEnabled, captureScreenshot (after M6)
- **Runtime:** enable, evaluate, callFunctionOn, getProperties, releaseObject, executionContextCreated / Destroyed, consoleAPICalled
- **DOM:** getDocument, describeNode, resolveNode, getBoxModel, getContentQuads, scrollIntoViewIfNeeded, setFileInputFiles
- **Input:** dispatchMouseEvent, dispatchKeyEvent, insertText
- **Network / Fetch:** enable, setExtraHTTPHeaders, getCookies, setCookies, requestPaused, continueRequest, fulfillRequest
- **Emulation:** setDeviceMetricsOverride, setUserAgentOverride, setTimezoneOverride, setLocaleOverride. These are checked against the persona, and conflicts are rejected.
- **Accessibility:** getFullAXTree, queryAXTree (for `getByRole`)
- **Custom `LightBrowser.*`:** setPersona, humanClick, humanType, humanScroll, fingerprintReport

## Reuse from stealth-browser

| From | To | How |
|---|---|---|
| `consistency/` (`PersonaProfile`, rules R01–R10) | `lb-persona` | Vendored in M2 (ahead of M4). Chrome rules R11–R16 added: UA-CH brands and GREASE, UA-CH platform, TLS profile ↔ UA major, client-hints presence, navigator constants. |
| `behavior/` (`human_click_events`, …) | `lb-behavior` | Vendored in M2 (ahead of M5), on rand 0.9. `lb_behavior::cdp` turns events into `Input.dispatch*` params; replay through the Input pipeline is M5. |
| `profiles/schemas/persona_v1.schema.json` | `profiles/schemas/persona_v2.schema.json` | Adds `browser.family` and `client_hints`. v1 is read as Firefox. |
| `profiles/tls/firefox_128_*.json` | `profiles/tls/` | Same format. Chrome profiles are captured with `lb-fptest` and add optional fields (`grease`, `permute_extensions`, `alps_protocols`, `ech_grease`, `server_padding_request`, `trust_anchor_ids`, `http2_priority.headers`). |
| SFC32 in `geckophantom_config.h` | `lb-persona::sfc32` | **Done.** Bit-identical, with a test against C output. |
| SDK `human_*`, `verify_fingerprint` | `sdk/python/lightbrowser` | Thin layer over Playwright `connect_over_cdp`. |

ShadowChannel and the binary mmap config aren't needed: everything runs in one
process and is controlled over CDP.

## Memory strategy

- One arena per document. Node structs are kept small (≤ 64 bytes). Attribute and text storage use interned atoms.
- No layout or style data until something asks for it, and dropped again under memory pressure.
- No paint layers, display lists or compositor.
- JS heap caps per page. The engine gets a low-memory notification when the event loop goes idle.
- HTTP cache in memory, bounded LRU, partitioned per context. Nothing goes to disk unless `--cache-dir` is given.
- Fonts: metric tables only (advance widths, ascent/descent) for persona fonts. Full outlines load only when a canvas draws text.

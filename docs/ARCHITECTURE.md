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
| Binary size (per family, stripped) | < 56 MB for Chrome with the prebuilt V8, whose floor is 42 MB (ADR 0013; was < 40 MB) |
| Disk writes during a session | none by default |
| Isolate/realm creation | < 5 ms (startup snapshot, ADR 0013) |

`bench/run.py` measures these (see the "Bench runner" row in the Stack table). Measured on
2026-11-03: idle RSS 9.9 MB, binary 54.96 MB, first/next realm 2.4/2.1 ms, no
disk writes. The Lightpanda comparison is not measured yet (no binary on the
bench machine). Details: `docs/ROADMAP.md`, "M6 status".

## Decisions (see `docs/adr/`)

| # | Decision |
|---|---|
| 0001 | Rust for everything we write |
| 0002 | Embed the JS engine instead of writing one: V8 for Chrome, SpiderMonkey for Firefox |
| 0003 | BoringSSL + forked `h2` for network fingerprints |
| 0004 | Lazy style/layout, CPU canvas, no painting by default |
| 0005 | CDP server as the control API |
| 0006 | Two browser families, one binary per family |
| 0007 | Our own local detection bench (`tests/detect-bench`) is the detection engine for M4/M5 |
| 0008 | Host objects are `(kind, id)` pairs with a per-realm wrapper cache (no weak wrappers yet) |
| 0009 | CDP bindings only in isolated worlds; `Emulation.*` must match the persona (clients pass `viewport: null`) |
| 0010 | Own box tree over taffy; font metric tables compiled in from `profiles/fonts` |
| 0011 | Request headers per destination from `profiles/headers`, captured from local Chromium; own HTTP/1.1 writer |
| 0012 | Each frame is a realm in the page's engine, built synchronously through `lb_js::RealmOps`; origins via V8 security tokens |
| 0013 | V8 startup snapshot (templates plus a window context, no persona data) made in memory once per process; binary budget follows the V8 floor |
| 0014 | Page screenshots painted on demand from a per-capture display list (no retained paint state, never noised) |

## Layers

```
┌───────────────────────────────────────────────────────────────┐
│ Control      lb-cdp: CDP over WebSocket (Browser, Target,      │
│              Page, Runtime, DOM, Input, Network, Fetch,        │
│              Emulation, Accessibility, LightBrowser.*)         │
│              sdk/python: launcher + persona + human_* helpers  │
├───────────────────────────────────────────────────────────────┤
│ Page         lb-page: event loop, navigation, frames, CDP host,│
│              virtual time (workers: not yet)                   │
│              lb-behavior: human input (LightBrowser.human*)    │
├───────────────────────────────────────────────────────────────┤
│ Web APIs     lb-webapi: DOM, events, fetch, XHR, timers, URL,  │
│              storage, canvas 2D, images, WebGL (no shaders),  │
│              audio, fonts, frames, input (no crypto/WebSocket) │
│ Bindings     lb-webidl-gen (build time) → generated glue       │
│ JS           lb-js: JsEngine trait → V8 | SpiderMonkey         │
├───────────────────────────────────────────────────────────────┤
│ Document     lb-dom (arena DOM, html5ever, selectors)          │
│              lb-css (lazy cascade)  lb-layout (taffy, on demand)│
│              lb-paint (tiny-skia canvas 2D, codecs, noise,     │
│              page screenshots)                                 │
├───────────────────────────────────────────────────────────────┤
│ Network      lb-net: BoringSSL, own h1 writer, forked h2,      │
│              cookies, per-destination headers                  │
│              (cache and proxies: not yet)                      │
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
| JS (Chrome) | `v8` crate (rusty_v8): startup snapshot of all bindings and one window context, made in memory when the first engine starts (ADR 0013); pointer compression, per-isolate heap cap, optional `--jitless` | Same engine as Chrome, so engine quirks match (error text, stack format, `Function.prototype.toString`, Math precision, timing). |
| JS (Firefox) | Servo's `mozjs` (SpiderMonkey), per-realm heap cap | Same engine as Firefox, with a maintained Rust embedding. |
| Bindings | Own WebIDL → Rust codegen with its own tolerant IDL parser (Blink's extended attributes parse as-is), using the Chromium IDL set for Chrome and the Gecko IDL set for Firefox. Members are filtered and ordered by a surface fixture captured from the local Chromium. | Interface list, prototype chains, property order and descriptors all match the claimed browser. This is a large fingerprint surface. |
| TLS | BoringSSL (`boring`, `tokio-boring`, `boring-sys` FFI for unwrapped knobs) | Chrome uses BoringSSL, so the ClientHello matches exactly: GREASE, extension permutation, ALPS, brotli cert compression, ML-KEM, ML-DSA, trust_anchors, server_padding. Firefox's ciphers, sigalgs, groups and ALPN match by configuration. delegated_credentials, record_size_limit, ffdhe and Firefox's extension order need small BoringSSL patches (M4b, ADR 0003). rustls can do neither. |
| HTTP | Own HTTP/1.1 writer (exact header-name case and order, `httparse` for responses; ADR 0011). A fork of `h2` (`vendor/h2`) exposes SETTINGS order/values, the initial WINDOW_UPDATE, pseudo-header order and the HEADERS priority block, and writes the preface before reading. h3 via quiche later. | Akamai H2 fingerprint. Pseudo-header order: Chrome `m,a,s,p`, Firefox `m,p,a,s`. |
| Net misc | hickory-resolver, per-context cookie jar, gzip/br/zstd. Planned, not built: in-memory LRU cache, HTTP and SOCKS5 proxies, connection reuse. | No disk use. |
| Request headers | `profiles/headers/chrome_153.json`: per request kind (document, iframe, script, module script, style, image, favicon, font, fetch/XHR, preflight) the header slots in h2 order, the h1 rules and Blink's observed subresource orders, captured from local Chrome for Testing 153 (69 requests kept in `profiles/headers/captures/`). `lb_net::header_profile` builds the list per hop; UA, client hints and language come from the persona; `Sec-Fetch-Site`, `Referer` and `Origin` are derived along redirect chains. Firefox keeps the navigation set plus a generic per-kind overlay (M4b). | Header names, case and order match Chrome per destination (ADR 0011). |
| HTML | `html5ever` tokenizer/tree builder → our own tree sink | Parses exactly like a browser. |
| DOM | Arena of compact nodes (u32 ids), interned atoms (`string_cache`). Each document is one arena, freed in one step on navigation. | Low RAM, cache-friendly, cheap teardown. |
| Selectors | Servo `selectors` + `cssparser` | `querySelector`, `matches`, cascade. |
| Style | Own cascade, resolved lazily per element | Pages that never ask for style pay nothing. |
| Layout | `taffy` (block/flex/grid/float) over our own box tree that implements taffy's traits, so `calc()` resolves. Inline content is approximated as wrapping flex rows. Text is measured with per-font metric tables (`profiles/fonts/`, generated offline with `skrifa`, compiled in) for the persona's fonts. | Only runs when geometry is requested. |
| Canvas 2D | `tiny-skia` raster (`lb-paint`), bitmap allocated on first draw. Text: widths and vertical metrics from the persona's `lb-layout` font tables (so `measureText` matches DOM text), shapes from glyph outline packs extracted offline with `skrifa` (`profiles/fonts/outlines/`, Liberation Sans/Serif/Mono Regular+Bold, SIL OFL, 205 KB). Encoders: PNG, JPEG, WebP (always lossless). Readback noise (`toDataURL`, `toBlob`, `convertToBlob`, `getImageData`; never the bitmap): only pixels with alpha > 0 that differ from a 4-neighbour when the lowest bit of each channel is ignored (edges, text, anti-aliasing; solid fills read back exactly); for row `y` an SFC32 seeded with `canvas_seed` XOR (width, `y`) in bytes 8..16 gives one `u32` per pixel from `x = 0`, and bit `c` replaces the lowest bit of channel `c` (R, G, B; alpha untouched). Changes are ±1, keyed by position, and idempotent (a `putImageData` round trip of opaque pixels reads back the same). | Real pixels, deterministic per persona. |
| WebGL | Stub WebGL 1/2 contexts behind the canvas `getContext` factory hook (`lb-webapi` `rt/webgl.rs`). Strings, limits, extensions and shader precision come from the persona (`hardware.gpu`, `webgl.*`); missing limits use ANGLE defaults for the persona's backend (D3D11, Metal or GL) and ANGLE's formulas for derived ones. The persona's extension list is the WebGL 2 list; WebGL 1 gets it with WebGL 2-only entries dropped and the WebGL 1-only ones (core in WebGL 2) added, in Chrome's order. Extension objects are instances of hidden interfaces with the IDL constants. No shaders run: `clear` fills, draws rasterize the first float attribute as clip-space x/y with a uniform/constant/hashed color. `readPixels`, `toDataURL` and `drawImage` read the same pixels with the Canvas 2D noise rule keyed by `webgl_noise_seed`. | No GPU, consistent answers. |
| Screenshots | `Page.captureScreenshot` (lb-page `cdp/screenshot.rs`): `lb_layout::LayoutEngine::display_list` gives boxes in paint order (stacking contexts, opacity groups, overflow clips, text lines in page coordinates); `lb_paint::page` paints them with tiny-skia at CSS px × clip scale × persona DPR: backgrounds (root/body propagate to the canvas), borders, text with the bundled glyph outlines, image and canvas pixels without readback noise, basic form controls, iframes. No background images, radius, shadows or transforms. | Playwright screenshots work; nothing is retained and nothing runs in any world (ADR 0014). |
| Fonts | `document.fonts`, `FontFace` (`local()` loads only for persona fonts; `url()` sources are not fetched), `queryLocalFonts` rejects without user activation. Detection by width measurement works because `lb-layout` has a table for every persona font: bundled real tables where we have them, `profiles/fonts/derived.json` estimates for the rest. | Persona fonts are exactly the detectable ones. |
| Frames | Each iframe is its own `Window` and realm in the page's engine, built synchronously when the element is connected (`lb_js::RealmOps`, `lb-webapi` `rt/frames.rs`, `lb-page` `frames.rs`). Origins are V8 security tokens; a cross-origin `contentWindow` is a proxy that throws Chrome's `SecurityError`; `postMessage` clones through JSON. The page event loop runs every window of the tree (ADR 0012). | `contentWindow` is never null, and frame realms expose the same surface as the top window. |
| Fingerprint report | `lb_page::fingerprint` (`fingerprint.js` + native checks): a collector in a throwaway isolated world, never announced to CDP clients, reads the live navigator, UA-CH, screen, time zone, WebGL, canvas, audio, fonts, permissions, media queries and automation tells, and compares each value with the persona (`ok`, `mismatch`, `unimplemented`, `info`). CLI `lightbrowser fingerprint-report`, CDP `LightBrowser.fingerprintReport`, CI gate `cargo test -p lb-page --test pages report`. | One answer to "does the live surface match the persona" without running a detection engine. |
| Audio | Web Audio graph rendered in 128-frame quanta (`rt/audio_dsp.rs`): Blink's band-limited oscillator tables and `DynamicsCompressorKernel`, gain, biquad, IIR, analyser (Blackman window, FFT), buffer/constant sources, delay, stereo panner, wave shaper, merger/splitter. The classic fingerprint scores 124.04346 (Chrome 153: 124.04348). Rendered and analyser float output get ±0.0001 seeded noise from `audio_seed` on non-zero samples only. Realtime contexts start `suspended`, rate and latency from persona `audio`. | Covers the standard audio fingerprint scripts. |
| Human input | `lb-behavior` generates event streams from first principles (Fitts' law with corrective submovements, minimum-jerk speed, curved paths with lateral noise, endpoint scatter by target size, 125 Hz reports coalesced to 60 Hz frames; log-normal key intervals and holds with per-key habits, rollover, Shift, typos fixed with Backspace; wheel flicks or trackpad momentum). Profile, seed and pointing device come from the persona's `behavior` block. `lb-page` `cdp/human.rs` replays a stream through the trusted Input pipeline for `LightBrowser.humanMove/humanClick/humanType/humanScroll`, on the real clock or the virtual clock; selectors are resolved from Rust, so no script runs in any world. | The bench's behaviour rules (T-010 `--input human`) see pointer paths, press holds, key rhythm and wheel patterns that look like a person. |
| Python SDK | `sdk/python/lightbrowser`: launches `lightbrowser serve --port 0` with a persona, connects stock Playwright over CDP and returns stock `Page` objects with `human_move/click/type/scroll` and `verify_fingerprint()` bound on; sync and async APIs; scenarios T-001 to T-007 against the bench. Loopback only. Needs Python Playwright >= 1.63: older versions put `__pwInitScripts` in the page's main world, which the browser cannot hide. | Replaces the GeckoPhantom SDK; control stays CDP only. |
| Bench runner | `bench/run.py`: 50-page generated corpus served on loopback; `fetch` and CDP crawls of LightBrowser and headless Chromium (and Lightpanda if on PATH); wall time, p50/p90 page time, peak RSS/PSS, CPU, idle RSS, startup, binary size, realm creation and disk writes, checked against the budgets above. Results in `bench/results/`. | Makes the budgets measurable and repeatable. |
| Runtime | Thread per core. Each thread runs a tokio `current_thread` runtime and owns its JS engine instances. | JS engines are single-threaded, so there's no locking. |
| Allocator | glibc malloc (mimalloc measured worse for idle and multi-page RSS, ADR 0013). Release build: fat LTO, `codegen-units=1`, `panic=abort`, stripped, `opt-level = "s"` for non-hot crates, linked with `--icf=safe` and packed relative relocations | Speed and size. |
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
- **CDP host.** The CDP server runs on the I/O thread; `lb-page::CdpHost`
  reaches each page on its worker, and a per-page agent there sends page
  events straight to the server. Values are described natively (no helper
  JS in any world). Trusted input goes through lb-webapi's input pipeline.
- **Pages.** Each page is pinned to one worker. Each frame is its own JS realm
  (its own `Window`, document, tasks and timers) in the page's engine; the
  page's event loop runs every window of the frame tree. An iframe's
  `about:blank` window is built while the inserting script runs (ADR 0012).
  CDP utility code (Playwright's injected script) runs in an isolated world, so
  nothing is ever installed in the page's main world.
- **Workers.** Not built yet. Dedicated workers will get their own engine
  instance on the same thread pool. Service workers are still open (M6).
- **Crash isolation.** `--process-per-context` is an option for later. The
  default is single-process for the RAM budget.

## Page lifecycle

1. `Page.navigate` → `lb-net` request. The persona's family picks the ClientHello, H2 settings and header order.
2. The response body streams into `html5ever`, which builds the DOM arena incrementally.
3. Scripts run in the realm (classic, `defer`, `async`, modules) through the HTML event loop: tasks, microtasks and timers.
4. Style and layout stay dirty. They are computed only when JS or CDP asks for geometry (`getBoundingClientRect`, `offsetWidth`, `elementFromPoint`, Playwright actionability, IntersectionObserver).
5. Load events fire. Lifecycle events go to CDP (`Page.lifecycleEvent`). After `load` of a top-level document the page requests its icon once per URL (`<link rel=icon>` or `/favicon.ico`), as Chrome does.
6. Navigating away runs the unload steps (`pagehide`, `visibilitychange` to hidden, `unload`; `sendBeacon` requests go out), then drops the document arena, its frames and the realm.

**Virtual time.** Timers and `requestAnimationFrame` can run on a virtual clock
to skip idle waiting: when no task is queued and nothing is in flight, the
clock jumps to the next timer. It is off when the persona needs realistic
timing.

## Fingerprint surface → module

| Surface | Module | Source of truth |
|---|---|---|
| JA3/JA4, ALPN, cert compression | lb-net | `profiles/tls/<family>_*.json` |
| H2 SETTINGS, WINDOW_UPDATE, PRIORITY, pseudo-header order | lb-net (h2 fork) | TLS profile `http2_*` |
| Header order and name case per destination, Accept, Accept-Language, Sec-CH-UA*, favicon request | lb-net (`header_profile`, `h1`), lb-page | `profiles/headers/<family>_*.json` plus persona `browser`, `locale`, `client_hints` |
| `navigator.*`, `userAgentData`, plugins, mimeTypes | lb-webapi + generated bindings | persona `browser`, `hardware` |
| Interface set, prototype/property order, `toString` | lb-webidl-gen | family IDL set |
| Engine quirks (errors, stacks, Math) | lb-js | real engine |
| `screen`, `devicePixelRatio`, window sizes, media queries | lb-webapi, lb-css | persona `screen` |
| Canvas 2D, image decode/encode | lb-paint, lb-webapi | real raster + `canvas_seed` |
| Iframes (`contentWindow`, `parent`, `postMessage`) | lb-webapi `rt/frames`, lb-page | one realm per frame, same persona |
| Unload steps, `sendBeacon`, `visibilityState` | lb-webapi, lb-page | Chrome's unload order |
| Live surface vs persona | lb-page `fingerprint` | persona (see "Fingerprint report" above) |
| WebGL params, extensions, precision, readPixels | lb-webapi | persona `webgl`, `hardware.gpu`, `webgl_noise_seed` |
| Audio | lb-webapi | DSP + `audio_seed` |
| Fonts (detection by width measurement, `measureText`, `FontFace` `local()`) | lb-layout, lb-webapi | persona `fonts` → bundled metric tables, plus approximate derived tables (`profiles/fonts/derived.json`) for persona fonts without one |
| Timezone, `Intl`, locale | lb-js (ICU data in the engine), set per realm by lb-webapi; ICU's default is process-wide (one zone per process) | persona `locale` |
| WebRTC (no local IP leak) | lb-webapi | one mDNS host candidate per persona and origin, no network |
| Permissions, battery, media devices, speech voices | lb-webapi | persona |
| `performance.now` resolution | lb-webapi (`coarse_time`) | Chrome 100 µs with Blink's jittered tick / Firefox 1 ms (RFP off: 20 µs) |
| Automation tells (`webdriver`, CDP leaks) | lb-cdp, lb-page | never exposed to the page world |
| Mouse/keyboard dynamics | lb-behavior, replayed by lb-page (`cdp/human.rs`) | persona `behavior` (preset, seed, pointing device) |

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
- **Page:** enable, navigate, reload, getFrameTree, getLayoutMetrics (real scroll and document size), createIsolatedWorld, addScriptToEvaluateOnNewDocument, lifecycleEvent, setLifecycleEventsEnabled, captureScreenshot (png/jpeg/webp, clip and scale, captureBeyondViewport; ADR 0014)
- **Runtime:** enable, evaluate, callFunctionOn, getProperties, releaseObject, executionContextCreated / Destroyed, consoleAPICalled
- **DOM:** getDocument, describeNode, resolveNode, getBoxModel, getContentQuads, scrollIntoViewIfNeeded, setFileInputFiles
- **Input:** dispatchMouseEvent, dispatchKeyEvent, insertText
- **Network / Fetch:** enable, setExtraHTTPHeaders, getCookies, setCookies, requestPaused, continueRequest, fulfillRequest
- **Emulation:** setDeviceMetricsOverride, setUserAgentOverride, setTimezoneOverride, setLocaleOverride. These are checked against the persona, and conflicts are rejected. setDefaultBackgroundColorOverride only affects screenshots.
- **Accessibility:** getFullAXTree, queryAXTree (for `getByRole`)
- **Custom `LightBrowser.*`:** setPersona, humanMove, humanClick, humanType, humanScroll (page sessions; generated by lb-behavior, replayed as trusted input on the real or virtual clock, selectors resolved from Rust), fingerprintReport (live surface vs persona, collected in a throwaway isolated world; also `lightbrowser fingerprint-report`)

## Reuse from stealth-browser

| From | To | How |
|---|---|---|
| `consistency/` (`PersonaProfile`, rules R01–R10) | `lb-persona` | Vendored in M2 (ahead of M4). Chrome rules R11–R16 added: UA-CH brands and GREASE, UA-CH platform, TLS profile ↔ UA major, client-hints presence, navigator constants. |
| `behavior/` (`human_click_events`, …) | `lb-behavior` | Vendored in M2 (ahead of M5), on rand 0.9. M5 rewrote the generators from first principles (Fitts' law, submovements, minimum jerk, frame-coalesced sampling, log-normal keystrokes with rollover and Shift, wheel flicks, trackpad momentum). `lb_behavior::cdp` turns events into `Input.dispatch*` params; lb-page replays them through the Input pipeline for `LightBrowser.human*`. |
| `profiles/schemas/persona_v1.schema.json` | `profiles/schemas/persona_v2.schema.json` | Adds `browser.family` and `client_hints`. v1 is read as Firefox. |
| `profiles/tls/firefox_128_*.json` | `profiles/tls/` | Same format. Chrome profiles are captured with `lb-fptest` and add optional fields (`grease`, `permute_extensions`, `alps_protocols`, `ech_grease`, `server_padding_request`, `trust_anchor_ids`, `http2_priority.headers`). |
| SFC32 in `geckophantom_config.h` | `lb-persona::sfc32` | **Done.** Bit-identical, with a test against C output. |
| SDK `human_*`, `verify_fingerprint` | `sdk/python/lightbrowser` | Thin layer over Playwright `connect_over_cdp`. |

ShadowChannel and the binary mmap config aren't needed: everything runs in one
process and is controlled over CDP.

## Memory strategy

- One arena per document. Node structs are kept small (≤ 64 bytes). Attribute and text storage use interned atoms.
- No layout or style data until something asks for it. `lb_layout::LayoutEngine::drop_layout` frees it; dropping it under memory pressure is not wired up yet.
- No paint layers or compositor. A screenshot builds a display list from the current layout, paints it and drops it (ADR 0014).
- JS heap cap per engine (512 MB). The engine gets a low-memory notification when a page's event loop goes idle and when a worker's queue drains after a page closed.
- Window realms come from the startup snapshot: about 1.4 MB of V8 heap each, with far less garbage than building them (one local page in `serve`: 67 MB RSS instead of 152 MB).
- HTTP cache in memory, bounded LRU, partitioned per context. Nothing goes to disk unless `--cache-dir` is given.
- Fonts: metric tables only (advance widths, ascent/descent) for persona fonts. Glyph outline packs (205 KB, compiled in) are parsed only when a canvas draws or measures text.
- Images: `<img>` loads keep the encoded bytes and their size; pixels are decoded on first `drawImage`.

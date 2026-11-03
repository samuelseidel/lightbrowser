# Roadmap

Each milestone ends with something we can run and measure.

| Milestone | Scope | Done when |
|---|---|---|
| **M0 Foundations** | Architecture docs, ADRs, workspace skeleton, CI, SFC32 port | `cargo build/test/clippy` green for both families ✅ |
| **M1 Network** | `lb-net`: BoringSSL ClientHello profiles, h1 + forked h2, cookies, decompression, proxy. `lightbrowser fetch <url>` | JA3/JA4 and Akamai H2 hash match real Chrome on our local TLS/H2 echo server ✅ (Chrome). Proxies moved to M2. |
| **M2 DOM + V8** | Arena DOM, html5ever sink, selectors, WebIDL codegen (Chromium set), V8 backend + snapshot, event loop, timers, fetch/XHR | `fetch --dump html` prints the DOM *after* scripts run. A WPT DOM subset passes. ✅ (Chrome; 2540/2788 WPT subtests at M2, 2552 now) |
| **M3 Layout + CDP core** | Lazy cascade, taffy layout, hit-testing, AX tree, CDP server with the domains in ARCHITECTURE.md | Playwright `connectOverCDP`: goto, evaluate, click, fill, getByRole, content all pass ✅ (Chrome; real V8 page host, 16 Playwright tests, see "M3 status") |
| **M4 Chrome fingerprints** | Persona v2, vendored consistency crate + Chrome rules, navigator/UA-CH, screen, canvas, WebGL, audio, fonts, Intl/timezone, WebRTC, permissions | Our detection engine (`tests/detect-bench`, ADR 0007) scores the Chrome personas as consistent: `run.js --scenario T-010 --cdp <ws> --expect 'fp<=30'`. Same seed gives the same fingerprint. ✅ (Chrome; fp 0 for both reference personas; surfaces and gaps in "M4 status: summary") |
| **M4b Firefox family** | SpiderMonkey backend (`mozjs`), Gecko IDL set, Firefox TLS/H2/header profiles | Existing Firefox 128 personas pass the same checks as on GeckoPhantom |
| **M5 Behavior + SDK** | Vendored behavior crate, `LightBrowser.human*`, Python SDK | `page.human_click` produces realistic event streams in the detection engine (`tests/detect-bench`, T-010 with `--expect 'beh<=30'`) |
| **M6 Performance** | `bench/` vs Lightpanda and headless Chrome, memory tuning, optional screenshots, service workers | Meets the budgets in ARCHITECTURE.md. Results published in `bench/results/`. |

## M1 status (2026-11-02)

Done:

- `lb-net`: TLS from profiles, h1 (hyper at the time; replaced by our own
  writer in M4, ADR 0011), h2 (fork in `vendor/h2`), cookies,
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
- Exact HTTP/1.1 header-name case (hyper title-cases `sec-ch-ua`) → done in
  M4 with our own h1 writer (ADR 0011).
- Connection reuse and a TLS session cache → M2. Today every fetch is a new
  connection, so there is no `pre_shared_key`.

## M2 status (2026-11-03)

The done-criteria are met for the Chrome build: `lightbrowser fetch --dump
html` prints the DOM after scripts ran, and a vendored WPT subset runs
against a loopback server with **2540 of 2788 subtests passing** (2552 after
M4 frames; 48 of 83
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
  out in Chrome's order (`chrome` was missing here and landed in M4). Unregistered members
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
- Pages: no workers, no cross-document history, form POST sent as GET.
  (Nested browsing contexts landed in M4, see "M4 status: frames".)
- Scripts: no dynamic `import()` or `import.meta`; script-inserted module
  scripts are skipped; dynamic `async = false` scripts are not ordered;
  scripts written by `document.write` run after the writer returns; no
  unhandled-rejection reporting.
- Network: sync XHR runs async; no CORS preflight; no `ReadableStream`
  bodies or FormData; M1 leftovers (proxies, connection reuse, TLS session
  cache). (Per-destination headers and exact h1 header-name case landed in
  M4, see "M4 status: request headers".)
- DOM: no shadow DOM, custom elements, Range, Selection, TreeWalker, CDATA
  or XML parsing; cross-document adopt copies the node.
- Realm creation is about 30 ms per navigation (budget < 5 ms with the M6
  snapshot).
- `lb-persona`: rule R04 rejects the upstream Apple Silicon profiles; the
  UA-CH GREASE logic is not checked against Chromium source.

## M3 status (2026-11-03)

Done for the Chrome build: stock Playwright `connectOverCDP` runs goto,
evaluate, click, fill, getByRole and content against our local pages
(`node --test tests/playwright/`: 14 tests at the end of M3, 16 now with
the frames spec).

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
  Decisions: `Runtime.addBinding` only reaches isolated worlds; a
  `setDeviceMetricsOverride` that contradicts the persona screen (including
  Playwright's default 1280x720) is rejected, so clients pass
  `viewport: null`. `MockHost` stays for lb-cdp's tests and the firefox
  build's `serve`.
- `lb-page::CdpHost` implements `lb_cdp::PageHost` on real V8 pages. The
  server thread reaches pages on their workers (async calls, blocking calls
  with a 30 s cap for sync methods, posted jobs for state changes); one
  agent per page reports events straight to the server. Navigation sends
  Chrome's event order; isolated worlds are recreated per document from
  world-scoped new-document scripts; object ids name their realm;
  values are described natively (`lb-js` `inspect`: V8-inspector
  descriptions, `returnByValue`, thrown values, promise state, own
  properties), so no helper JS runs in any world. `Network.*` events and
  `Fetch.*` interception wrap every request of a page (navigation,
  scripts, styles, fetch/XHR); cookies go through the context's jar;
  viewport overrides reach layout; AX tree from lb-layout. Browser
  contexts are lb-page `BrowserContext`s with their own jar and storage.
- `lb-webapi` input pipeline: trusted pointer/mouse/keyboard events in
  Chrome's order (boundary events, pointerdown/mousedown, focus,
  pointerup/mouseup, click, dblclick, auxclick, contextmenu, wheel with
  scrolling), screen coordinates from the persona window, focus and
  `change` on blur, text editing with selection in inputs and textareas
  (`beforeinput`/`input`, maxlength), basic contenteditable, Tab order,
  checkbox/radio groups, label activation, links, submit/reset buttons,
  implicit submission on Enter. UI event interfaces with init dictionaries,
  selection API, `labels`, `control`, `isContentEditable`,
  `checkVisibility`, `option.label`, `selectedOptions`, `XMLSerializer`.
  Inherited `[LegacyUnforgeable]` members (`isTrusted`) are now own
  properties of subclass instances, as in Chrome.
- CLI: `serve` and `fetch` load the full persona (reference persona by
  default, `--persona FILE`), validate it, and fail fast unless its
  network persona and TLS/H2 profile agree (family, profile id, browser
  major). `serve` gained `--profiles-dir`, `--insecure` and `--workers`.
- `tests/playwright/`: smoke, actions (click/dblclick order and
  coordinates, fill/type/press, check/radio/selectOption/labels,
  getByRole/Text/Label, locator reads, waitForSelector/Function, link
  navigation, form submit, a second document), network (route
  fulfill/abort, request/response events, cookies, two pages, two
  contexts), stealth (no Playwright/CDP globals in the main world,
  `navigator.webdriver` false, trusted events), raw CDP (DOM,
  Accessibility, Emulation, console, page errors, response bodies).
- Measured (debug build, loopback): `serve` start + `connectOverCDP` about
  35-90 ms; a cold `connectOverCDP` + newContext + newPage + goto about
  0.3-0.5 s.

Open (CDP and page host):

- No workers or popups; no screenshots (M6).
- `DOM.*` and `Input.*` on nodes inside child frames are refused (see
  "M4 status: frames and fingerprint report").
- Same-document navigations are not reported (`navigatedWithinDocument`).
- Console arguments arrive as one string argument.
- `Fetch` interception covers the request stage only. `Network.*` request
  headers are approximate (persona UA, `Accept-Language`, `sec-ch-ua`, the
  page's own headers, `Referer`); there are no `*ExtraInfo` events. The
  page host does not use `BrowserContext::fetch_traced` yet, so
  `redirectResponse` and the wire headers are not taken from the real
  trace.
- `Runtime.addBinding` registers a native function per world for the
  engine's lifetime.
- `lb-cdp`'s media-feature check in `Emulation.setEmulatedMedia` compares
  `forced-colors`, `prefers-contrast`, `prefers-reduced-transparency` and
  `prefers-reduced-data` with fixed values instead of the persona's
  `preferences`.
- Input: contenteditable editing appends at the end (no caret, Range or
  Selection); number inputs are not sanitized; form POST still navigates as
  GET.
- Layout has no real line boxes, shaping, bidi or table layout, and
  `clientWidth` ignores scrollbars (the persona's
  `preferences.scrollbar_width` is not used by `lb-layout` yet).
- The page layer does not fill `Window::load_info` (so `chrome.loadTimes`
  and `performance.navigation` guess), does not call
  `Engine::set_time_zone` when a worker switches personas, and does not
  call `load_visible_lazy_images` on scroll.

## M4 status: summary (2026-11-03)

M4 is done for the Chrome build (T-010 over CDP: fp 0 for both reference
personas, see "M4 status: detect-bench target and determinism"). The
sections below were written as each part landed, so the scores in them are
snapshots (98.8 at M2, then 98.2, 86.7, 75.7, 55.9, and 85.4/fp 0 at the
end); the last section has the final numbers. What exists now, all driven
by the context's `PersonaProfile` (never statics or env):

| Surface | Where | Section |
|---|---|---|
| Canvas 2D, `OffscreenCanvas`, `Path2D`, `ImageData`, seeded readback noise | `lb-paint`, `lb-webapi` `rt/canvas.rs` | canvas and images |
| `<img>`/`Image` loading, `decode()`, `srcset`, lazy loading | `lb-webapi` `rt/image.rs` | canvas and images |
| `plugins`, `mimeTypes`, `userAgentData`, `connection`, `permissions`, `Notification`, battery, media devices, voices, `performance.memory`/`timing`, `screen.orientation` | `lb-webapi` `rt/navigator.rs` | navigator and window |
| `window.chrome`, `RTCPeerConnection` (mDNS candidate only), Intl time zone and locale, 100 µs jittered `performance.now`, persona media queries | `rt/chrome.rs`, `rt/rtc.rs`, `lb-css` | navigator and window |
| WebGL 1/2 (strings, limits, extensions, precision, pixels), Web Audio (Blink DSP), `document.fonts`/`FontFace`, persona fonts by width | `rt/webgl*.rs`, `rt/audio*.rs`, `rt/fonts.rs`, `lb-layout` | WebGL, Web Audio and fonts |
| Iframes (same-origin, `srcdoc`, cross-origin proxy), `postMessage` across frames | `rt/frames.rs`, `lb-page` `frames.rs` | frames and fingerprint report |
| Request headers per destination, exact h1 name case, favicon request | `lb-net` `header_profile.rs`, `h1.rs`, `profiles/headers/` | request headers |
| Unload steps (`pagehide`, `visibilitychange`, `unload`), `sendBeacon` | `lb-webapi`, `lb-page` | detect-bench target and determinism |
| Fingerprint report (live surface vs persona) | `lb-page` `fingerprint.rs` + `.js`, CLI `fingerprint-report`, `LightBrowser.fingerprintReport` | frames and fingerprint report |

Known gaps after M4 (details in the sections below):

- Workers do not exist, so there is no navigator or `Worker` in workers
  (the `worker_mismatch` probe sees `Illegal constructor`). Firefox has no
  JS engine yet (M4b).
- WebGL runs no shaders and has no textures, framebuffers, blending or
  depth; `decodeAudioData` rejects; no `AudioWorklet`; no
  `ImageBitmap`/`createImageBitmap`; canvas filters are not applied.
- Fonts: glyph shapes exist only for Latin-1 in six outline packs
  (sans/serif/mono, regular/bold); tables for persona fonts without a real
  table are estimates (`profiles/fonts/derived.json`).
- Time zone and locale are process-wide ICU state: one zone per process.
- The two Chrome reference personas are hand-built from known Chrome
  values, not captured from a device (`profiles/README.md`).
- Header sets that the page adds beyond the captured ones are placed
  approximately; no preflight is issued; Firefox uses a generic overlay.
- Clicks into iframes, `window.frameName`, `sandbox` and structured clone
  for `postMessage` are missing.
- Behaviour (M5): the T-010 behaviour rules still fire (`beh` 85.4, target
  `<=30`).
- Budgets: the release binary is 62.1 MB (budget < 40 MB) and realm
  creation was about 30 ms when measured at M2 (budget < 5 ms with the M6
  snapshot).

## M4 status: request headers (2026-11-03)

Per-destination request headers (ADR 0011) replaced the single navigation
header set.

- Capture: Chrome for Testing 153.0.8010.12 (`--headless=new`) against a
  local logging server on loopback (HTTP/1.1 raw bytes; HTTPS h2 decoded).
  69 requests are kept in `profiles/headers/captures/chrome_153_linux.jsonl`:
  document, iframe, classic and module script, style, image, font,
  fetch/XHR (same-origin, cors, no-cors, POST/PUT/DELETE, sendBeacon), CORS
  preflight, favicon, a script-driven navigation with a cookie, and a 302.
- `profiles/headers/chrome_153.json`: per request kind, header slots in h2
  order, the h1 rules and the observed "renderer group" orders (Blink's
  hash-table order for subresources, stored as data because we could not
  reproduce the hash). Values (UA, `sec-ch-ua*`, language) come from the
  persona.
- `lb-net` `header_profile.rs` builds the list per hop (`RequestKind`,
  `RequestMeta`); it derives `Sec-Fetch-Site` along redirect chains,
  `Referer` (strict-origin-when-cross-origin) and `Origin`. `h1.rs` is now
  our own writer (exact name case: `sec-ch-ua*` lowercase, the rest
  Title-Case) with `httparse` for responses; hyper is no longer a
  dependency of `lb-net`. `Response` carries `method`, `request_headers`
  and `redirects`. Firefox keeps the navigation set plus a small per-kind
  overlay until a Firefox capture exists (M4b).
- `lb-page`: `BrowserContext::fetch_traced` and `request_favicon`. After
  `load` of a top-level document the page requests its icon once per URL
  (see the last M4 section).
- Chrome 153 differs from Chrome 131 (what the bench assumed): it sends
  `accept-language` right after `sec-ch-ua-platform`. The bench's
  `doc_header_order` was corrected (see the last M4 section).
- Verified: `cargo test -p lb-net` (74 unit tests, plus `--test
  fingerprint`, 2 tests: TLS/H2 unchanged), and T-010 no longer fires
  `header_order`.

Open: no test replays the capture file through `HeaderProfile::build` yet;
the capture server script is not in the repo (it lived in an agent's
scratch directory, so re-capturing is manual, see `tests/README.md`);
header sets the page adds beyond the captured ones are placed right after
`user-agent`; the referer slot after a redirect hop is a guess; form-POST
navigations, `Range` and revalidation headers are not captured; the layer
that builds fetch/XHR requests does not add the default `Content-Type` for
a body; `Destination` has no `Font`/beacon variants and `NetRequest` has no
user-activation flag (so `Sec-Fetch-User` for link clicks, and font/beacon
priorities, are only reachable through `lb-net`); nothing issues CORS
preflights.

## M4 status: canvas and images (2026-11-03)

Canvas 2D (M4 task 3, canvas part) and image loading have landed.

- `lb-paint`: tiny-skia raster per canvas (allocated on first draw), paths
  (arcs, `arcTo`, ellipses, `roundRect`, SVG path data for `Path2D`),
  fill/stroke with both fill rules, line styles and dashes, the transform
  stack, `globalAlpha`, all `globalCompositeOperation` modes (unbounded
  ones through a layer), linear/radial/conic gradients, patterns, shadows
  (box-blur approximation), clip masks, `drawImage`, image data, text with
  `measureText` from the persona's font tables and bundled outline packs,
  PNG/JPEG/WebP (lossless only) encoders, SFC32 readback noise
  (ARCHITECTURE.md, Canvas 2D row). Decoders: PNG, JPEG, GIF, WebP,
  ICO/BMP; SVG is sized only.
- `lb-webapi`: `HTMLCanvasElement` (`getContext`, `toDataURL`, `toBlob`,
  resize resets), `CanvasRenderingContext2D`, `OffscreenCanvas`,
  `CanvasGradient`, `CanvasPattern`, `Path2D`, `ImageData`,
  `TextMetrics`, a basic `DOMMatrix`. `getContext('webgl' | 'webgl2')`
  calls a factory set with `Runtime::set_canvas_context_factory` (null
  without one). `<img>`/`Image` load through the page's network
  (`Sec-Fetch-Dest: image`) or data: URLs, with `load`/`error`,
  `complete`, natural sizes, `decode()`, `srcset` by DPR, basic
  `loading=lazy`, canvas taint, and natural sizes in layout.
- Verified: `cargo test -p lb-paint`, `cargo test -p lb-page --test
  canvas` (same seed gives the same `toDataURL` hash across runs, another
  seed a different one; solid fills exact; image loads on loopback), WPT
  subset unchanged (2540/2788). Detect-bench (`fetch /shop` with the
  Windows reference persona): `images_blocked` no longer fires, the
  `canvas` probe passes (stable, solid fill exact); score 98.2 (was 98.8).

Open: no filters, no text shaping or kerning, no color emoji or CJK
glyphs (they advance but draw nothing), shadows are approximate,
`ImageBitmap`/`createImageBitmap`, `transferControlToOffscreen`, video
frames and `<picture>` are missing, blob: image URLs fail, lazy images
are re-checked only through `load_visible_lazy_images` (not yet called on
scroll).

## M4 status: navigator and window surface (2026-11-03)

M4 tasks 2 and 4 (navigator/window surface, no tells) have landed for the
Chrome build. Every value comes from the context's `PersonaProfile`;
shapes, identities, descriptors and error texts were compared with the
local Chromium 153 on a loopback page (same expressions in both).

- `lb-webapi` `rt/navigator.rs`: `plugins`/`mimeTypes` (Chrome's 5 PDF
  plugins and 2 MIME types; `plugins[i]` stable, `plugins[i][j]` new per
  access, `enabledPlugin` is `plugins[0]`, indices enumerable and
  read-only, names listed but not enumerable), `userAgentData` (new object
  per access, frozen `brands`, `getHighEntropyValues` with Chrome's key
  order, `toJSON`), `connection` (Chrome's rounding and per-host 0.9–1.1
  noise, salt from the persona), `permissions.query` (Chrome 153's table:
  states, internal names such as `video_capture`, disabled features and
  error texts; persona overrides), `Notification` (`permission` from the
  persona; `requestPermission` stays pending like an unanswered prompt),
  `getBattery` (same promise; persona or Chrome's desktop answer),
  `mediaDevices` (one empty entry per kind until permission; hashed ids
  and labels after), `speechSynthesis` (persona voices), `screen.orientation`,
  `performance.memory` (`MemoryInfo` is `[LegacyNoInterfaceObject]`, so a
  per-realm prototype with native getters; values are the page's heap
  growth on top of a blank Chrome page), `performance.timing` /
  `navigation`, `getGamepads`.
- `rt/rtc.rs`: `RTCPeerConnection` without networking: the offer SDP has
  Chrome's layout; ICE gathering yields one mDNS host candidate
  (`<uuid>.local`, stable per persona and origin) and ends. No IP leak.
- `rt/chrome.rs`: `window.chrome` = `{loadTimes, csi, app}` with Chrome's
  descriptors and position (right after `WebAssembly`), anonymous
  constructible native functions, no `runtime`. Built per main-world realm
  before page scripts; nothing remains in the page.
- Intl/`Date`: realm creation sets ICU's default time zone and locale from
  the persona (`lb_js::Engine::set_time_zone` / `set_default_locale`).
  ICU's defaults are process-wide: `Date` keeps the zone its isolate cached
  until that isolate is reset, but `Intl` reads the process default on
  every new formatter, and the locale sticks per isolate after its first
  `Intl` use. Personas with different zones in one process can therefore
  see each other's zone in `Intl`; run one zone per process (`serve
  --persona`) until the page layer switches zones per task.
- `performance.now()` and event time stamps: 100 µs ticks with Blink's
  jittered tick point (`murmur3(tick ^ secret) % resolution`).
- `lb-css`: `prefers-color-scheme`, `prefers-reduced-motion`,
  `prefers-contrast`, `prefers-reduced-transparency`, `forced-colors`,
  `color-gamut` (a P3 screen also matches `srgb`) and `dynamic-range`
  follow the persona's new `preferences`; `inverted-colors`,
  `prefers-reduced-data` and `video-dynamic-range` never match (Chrome 153
  does not support them).
- `lb-js`: interfaces can declare an indexed length (indices enumerate with
  Blink's read-only descriptor; assignment and `delete` are ignored) and
  `[LegacyUnenumerableNamedProperties]`.
- `lb-persona`: optional `preferences`, media device counts, battery
  times and `hardware.js_heap_size_limit`; reference Chrome personas gained
  speech voices, devices and preferences. Rules R01–R16 still pass.
- Verified: `cargo test -p lb-page --test navigator` (both reference
  personas), WPT subset unchanged (2540/2788), `node --test
  tests/playwright/` (14/14). Detect-bench T-010 over CDP (`serve`,
  Windows reference persona): score 86.7 (fp 80.9). None of
  `ua_ch_mismatch`, `plugins`, `chrome_object`, `timezone_utc`,
  `connection`, `battery`, `notification_permission`, `languages`,
  `hardware`, `native_functions`, `property_order`, `has_focus`,
  `perf_now_resolution`, `locale_mismatch` or the screen rules fire.
  Still firing: `iframe_mismatch`, `webgl_software`, `header_order`,
  `no_interaction`, `favicon_missing`.

Open: `chrome.app.isInstalled` accepts assignment (Chrome ignores it);
`MediaDevices.getUserMedia` never resolves with a stream; the macOS voice
list is approximate; no `navigatedWithinDocument`-style updates of
`loadTimes` for same-document navigations; `connectionInfo` is guessed
from the scheme unless the page layer fills `Window::load_info`; workers
have no navigator yet.

## M4 status: WebGL, Web Audio and fonts (2026-11-03)

M4 task 3 (non-canvas-2D parts) has landed for the Chrome build. Shapes
and default values were compared with the local Chromium 153 on a local
page (headless shell, SwiftShader; GPU limits come from the persona).

- `lb-webapi` `rt/webgl.rs`: WebGL 1 and 2 through the canvas context
  factory hook (element and `OffscreenCanvas`); no persona, no WebGL.
  `getParameter` for strings, state and limits (persona `webgl.parameters`
  by constant name, then ANGLE defaults per backend; WebGL 2-only pnames
  are `INVALID_ENUM` in WebGL 1), `WEBGL_debug_renderer_info` unmasked
  strings only once enabled, `getSupportedExtensions` (WebGL 1 list
  derived from the persona's WebGL 2 list, in Chrome's order),
  `getExtension` with hidden extension interfaces (`rt/webgl_ext.rs`,
  constants and methods from the IDL; `WEBGL_lose_context` works),
  `getShaderPrecisionFormat`, `getContextAttributes`, shaders and programs
  with active attributes and uniforms parsed from GLSL, buffers, VAOs,
  bindings, `getError`; every other operation is an argument-checked
  no-op. Pixels: `clear` and a small rasterizer for draws; `readPixels`,
  `toDataURL` and `drawImage` share the canvas noise rule keyed by
  `webgl_noise_seed`.
- `rt/audio.rs` + `rt/audio_dsp.rs`: `AudioContext` (persona sample rate
  and `baseLatency`, starts `suspended`, runs on the page clock after
  `resume()`), `OfflineAudioContext` (renders synchronously; promise and
  `complete` from tasks), the common nodes and `AudioParam` automation,
  `AudioBuffer` (one persistent `Float32Array` per channel), analyser data.
  Oscillator wavetables and the dynamics compressor follow Blink: the
  classic fingerprint (triangle 10 kHz into the compressor) sums to
  124.04346 before noise (Chrome 153: 124.04348; `reduction` -20.5381 vs
  -20.5383). Seeded ±0.0001 noise on non-zero rendered samples.
- `rt/fonts.rs`: `document.fonts` (`check`, `load`, `ready`, set
  operations), `FontFace` (`local()` loads when the persona has the font),
  `queryLocalFonts` rejects without user activation, as in Chrome.
- `lb-layout`: approximate derived tables (`profiles/fonts/derived.json`)
  for every persona font without a table, so width probes see exactly the
  persona's fonts, in layout and in canvas `measureText`.
- `lb-js`: `Arg::Float32Array` / `Int32Array` / `Uint32Array`,
  `CallCtx::{arg_view_kind, write_arg_bytes, write_handle_bytes,
  new_promise}`.
- Verified: `cargo test -p lb-page --test fingerprint_surfaces` (surfaces
  from the Windows reference persona; two runs with one seed give the same
  canvas, WebGL `readPixels`/`toDataURL` and audio output, another seed
  changes all of them), `cargo test -p lb-webapi -p lb-layout`, WPT subset
  unchanged (2540/2788). Detect-bench T-010 over CDP (`serve`, Windows
  reference persona, `viewport: null`): score 75.7, fingerprint and
  consistency categories 0. `webgl_software`, `webgl_os_mismatch`,
  `canvas`, `audio` and `fonts` (all 8 Windows and 6 common fonts, no
  foreign ones) do not fire. Still firing: `iframe_mismatch`,
  `header_order`, `no_interaction`, `favicon_missing`.

Open: no shader execution, textures, render-to-texture, blending or depth;
`preserveDrawingBuffer: false` is not cleared after compositing; extension
prototypes keep a `constructor` property (Chrome's have none);
`decodeAudioData` rejects (no codecs); no `audioprocess` events or
`AudioWorklet`; panner and convolver pass audio through; `FontFace` `url()`
sources are not fetched; derived font tables are estimates.

## M4 status: frames and fingerprint report (2026-11-03)

Nested browsing contexts (ADR 0012) and M4 task 5 (fingerprint report)
have landed for the Chrome build.

- `lb-js` `RealmOps`: realm building (new realm, evaluate, call, global,
  properties, wrap, native functions, ICU defaults, heap stats, security
  token) for `Engine` and, on a nested handle scope, for `CallCtx`, so a
  host function can build a realm. Realm setup in `lb-webapi` takes
  `&mut dyn RealmOps`. `PropCtx::holder_realm`, `CallCtx::entered_realm`.
- `lb-webapi` `rt::frames`: an iframe's `about:blank` window and realm are
  created when it is connected (parser or script; `load` fires during
  insertion when there is no `src`/`srcdoc`), `src`, `srcdoc` and
  `location` changes navigate the frame through `FrameHooks`, removal
  closes the frame tree. `contentWindow`, `contentDocument`, `parent`,
  `top`, `frameElement`, `length`, `frames[i]`/`window[i]` (indexed, as
  own properties), `postMessage` across frames (JSON-cloned data,
  `origin`, `source`; `targetOrigin` checked), `MessageEvent.source`.
  Realms carry their origin as V8 security token; cross-origin windows are
  a native `Proxy` that throws Chrome's `SecurityError`. `about:blank` and
  `about:srcdoc` take the creator's origin and secure-context state.
  Isolated worlds now get the per-realm navigator state too.
- `lb-page` `frames`: frame navigation with the top-level loader (network,
  `srcdoc`, origin-keyed storage), the parent's `load` waits for its
  frames, the `<iframe>` gets `load` afterwards (also on network errors).
  The event loop runs tasks, timers and animation frames of every window
  of the tree; virtual time advances all clocks.
- CDP: `Page.frameAttached`/`frameNavigated`/`frameDetached`, lifecycle
  events and loader ids per frame, a main-world context and the
  new-document worlds per frame document, `Page.getFrameTree` children,
  `Page.createIsolatedWorld` by frame id, `frameId` on `<iframe>` nodes.
  Playwright frame locators and `page.frame()` work.
- Fingerprint report (`lb_page::fingerprint`): a collector runs in a
  throwaway isolated world (never announced to CDP clients) and probes
  navigator, UA-CH (incl. high entropy), connection, screen, window sizes,
  time zone and locale, WebGL 2 strings, extensions, parameters and shader
  precision, canvas and audio stability, persona fonts by width
  measurement (and two made-up ones), permissions, `Notification`,
  battery, media devices, voices, media queries and `performance.now`
  resolution; the main world's global is read natively for automation
  tells and `window.chrome`. Each value is checked against the persona
  (`ok`, `mismatch`, `unimplemented` when a surface is missing or throws,
  `info` for hashes). `LightBrowser.fingerprintReport` (page session, or
  `targetId` on a browser session) and `lightbrowser fingerprint-report
  [--persona FILE] [--url URL]` (default `about:blank`, JSON on stdout,
  exit 1 on any mismatch). CI gate: `cargo test -p lb-page --test pages
  report` runs it for every `profiles/reference/chrome` persona on
  `about:blank` and on a loopback page.
- Verified: `cargo test -p lb-page --test pages` (frames, report gate),
  `lb-page` `cdp_host`, `navigator`, `fingerprint_surfaces`, `canvas`,
  `cargo test -p lb-js -p lb-cdp`, `lb-webapi` lib and `install` tests,
  WPT subset 2552/2788 (was 2540), `node --test tests/playwright/` (16,
  including frame locators and the report over CDP). Report for both
  reference personas: 116 checks (112 ok, 4 info), 0 mismatches, 0
  unimplemented (120 checks after the last M4 section added pixel, audio
  and font hashes).
  Detect-bench T-010 over CDP (`serve`, Windows reference persona, the
  `run.js` flow with `viewport: null`): score 55.9 (fp 37, beh 30);
  `iframe_mismatch` and `extra_globals` pass. Still firing:
  `header_order`, `no_interaction`, `favicon_missing`.

Open: `DOM.*` and `Input.*` on nodes inside frames (clicks into frames)
are refused; no named frame access (`window.frameName`), `sandbox`, or
browsing contexts for `<frame>`/`<object>`/`<embed>`; nodes passed between
frames' DOM methods are not supported; frame network events are
attributed to the main frame; workers still missing (`worker_mismatch`
probe reports `Illegal constructor`).

## M4 status: detect-bench target and determinism (2026-11-03)

M4 is done for the Chrome build. The ADR 0007 target, `node
tests/detect-bench/run.js --scenario T-010 --cdp <ws> --expect 'fp<=30'`,
passes against `lightbrowser serve` for every `profiles/reference/chrome`
persona: fp 0. Setup: bench `server.py` on 127.0.0.1, results in a temp
dir; a fresh `serve --persona <file>` per run (debug build).

| Persona | Before (score / fp / beh) | Fired before | After (score / fp / beh) | Fired after |
|---|---|---|---|---|
| `chrome153_win10_rtx3060_1080p` | `run.js` error; same flow with `viewport: null`: 55.9 / 37 / 30 | `header_order`, `favicon_missing`, `no_interaction` | 85.4 / **0** / 85.4 | behaviour only (below) |
| `chrome153_macos15_apple_m2_retina` | as Windows | as Windows | 85.4 / **0** / 85.4 | behaviour only (below) |

Behaviour rules that fire after (M5, target `beh<=30`): `beh_typing_no_keys`
(45, Playwright `fill` inserts text without keys), `beh_click_center` (40),
`beh_click_no_path` (35), `beh_wheel_pattern` (20), `page_dwell_short`
(15). Stock headless Chromium on the same T-001 flow fires these five plus
`beh_click_hold` and `beh_ttfi`.
Before this change `no_interaction` fired instead: the input trace never
reached the bench, because navigating away fired no `pagehide` and
`navigator.sendBeacon` was a stub. Recording the trace raised `beh` from 30
to 85.4; it is the honest M5 baseline.

What changed:

- `tests/detect-bench/run.js`: T-010 (`--cdp`) creates its context with
  `viewport: null`, as ADR 0009 requires. Without it `newPage` failed on
  `Emulation.setDeviceMetricsOverride` (Playwright's 1280x720 contradicts
  the persona screen). No rule, weight or probe changed.
- `tests/detect-bench/scoring.py`, `EXPECTED["chrome"]["doc_header_order"]`:
  a bench bug. It had Chrome 131's slot for `Accept-Language` (after
  `Accept-Encoding`). Chrome for Testing 153.0.8010.12 (`chromium
  --headless=new --lang=en-US` on loopback) sends it right after
  `sec-ch-ua-platform` and fired `header_order` the same way LightBrowser
  did (score 100 → 99.9 after the fix, still bot). T-001/T-003 run on the
  headless shell, which sends no `Accept-Language`, so they are unaffected:
  100/100 before and after (`>=90`, `>=70&fp>=70` pass). The order matches
  `profiles/headers/chrome_153.json` (ADR 0011). `test_scoring.py`'s clean
  session uses the same slot.
- `lb-page` `nav.rs`: after `load` of a top-level document, request the
  icon (last `<link rel=icon>` href, else `/favicon.ico`) through
  `BrowserContext::request_favicon` (image header set, `Referer`, cookies,
  once per URL). Not reported to CDP, as in Chrome. Fixes `favicon_missing`.
- `lb-webapi`: `Window::unload` runs Chrome's unload steps (`pagehide` once
  `load` ran, `visibilitychange` to `hidden`, `unload`); `lb-page` calls it
  before a document is dropped (navigation and page close).
  `document.visibilityState`/`hidden` follow it. `navigator.sendBeacon`:
  keepalive POST (string, Blob, ArrayBuffer, URLSearchParams bodies, 64 KiB
  quota, `no-cors` unless the content type is not CORS-safelisted) that is
  not tied to the window, so beacons from `pagehide` go out.
- Fingerprint report: new `webgl.pixelHash` (readPixels and `toDataURL` of a
  cleared buffer with a triangle) and `webgl.pixelStable` (ok/mismatch),
  `audio.hash` (all 5000 samples) and `fonts.hash` (widths of every persona
  font). Now 120 checks: 113 ok, 7 info, 0 mismatch, 0 unimplemented for
  both reference personas (`lightbrowser fingerprint-report --persona
  <file>`, exit 0).

Determinism. Same persona twice, fresh process each time; seed B is the
Windows persona with the three `fingerprint_seeds` changed:

| Persona | Report canvas | Report WebGL pixels | Report audio | Report fonts | Bench canvas / audio / fonts |
|---|---|---|---|---|---|
| Windows, run 1 = run 2 | `679f8f61` | `9f3bd535:cbd1b6b6` | `9342352a` (sum 124.042805) | `ccb5f19c` | `7f19ce5ef67b` / `1526b15c318b2` / `dc7ad69aaad32` |
| macOS, run 1 = run 2 | `5ed11079` | `3f972d54:e8d22d74` | `a841f4bc` (124.044803) | `e8e9e748` | `158c4655d21c01` / `7516bf91b3735` / `1b88c48973818c` |
| Windows seed B, run 1 = run 2 | `00dde923` | `58d8df64:8cd45917` | `fde8281b` (124.043528) | `ccb5f19c` | `8f6744fa181b2` / `33afc3caa49fc` / `dc7ad69aaad32` |

The whole `fingerprint-report` JSON is byte-identical between the two runs
of each persona. Between the Windows persona and seed B only the canvas,
WebGL pixel and audio values differ; fonts and WebGL parameters do not
depend on the seeds. The bench's WebGL parameter set hashes the same for
both runs of each persona.

Verified: `cargo test -p lb-webapi -p lb-page` (new
`pages::unload_events_beacon_and_favicon`; WPT subset 2552/2788
unchanged), `cargo test -p lb-net` (incl. `--test fingerprint`), `cargo
test -p lightbrowser -p lb-cdp`, `cargo clippy -p lb-net -p lb-webapi
-p lb-page -p lightbrowser --all-targets -- -D warnings`, firefox clippy
and tests for `lightbrowser`, `lb-js`, `lb-webapi`, `lb-page`, `cargo fmt
--all --check`, `node --test tests/playwright/` (16), and the bench's
`python3 -m unittest` (18).

Open: icons are requested for top-level documents only, without `sizes`
or `type` ranking; unload steps run for the top-level document only, not
for its child frames; `sendBeacon` with `FormData` sends the string form
(no `FormData` bodies yet); no `beforeunload`.

Whole-workspace check after M4 (both families green, no code changes
needed): `cargo fmt --all --check`, `cargo build --workspace`, `cargo
clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
(400 passed, 3 ignored), firefox clippy `--all-targets` for `lightbrowser`,
`lb-js`, `lb-webapi`, `lb-page`, `lb-cdp` and firefox tests (14 passed),
`cargo test -p lb-net --test fingerprint` (2), WPT subset 2552/2788, `node
--test tests/playwright/` (16), bench `python3 -m unittest` (18).
Release build (chrome): stripped binary 62.1 MB (was 58.9 MB at M2; budget
< 40 MB, M6). T-010 on the release build, default persona
(`chrome153_win10_rtx3060_1080p`): score 89.8, **fp 0**, beh 89.8, pass.
Release also fires `beh_click_hold` (30) on top of the five debug-build
behaviour rules (not investigated; likely the shorter press-to-release time
of the faster build).

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

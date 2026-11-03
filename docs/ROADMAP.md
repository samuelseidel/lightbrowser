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
| **M5 Behavior + SDK** | Vendored behavior crate, `LightBrowser.human*`, Python SDK | `page.human_click` produces realistic event streams in the detection engine (`tests/detect-bench`, T-010 with `--expect 'beh<=30'`). Chrome: `LightBrowser.human*` and the Python SDK (`sdk/python`) done. Both reference personas pass on their own seeds (beh 0); seed sweeps pass 20/20 (Windows) and 19/20 (macOS). See "M5 status" |
| **M7 OS personas** | Consistent desktop Windows 10/11, macOS (Intel, Apple Silicon) and Linux (Ubuntu/Fedora, X11/Wayland) Chrome personas: persona model and rules, rendering, web APIs, input and client hints per OS. Mobile and Firefox out of scope | Every reference persona: `fingerprint-report` 0 mismatch, T-010 `fp<=30` and `--input human` `beh<=30` with every OS rule of the bench at full weight; adversarial personas fire their rule. WP-A (Linux ground truth, OS bench rules), WP-B (persona model, rules R17-R26, catalog, generator, 8 reference and 11 adversarial personas, ADR 0015), WP-C (scrollbars, fonts, aliases, emoji per OS) WP-D (OS-gated APIs, codecs, WebGPU, keyboard, WebGL backends, audio, window position, wheel and shortcuts per OS) and WP-E (`Accept-CH` / `Critical-CH` client hints) done: all 8 reference personas fp 0 and beh 0. See "M7 status" |
| **M6 Performance** | `bench/` vs Lightpanda and headless Chrome, memory tuning, optional screenshots, service workers | Meets the budgets in ARCHITECTURE.md. Results published in `bench/results/`. Snapshot, memory tuning, binary size and screenshots (ADR 0014) done for Chrome; binary 39.63 MB, the < 40 MB budget is met (ADR 0013); `bench/` runner done; Lightpanda compared (user-approved nightly, loopback only): 4 of 6 checks pass, `fetch` CPU and CDP peak RSS still lose; service workers open. See "M6 status". |

**Where we stand (Chrome build, 2026-11-03).**

| Milestone | Chrome | Open |
|---|---|---|
| M0, M1, M2, M3, M4 | Done | M1 proxies and connection reuse; M2/M3/M4 leftovers in their status sections |
| M4b Firefox | Not started: no SpiderMonkey backend; the firefox build parses pages but runs no scripts and `serve` uses `MockHost` | All of it, plus the Firefox TLS gaps in ADR 0003 |
| M5 Behavior + SDK | Done: T-010 `--input human` passes for both reference personas (own seeds beh 0; seeds 1-20: Windows 20/20, macOS 19/20) | The SDK's own T-004 still types one short word; POST forms are submitted as GET (see "M5 status") |
| M7 OS personas | WP-A done: Linux ground truth (`tools/os_truth`, `tests/fixtures/os/`), 16 OS rules in the bench, `serve --unchecked-persona`. WP-B done: OS axis and accessors in `lb-persona`, rules R17-R26, `profiles/catalog/` and generator, 8 Chrome reference personas (Windows 10/11, macOS Intel/Apple Silicon, Ubuntu X11, Fedora Wayland), 11 adversarial ones. WP-C done: scrollbars, default and generic fonts, Linux aliases, emoji and `font:` keywords per persona. WP-D done: Web Share and Web Bluetooth only on Windows/macOS (OS overlay, deleted per realm), media type answers and `mediaCapabilities`, `navigator.keyboard`, `navigator.gpu`, WebGL backends, audio defaults, `screenX/Y`, wheel and select-all per OS. WP-E done: high-entropy `Sec-CH-UA-*` after `Accept-CH` per origin, `Critical-CH` restart, same-origin delegation, header order from a new capture. T-010 fp 0 for every reference persona with every OS rule | Windows/macOS values not captured; no `Permissions-Policy` delegation or device/preference hints; TCP/IP is the host's. See "M7 status: WP-E" |
| M6 Performance | Snapshot, memory, binary size (39.63 MB, < 40 MB met), `bench/` runner, screenshots, Lightpanda comparison (wall time wins in both modes; 4 of 6 checks pass) done | `fetch` CPU and CDP peak RSS vs Lightpanda, service workers |

Budgets (ARCHITECTURE.md), with the evidence for each:

| Budget | Result | Evidence |
|---|---|---|
| Idle RSS < 15 MB | Met: 10.2 MB (9.9-10.3 MB in earlier runs) | `bench/run.py`, "M6 status: Lightpanda comparison" |
| Binary < 40 MB (< 56 MB in between) | Met: 39.84 MB (39,841,656 bytes) after M7 WP-E, 0.16 MB under; 39.63 MB at the end of M6 | "M6 status: binary size, third pass", "M6 status: Lightpanda comparison" |
| Realm creation < 5 ms | Met: 3.17 ms first, 2.45 ms next after M7 WP-D (Linux realms, OS gate deletion included); 3.13 / 2.47 ms at the end of M6 (2.39 / 2.06 ms in an earlier session; 2.6 / 2.3 ms for the old code in the same session) | `realm_bench`; `bench/run.py` |
| No disk writes by default | Met: 0 files, `write_bytes` 0 | `bench/run.py` |
| Wall time, peak RSS and CPU <= Lightpanda | Partly met (Lightpanda 1.1.0-nightly.11582): wall time `fetch` 2.63 vs 3.50 s and CDP 1.32 vs 5.15 s, peak RSS `fetch` 43.8 vs 44.9 MB, CPU CDP 0.99 vs 1.06 s pass; **CPU `fetch` 2.51 vs 1.37 s and peak RSS CDP 42.2 vs 31.4 MB fail** | "M6 status: Lightpanda comparison" |

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

- No workers or popups. Screenshots landed in M6 (see "M6 status: screenshots").
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
- Behaviour (M5): the T-010 behaviour rules still fire with Playwright
  input (`beh` 85.4, target `<=30`); with `LightBrowser.human*` input see
  "M5 status: human input".
- Budgets: the release binary was 62.1 MB (budget then < 40 MB) and realm
  creation about 30 ms. Both are addressed in "M6 status: snapshot, memory
  and binary size".

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

## M5 status: human input (2026-11-03)

PLAN M5 items 1-2 are done for the Chrome build: `LightBrowser.humanMove`,
`humanClick`, `humanType` and `humanScroll` generate input with
`lb-behavior` and replay it through the trusted input pipeline. The Python
SDK (item 3) is done in `sdk/python` (launcher, `Persona`, sync and async
APIs, `human_*`, `verify_fingerprint`, scenarios T-001..T-007). Its bench
scenarios need Python Playwright >= 1.63: older versions define
`__pwInitScripts` in the page's main world through
`Page.addScriptToEvaluateOnNewDocument`, which LightBrowser cannot hide.

- CDP contract (page sessions): `humanMove {x, y}`; `humanClick {selector?
  | x?, y?, button?, clickCount?} -> {x, y}` (the clicked point);
  `humanType {text, selector?}`; `humanScroll {deltaY?, deltaX?, x?, y?,
  selector?}`. Each resolves after its last event. Errors are `-32000` with
  Chrome-like text (`No node found for selector: #x`, `Node is not
  visible`). lb-cdp checks params and calls `PageHost::human_input`
  (`types::HumanCommand`); `MockHost` answers "not supported".
- `lb-page` `cdp/human.rs`: selectors are resolved in the main document
  from Rust (no script in any world); a target outside the viewport is
  first brought into view with wheel input; the click point is chosen
  inside the element's box by the profile. Events are dispatched at their
  offsets with `tokio::time::sleep_until` on the page's worker, which keeps
  serving other pages; with virtual time the clock is advanced to each
  event instead. The pointer position persists per page (also after
  `Input.dispatchMouseEvent`), starting at a seeded point in the upper
  middle of the viewport. Calls on one page run one at a time.
- Profile and seed come from the persona (`lb_page::cdp::behavior_for`):
  `behavior.profile` preset; new optional `behavior.seed` (hex, default: a
  hash of `profile_id` and the fingerprint seeds) and
  `behavior.pointing_device` (`mouse` or `trackpad`; default trackpad on
  macOS, a notched wheel elsewhere), in `lb-persona` and the v2 schema.
  Call `n` on a page uses `mix_seed(seed, n)`: same persona and calls, same
  stream (tested under virtual time).
- `lb-behavior` generators rewritten from first principles (sources in each
  module's header): Fitts' law durations; a primary submovement plus
  corrective ones (Meyer et al. 1988), overshoot or undershoot; minimum-jerk
  speed with a longer deceleration phase; curved paths with lateral motor
  noise (Harris & Wolpert 1998; MacKenzie et al. 2001); endpoint scatter
  sigma = W/4.133 of the target (ISO 9241-9); 125 Hz device reports
  coalesced to 60 Hz frames, whole pixels; button holds around 95 ms.
  Keys: log-normal down-to-down intervals and holds (Dhakal et al. 2018),
  per-key hold habits (Killourhy & Maxion 2009), bigrams, word-start
  pauses, rollover, Shift around shifted characters, typos noticed 0-2
  keys later and fixed with Backspace. Wheel: fixed 100 px detents in
  flicks with re-grip pauses; trackpad: swipes with momentum and axis
  rails. Think times are log-normal.
- `tests/detect-bench/run.js --input human`: the T-010 flow through
  `LightBrowser.human*` on the page's CDP session, plus a reading pause per
  page from its text length. Default stays `--input playwright`. No rule,
  weight or probe changed.

T-010 `--input human`, `beh<=30` (fresh `serve --persona <file>` per
run, release build, bench on 127.0.0.1):

- **First version (six keys).** The flow typed `jacket` into `#q`.
  Windows passed (beh 0); macOS failed at beh 35 (`beh_key_dwell`). In a
  sweep over `behavior.seed` 1-8 plus the own seed, Windows passed 8/9 and
  macOS 6/9; every failure was `beh_key_rhythm` and/or `beh_key_dwell`.
  These rules judge a MAD/median spread (< 0.1 and < 0.08) over one page's
  keys. With 5-6 samples that estimate is noisy:
  `crates/lb-behavior/tests/key_spread.rs` runs the collector's logic on
  the model's own output, and gets a 26 % chance of firing on six keys
  (rhythm 16 %, dwell 13 %).
- **The model matches human data.** Pooled over sessions, the casual
  preset has a hold mean of 113 ms (CV 0.32) and an IKI mean of 231 ms
  (CV 0.57). Dhakal et al. 2018 (136M keystrokes) report a mean hold of
  116 ms and a mean IKI of 239 ms, both wide and right-skewed. The
  keyboard model was not changed.
- **Flow change (`run.js`, no rule touched).** T-010 now types what a
  shopper types, the same text for both inputs: a three-word query
  (`waterproof rain jacket`, 22 keys) on `/shop`, then "Sign in" and a
  made-up email and password on `/login` (40 keys and Shift). T-001
  shares this flow; T-003 is unchanged. Chance of the key rules firing:
  0.4 % for the query page and 0 % for the login page (simulation, 4000
  sessions).
- **Mouse model fix (`lb-behavior` `mouse.rs`).** The longer flow showed
  `beh_mouse_velocity` (25) firing on 11 of 42 runs, always on the search
  page, whose one movement is a long flick to "Sign in". The lateral
  noise was a first-order Ornstein-Uhlenbeck process (correlation time
  ~60 ms). Its paths are rough at every scale: at 60 Hz the pointer
  zigzagged +-10-20 px per frame, with 10-35 direction changes per
  movement. MacKenzie, Kauppinen & Silfverberg 2001 report a few per
  trial. The collector cut such movements at the > 25 degree turns into
  fragments that look flat. Now the noise is smooth: normal knots every
  110-190 ms with the same SD (`path_noise` x D + 0.4 px), joined by
  Catmull-Rom curves. `tests/mouse_rules.rs` applies the collector's
  stroke, velocity and straightness checks to single clicks. It now
  measures 1-5 direction changes per movement (asserted <= 8) and the
  velocity rule at 0-3 % (was 4-57 %, asserted < 5 %). The cost: a short
  move with little bow is now nearly straight, so a page of only such
  moves can fire `beh_mouse_linear` (simulated three-click page: 0-1 %;
  single ~430 px move: 4-11 %; it was 0 % when every path zigzagged).
  The bow (N(0, `curvature` x D)) was not changed.

Seed sweeps (`behavior.seed` 1-20 in a persona copy, `run.js --seed` the
same number; plus the persona's own seed with `--seed 1`):

| Build | Persona | Own seed | Seeds 1-20 | Rules fired |
|---|---|---|---|---|
| New flow, old mouse noise | Windows | beh 0, pass | 20/20 | `beh_mouse_velocity` (25) in 5 |
| New flow, old mouse noise | macOS | beh 0, pass | 20/20 | `beh_mouse_velocity` (25) in 6 |
| New flow, new mouse noise | Windows | beh 0, pass | **20/20** | none |
| New flow, new mouse noise | macOS | beh 0, pass | **19/20** | seed 10: `beh_mouse_linear` (35) on `/login` (three moves of 170-250 px, each almost straight) |

No key rule fired in any of the 84 runs. fp was 0 in every run. Controls on
the new build: T-010 `--input playwright` scores beh 92.3 for both personas
(`beh_typing_no_keys`, `beh_click_center`, `beh_click_no_path`,
`beh_click_hold`, `beh_ttfi`, `beh_wheel_pattern`, `page_dwell_short`);
stock Chromium T-001 and T-003 score 100. No persona seed was picked, and
neither model was tuned to a threshold. LightBrowser still submits the
`method="post"` login form as a GET (`lb-webapi` `submit_form`; the
credentials end up in the URL). The bench does not score this.

Verified: `cargo test -p lb-behavior` (65), `-p lb-cdp`, `-p lb-persona`,
`-p lb-page` (new `tests/human.rs`: trusted events, Chrome order,
monotonic timestamps, real-clock duration, typed value, Shift, scrolling
into view, errors; determinism and clock advance under virtual time),
clippy `-D warnings` (`--no-deps`) and fmt for the touched crates,
firefox clippy and tests for `lb-page` and `lightbrowser`, bench
`run.js` T-001/T-003 (100/100). After the flow change and mouse fix:
`cargo test -p lb-behavior` (65 unit tests, plus `key_spread` and
`mouse_rules`), clippy `--all-targets -D warnings` and fmt for
lb-behavior, `cargo test -p lb-page --test human` (2), and
`python3 -m unittest discover -s tests/detect-bench` (18). The sweeps
above ran on a release build (`target/release/lightbrowser`, 06:40) with
`run.js --input human --expect 'beh<=30' --json`.

Open: no drag, hover dwell or idle micro-movements between calls; clicks
into child frames; touch input; the pointer path ignores scroll
containers other than the window; the trackpad's mouse movement uses the
mouse model. The path bow is zero-mean normal, so some short moves are
almost straight, and a page with only such moves can fire
`beh_mouse_linear` (macOS seed 10). Human data on the distribution of path
curvature would settle whether that is too often. The SDK's T-004
(`sdk/python/lightbrowser/scenarios.py`) still types one six-letter word,
so its key rules fire by chance as described above. Form POSTs are sent as
GETs (lb-webapi).

## M6 status: snapshot, memory and binary size (2026-11-03)

PLAN M6 items 1 and 3 (ADR 0013). Release build, chrome, 4 cores.

Startup snapshot (`lb_js::Engine::with_snapshot`, `lb_webapi::snapshot`):
- Every interface template plus one window context with the fixup run,
  made in memory when the first engine starts (110 ms, 2.57 MB), no disk
  writes, no persona data. Restoring replays the binding definitions
  against per-interface digests; any difference falls back to building.
- `cargo run --release -p lb-webapi --example realm_bench` (full `rt`
  registry, medians of 7 engines x 20 realms):

| | engine | registry + define | first realm | next realm |
|---|---|---|---|---|
| before | 0.8 ms | 9.1 ms | 28.3 ms | 34.7 ms |
| snapshot | 2.7 ms | 5.2 ms | 2.4 ms | 2.0 ms |

  V8 heap per window realm is 1.4 MB either way. Realm creation meets the
  < 5 ms budget.
- `lb-webapi` test `snapshot`: full surface (keys, descriptors, functions,
  prototype links) of snapshot and built realms is identical, secure and
  insecure. All lb-js, lb-webapi and lb-page tests run on snapshot engines;
  WPT 2552/2788, Playwright 16/16, T-010 fp 0 for both reference personas.
- One snapshot per process, made before any other engine; afterwards every
  isolate comes from it, because V8 shares one read-only heap between
  isolates (mixing snapshots or making one next to live isolates crashed in
  V8; lb-js now refuses both and falls back to building). Realm ids moved
  from rusty_v8 context slots into engine state: the slots' per-context
  weak handle double-freed at teardown for deserialized contexts.

Memory (`lightbrowser serve`, idle RSS, local `/shop` page of the bench):

| | no page | 1 blank page | 1 local page | 10 local pages | 9 of them closed |
|---|---|---|---|---|---|
| no snapshot, 4 workers | 10.1 MB | 36.3 MB | 152.4 MB | 458.3 MB | 98.6 MB |
| snapshot, 4 workers | 10.3 MB | 45.3 MB | 66.9 MB | 196.1 MB | 95.9 MB |
| snapshot, 1 worker | 10.3 MB | 44.4 MB | 65.8 MB | 129.2 MB | 76.0 MB |

- Idle RSS with no page is 10.1 MB (budget < 15 MB). An engine with one
  blank page is ~44 MB: ~22 MB anonymous (V8 isolate, two window realms per
  page with Playwright's utility world), the rest mapped binary pages (ICU
  data, code).
- A worker now runs one V8 low-memory GC when its queue drains after a page
  closed; before, closing 9 of 10 pages freed nothing (195 MB stayed).
- The one-time peak while the snapshot is made is 95 MB, below the
  non-snapshot first-page peak (102 MB).
- mimalloc was measured and rejected: 20.5 MB idle, 244 MB for 10 pages.
- Open: dropping layout under memory pressure (lb-layout has
  `drop_layout`, `rt::Window` does not expose it); per-page caps below the
  512 MB per-engine heap cap.

Binary size (stripped):

| build | size |
|---|---|
| before (M4) | 62.1 MB |
| + `--icf=safe` (rust-lld) | 57.3 MB |
| + `opt-level = "s"` for non-hot crates | 56.8 MB |
| + `-z pack-relative-relocs` (DT_RELR, glibc >= 2.36) | 54.9 MB |
| V8 floor (`lb-js` example `v8_floor`, same flags) | 42.0 MB |

- The floor is one isolate, one context, one script: V8 code ~26 MB,
  builtins 2.2 MB, ICU code 1.7 MB and ICU data 10.9 MB. < 40 MB is not
  reachable with the prebuilt `v8` crate as linked then; ADR 0013 sets
  < 56 MB for the Chrome binary. The ICU data and unwind-table steps listed
  here were done later the same day (next section: 44.37 MB). Still not
  done: a from-source V8, `-no-pie` (~1 MB, loses ASLR).
- What we add over the floor (12.9 MB): lb-webapi 1.9 MB of code plus the
  generated table, BoringSSL 1 MB, tokio/hyper/h2/hickory, taffy, image
  codecs, font tables and glyph outlines.

## M6 status: binary size, second pass (2026-11-03)

Goal: the original < 40 MB. Reached 44.37 MB (54.96 before), stripped,
`cargo build --release -p lightbrowser`. ADR 0013, revision.

| build | size |
|---|---|
| before | 54.96 MB |
| + ICU data zstd-compressed instead of linked raw (`lb-js` `icu_data`) | 47.96 MB |
| + no `.eh_frame` (linker script in `bin/lightbrowser/build.rs`) | 44.37 MB |

- ICU: `lb-js` defines the archive's `icudt78_dat` symbol as an empty
  ICU package, so the archive's 10.88 MB copy is not linked; the same
  bytes ship as `crates/lb-js/icu/icudt78.dat.zst` (3.83 MB, `icu_pack`
  example; now `icudt78.dat.zch` in 128 KiB frames, see ADR 0013 Revision 3) and are installed with `udata_setCommonData` before V8 starts.
  `tests/v8.rs` `icu_data` checks them against the archive byte for byte.
  A 47.8 KB Intl probe (all constructors' supported locales, 40 locales x
  18 formatters and collators, segmentation, calendars, time zones,
  normalization) printed identical output before and after.
- Cost, at the first engine only (idle RSS unchanged): ~17 ms and a
  10.9 MB resident heap copy; peak RSS of a one-script process 15.9 →
  26.7 MB, of the Intl probe 43.3 → 45.7 MB.
- Unwind tables are gone: the release binary aborts on panic and links
  no C++ code that throws. Crash backtraces lose their frames;
  `LB_KEEP_UNWIND=1` keeps the tables.
- Unchanged: `realm_bench` 2.41 ms first realm, 2.08 ms next; snapshot
  113 ms, 2.57 MB. `lightbrowser fingerprint-report`: 113 ok, 0 mismatch
  for both Chrome reference personas.

Still over 40 MB by 4.4 MB at that point (the third pass below gets under). From the release link map: V8 C++ 26.6 MB,
ICU data 3.8 MB, Rust std 1.3 MB, our crates and their Rust deps 7.0 MB of
code plus 2.0 MB of constant data, BoringSSL 1.7 MB, zstd 0.4 MB, the rest
1.4 MB. The measured further steps (brotli for ICU -0.33 MB but +37 ms,
root crate at `opt-level = "s"` -0.77 MB, `--icf=all` -0.22 MB, binding
table as string offsets about -0.5 MB) add up to about -1.8 MB; the rest
would mean dropping Rust features or building V8 from source. See
ADR 0013.

## M6 status: binary size, third pass (2026-11-03)

Goal: the original < 40 MB without losing anything a page, a fingerprint
or a CLI user can observe. Reached **39,174,008 bytes (39.17 MB)**,
stripped, `cargo build --release -p lightbrowser` (44,367,736 before).
ADR 0013, second revision. Each step was measured with a fresh release
link; the link maps came from `LB_LINK_MAP=<path>` (`bin/lightbrowser/build.rs`).

| cut | size after | saved | cost |
|---|---|---|---|
| before | 44,367,736 | | |
| V8: Turbolev, flag-gated Turboshaft phases, Wasm revectorizer and fuzzing module generator not linked (`lb-js` `unused_v8`) | 41,189,240 | 3,178,496 | none: off by default, never turned on |
| `lb_net::fetch` returns a boxed future: the network stack (TLS, h1/h2, DNS) is compiled once, not once per crate that awaits it | 40,824,696 | 364,544 | one allocation per fetch |
| root crate `opt-level = "s"` (the fat-LTO pipeline for all Rust code) | 40,144,760 | 679,936 | see below |
| `rt::Runtime::win` not inlined into the 1,084 binding closures | 40,083,320 | 61,440 | one call per binding call |
| font metric tables (114 KB JSON) and glyph outline packs (210 KB) embedded zstd-compressed by `build.rs` (7 KB and 103 KB) | 39,874,424 | 208,896 | unpacked per use: < 0.2 ms |
| V8: LiveEdit, debug coverage, three compiler verifiers, Turboshaft JSON graph tracing not linked | 39,718,776 | 155,648 | none (as above) |
| V8: experimental (linear-time) regexp engine not linked (~38 KB), and `lb-webapi` at `opt-level = "s"` before LTO (~507 KB) | 39,174,008 | 544,768 | define +0.7 ms |

- `unused_v8`: each stub carries the mangled name of the only strong
  global symbols of one archive object, so the linker never extracts that
  object (same mechanism as the ICU data). If anything else needed the
  object, the link would fail with a duplicate symbol; nothing can be
  dropped silently. Every stubbed path needs a V8 flag that is off by
  default (Turbolev, `--turboshaft-assert-types`, typed and loop
  optimisation, random rescheduling, `--turbo-profiling` in mksnapshot,
  `--wasm-revectorize`, `--turbo-verify*`, `--verify-simplified-lowering`,
  `--trace-turbo`, the experimental regexp engine) or the natives syntax
  or the inspector (LiveEdit, coverage), none of which LightBrowser uses:
  V8 gets no flags from the CLI or pages, `init_v8` panics on the flags
  that would reach a stub, and V8 freezes its flags at initialisation. A
  stub aborts with the path's name. Tests: `tests/v8.rs` `unused_v8`
  (every stub names a symbol of the archive; hot JS, Maglev/Turbofan with
  OSR, a JS-to-Wasm call in a hot loop, regexps), lb-js unit test for the
  flag check.
- Not observable: fingerprint-report 113 ok / 0 mismatch for both Chrome
  reference personas; `lb-net` fingerprint test 2/2; WPT 2552/2788;
  Playwright 20/20 (release binary); T-010 fp 0 for both personas, and
  beh 0 for both with `--input human`; ICU data unchanged; all
  subcommands, content encodings and codecs unchanged.
- Cost (`bench/results/2026-11-03-f2d6632.json`, release, 5 x 50 pages,
  against `2026-11-03-ebe5f36.json`): `fetch` page median 140.0 → 141.6 ms,
  p90 146.8 → 151.6 ms, CPU 10.0 → 10.4 s; `cdp` page median 98.5 →
  99.9 ms, p90 103.9 → 103.8 ms, CPU 1.33 → 1.35 s; idle RSS 10.0 MB;
  `realm_bench` first/next realm 2.39 / 2.06 ms (unchanged), engine
  2.78 ms, registry + define 5.35 → 6.09 ms. Every page's median is below
  Chromium's median page (153 ms `fetch`, 104 ms `cdp` vs 194 ms); single
  `fetch` samples reached 198 ms once in 250 (noise: a different page in
  each of three runs).
- Measured, not applied: root `opt-level = "z"` (another -225 KB, slower
  everywhere); `--icf=all` (-0.22 MB earlier; breaks function-pointer identity,
  not shown safe for V8); brotli for the ICU data (+37 ms at start); hickory replaced by
  `getaddrinfo` (different behaviour: hosts file, nsswitch, address
  order); the binding table as string offsets (about -150 KB, large
  refactor). Next candidates if the binary grows again: more V8 printers
  and tracers (Maglev graph printer, Turbofan graph visualizer, about
  130 KB, each needs its call sites checked), `__cxa_demangle` (115 KB,
  changes crash output).

## M6 status: screenshots (2026-11-03)

PLAN M6 item 4, first half (ADR 0014). Chrome build.

- `Page.captureScreenshot` paints the main document from its layout:
  `lb_layout::LayoutEngine::display_list` (paint order per stacking
  context, `opacity` groups, `overflow` clips, text lines with their
  resolved fonts, page coordinates) and `lb_paint::page::paint_page`
  (tiny-skia). Formats png, jpeg (`quality`, default 80) and lossless
  webp; `clip` with `scale` in page coordinates; output is CSS px × scale ×
  persona DPR (2× for the macOS reference persona);
  `captureBeyondViewport: false` paints only the visible viewport;
  `fromSurface` and `optimizeForSpeed` are accepted and ignored.
- Painted: background colors (root, else body, fills the canvas),
  borders (solid, double, dashed, dotted; 3D styles shaded), text with
  the bundled outlines, `<img>` and `<canvas>` pixels (2D and WebGL,
  never noised), text inputs (value, placeholder, password bullets),
  textarea first line, select, button-like inputs, check boxes and radio
  buttons, iframes (from their own layout), `visibility`.
- `Page.getLayoutMetrics` reports the real scroll offset and document
  size (`PageHost::page_metrics`); `Emulation.setDefaultBackgroundColorOverride`
  is accepted and only changes the screenshot base color (`omitBackground`).
- Tests: `tests/playwright/screenshot.spec.mjs` (viewport, element,
  scrolled element, full page, jpeg quality and clip, omitBackground, raw
  CDP clip scale, captureBeyondViewport, un-noised canvas pixels, 0-width
  error), lb-paint `page` and lb-layout `display` unit tests. Playwright
  20/20, WPT 2552/2788 unchanged.
- Open: CSS background images and gradients (no `background-image`
  longhand; loading them changes network traffic), border radius,
  shadows, outlines, text decoration, transforms, filters, scrollbars,
  `object-fit`, video frames, native control theming. Pixels follow our
  approximate inline layout, so they do not match Chrome exactly.

## M6 status: bench runner (2026-11-03)

PLAN M6 item 2. `bench/` (see `bench/README.md`) crawls a generated corpus of
50 pages (6 kinds, 1.7 MB, scripts, stylesheets and images; each page sets
`data-bench-rendered` after its scripts run and the runner checks it) from a
loopback-only server. `python3 bench/run.py` runs 5 repetitions of
`fetch:lightbrowser`, `cdp:lightbrowser` and `cdp:chromium` (Playwright
`connect_over_cdp`), writes `bench/results/<date>-<sha>.json` and a summary
table, and exits 1 if a budget fails or a browser did not render every page.
`fetch:lightpanda` and `cdp:lightpanda` run only if a `lightpanda` binary is
on PATH (first measured on 2026-11-03, see "M6 status: Lightpanda
comparison"). `python3 -m unittest discover -s bench` runs the tooling
tests (12, no browser).

First committed run: `bench/results/2026-11-03-a060281.json`, release binary
sha256 `e801e8cd2f8ea13d` (54.88 MB, built before the screenshot work; the
tree was dirty), 5 runs x 50 pages, 4-core Xeon, 16 GB, Chromium 153.0.8010.12.
Median (min-max):

| | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium |
|---|---|---|---|
| crawl total | 18.4 s (17.7-19.0) | 14.2 s (13.8-15.0) | 10.3 s (9.8-10.9) |
| page median | 165.5 ms | 100.0 ms | 205.0 ms |
| page p90 | 1196 ms | 1084 ms | 221 ms |
| peak RSS | 95.8 MB | 95.3 MB | 2876 MB (PSS 690 MB) |
| CPU | 10.0 s | 1.41 s | 23.7 s |
| idle RSS | n/a | 9.95 MB | 1084 MB (PSS 377 MB) |
| start to listening | n/a | 4.1 ms | 103 ms |
| first new page / later pages | n/a | 151 ms / 5.7 ms | 89 ms / 71 ms |
| pages verified | 50/50 | 50/50 | 50/50 |

- Fetch times include process start. CDP peak RSS is the sum of per-process
  high-water marks, so it counts Chromium's shared pages once per process and
  overstates it; PSS is the fairer figure. LightBrowser's peak includes the
  one-time snapshot peak at the first page; after the crawl its RSS was 39.6 MB.
- Binary: 54.88 MB against 293 MB for Chromium's `chrome` executable.
  Realm creation: engine 2.83 ms, define 5.31 ms, first realm 2.41 ms, next
  realm 2.08 ms. Disk: no files created, `write_bytes` 0.
- Budgets met: idle RSS, binary size, realm creation, disk writes. The six
  checks against Lightpanda are skipped.
- Weak spot (fixed later the same day, see findings 1-3 and the refreshed
  run below): LightBrowser won on median page time, CPU and memory but lost
  on total time and p90 because 8 of the 50 pages (the product grids) took
  about 1 s instead of about 100 ms.

Findings from the run (1-3 fixed on 2026-11-03, see below):
1. **Slow image-grid pages (fixed; not layout).** On the 8 grid pages
   DOMContentLoaded took about 1 s. Layout was never the cost: the body's
   `getBoundingClientRect()` takes about 1 ms (one layout, one placement).
   The page waited for its 19 images: LightBrowser opened one connection
   per request with no limit, the corpus server's listen backlog (5)
   overflowed (`TcpExt.ListenOverflows`), and the dropped SYNs came back
   after the 1 s retransmit timeout. `lb_page::BrowserContext` now allows
   at most 6 simultaneous connections per origin, like Chrome's socket
   pool; more requests queue (`lb-page` test `connections`). p01 went from
   1.4 s to 0.18 s with `fetch`.
2. **Table APIs (done).** `HTMLTableElement` `rows`, `tBodies`, `caption`,
   `tHead`, `tFoot` (getters and setters), `create*`/`delete*`,
   `insertRow`/`deleteRow`; `HTMLTableSectionElement` `rows`,
   `insertRow`/`deleteRow`; `HTMLTableRowElement` `cells`, `rowIndex`,
   `sectionRowIndex`, `insertCell`/`deleteCell`; the obsolete string
   attributes (`align`, `vAlign`, `ch`, `chOff`, `border`, ...) reflect,
   except `bgColor`. Collections are live, cached HTMLCollections
   (`lb-webapi` `rt/table.rs`, `lb-page` test `tables`). No local WPT copy
   of `html/semantics/tabular-data`, so none was added.
3. **`fetch` no longer makes the snapshot** (ADR 0013): median page 167 →
   128 ms, CPU 9.9 → 8.8 s for 50 pages, peak RSS 97 → 112 MB (paired
   runs of the same tree).
4. **The first CDP page takes 151 ms** (the snapshot is made at the first
   engine); later pages take 5.7 ms.

After the fixes (`python3 bench/run.py --runs 2 --no-write`, release
binary of the shared tree at 06:09, 65.8 MB because another change was in
flight; medians of the two runs):

| | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium |
|---|---|---|---|
| crawl total | 7.1 s | 5.1 s | 10.2 s |
| page median | 140 ms | 100 ms | 203 ms |
| page p90 | 150 ms | 104 ms | 221 ms |
| peak RSS | 122 MB | 110 MB | 2851 MB |
| CPU | 10.2 s | 1.37 s | 23.6 s |
| pages verified | 50/50 | 50/50 | 50/50 |

Still open: stylesheets are fetched only after DOMContentLoaded, so a
deferred script that measures layout sees unstyled boxes (Chrome blocks
scripts on pending stylesheets); no HTTP keep-alive reuse yet (Chrome
reuses its 6 connections).

Refreshed run (`python3 bench/run.py`, 5 runs x 50 pages, release binary
44.37 MB, sha256 `6aa2ebe44ac8b5a2`, tree at `ebe5f36` plus the
uncommitted fixes above): `bench/results/2026-11-03-ebe5f36.json`. The
`a060281` file stays as history; `bench/results/README.md` points at the
new run. Median (min-max):

| | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium |
|---|---|---|---|
| crawl total | 7.01 s (6.92-7.12) | 4.94 s (4.80-5.01) | 9.72 s (9.62-10.12) |
| page median | 140.0 ms | 98.5 ms | 193.1 ms |
| page p90 | 146.8 ms | 103.9 ms | 206.0 ms |
| slowest page (any run) | 157 ms | 140 ms | 239 ms |
| peak RSS | 115.2 MB | 105.9 MB (PSS 55.3) | 2820 MB (PSS 687) |
| CPU | 10.0 s | 1.33 s | 23.4 s |
| idle RSS | n/a | 10.3 MB (PSS 7.5) | 1083 MB (PSS 377) |
| start to listening / CDP connect | n/a | 4.0 / 9.5 ms | 107 / 120 ms |
| first new page / later pages | n/a | 166 ms / 5.7 ms | 80 ms / 63 ms |
| pages verified | 50/50 | 50/50 | 50/50 |

Every LightBrowser page, in every run, is faster than Chromium's median
page. Budgets: idle RSS, binary size, realm creation (2.39 / 2.07 ms) and
disk writes pass; the six Lightpanda checks are skipped (no binary;
downloading one is not allowed here). Peak RSS is about 10-20 MB above the
`a060281` run: `fetch` no longer uses the snapshot, and the CDP peak was
not isolated.

After the third binary-size pass (39.17 MB, sha256 `0e2df17d93846b2b`):
`bench/results/2026-11-03-f2d6632.json` (the latest is now
`2026-11-03-ca3f1b4.json`, see "M6 status: Lightpanda comparison"). `fetch` page median 141.6 ms (p90 151.6),
`cdp` 99.9 ms (p90 103.8), Chromium 193.9 ms; idle RSS 10.0 MB; all
budgets pass, binary now checked against < 40 MB.

Not covered: the crawl is sequential, with no HTTPS, iframes or workers.
Re-run `python3 bench/run.py` after a rebuild to refresh the numbers; the
result file is named after HEAD, so identify a run by its binary hash.

## M6 status: Lightpanda comparison (2026-11-03)

PLAN M6 item 2, the budget "crawl benchmark (wall time, peak RSS, CPU)
<= Lightpanda". Measured against **Lightpanda 1.1.0-nightly.11582+592b76832**
(binary sha256
`71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), a
nightly the user approved for this comparison. It ran only against the
loopback corpus server (`bench/serve_corpus.py`, 127.0.0.1) with
`LIGHTPANDA_DISABLE_TELEMETRY=true` set for every process (`bench/run.py`
sets it), and is not in the repository.

Runner: `lightpanda fetch --dump html URL` and `lightpanda serve --host
127.0.0.1 --port P` with the same Playwright code as the other CDP targets;
the WebSocket URL now comes from `/json/version` (loopback-checked), and
the Lightpanda binary's sha256 is recorded in the result. Lightpanda runs
with its defaults, which fetch no images and no stylesheets
(`--load-resources`); LightBrowser and Chromium fetch both, as Chrome
does. Lightpanda rendered all 50 pages (markers verified) in both modes.

Recorded run: `bench/results/2026-11-03-ca3f1b4.json`, 5 runs x 50 pages,
release binary 39,628,664 bytes (sha256 `573c54b604e5669d`, tree
`ca3f1b4` plus uncommitted work), 4-core Xeon, 16 GB. Median (min-max):

| | fetch: LightBrowser | fetch: Lightpanda | CDP: LightBrowser | CDP: Lightpanda | CDP: Chromium |
|---|---|---|---|---|---|
| crawl wall time | **2.63 s** (2.57-2.78) | 3.50 s (3.44-3.61) | **1.32 s** (1.22-1.40) | 5.15 s (5.02-5.24) | 11.4 s |
| page median / p90 | 50.0 / 61.3 ms | 69.0 / 74.3 ms | 24.0 / 36.0 ms | 103.0 / 107.7 ms | 228 / 248 ms |
| peak RSS | **43.8 MB** (40.5-46.7) | 44.9 MB (38.5-46.7) | 42.2 MB (42.0-42.6) | **31.4 MB** (31.3-31.4) | 2841 MB (PSS 687) |
| CPU (user+sys) | 2.51 s (2.45-2.64) | **1.37 s** (1.32-1.44) | **0.99 s** (0.96-1.04) | 1.06 s (0.93-1.12) | 28.8 s |
| idle RSS (serve) | | | 10.2 MB | 12.4 MB | 1082 MB |
| first new page / later | | | 121 / 7.1 ms | 4.4 / 4.6 ms | 100 / 80 ms |
| pages verified | 50/50 | 50/50 | 50/50 | 50/50 | 50/50 |

Budget checks: **4 of 6 pass**. Wall time wins in both modes (fetch 25 %
faster, CDP 3.9x); peak RSS in `fetch` (43.8 vs 44.9 MB) and CPU in CDP
(0.99 vs 1.06 s) pass, by margins inside the run-to-run spread. Two fail:

- **`fetch` CPU: 2.51 vs 1.37 s** (about 50 vs 27 ms per process). A
  `fetch` process builds its window realm without a snapshot (making one
  costs more than it saves for one page): binding definition ~6 ms,
  realm ~7 ms, plus V8/isolate start-up and about 13 ms of page-fault
  (sys) time for ~40 MB touched. Lightpanda deserializes a build-time
  snapshot. A build-time snapshot embedded in the binary would close most
  of the gap, but it is about 0.4 MB compressed and the binary has 0.37 MB
  left under the 40 MB budget (ADR 0013, "Alternatives").
- **CDP peak RSS: 42.2 vs 31.4 MB.** LightBrowser holds every interface
  object eagerly in each window realm (1.6 MB of V8 heap per realm, two
  realms per page now that Playwright's utility world is made on use),
  the snapshot blob (2.6 MB) and ~23 MB of mapped binary pages. In a
  `fetch` process Lightpanda's anonymous memory at exit was 2.5 MB against
  16 MB for LightBrowser (whose V8 heap alone was 9.8 MB). Letting V8 schedule its own full
  GCs instead of one after each closed page brought CPU to 0.87-0.94 s
  but peak RSS to 94-124 MB, so it was not kept.

What changed, with the measured effect (same machine; before = the first
1-run comparison):

| Change | Where | Effect |
|---|---|---|
| `TCP_NODELAY` on CDP sockets (an event then a reply were held back by Nagle until the client's delayed ACK, 40 ms) | `lb-cdp` server | CDP page 100 → 31 ms; `page.close()` 44 → 1 ms |
| Window global's ~1,200 members defined after context creation (interface objects directly, the rest via 32-member chunk templates) instead of one big template (O(n²) map transitions in V8) | `lb-js` (ADR 0013, revision 3) | instructions per `fetch` 1.88G → 0.44G; built realm 35/43 → 7/6 ms; snapshot creation 130 → 92 ms; `fetch` page 157 → 68 ms, peak 113 → 48 MB |
| ICU data in 128 KiB zstd frames, paged in on first touch (a page reads ~120 KB of 10.9 MB) | `lb-js` `icu_data` (ADR 0013, revision 3) | `fetch` page (p05) 64 → 45 ms, CPU (user+sys) 66 → 49 ms, peak 48 → 39 MB; CDP peak 57 → 49 MB; binary +0.43 MB |
| Isolated worlds reported at once, realm made on first use (Playwright's utility world is never used in a plain crawl) | `lb-page` CDP agent | CDP CPU 1.47 → 0.97 s, peak 48 → 43 MB |
| GC after a closed page: one memory-reducing full GC instead of V8's low-memory notification (repeated full GCs, compilation cache dropped) | `lb-page` worker | CDP CPU 1.65 → 1.47 s, same peak |
| GC of a quiet page after 2 s of quiet instead of at once (like V8's memory reducer) | `lb-page` event loop | no GC while a page is being worked with |
| V8 code cache for classic scripts >= 16 KB compiled a second time (Playwright's ~100 KB utility script, per document) | `lb-js` `run_script` | CDP CPU about -3 % (noisy), no RSS cost (cache made before the run) |
| Font tables and UA stylesheet parsed once per worker and OS, not per document | `lb-layout` `LayoutBase`, `lb-css`, `lb-webapi` | ~5.7M instructions per CDP page (callgrind) |
| Fast global fixup for snapshot realms after the first (known engine extras deleted; full scan as fallback) | `lb-webapi` install | -0.1 ms per realm |
| Fx-style hasher for the binding registry (static keys) | `lb-webapi` registry | SipHash was ~30M instructions per `fetch` process (callgrind) |

Fidelity is unchanged: the full surface of the global and of every
interface (order, descriptors, prototype chains) is byte-identical before
and after in both built and snapshot realms, except the six
`[LegacyWindowAlias]` interface objects' `name`, which now matches Chrome
153 (`webkitURL.name === "URL"`, `WebKitCSSMatrix.name === "DOMMatrix"`,
`FeaturePolicy.name === "PermissionsPolicy"`; before, the alias name
leaked into the shared function, so `URL.name` was `"webkitURL"`). Checked
on the final release binary: fingerprint-report 113 ok / 0 mismatch for
both Chrome reference personas, T-010 fp 0 for both (and beh 0 with
`--input human`), Playwright 20/20, WPT 2552/2788, Firefox-family tests
14 pass / 2 ignored, clippy `-D warnings` for both families, fmt.

Costs: snapshot realms take 2.5 ms instead of 2.3 ms (`realm_bench`, same
session; first realm 3.1 ms) and keep 1.63 MB of V8 heap instead of
1.39 MB; the binary grew 0.45 MB (39.17 → 39.63 MB). The first CDP page
still makes the snapshot (121 ms; Lightpanda 4 ms).

Measured and not kept: V8-managed GC (above); a young-generation cap
(1-8 MB: -1 to -2 MB RSS, more CPU); `MALLOC_ARENA_MAX` and glibc
trim/mmap thresholds (no change); a growth threshold (8 MB) for the GC after
close (no measurable CPU gain, peak up to 50 MB); a code cache
made after the run (+6 MB RSS); HTTP keep-alive (network is ~1 % of CPU
here; still open).

## M7 status: WP-A, Linux ground truth and OS rules (2026-11-03)

**Ground truth.** `tools/os_truth` (`capture.js`, `page.html`, `xtest.py`,
`fontconf/`) captures the OS-dependent surface of the local Chrome for
Testing 153.0.8010.12 on loopback, without CDP or automation flags:
`tests/fixtures/os/linux_x11_cft153.json` (headed under `xvfb-run`,
1920x1080x24, 96 dpi), `_ubuntu24fonts` and `_fedora42fonts` (headed, with
distro-like fontconfig and GTK font) and `_headless` (`--headless=new`).
Limits, recorded in each file: no GPU, no audio server, no voices,
container fonts (Inter is the default sans; Ubuntu/Cantarell/Adwaita/Noto
text fonts not installed), no window manager, X11 only. Windows and macOS
are not captured here; their values below are the surface reference from
the M7 plan (confidence medium unless noted) and are marked
`"captured": false` wherever used.

Captured on Linux, and where the plan's assumptions were wrong:

| Surface | Chrome 153 on Linux (captured) | Consequence |
|---|---|---|
| UA-CH `platformVersion` | `""` in JS and in `Sec-CH-UA-Platform-Version`, headed and headless (kernel 6.18.44 here) | Linux personas must report `""`, not the kernel version (WP-B/WP-D/WP-E) |
| `navigator.deviceMemory` | 16 (15 GB RAM) | Chrome 153 does not cap at 8: no `device_memory <= 8` rule (WP-B R25 must allow 16 and 32) |
| Wheel, one X11 notch | `deltaY` 120, `wheelDeltaY` -120, `deltaMode` 0 | Linux notch is 120 px, ratio -1 (the plan guessed 53 px) |
| Scrollbars | 15 px (div and root, headed and headless); Playwright's headless shell: 0 | |
| OS-gated APIs | no `share`, `canShare`, `bluetooth`, `Bluetooth*`; `keyboard` (layout map 48-49 keys), `gpu` (adapter `null`), `mediaCapabilities`, `hid`, `serial`, `usb` present | |
| Codecs | H.264/AAC `probably`, HEVC `""`, VP9/AV1 `probably`; `decodingInfo` H.264 1080p supported, not power efficient (no GPU) | |
| Fonts | `sans-serif` = Arial (Liberation Sans), `serif` = Times New Roman (Liberation Serif), `monospace` = DejaVu Sans Mono; `system-ui` = fontconfig/GTK sans (Inter here, DejaVu Sans with the distro configs); `font: caption` etc. = Arial 16px; colour emoji (Noto Color Emoji) | |
| Audio | 44100 Hz, `baseLatency` 0.01, `outputLatency` 0, 2 channels (no audio server) | not representative of PipeWire desktops |
| Window | headed without a WM: outer width = inner width, outer height - inner height = 87, `screenX/Y` 10 | |
| Accept-CH | Critical-CH retries the first navigation once, cookie kept; hints follow `Accept` on the retry and `sec-ch-ua-mobile` on later navigations, in the order arch, platform, platform-version, model, bitness, wow64, full-version-list, form-factors | header order for WP-E |

Windows/macOS reference used by the bench fixtures (not captured):
`platformVersion` Windows 10 `10.0.0`, Windows 11 `13.0.0`+ (24H2
`19.0.0`), macOS three parts (`15.1.0`); arm on Apple Silicon; scrollbars
Windows 10 17 px (high), macOS 0 (overlay) or 15; `share`/`canShare`
present on Windows (high) and macOS, `bluetooth` present; WebGL D3D11
(`MAX_VIEWPORT_DIMS` 32767, line width [1,1], point size [1,1024]) and Metal
(16384, [1,1], [1,511]); HEVC `probably` and H.264 power efficient with a
GPU; wheel Windows `deltaY` 100 / `wheelDeltaY` 120, macOS ratio -3; local
voices Microsoft David/Mark/Zira on Windows, Apple voices on macOS.

**Bench.** New info probes (`os_surface`, `os_media`, `os_voices`,
`beh_wheel_deltas`) and 16 rules in `scoring.py` (`uach_platform_version`,
`uach_arch_gpu`, `ch_high_entropy`, `api_throws`, `webgl_backend_os`,
`fonts_os_markers`, `os_api_surface`, `generic_fonts_os`, `voices_os`,
`scrollbar_os`, `emoji_render`, `webgpu_os`, `os_bluetooth`; info until
Windows/macOS captures: `codecs_os`, `screen_os`, `wheel_os`). `server.py`
sends `Accept-CH` and `Critical-CH`. Existing rules, weights, thresholds and
probes are unchanged. Per-OS fixtures `tests/detect-bench/fixtures/os_*_chrome.json`
(Linux captured, Windows/macOS not) and 15 new unit tests; `python3 -m
unittest` 33 OK. Stock Chrome 153 over CDP, headed under `xvfb-run` and
`--headless=new`, fires no OS rule (also with the info rules at target
weights); T-001/T-003 still 100 (the headless shell now also fires
`scrollbar_os`: it hides scrollbars).

**`serve --unchecked-persona FILE`** (hidden, not in USAGE) serves a persona
that breaks the consistency rules and prints the violations, for bench
tests with adversarial personas. Example: the macOS persona with an
NVIDIA OpenGL renderer (R02 violation) is refused by `--persona`, served by
`--unchecked-persona`, and fires `webgl_backend_os` and `uach_arch_gpu`.
The same persona with `architecture` `x86` and the Apple M2 GPU only warns
in `validate` (R12) but fires `uach_arch_gpu`.

**LightBrowser against the OS rules** (debug build, fresh `serve` per run,
T-010, loopback). This is the to-do list for WP-B..E; nothing was fixed in
WP-A.

| Rule | Windows 10 persona | macOS 15 persona | What fires | WP |
|---|---|---|---|---|
| `ch_high_entropy` (40) | fires | fires | no high-entropy `Sec-CH-UA-*` after `Accept-CH` | E |
| `api_throws` (40) | fires | fires | `canPlayType`, `MediaSource.isTypeSupported`, `MediaRecorder.isTypeSupported` throw `NotSupportedError`; `navigator.mediaCapabilities`, `keyboard`, `gpu` missing | D |
| `os_api_surface` (30) | fires | fires | `share`/`canShare` missing | D |
| `scrollbar_os` (25) | fires | quiet | 0 px on Windows 10 (want 17) | C |
| `emoji_render` (25) | fires | fires | canvas emoji draws no pixels | C |
| `os_bluetooth` (15) | fires | fires | `bluetooth` missing | D |
| `wheel_os` (info) | quiet | fires (human input) | `wheelDeltaY/deltaY` -1.0 to -1.18 on macOS (want about -3) | D |

Scores: Playwright input fp 88.0 (Windows) and 83.9 (macOS), was 0 for
both; `--input human` fp 88.0 / 83.9, beh 0 for both (the info rule
`wheel_os` counts 0). Quiet for both: `uach_platform_version`,
`uach_arch_gpu`, `webgl_backend_os`, `fonts_os_markers`,
`generic_fonts_os`, `voices_os`, `webgpu_os` (no WebGPU), `codecs_os`
(codecs throw), `screen_os`.

## M7 status: WP-B, OS persona model (2026-11-03)

**Model (ADR 0015).** `lb-persona` `os.rs`: `OsKind`, `DisplayServer`,
`Distro` and the accessors every layer reads instead of checking `os.name`:
`font_defaults`, `font_aliases`, `window_position`, `scrollbar_width`,
`wheel`, `audio_defaults`, `webgpu`, `codecs`, `os_apis`,
`high_entropy_hints` (plus `windows_major`, `macos_version`, `apple_chip`,
`linux_distro`). Each takes the persona's optional field, else the OS
default (table in ARCHITECTURE.md, "OS axis"). New optional schema fields:
`os.kernel/distro/display_server`, `screen.window_x/window_y/maximized`,
`fonts.defaults`, `audio.output_latency`, `behavior.wheel`,
`client_hints.form_factors`, `webgpu`, `media`, `apis`, `provenance`. No
TCP/IP field (TTL and TCP options are the host's).

Accessor names that differ from the plan: the font keyword size is
`FontDefaults::system_font_px` (with its family in `system_font`, Arial on
Linux as captured), not `system_ui_px`; the wheel ratio is
`behavior.wheel.delta_ratio` in JSON and `WheelModel::wheel_delta_per_px`
in Rust.

**Rules.** R17 platform version (Windows build table: 19045 -> `10.0.0`,
22631 -> `15.0.0`, 26100 -> `19.0.0`; macOS = `os.version`; Linux `""`,
captured), R18 architecture/GPU/wow64, R19 ANGLE backend per OS (D3D11,
Metal, GL or Vulkan), `Google Inc. (X)` wrapper, limits per backend and
the kernel in radeonsi strings, R20 screen geometry (taskbar 40/48 px per
DPR, menu bar 23-25 or 30-39, Wayland avail = screen and window 0,0, outer
>= inner, width difference 0 or 16), R21 fonts (foreign markers, Windows
11 fonts iff Windows 11, Ubuntu fonts on Ubuntu, defaults installed), R22
voices, R23 audio (warning), R24 preferences (forced colors only on
Windows, scrollbar width per OS, 30-bit means wide gamut), R25 hardware
(`deviceMemory` a power of two up to 32, as captured: no cap at 8; Apple
chip thread counts), R26 macOS versions (Intel Macs end at 26; Apple chips
have a first version). R12's architecture check is now a violation, its
platform-version check moved to R17; R08 also accepts 11, 14, 18 and 22
threads. R17-R26 skip Firefox personas.

**Personas.** Six new Chrome references (Windows 11 24H2 RTX 4060, Windows
11 23H2 Iris Xe laptop at 1.25, macOS 14 Intel Iris Plus, macOS 26 Apple
M4, Ubuntu 24.04 X11 Intel UHD 630, Fedora 42 Wayland AMD RX 6600; table
in `profiles/README.md`). The macOS 15 persona now has the full default
macOS voice list (176 Apple voices, including the Eloquence voices in 14
locales, plus 19 Google), was 64 + 19. `profiles/catalog/` and
`lb_persona::generate` (`examples/gen_personas.rs`) produce consistent
personas; 5 per OS are committed in `profiles/generated/`. 11 adversarial
personas each break exactly one of R17-R26. Derived font entries were added
for Windows 11 and the Linux distro fonts (`profiles/fonts/README.md`).
Windows/macOS values are the surface reference (not captured); Linux
WebGL limits, distro font sets, panel sizes and audio are not captured
either (no GPU, fonts or audio server in the capture container).

**Checks.** `cargo test -p lb-persona`: 13 + 41 pass (5 new unit and 16
new integration tests: every
reference, generated and adversarial file, 50 generated personas per OS,
determinism, schema coverage, font-table coverage, accessor defaults, each
new rule). Every persona file except the three adversarial ones meant to
break the schema validates against `persona_v2.schema.json` (Python
`jsonschema`, 36 files). `lightbrowser validate`: all 8 Chrome references
OK with 0 warnings. `fingerprint-report`: 0 mismatch, 0 unimplemented for
all 8 (112-115 ok each); `cargo test -p lb-page --test pages` 19 pass
(includes the report gate over all 8). `lb-layout` (font coverage), `lb-css`,
`lb-cdp`, `lightbrowser` tests pass; clippy `-D warnings` workspace and
firefox clean; firefox tests 15 pass, 2 ignored; `sdk/python` 93 pass, 4
skipped; detect-bench unittest OK (unchanged). Release: stripped binary
39,690,104 bytes (< 40,000,000; +61,440 since the M6 measurement, which
also includes WP-A); `realm_bench` with the snapshot: first realm 3.39 ms,
next 2.62 ms.

**Bench** (debug build, fresh `serve` per run, T-010, loopback; Playwright
input / `--input human`):

| Persona | fp | beh (human) | OS rules still firing | WP |
|---|---|---|---|---|
| Windows 10 RTX 3060 | 88.0 / 88.0 | 0 | `ch_high_entropy`, `api_throws`, `os_api_surface`, `scrollbar_os`, `emoji_render`, `os_bluetooth` | C, D, E |
| Windows 11 RTX 4060 | 83.9 / 83.9 | **35** (`beh_mouse_linear`, ratio 0.87, same on a rerun) | as Windows 10 without `scrollbar_os` (Windows 11 accepts 0 px) | C, D, E; beh: lb-behavior |
| Windows 11 Iris Xe laptop | 83.9 / 83.9 | 0 | as Windows 11 | C, D, E |
| macOS 14 Intel | 83.9 / 83.9 | 0 | `ch_high_entropy`, `api_throws`, `os_api_surface`, `emoji_render`, `os_bluetooth` | C, D, E |
| macOS 15 M2 | 83.9 / 83.9 | 0 | as macOS 14 (unchanged by the voice fix) | C, D, E |
| macOS 26 M4 | 83.9 / 83.9 | 0 | as macOS 14 | C, D, E |
| Ubuntu 24.04 X11 | 85.8 / 85.8 | 0 | `ch_high_entropy`, `api_throws`, `generic_fonts_os` (no Arial: the alias layer is WP-C), `scrollbar_os` (0 px, want 15), `emoji_render` | C, D, E |
| Fedora 42 Wayland | 85.8 / 85.8 | 0 | as Ubuntu | C, D, E |

The Windows 10 and macOS 15 scores are the same as after WP-A. The Linux
personas do not fire `os_api_surface`, `os_bluetooth`,
`uach_platform_version`, `voices_os`, `fonts_os_markers` or
`webgl_backend_os`. Adversarial personas through `serve
--unchecked-persona` fire the bench rule listed in `profiles/README.md`
for 7 of 11; the other 4 (kernel mismatch, Linux with Segoe fonts, Wayland
work area, macOS 17 px scrollbars) have no bench rule that can see them
yet.

Dark color schemes: `run.js` opens its page with Playwright's default
`colorScheme: 'light'`, which `Emulation.setEmulatedMedia` refuses for a
dark persona (ADR 0009), so the reference personas are light; about a
quarter of generated personas are dark and need `colorScheme: null` (or
`dark`) in the client.

## M7 status: WP-C, rendering per OS (2026-11-03)

**What changed (ADR 0015).** `lb_css::MediaEnv` carries the persona's
`scrollbar_width` and `font_defaults`; lb-css, lb-layout and lb-paint no
longer pick fonts or scrollbars by OS.

- **Scrollbars.** taffy reserves the persona's width for `overflow:
  scroll`; `overflow: auto` boxes and the viewport get it when their
  content overflows (a second taffy pass with the gutters reserved), never
  with `overflow: hidden` on the root or body. `clientWidth/Height`, the
  root's `clientWidth`, `scrollTo` clamping and `scrollWidth` account for
  them. Textareas include the scrollbar in their width (Blink). The
  default `MediaEnv` (no persona, tests and tools) has none.
- **Fonts.** `FontAvailability::for_persona` holds the installed list,
  `font_defaults` and `font_aliases`. A family resolves if installed, else
  through its alias (Linux: Arial, Helvetica, Arimo -> Liberation Sans and
  so on; elsewhere Blink's alternates); generic keywords name the
  `font_defaults` families (Windows monospace is now Consolas, Linux
  DejaVu Sans Mono or Noto Sans Mono, Linux system-ui the GTK font);
  `ui-*` keywords are generic only on macOS; the fallback is the standard
  font. A persona font with no table on its OS borrows one from another
  OS, so foreign fonts are detectable.
- **`font:` system keywords** (`caption`, `icon`, `menu`, `message-box`,
  `small-caption`, `status-bar`) use the persona's system font (Linux Arial
  16px, captured; Windows Segoe UI 12px and macOS system-ui 13px, not
  captured); `-webkit-*-control` use the UA control font. In CSS and in
  canvas `font`.
- **Emoji.** Advance by the OS emoji font (Noto Color Emoji 136/109 em,
  captured; Segoe UI Emoji 1.3 and Apple Color Emoji 1.25 em, not
  captured); `line-height: normal` grows to the emoji font's metrics.
  Canvas `fillText` and screenshots draw emoji in colour with flat
  stand-in shapes (yellow face for smileys, coloured disc otherwise),
  whatever the fill style, as Chrome draws colour emoji. The pixels are not
  Chrome's (no emoji images are bundled).
- **Tables.** Real Linux tables for Carlito and Caladea from the local SIL
  OFL files (`gen_font_tables`; regenerating every other table gives
  identical files). Noto Color Emoji's vertical metrics come from the local
  font file. No downloads.

**Checks.** New `crates/lb-layout/tests/os.rs` (7 tests): scrollbars for
every reference persona (`overflow: scroll`/`auto`/`hidden`, root of a tall
and a short page, `overflow: hidden`/`scroll` roots, textarea, body width);
Ubuntu and Fedora widths and heights at 72px for 12 aliased families, 3
DejaVu families and 12 generic keywords equal the Linux capture exactly;
the Linux scrollbar probe equals the capture (15/15, root 15/0, textarea
182x36, client 180); generic keywords follow `font_defaults` per persona;
emoji advance and line height per OS (Linux 125x117 as captured); system
font keywords; foreign fonts detectable. Also new: lb-css
`persona_font_defaults_and_system_fonts`, lb-paint
`emoji_draw_in_colour_for_every_reference_persona`, system keywords in
canvas `font`. Updated: two tests that assumed Courier New as Windows
monospace (lb-layout `persona_viewport_and_fonts`, lb-page
`fingerprint_surfaces` now measures against `serif`). Results: `lb-css`
13 + 6, `lb-layout` 7 + 10 + 7, `lb-paint` 26, `lb-persona` 13 + 41,
`lb-webapi` 23 + 10 + 4, `lb-page` (lib 9, `canvas` 3, `cdp_host` 2,
`connections` 1, `fingerprint_surfaces` 2, `human` 2, `navigator` 1, `pages`
19, `tables` 3), `lb-cdp` 13 + 10, `lightbrowser` 6 + 1, all pass; WPT
subset 2552/2788 (unchanged); Playwright 20/20; `fingerprint-report` 0
mismatch, 0 unimplemented for all 8 references (112-115 ok). Clippy `-D
warnings` (workspace and firefox) and `fmt` clean; firefox tests 15 pass, 2
ignored. Release: stripped binary 39,722,872 bytes (< 40,000,000; +32,768
since WP-B: two tables, the emoji and keyword code); `realm_bench`: first
realm 3.09 ms, next 2.46 ms with the snapshot.

**Bench** (debug build, fresh `serve` per run, T-010, loopback):

| Persona | fp (Playwright input) | beh (`--input human`) | OS rules still firing | WP |
|---|---|---|---|---|
| Windows 10, Windows 11 (both) | 78.6 | 0 (Windows 10, Windows 11 RTX 4060) | `ch_high_entropy`, `api_throws`, `os_api_surface`, `os_bluetooth` | D, E |
| macOS 14, 15, 26 | 78.6 | 0 (macOS 15) | as Windows | D, E |
| Ubuntu 24.04, Fedora 42 | 64 | 0 (Ubuntu) | `ch_high_entropy`, `api_throws` | D, E |

`scrollbar_os`, `emoji_render` and `generic_fonts_os` no longer fire for
any reference persona. The Windows 11 RTX 4060 reference now scores beh 0
with human input (35 after WP-B): the page geometry changed with the
scrollbar, not the seed or lb-behavior, so the `beh_mouse_linear` issue
noted in WP-B may still show on other layouts. Adversarial personas: 
`mac_scrollbar17` now fires `scrollbar_os` and `linux_segoe_fonts` fires
`fonts_os_markers`; `linux_mesa_kernel_mismatch` and `wayland_avail_top`
still have no bench rule that can see them.

**Not done in WP-C.** Pixel-accurate emoji (needs the OS emoji images);
real tables for Ubuntu Sans, Cantarell, Adwaita, Noto text fonts and URW
fonts (no local files; derived entries stay); content that overflows only
because the viewport scrollbar narrowed it gets no second scrollbar pass;
horizontal viewport scrollbars are not modelled; screenshots do not draw
scrollbars.

## M7 status: WP-D, web APIs and input per OS (2026-11-03)

**What changed (ADR 0015).**

- **OS-gated members.** `tests/fixtures/chrome_153_os_overlay.json` adds
  what Chrome 153 has on Windows and macOS only: `navigator.share`,
  `canShare`, `bluetooth` and the 8 `Bluetooth*` interfaces
  (`BluetoothUUID` is not `[SecureContext]`). `lb-webidl-gen` merges it into
  both fixtures, in Chrome's order (not captured: attributes in IDL order,
  operations alphabetical, the secure-only group alphabetical, the main
  group reverse alphabetical, as in the captured Linux fixture), and emits
  the members as `OS_GATED`. The V8 snapshot keeps them all (persona-free);
  `WindowRealmOptions::os_apis` deletes the gates the persona's `os_apis()`
  turns off when a realm (main or isolated world) is created, which keeps
  the order of the rest. A Linux realm equals the Linux capture exactly.
  `share()` rejects `TypeError` for empty data, `NotAllowedError` without
  transient user activation (5 s after a trusted mousedown or keydown, now
  tracked) and `AbortError` (share sheet dismissed) with it; `canShare()`
  checks the data. `bluetooth.getAvailability()` resolves `true`;
  `requestDevice()` rejects `SecurityError` without activation, else
  `TypeError` or `NotFoundError` (chooser cancelled).
- **Media** (`rt/media.rs`). `canPlayType`, `MediaSource.isTypeSupported`
  and `MediaRecorder.isTypeSupported` from Chrome's container and codec
  tables: all 35 + 35 + 16 answers of the Linux capture match (a unit test
  reads the capture). HEVC needs `codecs().hevc` (Windows/macOS with a
  hardware GPU). `navigator.mediaCapabilities.decodingInfo/encodingInfo`
  validate like Blink and resolve `{powerEfficient, smooth, supported,
  configuration, keySystemAccess}`, `powerEfficient` from hardware decode
  (Linux all false, as captured).
- **`navigator.keyboard`**: `getLayoutMap()` resolves a US map of 48 keys
  (captured size); `lock()` resolves.
- **`navigator.gpu`** (`rt/gpu.rs`): `requestAdapter()` resolves `null`
  without a persona adapter (Linux, captured) or a `GPUAdapter` whose
  `info` has the persona's vendor and architecture; `features`, `limits`
  and `wgslLanguageFeatures` are the surface reference (not captured);
  `getPreferredCanvasFormat()` is `rgba8unorm` on Linux (captured),
  `bgra8unorm` elsewhere. `requestDevice()` rejects `OperationError`.
- **WebGL backends.** D3D11 / Metal / OpenGL on Mesa / OpenGL on NVIDIA /
  Vulkan, each with its own default `MAX_VIEWPORT_DIMS`, line width, point
  size and `MAX_VERTEX_UNIFORM_VECTORS` (persona values still win).
- **Audio.** `AudioContext` rate, `baseLatency`, `outputLatency` and
  channel count come from `audio_defaults()` (Linux now 48 kHz).
- **Window position.** `screenX/Y`, `screenLeft/Top` and the screen
  coordinates of trusted input come from `window_position()` (Ubuntu
  64,32; Wayland 0,0; macOS below the menu bar).
- **Wheel.** Trusted `wheelDelta` is `-delta x wheel().wheel_delta_per_px`
  (Windows 1.2, Linux 1, macOS 3); script-made `WheelEvent`s use the init's
  `wheelDeltaX/Y` or `-delta` (Blink). `LightBrowser.human*` scrolls in the
  OS's detent (Windows 100 px, Linux 120, macOS 40) and lb-behavior has a
  macOS acceleration model for mouse wheels (`ScrollProfile::accelerate`;
  macOS personas default to a trackpad).
- **Shortcuts.** Select-all is Cmd+A on macOS personas, Ctrl+A elsewhere.
- **Smaller.** Windows lists a "communications" audio device after
  "default" once microphone permission is granted. `getHighEntropyValues`
  `formFactors` and `Emulation.setUserAgentOverride` (`formFactors`,
  `wow64`) follow `high_entropy_hints()`.
- **fingerprint-report** adds `window.screenX/Y`, `layout.scrollbarWidth`,
  `fonts.generic.*` (keyword width equals the default family's),
  `navigator.share/canShare/bluetooth`, `window.Bluetooth`, `media.hevc`,
  `media.h264PowerEfficient`, `media.av1PowerEfficient`, `webgpu.adapter`,
  `highEntropy.formFactors`, `speechSynthesis.voiceCount` and the audio
  defaults including `outputLatency`.

**Checks.** New tests: `lb-webapi` `install::os_gated_members_follow_os_apis`,
`media::linux_capture_matches`, `media::hevc_follows_hardware_decode`,
`gpu::limits_cover_the_interface`; `lb-webidl-gen` `os_overlay_is_gated`
(the generator tests now compare against the overlaid fixture);
`lb-behavior` `accelerated_wheel_grows_within_a_flick`; `lb-page`
`os_apis` (Windows 10, Ubuntu, Fedora and macOS 15 on a loopback page) and
`human::wheel_and_shortcuts_follow_the_os` (wheel detents and ratios on
Windows, Ubuntu and macOS; select-all modifier); `lb-cdp`
`formFactors`/`wow64` overrides. Results: `lb-webapi` 26 + 11 + 4,
`lb-webidl-gen` 1 + 7, `lb-behavior` 66 + 1 + 1, `lb-cdp` 13 + 10,
`lb-page` (lib 9, `canvas` 3, `cdp_host` 2, `connections` 1,
`fingerprint_surfaces` 2, `human` 3, `navigator` 1, `os_apis` 1, `pages`
19, `tables` 3, `wpt`), `lightbrowser` 6, all pass; WPT subset 2552/2788
(unchanged); Playwright 20/20; detect-bench unittest 33 OK; `sdk/python` 93
pass, 4 skipped. `lb-webidl-gen --check` clean. Clippy `-D warnings`
(workspace and firefox) and `fmt` clean; firefox tests 15 pass, 2 ignored. Release:
stripped binary 39,841,656 bytes (< 40,000,000; +40,960 since WP-D, 158,344
bytes left), `--version` prints chrome. `realm_bench` was not re-run: no
realm or engine code changed.
`fingerprint-report`: 0 mismatch, 0 unimplemented for all 8 references
(130-133 ok). Release: stripped binary 39,800,696 bytes (< 40,000,000;
+77,824 since WP-C: media, GPU, keyboard and Bluetooth code and 8 more
interfaces; 199,304 bytes left); `realm_bench` with Linux realms (the gate
deletion runs): first realm 3.17 ms, next 2.45 ms with the snapshot (976 of
976 templates restored).

**Bench** (debug build, fresh `serve` per run, T-010, loopback, own seeds):

| Persona | fp (Playwright input) | beh (`--input human`) | OS rules firing |
|---|---|---|---|
| Windows 10, Windows 11 (both) | 40 | 0 | `ch_high_entropy` (WP-E) |
| macOS 14, 15, 26 | 40 | 0 | `ch_high_entropy` |
| Ubuntu 24.04, Fedora 42 | 40 | 0 | `ch_high_entropy` |

`api_throws`, `os_api_surface` and `os_bluetooth` no longer fire;
`codecs_os`, `screen_os` and `wheel_os` stay quiet at their target weights
(15, 15, 20) for every run, Playwright and human input. All 8 references
pass `beh<=30` with human input (beh 0). The adversarial personas fire the
same rules as after WP-C (9 of 11).

**Not done in WP-D.** Windows and macOS answers (overlay order, codecs,
WebGPU limits and features, wheel ratios, select-all, communications
device) are the surface reference, not captures. `GPUAdapter.requestDevice`
cannot create a device. `AudioContext.resume()` without a user gesture
still starts the context (the Linux capture stays `suspended`). Files in
`canShare`/`share` are counted, not type-checked.

## M7 status: WP-E, client hints after Accept-CH (2026-11-03)

**Capture.** `tools/header_capture/accept_ch.js` (new) serves three
HTTP/1.1 origins and one h2 origin on 127.0.0.1 and drives the local Chrome
for Testing 153 (throwaway profile, no CDP) through `Accept-CH` with the
bench's 7 hints plus `Critical-CH: Sec-CH-UA-Platform-Version`, a page with
every subresource kind, same- and cross-origin frames, later and
redirected navigations, a hint subset on a fresh origin and a second
launch. 88 requests are kept in
`profiles/headers/captures/chrome_153_linux_accept_ch.jsonl`; headed and
`--headless=new` are identical. Findings:

| Finding | Captured |
|---|---|
| Hints on navigations and frames | after `sec-ch-ua-mobile`: full-version, arch, platform, platform-version, model, bitness, wow64, full-version-list, form-factors; hints not asked for are skipped |
| `Critical-CH` | one retry, cookie from the first response kept, hints after `Accept` and before `Sec-Fetch-*` |
| Redirect hop | hints after `Sec-Fetch-Dest`, then `Referer` (the old profile put `Referer` first) |
| `Accept-CH` on a redirect | counts for the next hop |
| Subresources | hints join Blink's hash-ordered group; 14 orders recorded per hint set and page headers |
| Cross-origin frame | no hints, nor for anything it loads (even from the top origin); its own `Accept-CH` is ignored |
| https with certificate errors | `Accept-CH` ignored; the capture trusts its key with `--ignore-certificate-errors-spki-list` |
| h2 `cookie` | before `priority`; the first capture's decoder (Python `h2`) had moved it last |

**What changed (ADR 0015, ADR 0011 update).**

- `lb_net::client_hints`: `UaHint`, the per-origin `ClientHintsStore`
  (memory only, one per browser context, shared like the cookie jar),
  trustworthy-origin check and the delegation rule (`hints_for_request`).
- `lb_net::fetch`: stores `Accept-CH` from top-level navigation responses
  (redirects too), sends the hints the origin asked for, and repeats a
  navigation once when `Critical-CH` names a missing hint. Chrome personas
  only; Firefox sends none.
- `lb_persona::Persona` carries `high_entropy` (from
  `high_entropy_hints()`), so headers and `getHighEntropyValues` agree.
- `profiles/headers/chrome_153.json`: hint slots in `document`, `iframe`
  and `document_redirect` (its `Referer` moved after the hints), a new
  `document_restart` list, `cookie` before `priority`, and
  `renderer_group.observed_hints` (from
  `tools/header_capture/derive_groups.py`).
- lb-page passes the top-level document (`RequestMeta::top_frame`) for
  frame and subresource requests.
- Not done: the TLS profile id alias (`chrome_153`) from the plan; nothing
  at the TCP/IP level (see Risks).

**Checks.** `lb-net` lib 79 (new `client_hints` tests: trustworthy
origins, hint parsing, store, delegation, values), `--test accept_ch` 5
(golden: 84 of 88 captured requests match in names, case, order and values,
75 with hints, both `Critical-CH` retries; 4 cross-site frame requests of
the https page skipped, see below; 4 pipeline tests on a loopback server),
`--test fingerprint` 3 (new: every reference Chrome persona gives the same
exact TLS/H2 match). `lb-page --test client_hints` 1 (real page, same- and
cross-origin frames). Also run, all pass: `lb-persona` 13 + 41, `lb-page`
(lib 9, `canvas` 3, `cdp_host` 2, `client_hints` 1, `connections` 1,
`fingerprint_surfaces` 2, `human` 3, `navigator` 1, `os_apis` 1, `pages`
19 with the report gate over all 8 references, `tables` 3), WPT subset
2552/2788 (unchanged), `lb-webapi` 26 + 11 + 4, `lb-cdp` 13 + 10 + 1,
`lightbrowser` 6; Playwright 20/20; detect-bench unittest 33 OK;
`sdk/python` 93 pass, 4 skipped. Clippy `-D warnings` (workspace and
firefox) and `fmt` clean; firefox tests 15 pass, 2 ignored.

**Bench** (debug build, fresh `serve` per run, T-010, loopback, own seeds):

| Persona | Playwright input | `--input human` |
|---|---|---|
| All 8 reference personas (Windows 10, Windows 11 x2, macOS 14/15/26, Ubuntu 24.04, Fedora 42) | fp 0, `fp<=30` pass (only behaviour rules fire) | score 0, beh 0, `beh<=30` pass (no rule fires) |

Re-scored with `codecs_os`, `screen_os` and `wheel_os` at their target
weights (15, 15, 20), no OS rule fires in any of the 16 sessions;
`ch_high_entropy`, `header_order` and `cookie_not_echoed` are quiet. Each
session shows the `Critical-CH` retry (5 documents for 4 pages). The
hinted document headers of the Ubuntu persona equal stock Chrome's
(`tests/detect-bench/fixtures/os_linux_chrome.json`) name for name, apart
from `Sec-Fetch-User` after a click (see below). This meets the M7 bench
target for every reference persona.

**Not done in WP-E.** No `Permissions-Policy` / `Delegate-CH` delegation
to other origins; device and preference hints (`DPR`, `Viewport-Width`,
`Device-Memory`, `Sec-CH-Prefers-*`) are not sent; hint sets we have not
captured on subresources get an approximate order. Seen in the capture but
not done: `Sec-Fetch-Storage-Access` on cross-site frames of an https page,
`priority: i` for CSS background images (not fetched yet), and
`Sec-Fetch-User` on link-click navigations (stock Chrome sends it after a
click in the bench; LightBrowser does not, ADR 0011). Windows and macOS
hint values are the persona's (surface reference, not captured).

## Workspace check (2026-11-03)

Chrome build unless noted, 4 cores. Re-run on the final tree after the
Lightpanda start-up and memory work (`bench/results/2026-11-03-ca3f1b4.json`
stands as the recorded bench run; `bench/run.py` was not run again). The
disk could not hold every test binary at once, so tests were run per
package and per test target, deleting test binaries after each.

- `cargo fmt --all --check`: clean. `cargo clippy --workspace --all-targets
  -- -D warnings`: clean.
- Tests, all pass, 0 fail: `lb-persona`, `lb-behavior`, `lb-css`, `lb-dom`,
  `lb-layout`, `lb-paint`, `lb-webidl-gen`, `lb-fptest`; `lb-js` (lib 9,
  `v8` 32, `snapshot` 5); `lb-net` (lib 74, `fingerprint` 2/2);
  `lb-webapi` (lib 23, `install` 10, `snapshot` 4); `lb-cdp` (13 + 10 + 1);
  `lb-page` (lib 9, `canvas` 3, `cdp_host` 2, `connections` 1,
  `fingerprint_surfaces` 2, `human` 2, `navigator` 1, `pages` 19, `tables`
  3, `wpt`); `lightbrowser` (1 + 5).
- WPT subset 2552/2788 (51/83 files fully pass, 3 harness errors or
  timeouts), as before.
- Firefox: clippy `-D warnings` for `lightbrowser`, `lb-js`, `lb-webapi`,
  `lb-page`: clean; tests for the same four: 14 pass, 2 ignored.
- Playwright (`node --test tests/playwright/`, debug build): 20/20.
  detect-bench unittest 18 OK; `bench` unittest 12 OK; `sdk/python`
  pytest 93 pass, 4 skipped (the scenarios need Python Playwright >= 1.63;
  1.52 is installed).
- Release: stripped binary 39,628,664 bytes (39.63 MB, budget < 40 MB, sha256
  `573c54b604e5669d`, the same binary as the recorded bench run);
  `--version` prints `lightbrowser 0.0.1 (chrome)`. `realm_bench` (release):
  first realm 3.04 ms, next realm 2.43 ms with the snapshot (6.82 / 6.01 ms
  without); a snapshot realm keeps 1.63 MB of V8 heap.
- `lightbrowser fingerprint-report` (release): 113 ok, 0 mismatch for both
  Chrome reference personas.
- detect-bench on the debug build (`serve --persona`, a fresh `serve` per
  run, bench on 127.0.0.1, own persona seeds):

| Run | Windows persona | macOS persona |
|---|---|---|
| T-010, Playwright input, `fp<=30` | 89.8, fp 0, pass | 89.8, fp 0, pass |
| T-010, `--input human`, `beh<=30` | 0, beh 0, pass | 0, beh 0, pass |
| T-001 stock headless Chromium, `>=90` (the bot baseline) | 100, fp 100, pass | |

With Playwright input both personas fire the same behaviour rules
(`beh_typing_no_keys`, `beh_click_center`, `beh_click_no_path`,
`beh_click_hold`, `beh_wheel_pattern`, `page_dwell_short`); fp is 0.

Not green: the two Lightpanda checks that fail in the recorded run (`fetch`
CPU and CDP peak RSS), see "M6 status: Lightpanda comparison".

## Risks

- **TCP/IP fingerprint.** SYN TTL and TCP options always come from the host
  OS, so a passive TCP/IP fingerprint can contradict a Windows or macOS
  persona; LightBrowser does not change them.
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

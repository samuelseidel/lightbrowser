# Plan to finish LightBrowser

Status: Proposed (2026-11-02). `ROADMAP.md` stays the source of truth for
milestone definitions. Live status is in `ROADMAP.md` ("M2 status" to "M4
status"); section 1 below is the dated starting point and is not updated. This plan breaks M2–M6 into concrete tasks, orders
them, names what blocks what, and lists the gaps found in the current tree.

## 1. Where things stand

| Area | State |
|---|---|
| M0 docs, workspace, CI, SFC32 | Done |
| M1 `lb-net`: BoringSSL profiles, h1, forked h2, cookies, decoding, redirects; `fetch` CLI; `lb-fptest`; Chrome 153 profile | Done. Chrome JA3/JA4/Akamai match on loopback. Firefox JA3/JA4 gaps pinned in ADR 0003. |
| `lb-js` | V8 backend done (M2 task 1), no snapshot. No SpiderMonkey. |
| `lb-persona` | Persona v2 schema, v1/v2 loader, rules R01–R16 (M4 task 1, done early). |
| `lb-dom` | Arena DOM, html5ever sink, selectors done (M2 task 2). |
| `lb-behavior` | Vendored, plus CDP event conversion (M5 task 1 vendoring, done early; replay not done). |
| `lb-css`, `lb-layout`, `lb-paint`, `lb-page`, `lb-webapi`, `lb-webidl-gen`, `lb-cdp` | Empty. |
| `tests/detect-bench` | Local loopback detection bench. |
| `sdk/python`, `bench/` | README placeholders. |
| Deferred from M1 | Proxies, exact h1 header-name case, connection reuse, TLS session cache, Firefox TLS gaps and re-capture. |

What remains is, by size: the DOM and JS stack (M2), layout and CDP (M3),
fingerprint surfaces (M4), the Firefox engine (M4b), behavior and SDK (M5),
and performance (M6). M2 → M3 is the critical path: nothing can be driven by
Playwright until both exist.

Reuse from `stealth-browser` is limited to the Rust crates and the schema:
`behavior/` (2.5k lines), `consistency/` (1.3k lines, rules R01–R10) and
`profiles/schemas/persona_v1.schema.json`. The GeckoPhantom Firefox fork
has never been built in that checkout and is not a dependency.

## 2. Milestones and tasks

Sizes are rough, for one engineer plus coding agents, and are estimates.

### M2 DOM + V8 (critical path, ~6–8 weeks)

Done when `lightbrowser fetch --dump html` prints the DOM **after** scripts
ran, and a WPT DOM subset passes against a local server.

1. **V8 backend** (`lb-js`, feature `chrome`). `v8` crate pinned; one
   isolate per worker thread; `Realm` = V8 context; heap cap;
   microtask checkpoint; `low_memory_notification`. Startup snapshot waits
   until bindings exist (M6). Risk: the `v8` crate downloads a ~100 MB
   prebuilt static library at build time, so CI needs network or a cached
   artifact.
2. **Arena DOM** (`lb-dom`). u32 node ids, `string_cache` atoms, one arena
   per document, node structs ≤ 64 bytes, `html5ever` tree sink fed
   incrementally from the response body, Servo `selectors` for
   `querySelector` / `matches` / `closest`. Document, Element, Text,
   Comment, DocumentFragment, attributes, mutation hooks for later
   MutationObserver.
3. **WebIDL codegen** (`lb-webidl-gen`, build time). `weedle2` over the
   Chromium IDL set, emitting V8 glue: constructors, prototype chains,
   property order, attribute descriptors, `Function.prototype.toString`
   names, `Symbol.toStringTag`. Implement the interfaces a page load
   touches first: `Window`, `Document`, `Node`, `Element`, `HTMLElement`
   plus common subclasses, `Event`/`EventTarget`, `Navigator`, `Location`,
   `History`, `Storage`, `URL`, `console`, timers, `XMLHttpRequest`,
   `Headers`/`Request`/`Response`. Generate every other interface as a
   spec-shaped stub: presence and order are a fingerprint on their own.
   Track coverage as a number in CI.
4. **Event loop** (`lb-page`). HTML task sources, microtasks, timers,
   `requestAnimationFrame`, script order (classic, `defer`, `async`,
   modules), `DOMContentLoaded` / `load`, navigation, one-step teardown of
   arena + realm, virtual time switch. Thread-per-core with a
   `current_thread` tokio runtime each, pages pinned to a worker.
5. **fetch / XHR** (`lb-webapi`) over `lb-net`: cookie jar, same-origin
   and CORS, `Accept` headers per request destination. Fold in the M1
   leftovers here: connection reuse and TLS session cache (so
   `pre_shared_key` appears like a real second connection), exact h1
   header-name case, HTTP and SOCKS5 proxies.
6. **Tests.** Vendor a WPT DOM/HTML subset under `tests/wpt/`, served
   locally; `fetch --dump html` golden tests on a few local pages with
   scripts.

### M3 Layout + CDP core (~5–6 weeks)

Done when stock Playwright `connectOverCDP` runs goto, evaluate, click,
fill, getByRole and content against our local pages on the Chrome build.

1. **Lazy cascade** (`lb-css`). `cssparser` stylesheets, computed style per
   element on demand, inheritance, `display`/`visibility`/`opacity`,
   box properties taffy needs, media queries from the persona screen,
   `getComputedStyle`.
2. **Layout** (`lb-layout`). `taffy` block/flex/grid on demand, text width
   from persona font metric tables via `skrifa`, `getBoundingClientRect`,
   `offsetWidth/Height`, `elementFromPoint`, scroll geometry,
   IntersectionObserver, ResizeObserver stub. Layout data droppable under
   memory pressure.
3. **Accessibility tree** for `getByRole` and `Accessibility.getFullAXTree`
   / `queryAXTree`.
4. **CDP server** (`lb-cdp`). `tokio-tungstenite` WebSocket, the domains
   in `ARCHITECTURE.md`: Browser, Target (flatten mode, browser contexts),
   Page (navigate, frame tree, isolated worlds, lifecycle events,
   `addScriptToEvaluateOnNewDocument`), Runtime (evaluate, callFunctionOn,
   getProperties, console, execution contexts), DOM (getDocument,
   describeNode, resolveNode, box model, content quads, scrollIntoView,
   file inputs), Input (mouse, key, insertText through the DOM event
   pipeline), Network/Fetch (extra headers, cookies, request interception),
   Emulation (checked against the persona; conflicts rejected). Playwright's
   utility script runs in an isolated world and nothing lands in the main
   world. `lightbrowser serve --port` becomes real.
5. **Tests.** A Playwright test suite in `tests/playwright/` run in CI
   against the Chrome build.

### M4 Chrome fingerprints (~4–5 weeks)

Done when the detection engine scores Chrome personas as consistent and
the same seed gives the same fingerprint on every run.

1. **Persona v2** (`lb-persona`). `profiles/schemas/persona_v2.schema.json`
   (v1 + `browser.family` + `client_hints`), loader that reads v1 as
   Firefox, vendored `consistency/` rules R01–R10 plus Chrome rules (UA-CH
   brands and GREASE brand per major, `userAgentData` ↔ UA, TLS/H2 profile
   ↔ UA major, `sec-ch-ua-platform` ↔ OS). Reject inconsistent personas at
   startup and on `LightBrowser.setPersona`. Add a few Chrome reference
   personas under `profiles/reference/`.
2. **Navigator and window surface** (`lb-webapi`): `navigator.*`,
   `userAgentData` (incl. `getHighEntropyValues`), plugins/mimeTypes (the
   fixed PDF set), `hardwareConcurrency`, `deviceMemory`, `screen`, DPR,
   `window.*` sizes, media queries, permissions, battery, media devices,
   speech voices, WebRTC stub candidates, `performance.now` 100 µs
   resolution, Intl/timezone from the persona through V8's ICU.
3. **Canvas 2D** (`lb-paint`): tiny-skia raster, text via `skrifa`
   outlines, SFC32 noise (±1 per channel) on `toDataURL` / `getImageData` /
   `toBlob`. **WebGL** stub: parameters, extensions, shader precision and
   `UNMASKED_*` from the persona GPU; seeded `readPixels`. **Audio**:
   OfflineAudioContext with oscillator, dynamics compressor and analyser,
   seeded ±0.0001 noise. **Fonts**: width-measurement detection from
   persona font metric tables, with a bundled table set per OS.
4. **No tells.** No `navigator.webdriver`, no CDP objects in the page
   world, no `Error.stack` frames that reveal injected code.
5. `LightBrowser.fingerprintReport` CDP method and a
   `lightbrowser fingerprint-report` command that diff the live surface
   against the persona, used as a CI gate.

### M4b Firefox family (~4–6 weeks, parallel from M3 on)

Done when the existing `persona_v1` Firefox 128 profiles pass the M4
checks on the Firefox build.

1. SpiderMonkey backend (`mozjs`) behind feature `firefox`, same `JsEngine`
   trait. Own CI job with a warm cache; it must never slow Chrome work.
2. Gecko IDL set through the same codegen (`InstallTrigger`,
   `mozInnerScreenX`, no `userAgentData`).
3. Close the ADR 0003 TLS gaps: BoringSSL patches for
   `delegated_credentials`, `record_size_limit`, ffdhe groups, Firefox
   extension order; `pre_shared_key` via session reuse. Re-capture Firefox
   128 (or the current ESR) on loopback with `lb-fptest capture` from a
   stock Firefox and update the profiles.
4. Firefox header order, `Accept` values, H2 PRIORITY frames,
   `performance.now` at 1 ms, `navigator` differences.

### M5 Behavior + SDK (~2–3 weeks)

Done when `page.human_click` produces event streams the detection engine
scores as human.

1. Vendor `behavior/` into `lb-behavior`; replay generated events through
   the `Input` pipeline on the real clock.
2. `LightBrowser.humanClick / humanType / humanScroll / setPersona`.
3. `sdk/python/lightbrowser`: launch binary with persona, wait for CDP,
   return a stock Playwright browser, add `human_*` and
   `verify_fingerprint`. Port the useful parts of the GeckoPhantom SDK and
   its T-001..T-007 scenarios; drop ShadowChannel.

### M6 Performance (~2–3 weeks)

Done when the budgets in `ARCHITECTURE.md` hold and results are in
`bench/results/`.

1. V8 startup snapshot with all bindings (realm creation < 5 ms).
2. `bench/` runner: Lightpanda crawl benchmark against local pages vs
   Lightpanda and headless Chrome; idle RSS; binary size; pinned versions;
   median and spread.
3. Memory tuning: node size, atom interning, dropping layout under
   pressure, JS heap caps, LRU cache bounds. mimalloc, fat LTO already on.
4. Optional `Page.captureScreenshot` (paint through tiny-skia) and
   service workers.

## 3. Order

```
M2 DOM+V8 ──► M3 Layout+CDP ──► M4 Chrome FP ──► M5 Behavior+SDK ──► M6 Perf
                    │                                  ▲
                    └──── M4b Firefox (SpiderMonkey) ──┘  parallel from M3
```

Inside M2, the V8 backend and the arena DOM are independent and can run
in parallel; codegen needs both; the event loop needs all three.

## 4. Housekeeping

- Add `persona_v2.schema.json` in M2 rather than M4 so later crates use
  the final persona ids from the start.
- CI: cache the V8 prebuilt; separate slower SpiderMonkey job; Playwright
  suite job once M3 lands.
- Chrome version policy: re-capture the TLS profile and bump
  `Persona::default_chrome()` on each Chrome stable, gated by the
  fingerprint test. Automate `tests/README.md`.
- In `stealth-browser`, `scripts/examples/*` and the README Docker example
  target third-party sites, which both CLAUDE.md files forbid. Remove or
  gate them. Decide, in an ADR in both repos, that the Firefox fork is
  frozen and LightBrowser M4b takes over Firefox personas.
- Update `README.md` status line (still says M0) and `ROADMAP.md` on each
  milestone close.

## 5. Risks

| Risk | Effect | Mitigation |
|---|---|---|
| WebIDL breadth (1,000+ interfaces) | M2/M4 slip | Stubs for all, implementations by what the detection engine and local pages touch; coverage number in CI. |
| V8 snapshot with generated bindings | Realm creation > 5 ms | Ship without snapshot first; snapshot is M6. |
| `mozjs` build time and ESR pinning | M4b slips | Own CI job, warm cache, never blocks Chrome. |
| Layout fidelity | Sizes differ from Chrome | Detection relies on font metrics and probes; cover those exactly, accept the rest. |
| V8 prebuilt download in CI | Builds fail offline | Cache the artifact; pin the crate version. |

## 6. After the browser: agent layer and labeled traffic

Not part of finishing the browser, but the reason it exists. Once M3
lands, an LLM-driven agent (Python, over Playwright/CDP, so it also runs
on headless Chromium and GeckoPhantom) can drive LightBrowser against our
own local test sites, in naive and human-like (M5) modes, writing labeled
session records that the detection engine trains and evaluates on. This
needs its own ADR and milestone (M7/M8) and is deliberately out of scope
for M2–M6.

## 7. First two weeks

1. M2 task 1: V8 backend spike behind `lb-js` (`new_realm`, `evaluate`,
   heap cap) with the CI artifact cache.
2. M2 task 2 in parallel: arena DOM with the html5ever sink and
   `querySelector`; `fetch --dump html` switched from the tag stripper to
   the real DOM.
3. `persona_v2.schema.json` and the v1 loader in `lb-persona`.
4. Quarantine the third-party example scripts in `stealth-browser`.

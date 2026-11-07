# Architecture Decision Records

Short records of the main decisions behind LightBrowser. Each has a Title,
Status, Context, Decision, Alternatives considered and Consequences.
`../ARCHITECTURE.md` and `../ROADMAP.md` summarize how they fit together.

| ADR | Title | Status |
|---|---|---|
| [0001](0001-rust.md) | Rust for everything we write | Accepted (2026-11-02) |
| [0002](0002-embedded-js-engines.md) | Embed the JS engine: V8 for Chrome, SpiderMonkey for Firefox | Accepted (2026-11-02) |
| [0003](0003-boringssl-h2-fork.md) | BoringSSL and a forked h2 for network fingerprints | Accepted (2026-11-02) |
| [0004](0004-lazy-layout.md) | Lazy style and layout, CPU canvas, no painting by default | Accepted (2026-11-02) |
| [0005](0005-cdp.md) | CDP as the control API for both families | Accepted (2026-11-02) |
| [0006](0006-browser-families.md) | Two browser families, one binary per family | Accepted (2026-11-02) |
| [0007](0007-local-detection-bench.md) | Our own local detection bench is "our detection engine" | Accepted (2026-11-03) |
| [0008](0008-host-object-model.md) | Host objects are `(kind, id)` pairs with a per-realm wrapper cache | Accepted (2026-11-03); node refs and node wrappers superseded by 0017 |
| [0009](0009-cdp-isolated-worlds-and-emulation.md) | CDP bindings only in isolated worlds; Emulation must match the persona | Accepted (2026-11-03) |
| [0010](0010-own-box-tree-and-font-tables.md) | Our own box tree over taffy, and font metric tables compiled in | Accepted (2026-11-03) |
| [0011](0011-per-destination-headers.md) | Request headers per destination, captured from local Chromium | Accepted (2026-11-03) |
| [0012](0012-frames-as-realms.md) | Frames are realms in the page's engine, built on the spot (`RealmOps`) | Accepted (2026-11-03); cross-frame gaps closed by 0017 |
| [0013](0013-startup-snapshot-and-size-budget.md) | V8 startup snapshot made in memory; binary budget follows the V8 floor | Accepted (2026-11-03) |
| [0014](0014-on-demand-page-screenshots.md) | Page screenshots painted on demand from the layout | Accepted (2026-11-03); scope extended by 0020 |
| [0015](0015-os-personas.md) | OS personas: one OS axis, read through accessors | Accepted (2026-11-03) |
| [0016](0016-lazy-interface-objects.md) | Lazy interfaces and interface objects; hot code packed at link time | Accepted (2026-11-03) |
| [0017](0017-cross-document-nodes-and-detached-frames.md) | Nodes carry their document; one node wrapper per world; removed frames stay usable | Accepted (2026-11-03) |
| [0018](0018-connection-reuse.md) | Connection reuse as Chrome does it: per-context pool, HTTP/1.1 keep-alive, one HTTP/2 session per group, TLS resumption, network thread | Accepted (2026-11-04) |
| [0019](0019-style-sheet-loading-and-cssom.md) | Style sheets load as Chrome loads them: script- and parser-blocking sheets, `@import`, CSSOM | Accepted (2026-11-04) |
| [0020](0020-css-background-images-and-box-decorations.md) | CSS background images load like Chrome's; screenshots paint backgrounds, radius and shadows | Accepted (2026-11-04) |
| [0021](0021-workers.md) | Dedicated and shared workers on their own threads and isolates; worker globals from a Chrome capture; V8 structured clone, message ports | Accepted (2026-11-04) |
| [0022](0022-service-workers.md) | Service workers and the Cache API, in memory per context: registry and jobs in the page layer, versions as workers, fetch interception, clients | Accepted (2026-11-04) |
| [0023](0023-spare-realms-and-realm-cost.md) | Cheaper window realms (per-interface plans, engine extras deleted by name) and spare realms made while the worker waits; one matching context per element with a selector pre-check | Accepted (2026-11-04) |
| [0024](0024-image-reuse-within-a-document.md) | Images reused within a document as Blink does: one request per URL in flight, the list of available images, current image kept while a new source loads | Accepted (2026-11-04) |
| [0030](0030-software-webgl.md) | Software WebGL in `lb-gl`: own GLSL ES 1.00/3.00 compiler with ANGLE's messages, a 16-lane interpreter, the rasterizer with multisampling, depth, stencil, blending, textures and framebuffers | Accepted (2026-11-04) |
| [0029](0029-workers-as-cdp-targets.md) | Workers as CDP targets (auto-attach with `waitForDebuggerOnStart`, Runtime on the worker thread, console, errors, Network, `Inspector.workerScriptLoaded`), `fromServiceWorker`, idle service workers stopped after 30 s unless a CDP session is attached |
| [0028](0028-indexeddb.md) | IndexedDB in memory per browser context and origin: one `Send` backend (connections, open/delete queue, scheduling, rollback copies), replies as window tasks in backend order, activation until the next task boundary and auto-commit, Chrome's checks and texts |
| [0027](0027-proxies.md) | Proxies per browser context: HTTP `CONNECT` tunnels (direct ClientHello/H2), forward requests with Chrome's headers, SOCKS5 with remote DNS, Chrome's `407` round (configured credentials or CDP `Fetch.authRequired`), Chrome's bypass rules; Chrome's header set for non-trustworthy `http:` destinations |
| [0026](0026-shadow-dom-and-custom-elements.md) | Shadow roots as arena nodes of the host's document, slot assignment on demand, flat tree for style/layout/hit testing, Chrome's event retargeting and `slotchange` timing; custom elements with reactions derived from mutation records, the parser pausing at possible custom elements, customized built-ins and form-associated elements |
| [0025](0025-web-crypto-websocket-dynamic-import-sync-xhr.md) | Web Crypto on BoringSSL (Blink's normalization, Chromium's status texts, OS randomness), WebSocket through lb-net's own client (Chrome's handshake, http/1.1-only ClientHello), dynamic `import()` sharing the module map, `blob:`/`data:` in `fetch()`/XHR, synchronous XHR |
| [0031](0031-tcp-stack-fingerprint.md) | TCP/IP stack fingerprint: socket options plus a netfilter shim | Accepted (2026-11-03) |
| [0032](0032-whole-connection-tcp-shaping.md) | Shape the whole connection, not only the SYN | Accepted (2026-11-03) |
| [0033](0033-public-data-source-of-truth.md) | Public data is the source of truth for TCP profiles | Accepted (2026-11-03) |

## Adding an ADR

1. Copy the format of an existing ADR. Use the next number: `NNNN-short-name.md`.
2. Keep it to 30-70 lines, in plain English.
3. Add a row to the table above, and to the Decisions table in
   `../ARCHITECTURE.md` if it changes the architecture.
4. Do not edit an accepted ADR to change its decision. Write a new ADR that
   supersedes it, and change the old one's status to
   `Superseded by NNNN`.

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
| [0014](0014-on-demand-page-screenshots.md) | Page screenshots painted on demand from the layout | Accepted (2026-11-03) |
| [0015](0015-os-personas.md) | OS personas: one OS axis, read through accessors | Accepted (2026-11-03) |
| [0016](0016-lazy-interface-objects.md) | Lazy interfaces and interface objects; hot code packed at link time | Accepted (2026-11-03) |
| [0017](0017-cross-document-nodes-and-detached-frames.md) | Nodes carry their document; one node wrapper per world; removed frames stay usable | Accepted (2026-11-03) |
| [0018](0018-connection-reuse.md) | Connection reuse as Chrome does it: per-context pool, HTTP/1.1 keep-alive, one HTTP/2 session per group, TLS resumption, network thread | Accepted (2026-11-04) |

## Adding an ADR

1. Copy the format of an existing ADR. Use the next number: `NNNN-short-name.md`.
2. Keep it to 30-70 lines, in plain English.
3. Add a row to the table above, and to the Decisions table in
   `../ARCHITECTURE.md` if it changes the architecture.
4. Do not edit an accepted ADR to change its decision. Write a new ADR that
   supersedes it, and change the old one's status to
   `Superseded by NNNN`.

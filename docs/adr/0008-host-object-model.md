# ADR 0008: Host objects are `(kind, id)` pairs with a per-realm wrapper cache

Status: Accepted (2026-11-03). Node refs (`document_slot`) and the
per-realm cache for nodes are superseded by ADR 0017: a node ref carries a
runtime-wide document key, and main-world realms share one wrapper per node.

## Context

Web APIs are implemented in Rust (`lb-webapi`) on top of `lb-dom`, `lb-css`
and `lb-layout`. JavaScript sees DOM nodes, the window and other platform
objects through wrappers owned by the engine. We need a model that:

- works the same for V8 and SpiderMonkey behind `lb-js::JsEngine`;
- gives `a === a` identity for the same node, as pages and fingerprinting
  code expect;
- keeps Rust ownership simple (no `Rc` cycles between the DOM and JS);
- lets an isolated world (CDP helper code) see the same nodes as the page.

## Decision

A host object is a `HostRef { kind: u32, id: u32 }`, chosen by the host and
stored in the internal fields of the JS wrapper. The engine knows nothing
else about it.

`lb-webapi::rt` uses three kinds:

| Kind | `id` | Meaning |
|---|---|---|
| `K_NODE` (1), with `document_slot << 8` | arena `NodeId` | A DOM node. Slot 0 is the page document; `DOMParser` and `createHTMLDocument` documents get further slots. |
| `K_WINDOW` (2) | 0 | The realm's global (`WINDOW_REF`). |
| `K_OBJ` (3) | index into the window's object table | Everything else: events, collections, `URL`, `Headers`, observers, ranges, canvas contexts and so on. |

- The engine keeps one wrapper cache, keyed by `(RealmId, HostRef)`.
  `Engine::wrap` and `CallCtx::ret` with `Arg::Host` return the same JS
  object for the same pair in the same realm. Another realm (an isolated
  world) gets its own wrapper for the same node, so page code and helper code
  never share JS objects, only host state.
- The wrapper is created from the interface's instance template, so the
  prototype chain, property order and descriptors come from the generated
  interface table.
- Host functions find their `Window` from the calling realm. Rust code holds
  JS values only through engine `Handle`s.
- A dead host object is dropped with `Engine::forget_host`; a later wrap makes
  a fresh wrapper. Dropping a realm drops its wrappers.
- Cache entries are strong V8 `Global`s. **There are no weak wrappers.** A
  host object lives until its window is dropped (each navigation makes a new
  `Window` and realm).

## Alternatives considered

- **`Rc<RefCell<Node>>` stored in the wrapper.** Easy identity, but cycles
  between nodes and wrappers, and the DOM becomes tied to one engine's GC.
- **Weak wrappers with finalizers now.** Needed for long-running pages, but
  V8 weak callbacks run at arbitrary times and need a safe "node still
  referenced from Rust" story. Deferred until the memory budget work (M6).
- **A raw pointer in the internal field.** Fewer lookups, but it gives up safe
  Rust for speed and makes cross-realm lookups harder.

## Consequences

- Wrapper identity and cross-world sharing come for free. A SpiderMonkey
  backend only has to implement `wrap`, `unwrap_host` and `forget_host`.
- Memory grows with the number of distinct objects touched during a page's
  life. Fine for validation runs, not for a day-long page.
- Open: weak wrappers need a liveness hook in `Window` and a V8 weak callback
  that queues `forget_host` for the event loop to run.
- Open: `kind` packs the document slot into bits 8 and up; kinds above 255
  need a wider type.
- Open: listeners registered from an isolated world are called with wrappers
  of the dispatching realm (known gap in `rt/mod.rs`).

# ADR 0017: Nodes carry their document; removed frames stay usable

Status: Accepted (2026-11-03). Supersedes the node part of ADR 0008 (node
host refs and their wrapper cache) and the cross-frame gaps of ADR 0012.

## Context

`tools/surface_diff` found three Chrome differences around frames, all
reachable by pages:

- A node host ref named a per-window document slot (`K_NODE | slot << 8`),
  and host functions resolved it in the calling realm's window. A method
  of one frame called on another document's node acted on the wrong node:
  a frame's `nodeName` getter on the top `body` returned `"TITLE"`, and
  `Node.prototype.appendChild` on frame nodes panicked the worker (index
  out of bounds in `lb-dom`; the release build aborts).
- A removed iframe's realm was dropped at once: its window's accessors
  threw `Illegal invocation`, and lazy interface objects were still made.
- `Image.prototype !== HTMLImageElement.prototype`.

## Decision

- **Document keys.** A node ref is `K_NODE | key << 8`, where `key` is a
  runtime-wide document key (24 bits). The runtime maps keys to documents
  (weakly) and their owning window. Inside a window code still uses slots
  (0 is the main document): `Window::decode_node` turns a ref into a slot,
  giving another window's document a slot of its own (the same shared
  `Rc<RefCell<Document>>`), and checks that the node exists, so no stale
  or foreign id reaches `lb-dom`. `Window::node_ref` encodes.
- **Receiver's window.** A host function runs with the window of its
  receiver (`this`'s document for nodes; the creation realm for other
  objects and the global), so per-window state (listeners, form values,
  layout, object table) is the right one. Host objects it returns are
  wrapped in the receiver's realm when a main-world realm calls into
  another one (Blink's holder creation context); exceptions stay in the
  calling realm. Interceptors use the holder's realm.
- **One wrapper per node per world.** Node refs are shared hosts
  (`Engine::set_shared_hosts`): all main-world realms share one wrapper
  per node, made in the realm that first wraps it; isolated worlds keep
  their own. Other host kinds stay per realm.
- **Adoption keeps identity.** Each document is its own arena, so moving a
  node to another document copies the subtree, detaches the original and
  rehosts the wrappers (`CallCtx::rehost`, every realm), listeners, inline
  handlers, form values and selections to the copies. The source window
  processes the removal at once.
- **Removed frames stay usable.** A removed iframe's window keeps its
  realm, closed and marked detached (`RealmOps::detach_realm`), until the
  page navigates, at most 16 per page. Values follow Chrome (checked
  against Chrome for Testing 153): untouched interface objects read
  `undefined`, `parent`/`top`/storage are `null`, `closed` is true,
  geometry and timers are 0, the user agent is empty, `history` throws.
- **Legacy factory functions** take their interface's prototype object
  (`InterfaceSpec::prototype_from`); the returned element gets
  `new.target`'s prototype.

## Alternatives considered

- **One arena for all documents of a page.** Adoption without copying,
  but `lb-dom`, the parser, selectors and layout assume one document per
  arena.
- **Window id plus slot in the ref.** Keeps literal slot 0 everywhere but
  limits a window to 256 documents and still needs the cross-window
  lookup.
- **A per-realm wrapper per node (as before).** Breaks `appendChild(n) ===
  n` across frames and gives two objects for one node.
- **Dropping removed frames' realms (as before).** Scripts holding the
  window get `Illegal invocation` where Chrome answers.

## Consequences

- A per-call receiver check (`this_realm`) on every host call; the bench
  (ROADMAP "Frames fidelity") shows no measurable cost.
- Per-node state outside the moved set (canvas contexts, image loads,
  mutation observers' targets) is not carried over on adoption; the
  adopted copy starts fresh there.
- Removed frames hold their realm until navigation (16 at most, oldest
  dropped first; a dropped one throws `Illegal invocation` again).
- Still different from Chrome on a detached window: the `CSS` namespace is
  installed eagerly (Chrome: `undefined`), and globals LightBrowser lacks
  everywhere (`crypto`, `customElements`, `indexedDB`, ...) stay missing.

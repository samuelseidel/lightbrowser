# ADR 0012: Frames are realms in the page's engine, built on the spot

Status: Accepted (2026-11-03). The cross-frame node gap below is closed by
ADR 0017 (document keys in node refs, one node wrapper per world, removed
frames stay usable).

## Context

Fingerprint scripts create an `about:blank` iframe and compare its
`navigator`, `screen`, global property list and `Function.prototype.toString`
with the top window's (the bench's `iframe_mismatch` and `extra_globals`).
They read `contentWindow` right after `appendChild`, inside the same script,
as Chrome allows: Chrome creates the initial empty document synchronously
and fires `load` for a `src`-less iframe during insertion. Until now
`contentWindow` was `null`.

A host function cannot reach the `lb_js::Engine` (the running callback
borrows its isolate), yet that is where realms are made. Cross-origin frames
must also be unreachable from the parent, as in Chrome.

## Decision

- **One realm per frame document, same engine and thread.** Each frame is
  its own `lb_webapi::rt::Window` (own document, tasks, timers, clock) with
  a main-world realm built from the same bindings and the same
  `Arc<PersonaProfile>`. The page's event loop runs every window of the
  tree. `lb_webapi::rt::frames` owns the tree; `lb-page` loads frame
  documents (`src`, `srcdoc`, script navigation) with the top-level loader
  and reports frames to CDP.
- **Realms from inside callbacks: `lb_js::RealmOps`.** The realm-building
  operations (new realm, evaluate, call, global, properties, wrap, native
  functions, ICU defaults, security token) are a trait implemented by
  `Engine` and by `CallCtx` (on a nested handle scope of the running
  callback). Realm setup in `lb-webapi` takes `&mut dyn RealmOps`, so the
  same code runs from the event loop and from `appendChild`.
- **Origins through V8 security tokens.** Each realm's token is its
  window's serialized origin (`about:blank`/`about:srcdoc` take their
  creator's). Same-origin windows reach each other directly; V8 denies
  access between different tokens. For a cross-origin frame,
  `contentWindow` (and `parent`, `top`, `frames[i]` after first use) is a
  native `Proxy` built in the accessor's realm: `postMessage`, `closed`,
  `length`, `window`, `self`, `frames`, `top`, `parent` work, everything
  else throws Chrome's `SecurityError`. `contentDocument` and
  `frameElement` are `null` across origins.
- **CDP.** Child frames get `Page.frameAttached`, `frameNavigated` (with
  `parentId`), lifecycle events with their own loader id,
  `frameStoppedLoading` and `frameDetached`; a main-world context and the
  new-document worlds per frame document; `Page.getFrameTree` children;
  `Page.createIsolatedWorld` by frame id; `frameId` on `<iframe>` nodes in
  `DOM.describeNode`. Playwright frame locators work.

## Alternatives considered

- **Lazy `contentWindow`, built on the next event-loop turn.** Breaks the
  common `appendChild` + `contentWindow` pattern: the probe sees `null`.
- **A spare realm prepared before scripts run.** No re-entrancy needed, but
  every navigation pays for a second realm (about 30 ms before the M6
  snapshot) and two iframes in one script still fail.
- **V8 access-check callbacks on the window template (Chrome's way).**
  Exact `WindowProxy` semantics, but every window global then goes through
  access checks, and isolated worlds and native inspection would need
  care. The proxy plus tokens covers what pages observe.
- **A separate engine per frame.** Cross-frame object identity
  (`frames[0] === f.contentWindow`, `e.source`) would be impossible.

## Consequences

- `iframe_mismatch` and `extra_globals` pass; the WPT subset and the
  Playwright suite run with frames.
- A frame costs a realm (same cost as a navigation). Script-created frames
  build it inside the inserting callback.
- Gaps: host refs were per window, so nodes of one frame passed to
  another frame's DOM methods were not supported (fixed by ADR 0017), and
  `DOM.*`/`Input.*` on nodes inside frames (clicks into frames) are
  refused for now; no named frame
  access (`window.frameName`), `sandbox`, `<frame>`/`<object>` browsing
  contexts; `postMessage` clones through JSON; network events of frame
  requests are attributed to the main frame.

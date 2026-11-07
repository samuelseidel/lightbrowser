# ADR 0035: API gaps found by public bot-detection pages

Status: Accepted (2026-11-07).

## Context

At the user's request, a release build with the macOS 26 persona was
run against public community bot-detection pages (sannysoft, rebrowser,
CreepJS, deviceandbrowserinfo, incolumitas, browserscan, browserleaks,
tls.peet.ws). Automation, headless, TLS (JA4 equal to the capture), HTTP/2
and TCP checks passed. What failed came from page-visible API gaps: every
unimplemented WebIDL operation throws `NotSupportedError`, which Chrome
never does, and a few behaviours differed (ROADMAP "Public bot-detection
pages"). Each finding is reproduced on loopback and checked against local
Chrome (153 headless shell or 156), never against those sites.

## Decision

- **Crash and load fixes.** `import_specifiers` scans bytes (a multi-byte
  character no longer panics the worker). A frame navigation keeps its
  parent from `start`, so an iframe removed before its navigation runs
  still settles the parent's `pending_loads` (`load` fired never before).
- **Host errors stay private.** `JsErrorKind::Host` (unknown realm, stale
  handle) is never reported to the page: no `error` event, console
  message or CDP exception.
- **Blink's `NavigationRateLimiter`.** 200 history/location navigations
  per 10 s per window; later ones return silently with one warning.
- **`DOMTokenList.supports`** with Blink's `relList` (link, a/area) and
  `sandbox` token sets, as recorded.
- **`Range`, `StaticRange`, `Selection`** (`rt/range.rs`,
  `rt/selection.rs`): live boundary points from mutation records (dom.rs
  leaves offset notes for character-data edits), Blink's rect algorithm
  on our layout (`LayoutEngine::text_rects`), one selection per document.
- **SVG geometry** (`rt/svg.rs`): `getBBox` with Skia's tight path bounds,
  text content methods on the persona's font tables, shape lengths, CTMs,
  `SVGRect`/`SVGPoint`/`SVGMatrix`. Outer `svg` is a replaced box; SVG
  presentation attributes feed the cascade.
- **`CSS.supports` and `@supports`** share `lb-css` `supports.rs`; property
  names and accepted value kinds, keywords and functions per property are
  data recorded from Chrome 153 (`supports_data.rs`,
  `tools/css_supports/record.py`); lb-css's own parser decides the values
  it implements. `CSS` is not exposed in workers (as in Chrome).
- **`BroadcastChannel`** (`rt/broadcast.rs`): one process-wide table of open
  channels keyed by browser context (a non-owning token of the context's
  IndexedDB backend), origin and name; each post is one structured clone
  delivered as a task through the owners' cross-thread inboxes, so
  windows, frames and all worker kinds reach each other.
- **`import()` resolves against the calling script.** The referrer V8
  passes (the script's origin URL) is the base for external modules and
  classic scripts; inline scripts and CDP evaluations use the document's
  base URL. Before, only modules already in the module map counted, so a
  top-level module's lazy imports loaded from the document's directory.
- **Legacy `navigator.getUserMedia` / `webkitGetUserMedia`** check their
  arguments in Chrome's order and with its messages, return `undefined`
  and call the error callback only when the persona denies the
  permission (otherwise the prompt stays unanswered, as for
  `mediaDevices.getUserMedia`).
- **`local()` font faces per OS release** (`profiles/local_fonts/`): face
  names Chrome resolves on a default install, picked by `os.name` and
  `os.version`. Recorded for macOS 26 only.

## Consequences

- CreepJS's svg, rects, worker and headless sections run; browserscan
  renders; sannysoft and incolumitas reach `load`.
- Open: no Content Security Policy is enforced (rebrowser `bypassCsp`);
  third-party storage is not partitioned (rebrowser
  `storagePartitioning`); canvas readback noise is still detected as a lie by CreepJS
  ("pixel data modified"); `local()` lists for other OS releases;
  `CSS.supports` value grammar is approximate (`font: bold` and
  `text-shadow: red` are true, Chrome false); BroadcastChannel ignores
  third-party partitioning; `Selection.modify` beyond character/word.

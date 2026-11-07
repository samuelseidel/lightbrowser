# ADR 0046: Performance Timeline from the loader's own measurements

Status: Accepted (2026-11-07).

## Context

`performance.getEntries()`, `getEntriesByType()` and `getEntriesByName()`
returned empty arrays, `mark`, `measure` and `PerformanceObserver` did not
exist, and `performance.timing` and `chrome.loadTimes()` were made up
(the fetch placed in the first four milliseconds after the window was
made). Real pages and libraries (web-vitals, RUM scripts, frameworks) read
the timeline, and a page with no entries at all differs from Chrome in
a way scripts can see.

## Decision

Implement the Performance Timeline as Chrome 153 does, from data the
engine already has. Every shape was recorded from Chrome 153 on loopback
pages (`crates/lb-page/tests/perf/`: `serve.py`, `chrome.mjs`, 15 cases,
`chrome.json`; `fulfill.mjs` for CDP-fulfilled answers) and the tests
compare logs of names, key sets and relations, never absolute times.

**Measurements** (`lb-net` `timing.rs`). A `FetchTiming` rides on every
`lb_net::Response`: when the fetch began, the redirect span (first hop
start to the end of the last redirect response) and the URL and
`Timing-Allow-Origin` of each redirect, the final hop's start, the setup
of the connection opened for it (DNS start and end, TCP connect, TLS
start, end), when the request was written, when the response head and
body had arrived, and the body size on the wire and decoded. A reused
connection or HTTP/2 session has no setup (`conn: None`): the page layer
then reports `domainLookup*` and `connect*` equal to `fetchStart`, as
Chrome does. The pool measures each connection once, when it opens it
(`Conn::timing`). No extra system calls: the instants are taken at steps
that already happen.

**Entries** (`lb-webapi` `rt/perf.rs`, `Obj::Perf`).

- *Resource.* `Window::net_fetch`, `sendBeacon` and `importScripts` wrap
  the request's future (`perf::track`); when it ends an entry is made
  from the `NetTiming` (`NetResponse::timing`, `None` for answers that did
  not come from the network stack: a CDP `Fetch.fulfillRequest`, a service
  worker's). The initiator type comes from the request
  (`NetRequest::initiator_type`, else from the destination: `script`,
  `link`, `css` for `@import` and background images, `img`, `iframe`,
  `fetch`, `xmlhttprequest`, `beacon`, `other` for `importScripts` and a
  dedicated worker's script on its creator's timeline); `renderBlockingStatus`
  from `NetRequest::render_blocking` (a parser-inserted style sheet, the
  imports of one, a parser-blocking script in the head). The cross-origin
  rules are Blink's: each cross-origin hop of the redirect chain needs
  `Timing-Allow-Origin` (`*` or the document's origin) for the timing
  steps, sizes, `nextHopProtocol` and `serverTiming` to be exposed (else 0
  and empty; `startTime`, `fetchStart`, `responseEnd` stay); `responseStatus`,
  `contentType` and `contentEncoding` are exposed unless the response is
  opaque (cross-origin `no-cors`) or the CORS check failed. A failed
  request still gets an entry, all steps 0. `transferSize` is the encoded
  body size plus 300, as Chrome reports on loopback. A CDP-fulfilled
  answer gets an entry with an empty `nextHopProtocol`, every step at the
  moment it arrived and sizes from the body (recorded from Chrome,
  `tests/playwright/perf.spec.mjs`).
- *Buffer.* Sorted by start time, 250 entries; the spec's secondary
  buffer, `resourcetimingbufferfull` (a task, fired once per excess entry
  until the handler makes room or the entry is dropped),
  `setResourceTimingBufferSize`, `clearResourceTimings`.
- *Navigation.* One entry per document (name = the document's URL at
  creation, `startTime` 0), built lazily from `NavInfo` that the page
  layer gives the window (`Window::set_nav_info`: the document request's
  `FetchTiming`, the navigation type, status and headers) and from points
  the load marks (`Window::nav_mark` for `domContentLoadedEvent*` and
  `loadEvent*`, `set_ready_state` for `domInteractive` and `domComplete`).
  The window's clock starts at the navigation start (`Window::new_at`), so
  the document's own fetch has no negative times and `performance.now()`
  begins a few milliseconds in, as in Chrome. `performance.timing`,
  `performance.navigation`, `chrome.loadTimes()` and the entry are read
  from the same data, so they agree. `type` is `navigate`, `reload`
  (`location.reload()`, `Page.reload`) or `back_forward`
  (`history.back()` past a document's own entries now moves through the
  page's top-level session history, `PageState::nav_history`, instead of
  doing nothing). Iframes get their own entry and an `iframe` resource
  entry on the parent's timeline.
- *Paint.* The engine does not paint. `first-paint` and
  `first-contentful-paint` are synthesized at the first frame boundary
  after the document is interactive (or, for a page that fills in later,
  after a script mutation) and it has text, an image, a canvas, a video or
  an SVG in the body: `Window::arm_paint` sets a due time on the 60 Hz
  frame grid, the page loop runs `Window::run_paint` then (it is in the
  loop's deadline, so a virtual clock reaches it) and both entries get that
  frame's time. Pages without paintable content get none, as in Chrome.
- *User Timing.* `mark`, `measure` (every argument form, with Blink's
  texts and exceptions: reserved `PerformanceTiming` names, negative
  times, `start`/`end`/`duration` combinations, timing attributes read as
  whole-millisecond differences), `PerformanceMark` constructor,
  `clearMarks`, `clearMeasures`, `detail` as a structured clone made when
  the entry is made and rebuilt once on first read.
- *Observers.* `PerformanceObserver` with `type` or `entryTypes`
  (Chrome's exclusive modes and their `InvalidModificationError` texts,
  kept across `disconnect`), `buffered` replay for `type`, `takeRecords`,
  `disconnect`, `supportedEntryTypes` (frozen, Chrome's order; three types
  in workers), delivery in a task with `(list, observer, options)`; the
  first callback after each `observe()` carries `droppedEntriesCount: 0`,
  as recorded.
- *Also.* `visibility-state` (one entry, `visible`), `eventCounts` (36
  event types, all 0), `navigationId`, `Performance.toJSON` with the live
  `timing` and `navigation` objects.
- *Workers.* Dedicated, shared and service worker scopes have their own
  timeline (marks, measures, resources, observers; entry types `mark`,
  `measure`, `resource`). `importScripts` and `fetch` make entries; a
  service worker's script cache gives its `importScripts` entries sizes 0.
  A dedicated worker's script is an `other` entry on its creator's
  timeline (a `RemoteTask`).

**Cost.** A page that never reads the timeline pays one `ResData` and
`Rc<Entry>` per request plus the observer walk (empty) and the first-paint
scan; entry objects are made when script asks. `PerfState` is a few empty
vectors; the registrations are table entries like any interface's. See the
ROADMAP note for the measured realm time and binary size.

## Skipped

Event Timing (`event`, `first-input`, `interactionCount` stays 0),
`layout-shift`, `largest-contentful-paint`, `longtask`,
`long-animation-frame`, `element`, `soft-navigation` and
`interaction-contentful-paint` accept `observe()` (so detectors see
Chrome's `supportedEntryTypes`) but never produce entries: there is no
layout-shift or long-task measurement worth reporting without a renderer.
Web fonts are not loaded, so there is no font resource entry.
`unloadEventStart/End`, `activationStart`, `notRestoredReasons`
and the worker-routing fields read 0 or null. `nextHopProtocol` is `h2` or
`http/1.1` (no QUIC). Back/forward navigations re-fetch the document (no
bfcache).

## Alternatives considered

- *Synthesizing every timing from one number.* What `performance.timing`
  did; it cannot give a reused connection, redirects, or cross-origin
  zeroing, which scripts compare.
- *A resource entry per load event.* The network future knows the
  response end; the load event comes later and not every request has one.
- *Painting for real to time first paint.* Out of scope (ADR 0014 renders
  on demand).

## Consequences

- Scripts see a Chrome-shaped timeline whose values obey Chrome's
  relations (startTime <= fetchStart <= responseEnd, sizes, TAO zeroing).
- A cross-document `history.back()` now navigates (before, it did nothing).
- `performance.now()` at the first script is no longer near 0 for network
  documents (it counts from the start of the fetch).

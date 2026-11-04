# ADR 0021: Dedicated and shared workers on their own threads and isolates

Status: Accepted (2026-11-04). Closes the M2/M4 open items "no workers"
(`worker_mismatch` saw `Illegal constructor`) and "structured clone through
JSON".

## Context

`Worker`, `SharedWorker`, `MessageChannel` and `MessagePort` were stubs,
`structuredClone` and `postMessage` cloned through JSON, and nothing could
run in a worker global. Bot detectors start a worker and compare its
`navigator` and UA client hints with the page's: a persona patched only
into the page differs there. Local Chrome for Testing 153.0.8010.12 on
loopback gives the ground truth: the dedicated and shared worker globals
(`tools/dump_worker_surface.js`, fixture
`tests/fixtures/chromium_153_worker_surface.json`: own properties in order,
interface objects, member orders) and 10 behaviour cases (`lb-page`
`tests/workers/`, logs in `chrome.json`): messages and their events,
errors and their locations, terminate and close, `importScripts`, `fetch`
and XHR with the page's cookies, transfers, `blob:` / `data:` / module
workers, shared workers, `structuredClone`.

## Decision

- **One thread and one engine per worker**, started only when a page
  creates one (`lb-page` `worker.rs`): a `current_thread` tokio runtime,
  a V8 isolate, `Bindings::define_worker` for the scope's global and a
  worker-scope `rt::Window` without a document, so timers, `fetch`, XHR,
  events, `URL`, encoders and `performance` are the window's code. Shared
  workers are kept per browser context by (script URL, name) and end with
  the context; dedicated workers end with their creating document.
- **Bindings from the capture**: `lb-webidl-gen` emits `WORKER_SCOPES`, the
  scope's own properties in Chrome's order plus order overrides for the 16
  interfaces whose members differ in workers (`Performance` without
  `timing`, `XMLHttpRequest` without `responseXML`, ...). Worker-only
  interfaces (`WorkerNavigator`, `WorkerLocation`, `WorkerGlobalScope`)
  take their order from it.
- **Persona**: the worker receives the creating window's `Arc<Persona>`
  and `Arc<PersonaProfile>`; `WorkerNavigator` getters are the window's
  `Navigator` getters, and the realm sets the same ICU time zone and
  locale. Values equal the page's by construction.
- **Messages** are V8 structured clones (`lb_js::Serialized`, V8's
  `ValueSerializer`): V8's wire format and error texts; `ArrayBuffer`s
  transfer (bytes copied, sender detached); `MessagePort`s transfer;
  `Blob`s clone; other host objects throw Chrome's `DataCloneError`.
  Windows on other threads get tasks through an inbox (a channel pumped
  into the posted-message task source), made on first use.
- **Network**: requests use the context's `fetch` (headers, cookie jar,
  connection pool); `importScripts` blocks the worker thread on a helper
  thread's runtime.
- **Errors**: uncaught errors fire at the scope, then (not canceled) at
  the `Worker`; locations come from the error's creation site and
  `ErrorEvent.error` is the thrown value, as in Blink (also for windows).

## Alternatives considered

- **Run workers as realms on the page's thread.** No threads or extra
  isolate, but a busy worker blocks the page, `Atomics.wait` and timing
  differ, and a detector can see the shared event loop.
- **A pool of worker threads.** Fewer threads, but isolates would share
  threads with unrelated pages; one thread per worker matches Chrome and
  costs nothing until a page makes a worker.

## Consequences

- The worker globals match the capture (`surface_matches_chrome`); the 10
  behaviour cases match Chrome line for line; `fingerprint-report` checks
  42 worker values; the bench gains `worker_navigator_mismatch`.
- No change to start-up, realm creation or idle memory: no worker, no
  thread. A worker costs a thread and an isolate (a few MB).
- Not modelled: service workers, nested workers' CDP targets (workers are
  not CDP targets), dynamic `import()`, `OffscreenCanvas` transfer,
  synchronous XHR, worker script request headers (sent as a script
  request), `blob:` URLs in `fetch()`, `Atomics.wait` across workers
  (no `SharedArrayBuffer`).

# ADR 0022: Service workers and the Cache API, in memory per context

Status: Accepted (2026-11-04). Closes the M6 open item "service workers".

## Context

`navigator.serviceWorker`, `caches` and the service worker interfaces were
stubs: `register()` threw `Illegal invocation`, nothing could control a
page or answer its requests. Sites register service workers on first visit
and detectors read a service worker's `navigator` as they read a dedicated
worker's. Ground truth is local Chrome for Testing 153.0.8010.12 on
loopback: the `ServiceWorkerGlobalScope` surface
(`tools/dump_worker_surface.js`, `service` in
`tests/fixtures/chromium_153_worker_surface.json`: 318 own properties, 265
interface objects) and 11 behaviour cases (`lb-page`
`tests/service_workers/`, Chrome's logs in `chrome.json`): lifecycle and
event order, `clients.claim`, fetch interception, messages both ways, the
Cache API, scope rules and error texts, `update()` and waiting workers,
the update check after a navigation, `Clients`, failed installs, a page
served from the cache after a reload.

## Decision

- **Registry per browser context, in memory** (`lb-page` `sw.rs`):
  registrations, versions, the listed windows and the Cache API storage
  live in the context (`Arc<SwShared>`); nothing is written to disk, a new
  context starts empty and versions end with their context. Jobs
  (`register`, `update`, update checks) run one at a time and follow the
  Service Workers algorithms: fetch the script (2xx, JavaScript MIME type,
  maximum scope or `Service-Worker-Allowed`), compare it byte for byte
  with the newest version, run a new version, Install (`installing`, the
  job's promise, `updatefound`, `install` with `waitUntil`, `waiting`),
  Try Activate (waits while the active version controls a client, unless
  `skipWaiting()`), Activate (`active`, `activating`, new controller for
  the old version's clients, `ready`, `activate`, `activated`).
- **Windows see state through tasks**: the registry sends tasks to the
  inboxes of the windows of the origin (registration slots, worker states
  with `statechange`, `updatefound`, `controllerchange`, messages), so the
  order of events and promise resolutions is the order Chrome showed.
  Each window keeps one object per registration and per version
  (`reg.active === navigator.serviceWorker.controller`).
- **A version is a worker** (ADR 0021): own thread, `current_thread`
  runtime and V8 isolate, `ServiceWorkerGlobalScope` from the capture,
  `WorkerNavigator` from the page's `Arc<Persona>`. Extendable events
  (`install`, `activate`, `fetch`, `message`) wait for their `waitUntil` /
  `respondWith` promises through a settle helper made from the realm's own
  `Promise` before the script runs.
- **Fetch interception**: a controlled window's requests (`fetch`, XHR,
  scripts, style sheets, images, CSS images, frame and top-level
  navigations) become `fetch` events once the version is `activated`;
  no `respondWith` goes to the network, `respondWith(response)` answers,
  redirects in the answer are followed through the worker, a rejected
  promise or `Response.error()` is a network error. Versions without a
  fetch handler are skipped. A controlled navigation schedules an update
  check 1.5 s later (Chrome: 1.5-2 s; dropped if the version is no longer
  the newest).
- **Clients**: windows are listed when controlled, when their origin has a
  registration at creation, or when they touch `navigator.serviceWorker`;
  `Clients.get/matchAll/claim`, `WindowClient` (`top-level` / `nested`,
  `visible`, focused top-level), `postMessage` both ways (the client
  message queue opens on `DOMContentLoaded`, `startMessages()` or
  `onmessage`).
- **Cache API** per context and origin, shared by pages and workers:
  `CacheStorage` and `Cache` with Chrome's ordering, `ignoreSearch` /
  `ignoreMethod` / `ignoreVary`, `Vary`, and Chrome's error texts.

## Alternatives considered

- **Run versions on the page's thread.** Fewer threads, but a version
  outlives the page that registered it and serves several pages; a
  separate thread matches Chrome's process model.
- **Persist registrations** (as Chrome does in a profile). Contradicts "no
  disk writes by default"; contexts are incognito-like, as Playwright's
  `newContext()` in Chrome.

## Consequences

- The 11 Chrome-recorded cases match line for line; the service worker
  global equals the capture (`surface_matches_chrome`); `fingerprint-report`
  checks 21 service worker values; the bench gains `sw_navigator_mismatch`.
- Nothing runs and nothing is allocated until a page registers a worker.
- Worker threads (dedicated, shared, service) hold their context weakly,
  so a context whose pages and CDP entry are gone drops and ends its
  workers (before, a running shared worker kept its context alive).
- Not modelled: stopping idle versions (Chrome stops them after 30 s and
  restarts on the next event; here a version runs until it is redundant or
  its context ends), worker clients (dedicated workers are not
  controlled), service workers as CDP targets and `fromServiceWorker` in
  CDP Network events (requests a worker answers are not reported),
  navigation preload, `importScripts` limits after install, Background
  Sync, Push and Notifications, the update check of imported scripts,
  `Location.reload()` as a reload (only `Page.reload` / CDP reload marks
  the request `no-cache`).

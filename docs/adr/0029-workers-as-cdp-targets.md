# ADR 0029: Workers as CDP targets; idle service workers stop

Status: Accepted (2026-11-04).

## Context

Workers (ADR 0021) and service workers (ADR 0022) ran but were not CDP
targets: Playwright's `page.workers()` stayed empty, `worker.evaluate`
and the `worker`/`serviceworker` events never came, worker console and
errors were lost, and requests a service worker answered had no Network
events. Service worker versions also ran forever, where Chrome stops an
idle version and starts it again (fresh globals) for the next event.
We recorded what Chrome for Testing 153.0.8010.12 sends to stock
Playwright 1.63 on loopback (`tests/playwright/workers/record.mjs`,
`chrome.json`) and when it stops an idle version (`record_idle.mjs`,
`idle_chrome.json`).

## Decision

- **Targets.** A worker thread whose browser context belongs to the CDP
  host announces itself once its realm exists and before its script runs
  (`lb-page` `cdp/workers.rs`): a Chrome-shaped target id, type `worker`,
  `shared_worker` or `service_worker`, Chrome's title (name, URL, or
  `Service Worker <url>`), `parentId`/`parentFrameId` for dedicated
  workers, and one execution context (id 1, origin = script URL, name,
  no `auxData`). `PageEvent::WorkerCreated` / `WorkerDestroyed` carry it
  to the server (`lb-cdp`).
- **Auto-attach.** The server attaches dedicated workers to the sessions
  of their parent (page or worker) that have `Target.setAutoAttach` on,
  shared and service workers to browser sessions, and a starting service
  worker also to page sessions whose page is inside its scope. With
  `waitForDebuggerOnStart` the script waits until every waiting session
  sent `Runtime.runIfWaitingForDebugger` or detached
  (`PageHost::resume_worker`), as Chrome's paused worker does; without a
  waiting session it runs at once. A page's dedicated workers are
  detached before the page when it closes.
- **Worker sessions** answer Runtime (`enable`, `evaluate`,
  `callFunctionOn`, `getProperties`, `releaseObject(Group)`,
  `runIfWaitingForDebugger`), Network, Fetch, Target, Log, Security and
  `Emulation.setUserAgentOverride`; other domains are "not found".
  Commands go to the worker thread over a channel its event loop polls;
  remote objects and awaited promises live there. Nothing is installed in
  the worker's global scope.
- **Events.** Console calls and uncaught exceptions
  (`Runtime.consoleAPICalled` / `exceptionThrown`), the script's
  `Network.responseReceived`/`dataReceived`/`loadingFinished` (service
  workers also `requestWillBeSent`), `Inspector.workerScriptLoaded`
  (Playwright waits for it), and Network events for the worker's own
  requests. The page gets a dedicated worker's console calls and
  uncaught errors as `Log.entryAdded` with source `worker`, as Chrome
  (Playwright ignores those, so nothing is reported twice).
- **fromServiceWorker.** A page request or navigation the service worker
  answers is reported on the page with `fromServiceWorker: true`
  (`NetHost::sw_answered`); its own requests appear on its target.
- **Idle stop.** A running version with no event in flight (fetch,
  lifecycle; a message counts at dispatch) is stopped 30 s after its last
  event (Chrome: exactly 30 s) and its target destroyed; the next fetch
  or message starts it again from the stored script, without fetching it
  and without install/activate, with a new target. A version with an
  attached CDP session is never stopped, as in Chrome (so under
  Playwright, which attaches, it keeps running).

## Consequences

- Playwright's worker APIs work as with Chrome (24 Playwright tests;
  results equal Chrome's except console object formatting).
- In `serve`, every worker waits one cross-thread round trip before its
  script runs; `fetch` and other embedders are unchanged (no inspector).
- Page-visible change: an idle service worker loses its globals after
  30 s, as in Chrome.
- Not modelled: worker request interception (`Fetch` on worker sessions
  is accepted but does not pause requests), `Network.*ExtraInfo`, Chrome's
  second page attach of a service worker (waiting false), a nested
  worker's script request on its parent's session, console arguments as
  separate remote objects, `Inspector.targetCrashed` (a stopped version
  ends its target instead; Chrome does the same without DevTools).

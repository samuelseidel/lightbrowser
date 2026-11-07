# ADR 0045: CDP coverage for stock clients, session history and referrers

Status: Accepted (2026-11-07).

## Context

`docs/ARCHITECTURE.md` says stock Playwright and Puppeteer drive
LightBrowser. Playwright had 41 passing tests; Puppeteer had none, and the
two clients send methods `serve` did not answer. A source diff of both
clients (their public repositories, `main`; nothing was installed or run)
and a raw replay of what they send against local Chrome 153 and against
`serve` (`tests/cdp-raw/`) showed:

- `Audits.enable`, which Puppeteer sends for every page, was unknown.
- `Page.getNavigationHistory` answered one made-up entry,
  `Page.navigateToHistoryEntry` did not exist, and `history.back()`,
  `forward()` and `go()` only moved inside the entries of one document:
  `goBack` and `goForward` did nothing (`tests/playwright/actions.spec.mjs`
  swallowed the failure).
- `Page.navigate` read `referrer` and `referrerPolicy` and dropped them,
  while Playwright sends `referrerPolicy: unsafeUrl` with every `goto`.
- Missing: `Emulation.setScriptExecutionDisabled`,
  `Network.emulateNetworkConditions` (and `overrideNetworkState`,
  `emulateNetworkConditionsByRule`), `Network.setBlockedURLs`,
  `Network.setBypassServiceWorker`, `DOM.getFrameOwner`,
  `Runtime.getHeapUsage`.
- Same-document navigations (`pushState`, a fragment) sent no
  `Page.navigatedWithinDocument`; `Page.navigate` to a fragment reloaded the
  page; a redirect chain was reported as one request, so
  `request.redirectedFrom()` was always null.
- Chrome 153 sends `Page.frameStartedNavigating` first in every navigation
  and, at a main-frame commit, orders `Runtime.executionContextsCleared`,
  lifecycle `init`, `executionContextsCleared`, `Page.frameNavigated`.

## Decision

Match Chrome 153 as recorded. Every expected value in `tests/cdp-raw`
comes from `chrome.json`, recorded from the Chrome 153 headless shell on
loopback pages (350 steps; how to re-record is in its README).

- **Session history** (`lb-page` `session.rs`): one list per page, each
  entry with an id, URL, the URL as typed, title, `Page.TransitionType`,
  `history.state` and the document that holds it. `pushState`,
  `replaceState` and fragment navigations write to it through
  `lb_webapi::rt::SessionHost` (`history.length` and `history.state` read
  it); a script's `history.back()`, `forward()` and `go(n)` become a
  traversal request the page layer decides. An entry of the same document
  is a same-document move (`popstate`, then `hashchange` when only the
  fragment differs); any other entry loads its URL again and gets its state
  back. `go(0)` reloads and keeps the entry's transition; a navigation to
  the URL the page is on replaces the entry; a load after going back drops
  the forward entries. A title belongs to the entry that was current when
  the document's title changed (the same-document entries of one document
  keep their own). `Page.getNavigationHistory`,
  `navigateToHistoryEntry` (`-32602 No entry with passed id`) and
  `resetNavigationHistory` use the same list. The page's frames keep a
  history of their own (see open items).
- **Events**: `Page.frameStartedNavigating` (with `navigationType`),
  Chrome's commit order, `Page.navigatedWithinDocument` for `pushState`
  (`historyApi`), fragments and traversals (`fragment`, as Chrome reports
  every traversal), `Page.navigate` to a fragment stays in the document and
  answers no `loaderId`, `Page.reload` with `ignoreCache` is
  `reloadBypassingCache`, and a failed navigation answers its `loaderId`.
  `executionContextDestroyed` is no longer sent at a navigation (Chrome
  sends only `executionContextsCleared`).
- **Referrer** (`lb-net` `header_profile.rs`): `Page.navigate` `referrer`
  and `referrerPolicy` become a referrer override of the request: Chromium's
  `ComputeReferrerForPolicy` for all eight policies on every redirect hop,
  fragments and credentials stripped, a downgrade is `https` to a
  non-`https` scheme. `Sec-Fetch-Site` stays `none` and there is no
  `Origin` (browser-initiated). `document.referrer` follows the policy
  against the final URL. An invalid policy is `-32602 Invalid
  referrerPolicy`, an invalid URL `-32000 Cannot navigate to invalid URL`.
- **Audits, WebMCP**: `enable` and `disable`, no events.
- **Emulation**: `setScriptExecutionDisabled` stops `<script>` elements
  (classic and module) of the page's windows and its next documents; CDP
  evaluation still runs, as in Chrome. `clearGeolocationOverride`.
  `setCPUThrottlingRate`, `setEmulatedVisionDeficiency`, `setIdleOverride`
  and `clearIdleOverride` are accepted with Chrome's parameter checks and
  have no effect.
- **Network**: `emulateNetworkConditions`, `overrideNetworkState`,
  `emulateNetworkConditionsByRule` (one rule): `offline` fails every
  request after interception with `net::ERR_INTERNET_DISCONNECTED`;
  latency and throughput are accepted and not applied.
  `setBlockedURLs` fails matching requests with `net::ERR_BLOCKED_BY_CLIENT`
  (the pattern is a `*` wildcard on the whole URL; an unwritten query or
  fragment matches anything, as a URLPattern constructor string does; also
  the old `urls` list); `setBypassServiceWorker` starts documents without a
  controlling service worker. Each redirect hop is now a
  `Network.requestWillBeSent` with `redirectResponse` (Playwright's
  `redirectedFrom`).
- **DOM, Runtime**: `DOM.getFrameOwner`, `Runtime.getHeapUsage` (V8's heap
  statistics), `Page.setBypassCSP` and `Page.setFontFamilies` (accepted: no
  CSP is enforced and the fonts are the persona's), `Frame` gets
  `adFrameStatus`, `securityOriginDetails` and Chrome's `gatedAPIFeatures`
  spelling, the cookie has no `sameParty`.
- **Parameter errors** use Chrome's code and text where `chrome.json` has
  them (`Invalid parameters` for a missing or mistyped required parameter;
  `-32602` with Chrome's text for a bad id or enum value of
  `Fetch.failRequest`, `Fetch.enable`, `Target.closeTarget`, `attachToTarget`,
  `Page.captureScreenshot`, `Network.setCookie` and others).
- **IndexedDB**: `transaction()` takes a `DOMStringList` (`objectStoreNames`)
  as a sequence of names. Playwright's `storageState({indexedDB: true})`
  failed on it.

`tests/cdp-raw/replay.mjs compare` replays the sequences against `serve`
and compares with `chrome.json`: success versus error, error code, error
text where a step asks for it, the shape of results (keys and types),
picked values (history entries, `Referer`, `Sec-Fetch-*`, URLs) and the
ordered Page events of navigation steps. Differences that are understood
are listed with their reason in `KNOWN`; a step that starts to match fails
the run until its entry is removed.

## Before and after, by method

Methods of the sequences each client sends that failed against `serve`
(Chrome answered; `tests/cdp-raw`, 350 steps, 24 error-code differences
before and 2 after, the first run of `replay.mjs` against the parent
commit):

| Method | Sent by | Before | After |
|---|---|---|---|
| `Audits.enable` / `disable` | Puppeteer (every page) | -32601 | works |
| `WebMCP.enable` | Puppeteer (errors caught) | -32601 | works |
| `Page.navigateToHistoryEntry`, `resetNavigationHistory` | both (`goBack`, `goForward`) | -32601; history one entry | works, real history |
| `Page.navigate` `referrer`, `referrerPolicy` | both (`goto` option; Playwright always) | dropped | applied |
| `Emulation.setScriptExecutionDisabled` | Playwright (every page), Puppeteer | -32601 | works |
| `Network.emulateNetworkConditions` | Playwright (every page, offline) | -32601 | works |
| `Network.overrideNetworkState`, `emulateNetworkConditionsByRule` | Puppeteer (main) | -32601 | works |
| `Network.setBypassServiceWorker` | both | -32601 | works |
| `Network.setBlockedURLs` | DevTools-style clients | -32601 | works |
| `DOM.getFrameOwner` | both (`frameElement`) | -32601 | works |
| `Runtime.getHeapUsage` | both | -32601 | works |
| `Page.setBypassCSP`, `Page.setFontFamilies` | Playwright | -32601 | accepted |
| `Emulation.setCPUThrottlingRate`, `setEmulatedVisionDeficiency`, `clearIdleOverride`, `clearGeolocationOverride` | Puppeteer | -32601 | accepted |
| `Emulation.setDeviceMetricsOverride` with Puppeteer's default 800x600 viewport | Puppeteer (every page) | -32000 persona | **-32000 persona** |
| `Emulation.setTouchEmulationEnabled`, `setTimezoneOverride`, `setLocaleOverride` with values that differ from the persona | Playwright (context options) | -32000 persona | **-32000 persona** |
| `Browser.getWindowForTarget`, `getWindowBounds` | Playwright (headful), Puppeteer | -32000 / -32601 | **same** |
| `Performance.getMetrics`, `Page.getResourceTree`, `Page.setDocumentContent`, `Runtime.removeBinding` | Puppeteer | -32601 | **-32601** |

Per client, what newly works: Puppeteer's per-page setup (all of it except
`setDeviceMetricsOverride` with the default viewport, below), `goBack`,
`goForward`, `goto` with `referer`/`referrerPolicy`, `setJavaScriptEnabled`,
offline mode, `emulateNetworkConditions`, `frame.frameElement()`;
Playwright's `goBack`, `goForward`, `goto` `referer`, offline mode,
`javaScriptEnabled: false`, `serviceWorkers: 'block'`, `storageState` with
`indexedDB`, `request.redirectedFrom()`, `waitForLoadState('networkidle')`
(unchanged, now tested), `addInitScript` (unchanged, now tested) and
`ariaSnapshot` (unchanged, now tested). `tests/playwright/session.spec.mjs`
(12 tests) passes against LightBrowser and, run with the Chrome 153
headless shell as the browser (see the README of `tests/cdp-raw`), against
Chrome.

## Kept differences

Listed in `KNOWN` of `replay.mjs`, each with its reason:

- **Persona consistency (ADR 0009)**: a value that contradicts the persona
  is refused, where Chrome applies it. For Puppeteer this includes
  `Emulation.setDeviceMetricsOverride` with its default viewport: it sends
  `screenOrientation` `portraitPrimary` angle 0 for 800x600 (and angle 90
  for `isLandscape`), which does not match the persona's landscape screen,
  so `newPage()` fails on that call unless `defaultViewport: null` is used.
  Accepting a viewport's orientation without changing the persona is an
  owner decision on ADR 0009 and was not made here (see open items).
- **No browser window is exposed**: `Browser.getWindowForTarget` still
  answers "Browser window not found". Answering it makes Playwright call
  `Browser.setWindowBounds` and resize the persona's window (ADR 0009
  revision 2; a plain `newContext()` test failed on it).
- Not implemented, not in this package: `Runtime.removeBinding`,
  `Runtime.queryObjects`, `Performance.getMetrics`, `Page.getResourceTree`,
  `Page.setDocumentContent`, internal properties in `getProperties`, stack
  traces and the promise text in `exceptionDetails`, `chromeRole` of AX
  nodes, request headers of `Fetch.requestPaused` as Chrome lists them, a
  cookie without `SameSite` listing `sameSite`, and the leading dot Chrome
  does not add to a domain cookie set through `Storage.setCookies`.
- Chrome commits an error page after a failed navigation (`chrome-error://`
  with its own events); LightBrowser keeps the document.
- Script-initiated navigations send `Page.frameScheduledNavigation`,
  `frameRequestedNavigation` and `frameClearedScheduledNavigation` in
  Chrome, not here (not compared by the replay). Events of child frames are
  not compared.

## Open items

- Subframes have a session history of their own: Chrome has one joint
  history, so `history.back()` in a frame moves the whole page and a
  frame's navigation is an entry of the page.
- A script's `location.href = x` and form submissions join the history as
  `link` and `form_submit` entries; their `frameRequestedNavigation` events
  are missing (above).
- Puppeteer's default viewport (orientation, above).
- Screencast (`Page.startScreencast`, Playwright video), `DOMSnapshot`
  and network throttling by latency and throughput were not done: each is
  more than a day of its own.
- `Network.setBlockedURLs` takes `*` wildcards, not the URLPattern grammar
  (`:name`, groups, regular expressions).
- `Emulation.setScriptExecutionDisabled` does not stop inline event handler
  attributes, timers registered earlier, or the scripts of frames made
  after the call.
- Unsafe ports: Chrome refuses `127.0.0.1:1` with `net::ERR_UNSAFE_PORT`,
  LightBrowser connects and fails with `ERR_CONNECTION_REFUSED`.

## Consequences

- Pages that never call these methods pay nothing: the session history is a
  `Vec` of a few entries per page; the realm path is unchanged.
- `Runtime.executionContextDestroyed` is gone from navigations, and
  `lifecycle:init` moved before `Page.frameNavigated`. Playwright's 53 tests
  pass; clients that rely on `executionContextsCleared` (both) are
  unaffected.
- `NetRequest` gained `referrer`, `RequestMeta` a `referrer`;
  `PageEvent::NavigatedWithinDocument` a `navigation_type`;
  `PageEvent::FrameStartedNavigating` is new; `PageHost` gained default
  methods for the new commands (`MockHost` and the firefox build keep the
  defaults).

# Raw CDP replay

`replay.mjs` plays the CDP command sequences that stock Puppeteer and
Playwright send, over a plain WebSocket (Node 24 global `WebSocket`, no
dependencies), against local Chrome 153 and against `lightbrowser serve`.
It compares success versus error, the shape of each result (keys and types,
not values), a few picked values (URLs, history entries, request headers) and
the ordered Page events of navigation steps.

Chrome's answers are recorded into `chrome.json`; the comparison with
LightBrowser runs from that file, so it needs no Chrome.

```sh
# compare (needs a debug build: cargo build -p lightbrowser)
node tests/cdp-raw/replay.mjs compare --launch lightbrowser --port 19261
# one scenario, with every step printed
node tests/cdp-raw/replay.mjs compare --launch lightbrowser --only history --verbose
# re-record from Chrome 153 (headless shell; --bin or CHROME_BIN overrides the path)
node tests/cdp-raw/replay.mjs record --launch chrome --port 19260
```

Everything is loopback: the browser, and two test sites on 127.0.0.1 with
different ports (one origin and one same-site origin, for `Referer` and
`Sec-Fetch-Site`). Each scenario uses its own connection, because
`Target.setAutoAttach` and similar belong to the connection.

Steps that differ from Chrome on purpose are listed in `KNOWN` in
`replay.mjs` with the reason; they are reported but do not fail the run.

## What the clients send

Read from the public sources (puppeteer `packages/puppeteer-core/src/cdp/*.ts`,
playwright `packages/playwright-core/src/server/chromium/*.ts`, both `main`
as of this work). Commands are in the order a client sends them; parameters
are the ones that matter.

### Puppeteer

**connect and new page** (`Browser.ts`, `TargetManager.ts`, `BrowserContext.ts`)

- `Browser.getVersion`, `Target.setDiscoverTargets {discover: true, filter}`,
  `Target.setAutoAttach {waitForDebuggerOnStart: true, flatten: true,
  autoAttach: true, filter: [{type: 'page', exclude: true}, ...]}`.
- `Target.createBrowserContext {proxyServer?, proxyBypassList?}`;
  `Target.createTarget {url: 'about:blank', browserContextId, left?, top?,
  width?, height?, windowState?}`; `Target.attachToTarget {targetId, flatten:
  true}`.
- Per page (`FrameManager.initialize`, `Page.#initialize`), sent together:
  `Network.enable`, `Page.enable`, `Page.getFrameTree`,
  `Page.setLifecycleEventsEnabled {enabled: true}`, `Runtime.enable` then
  `Page.createIsolatedWorld {frameId, worldName, grantUniveralAccess: true}`,
  **`Audits.enable`** (when `issuesEnabled`, which is the default),
  `Performance.enable`, `Log.enable`, `WebMCP.enable` (errors are caught),
  `Target.setAutoAttach {waitForDebuggerOnStart, flatten, autoAttach,
  filter}`, `Runtime.runIfWaitingForDebugger`.
- Viewport (`EmulationManager.ts`): `Emulation.setDeviceMetricsOverride
  {mobile, width, height, deviceScaleFactor, screenOrientation}` and
  `Emulation.setTouchEmulationEnabled {enabled}`; reset with
  `Emulation.clearDeviceMetricsOverride`. Other emulation:
  `setTimezoneOverride {timezoneId}`, `setLocaleOverride {locale}`,
  `setEmulatedMedia {media | features}`, `setGeolocationOverride` /
  `clearGeolocationOverride`, `setDefaultBackgroundColorOverride {color}`,
  **`setScriptExecutionDisabled {value}`**, `setFocusEmulationEnabled
  {enabled}`, `setCPUThrottlingRate {rate}`, `setEmulatedVisionDeficiency
  {type}`, `setIdleOverride` / `clearIdleOverride`.

**goto, goBack, goForward, reload** (`Frame.ts`, `Page.ts`)

- `Page.navigate {url, referrer, frameId, referrerPolicy}`; the result's
  `loaderId` says whether a new document loads (absent: same-document).
- `Page.reload {ignoreCache}`.
- goBack / goForward: `Page.getNavigationHistory`, take
  `entries[currentIndex +- 1].id` (throw if absent), then
  `Page.navigateToHistoryEntry {entryId}` while waiting for a navigation.
- `Page.setDocumentContent {frameId, html}` (`setContent`).
- `Page.addScriptToEvaluateOnNewDocument {source}` /
  `removeScriptToEvaluateOnNewDocument {identifier}`, `Runtime.addBinding
  {name, executionContextName?}` / `removeBinding {name}`.
- `DOM.getFrameOwner {frameId}` (`frame.frameElement()`).

**screenshot** (`Page.ts`): `Page.captureScreenshot {format, optimizeForSpeed,
fromSurface, quality?, clip?: {x, y, width, height, scale}, captureBeyondViewport}`;
`Emulation.setDefaultBackgroundColorOverride` around it for
`omitBackground`; `Page.printToPDF` with `IO.read` / `IO.close`.

**evaluate, waitForFunction** (`ExecutionContext.ts`, `IsolatedWorld.ts`):
`Runtime.evaluate {expression, contextId, returnByValue, awaitPromise: true,
userGesture: true}`, `Runtime.callFunctionOn {functionDeclaration,
executionContextId | objectId, arguments, returnByValue, awaitPromise,
userGesture}`, `Runtime.getProperties`, `Runtime.releaseObject`,
`DOM.describeNode {objectId}`, `DOM.resolveNode {backendNodeId,
executionContextId}`, `Runtime.queryObjects`, `Accessibility.getFullAXTree`.
`waitForFunction` polls in the page through `callFunctionOn` with
`awaitPromise`.

**request interception and network** (`NetworkManager.ts`):
`Fetch.enable {handleAuthRequests: true, patterns: [{urlPattern: '*'}]}` /
`Fetch.disable`, `Fetch.continueRequest`, `Fetch.fulfillRequest`,
`Fetch.failRequest`, `Fetch.continueWithAuth`; `Network.setCacheDisabled
{cacheDisabled}`, `Network.setExtraHTTPHeaders {headers}`,
`Network.setUserAgentOverride {userAgent, acceptLanguage,
userAgentMetadata, platform}`, offline and throttling:
**`Network.overrideNetworkState`** and
**`Network.emulateNetworkConditionsByRule`** (`main`), older releases send
**`Network.emulateNetworkConditions {offline, latency, downloadThroughput,
uploadThroughput}`**; `Network.setBypassServiceWorker`.

**cookies**: `Network.getCookies {urls}`, `Network.setCookies {cookies}`,
`Network.deleteCookies {name, url, domain, path, partitionKey}`; on the
context `Storage.getCookies {browserContextId}`, `Storage.setCookies`.

**close**: `Page.close` (with `runBeforeUnload`) or `Target.closeTarget
{targetId}`, `Target.disposeBrowserContext {browserContextId}`,
`Browser.close`.

### Playwright

**connect and new page** (`crBrowser.ts`, `crPage.ts`): `Browser.getVersion`,
`Target.setAutoAttach {autoAttach: true, waitForDebuggerOnStart: true,
flatten: true}`, `Target.createBrowserContext {disposeOnDetach: true}`,
`Browser.setDownloadBehavior`, `Target.createTarget`. Per page, together:
`Browser.getWindowForTarget`, `Page.enable`, `Page.getFrameTree`,
`Log.enable`, `Page.setLifecycleEventsEnabled`, `Runtime.enable`,
`Page.addScriptToEvaluateOnNewDocument {source: '', worldName}`,
`Page.createIsolatedWorld`, `Network.enable`, `Target.setAutoAttach`,
`Emulation.setFocusEmulationEnabled`, `Emulation.setDeviceMetricsOverride`
(with `screenWidth`, `screenHeight`), `Emulation.setUserAgentOverride`,
`Page.setFontFamilies` (headless), `Emulation.setEmulatedMedia`,
`Page.setBypassCSP`, `Security.setIgnoreCertificateErrors`,
`Emulation.setTouchEmulationEnabled`, **`Emulation.setScriptExecutionDisabled`**,
`Emulation.setTimezoneOverride`, `Emulation.setLocaleOverride`,
`Page.setInterceptFileChooserDialog`, **`Network.setBypassServiceWorker`**
(storage-state pages), `Runtime.runIfWaitingForDebugger`.

**goto and history**: `Page.navigate {url, referrer, frameId, referrerPolicy:
'unsafeUrl'}` (always); `Page.reload`; goBack / goForward through
`Page.getNavigationHistory` and `Page.navigateToHistoryEntry {entryId}`,
returning `false` when there is no entry; `Page.bringToFront`.

**network**: `Network.enable`, `Fetch.enable {handleAuthRequests: true,
patterns: [{urlPattern: '*', requestStage: 'Request'}]}`,
`Network.setCacheDisabled`, `Network.setExtraHTTPHeaders`,
**`Network.emulateNetworkConditions`** (offline),
`Network.clearBrowserCache`, `Network.getResponseBody`,
`Network.loadNetworkResource` with `IO.read`, `Fetch.continueRequest`,
`Fetch.fulfillRequest`, `Fetch.failRequest`, `Fetch.continueWithAuth`.

**screenshot and viewport**: `Page.getLayoutMetrics`, `Page.captureScreenshot
{format, quality, clip, captureBeyondViewport}`,
`Emulation.setDefaultBackgroundColorOverride`, `Browser.getWindowBounds` /
`setWindowBounds` (headful), `Page.startScreencast` / `stopScreencast` and
`screencastFrameAck` (video; not implemented, see ADR 0045).

**evaluate**: `Runtime.evaluate`, `Runtime.callFunctionOn`,
`Runtime.getProperties`, `Runtime.addBinding`, `DOM.describeNode`,
`DOM.resolveNode`, `DOM.getContentQuads`, `DOM.getBoxModel`,
`DOM.scrollIntoViewIfNeeded`, `DOM.setFileInputFiles`,
**`DOM.getFrameOwner`**.

**cookies and permissions**: `Storage.getCookies`, `Storage.setCookies`,
`Storage.clearCookies`, `Browser.grantPermissions`,
`Browser.resetPermissions`, `Emulation.setGeolocationOverride`.

**close**: `Page.close {runBeforeUnload}`, `Target.closeTarget`,
`Target.disposeBrowserContext`, `Browser.close`.

## Scenarios in `replay.mjs`

| Scenario | What it replays |
|---|---|
| `puppeteer.newPage` | Puppeteer's connect, context, target, per-page setup (with `Audits.enable`), viewport |
| `playwright.newPage` | Playwright's connect, context, target, per-page setup |
| `goto` | `Page.navigate` with every `referrerPolicy`, what the server sees (`Referer`, `Sec-Fetch-*`), `document.referrer`, errors for bad parameters, redirects, hash navigation |
| `history` | `getNavigationHistory`, `navigateToHistoryEntry`, `resetNavigationHistory`, `history.back/forward/go`, `pushState`, hash, reload, with the Page events |
| `emulation` | device metrics, touch, focus, media, geolocation, timezone, locale, script execution, CPU throttling |
| `screenshot` | `getLayoutMetrics`, `captureScreenshot` variants, `printToPDF` |
| `evaluate` | `evaluate`, `callFunctionOn`, objects, bindings, `getHeapUsage`, polling |
| `frames` | `DOM.getFrameOwner`, isolated worlds in a child frame |
| `network` | headers, cache, offline and throttling, bypass service worker, blocked URLs |
| `fetch` | `Fetch.enable`, `requestPaused`, continue, fulfill, fail |
| `cookies` | `Network.*Cookies`, `Storage.*Cookies` |
| `browser` | permissions, downloads, window, `Page.setDocumentContent`, `Audits` |
| `close` | `Page.close`, `Target.closeTarget`, `Target.disposeBrowserContext` |

## Before and after

See `docs/adr/0045-cdp-coverage.md` for the list of methods that failed
before this package and the state after it.

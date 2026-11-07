# ADR 0009: CDP bindings only in isolated worlds; Emulation must match the persona

Status: Accepted (2026-11-03)

## Context

ADR 0005 makes CDP the control API and says nothing the control layer does
may be visible to the page. Two places in the protocol break that rule if
implemented the obvious way:

1. **Bindings and helper scripts.** Playwright calls `Runtime.addBinding` and
   `Page.addScriptToEvaluateOnNewDocument` to install its injected-script
   machinery, and runs helper code through `Runtime.callFunctionOn`. Done in
   the page's main world, that adds globals, extra frames in `Error.stack`
   and functions with a native-looking `toString` that is not.
2. **Emulation.** `Emulation.setDeviceMetricsOverride`,
   `setUserAgentOverride`, `setTimezoneOverride` and friends change what the
   page sees. A client can ask for values that contradict the persona (for
   example a 1280x720 screen on a persona with a 1920x1080 screen, or a
   Firefox UA on a Chrome persona), which breaks the consistency rules the
   persona was validated against (R01-R16).

Playwright's `newContext()` sends a default 1280x720 viewport unless the
client passes `viewport: null`.

## Decision

**Isolated worlds only.** `lb-cdp` installs automation bindings only in
isolated worlds (`BindingScope`: `AllIsolatedWorlds`, `World(name)` or one
`Context(id)` that is not a main world).

- `Runtime.addBinding` with an `executionContextId` of a main world is
  rejected ("bindings are only installed in isolated worlds, never in the
  page's main world"). Without an id or name the binding goes to every
  isolated world, now and after each navigation.
- `Page.createIsolatedWorld` and world-scoped `addScriptToEvaluateOnNewDocument`
  create the utility world Playwright uses. `PageHost` implementations must
  never install anything in the main world for CDP's sake (stated in
  `lb-cdp::host` docs).
- Each isolated world is its own realm. It shares host state with the page
  (same DOM nodes, ADR 0008) but not JS objects.
- Requirement: nothing from isolated worlds may appear in the main world's
  `Error.stack`. Helper code runs in its own realm, so page stacks contain
  page frames only.

**Emulation checked against the persona.** `lb-cdp::emulation` compares each
`Emulation.*` and `Network.setUserAgentOverride` request with the context's
`PersonaProfile`:

- A value equal to the persona's own is accepted and needs no change.
- A value that contradicts a persona field is rejected with an error naming
  the field ("... it contradicts the persona (use LightBrowser.setPersona to
  change it)"). Rules: `mobile` must equal the persona's mobile flag;
  `deviceScaleFactor` must equal `screen.device_pixel_ratio` (0 means
  unset); `screenWidth` / `screenHeight` must equal the persona screen;
  `screenOrientation` must match the screen; `positionX/Y` must be 0. A UA
  override must equal the persona's UA string (a different family gets a
  specific message); `acceptLanguage` must start with the persona's
  language, `platform` must equal `os.platform`, and `userAgentMetadata`
  (platform, version, architecture, bitness, model, mobile, brands) must
  equal the persona's client hints. Locale and timezone overrides must equal
  the persona's, an empty value clears; `setEmulatedMedia` accepts `screen`
  only and features equal to what the persona's media environment answers.
- **Viewport.** A viewport (`width` x `height`) is accepted when it fits in
  the persona screen (no larger than the available or full screen, whichever
  is bigger), and passed to the host as a `ViewportOverride`. If it equals the
  persona's inner size it is a no-op. A viewport larger than the persona
  screen is rejected. Because Playwright's default is 1280x720, **clients
  must pass `viewport: null`** to `browser.newContext()`, which
  keeps the persona's own window size. The Playwright tests (`harness.mjs`,
  `smoke.spec.mjs`, `network.spec.mjs`) do this.
- To change identity on purpose use `LightBrowser.setPersona`, which swaps the
  whole persona after the consistency rules pass.

## Alternatives considered

- **Silently ignore contradicting emulation.** Playwright would think it
  worked and tests would pass while the page saw something else.
- **Accept everything.** Simple, and it is what a stock browser does, but a
  persona that stops being consistent is exactly what the detection runs must
  not produce by accident.
- **Install bindings in the main world with hidden properties.** Hidden
  properties still show up through `Reflect.ownKeys`, timing and
  `toString`; no reliable way to hide them.

## Consequences

- Stock Playwright works, with `viewport: null` as a known client setting. A
  missing `viewport: null` gives a clear error, not a wrong page.
- Playwright features that rely on main-world bindings (for example
  `page.exposeFunction` in the main world) behave as Playwright does in other
  browsers: it uses the binding through an isolated-world proxy.
- Open: only the overrides listed above are checked. Others (geolocation,
  touch, CPU throttling) fall back to the host's default, which is
  "unsupported" until the page host implements them.
- Open: the real page host (`lb_page::CdpHost`) must keep the rule for
  `Page.addScriptToEvaluateOnNewDocument` without a world name: it runs in
  the main world by protocol, so only clients that ask for it get it.

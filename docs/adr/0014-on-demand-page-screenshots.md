# ADR 0014: Page screenshots painted on demand from the layout

Status: Accepted (2026-11-03). Scope extended by ADR 0020 (background
images and gradients, border radius, box shadows; background images now
load like Chrome's).

## Context

ADR 0004 keeps style and layout lazy and paints nothing by default; it left
page screenshots for M6, on the CPU rasterizer. Playwright's
`page.screenshot()` and `locator.screenshot()` call
`Page.getLayoutMetrics` and `Page.captureScreenshot` (with `clip`,
`captureBeyondViewport`, `format`, `quality`) and, for `omitBackground`,
`Emulation.setDefaultBackgroundColorOverride`. Until now the first two
returned "not supported" or a fixed scroll of 0.

Screenshots are for people and tests looking at a page, not for the page.
The page cannot read them, so they need no fingerprint noise, and they must
not change anything the page can observe.

## Decision

- **No retained paint state.** Each capture asks lb-layout for a
  `DisplayList` (boxes in paint order with page geometry, clips, opacity
  groups and text lines), paints it with tiny-skia (`lb_paint::page`) and
  drops it. Nothing is kept between captures; pages that are never
  captured pay nothing.
- **Same inputs as layout and canvas.** Text uses the fonts layout
  resolved and the bundled glyph outlines (`profiles/fonts/outlines/`), so
  painted text has the advances the page measures. Image and canvas pixels
  come from lb-webapi without readback noise (`rt::screenshot`); WebGL
  contexts expose an un-noised `raw` readback for this only.
- **Chrome's coordinates.** `clip` is in page coordinates; output pixels
  are CSS px times `clip.scale` times the persona's device pixel ratio.
  `captureBeyondViewport: false` paints only what lies in the visible
  viewport. `Page.getLayoutMetrics` reports the real scroll offset and
  document size through `PageHost::page_metrics`.
- **Scope.** Backgrounds (with root/body propagation to the canvas),
  borders, text, images, canvas, basic form controls, iframes, opacity,
  overflow clips, visibility and stacking order. No CSS background images
  or gradients, border radius, shadows, outlines, text decoration,
  transforms, filters or scrollbars.
- **Nothing in any world.** Capturing runs no script; it only reads
  layout and element state on the page's worker.

## Alternatives considered

- **A retained display list or layer tree updated on change**, like a
  compositor. Faster repeated captures, but it costs memory on every page
  and contradicts ADR 0004.
- **Rendering through Canvas2d.** Reuses the 2D API, but groups, clips and
  device scale are awkward there and it would share state with page canvases.
- **Returning a blank image of the right size.** Enough for Playwright not
  to fail, but useless for people debugging a run.

## Consequences

- `page.screenshot()`, `fullPage`, clips, element screenshots, JPEG/WebP
  and `omitBackground` work with stock Playwright
  (`tests/playwright/screenshot.spec.mjs`).
- Pixels follow LightBrowser's approximate layout (ADR 0010), not Chrome's
  exact rendering, and form controls are drawn plainly, not with Chrome's
  native theme. Screenshots are not suitable for pixel comparison against
  Chrome.
- CSS background images are not loaded at all yet; painting them needs a
  `background-image` longhand in lb-css and load-time fetching in the page
  layer, which also changes network traffic.

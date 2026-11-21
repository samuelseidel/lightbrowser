# ADR 0020: CSS background images load like Chrome's; screenshots paint backgrounds, radius and shadows

Status: Accepted (2026-11-04). Extends the scope of ADR 0014.

## Context

ADR 0014 left CSS background images, gradients, border radius and box
shadows out of screenshots, and noted that loading background images
changes network traffic. Chrome requests the `url()` background images of
rendered elements (not in a `display: none` subtree) when it computes
style for a frame, and those requests delay the window's `load` event.
`getComputedStyle` reports `background-image`, `-position`, `-size`,
`-repeat`, `-origin`, `-clip`, `-attachment`, the corner radii and
`box-shadow` in its own formats (absolute `url("...")`, `calc(100% - 5px)`,
gradients with `rgb()` stops, shadows color first, `background-position`
listed instead of its x / y longhands).

## Decision

- **lb-css** parses and computes those properties (`bg.rs`): background
  layer lists, linear and radial gradients (repeating too, transition
  hints), the `background`, `background-position` and `border-radius`
  shorthands, and serializes them as Chrome does (checked on 9 elements,
  `stylesheets` case `computed_bg`). `getComputedStyle(el).length` grows
  from 76 to 88 names.
- **Loading** (`lb-webapi` `rt/bgimage.rs`): with no frames to render,
  the page layer computes the background URLs of rendered elements at
  `DOMContentLoaded`, after each style sheet loads and before a
  screenshot, and fetches new ones as images (in memory, decoded only
  when painted). Loads started before `load` delay it. Pages whose sheets
  and `style` attributes never mention `url(` skip the pass.
- **Painting** (`lb-paint` `decor.rs`): layers bottom to top with size
  (`cover`, `contain`, lengths, `auto` by the image ratio), position,
  repeat (`repeat`, `no-repeat`, `space`, `round`), origin, clip and
  `fixed` attachment; the root's layers cover the canvas. Gradients follow
  CSS Images 3 (gradient line, corner angles, ending shapes, stop fix-up)
  and interpolate premultiplied. Border radius clips backgrounds, borders
  (ring split along the corner diagonals) and replaced content; outer and
  inset shadows are blurred with three box blurs. `Page.captureScreenshot`
  waits up to 5 s for background images still loading.

## Alternatives considered

- **Load background images only for screenshots.** No network change for
  normal crawls, but Chrome's traffic and `load` timing differ, which a
  server can see.
- **A full style pass on every page.** Simple, but costs CPU on pages
  without background images (most of the bench corpus).

## Consequences

- Screenshots of backgrounds, gradients, radius and shadows match Chrome
  within anti-aliasing (`lb-page` `screenshot_golden`: mean channel
  difference 0.05-0.68 per page, at most 0.5% of pixels off by > 48).
- Not done: SVG background images (painted transparent), `image-set()`,
  `cross-fade()`, conic gradients, `background-clip: text`, background
  `blend-mode`, the stylesheet URL as the `Referer` of CSS images (the
  document's is sent), background images of elements inserted after the
  last pass that are not captured, and per-frame repaint timing.

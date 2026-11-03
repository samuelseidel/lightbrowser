# ADR 0004: Lazy style and layout, CPU canvas, no painting by default

Status: Accepted (2026-11-02). Screenshots: see ADR 0014.

## Context

A real browser spends most of its CPU and RAM on style, layout, painting and
compositing. LightBrowser has no screen. Most pages we load never need
geometry. But some things do:

- scripts that call `getBoundingClientRect`, `offsetWidth`, `elementFromPoint`
  or use IntersectionObserver
- Playwright actionability checks (visible, stable, hit target) before a click
- font detection by measuring text width
- canvas 2D fingerprinting, which needs real pixels
- screenshots (optional, M6)

Detection also does not need pixel-perfect layout. It relies mostly on font
metrics and a few probes. The RAM budget (idle under 15 MB) rules out keeping
style and layout data for every page.

## Decision

- **Style:** our own cascade, resolved lazily per element, using Servo's
  `selectors` and `cssparser`. Pages that never ask for style pay nothing.
- **Layout:** `taffy` (block, flex, grid), run on demand. Style and layout stay
  dirty until JS or CDP asks for geometry. Layout data can be dropped again
  under memory pressure.
- **Text:** measured with `skrifa` metrics of the persona's fonts. We keep
  metric tables only (advances, ascent, descent). Full glyph outlines load only
  when a canvas draws text.
- **Canvas 2D:** real rasterization with `tiny-skia` on the CPU. Seeded SFC32
  noise is added on readback (`toDataURL`, `getImageData`).
- **WebGL:** a stub context. Parameters, extensions and precision come from the
  persona, and `readPixels` returns seeded deterministic output.
- **No painting:** no paint layers, display lists, compositor or GPU. Page
  screenshots come later and are optional.
- **Virtual time:** timers and `requestAnimationFrame` may run on a virtual
  clock, and it is off when the persona needs realistic timing.

## Alternatives considered

- **Eager full style and layout after every change**, like a normal browser.
  Simple and faithful, but costs CPU and RAM on pages that never read geometry.
  That conflicts with the Lightpanda comparison.
- **No layout at all** (geometry APIs return zeros). Fast, but
  Playwright actionability checks, `getByRole` with visibility, hit testing and
  font-width probes would fail or look wrong. Zeroed geometry is itself a bot
  signal.
- **Embed Servo's layout or Blink or Gecko layout.** Faithful, but heavy, hard
  to embed, and too large for the size and RSS budgets.
- **Skia (full) or a GPU path for canvas.** Better fidelity but larger binary,
  more dependencies, and GPU output would differ per machine, which breaks
  seed-reproducible fingerprints.

## Consequences

- Idle and light pages use little memory and CPU. This is how we plan to meet
  the Lightpanda budget.
- Geometry is computed on first use, so the first geometry call on a big page
  can be slow. We accept that.
- Layout is simplified. Sizes will not match Chrome pixel-for-pixel. Persona
  metric tables cover the font probes, and this is a listed risk.
- Canvas output is deterministic per persona seed (same seed, same fingerprint).
- Screenshots are not available until M6, and they will use the CPU
  rasterizer.
- Any code that touches style or layout must mark them dirty correctly, or JS
  will read stale values. This needs good tests.

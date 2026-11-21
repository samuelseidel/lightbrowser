# ADR 0024: Images are reused within a document, as Blink does

Status: Accepted (2026-11-04).

## Context

Every `<img>` and `new Image()` made its own request. Chrome asks once per
URL in a document: Blink's resource fetcher reuses a resource that is still
loading whatever its cache headers say, and an image that another element
holds loaded is taken from the HTML "list of available images". The
bench's product pages build 19 cards over 5 image URLs (`Cache-Control:
no-store`): Chrome sends 5 requests, LightBrowser sent 19. Recorded in
local Chrome for Testing 153.0.8010.12 on loopback (`lb-page`
`tests/images/`, 5 cases with the server's request counts).

## Decision

- An element asking for a URL (fragment ignored, same CORS mode) that is
  loading in its document waits on that request; when it ends, every
  waiting element has the image before any `load` runs, and each `load` /
  `error` is its own task.
- An element asking for a URL another element of the document holds
  loaded is complete at once (`complete`, `naturalWidth` and the layout
  size right after `src` is set) and gets `load` in a task.
- Failed loads are not reused; an image no element holds any more is
  fetched again (Blink drops it once collected; Chrome refetched it after
  300 ms in the recordings).
- While a new source loads, the element keeps its current image
  (`naturalWidth`, layout size), as the spec's current request does.

## Consequences

- The 5 Chrome-recorded cases match, request counts included. Blink keeps
  the waiting elements in a hash set, so their `load` order is not compared
  (ours is document order), nor is a `load` task against a
  `setTimeout(0)` queued between.
- CDP `Network.*` events: one request per URL, as Chrome reports.
- Not modelled: in-document reuse of scripts and style sheets while the
  document loads (Blink reuses those too), CSS background images (one
  request per URL already), the preload scanner.

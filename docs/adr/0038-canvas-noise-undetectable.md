# ADR 0038: Canvas and WebGL fingerprints from rasterization, not readback noise

Status: Accepted (2026-11-07). Replaces the readback noise of ADR 0004 and
ADR 0014 (Canvas 2D) and ADR 0030 (WebGL, "Readback noise unchanged in
kind").

## Context

Each persona needs its own stable canvas and WebGL fingerprint, so that
LightBrowser instances do not all share one image hash. Until now that came
from readback noise: `getImageData`, `toDataURL`, `toBlob` and
`convertToBlob` (and WebGL `readPixels`/`toDataURL`) changed the lowest bit
of R, G and B of every edge pixel (a pixel that differs from a 4-neighbour),
keyed by `canvas_seed` (`webgl_noise_seed`) and the pixel's position.

On the public CreepJS page the canvas was reported as lied ("rendering:
15% rgba noise", `getImageData` "pixel data modified") and as "suspicious
pixel data". Reproduced on loopback with a reconstruction of CreepJS's
canvas, lie and WebGL code (`lb-page` `tests/canvas_chrome/creep.html`) and
a probe of the other readback consistency checks that FingerprintJS,
CreepJS and similar scripts can make (`probe.html`), each recorded in
Chrome for Testing 153.0.8010.12 (headless shell, loopback) on macOS 26
arm64 and Ubuntu 24.04 x86_64, which agree on every probe line. With the
noise, LightBrowser failed:

- CreepJS `getPixelMods`: 64 random 1x1 `fillRect`s read back one by one;
  every pixel is an "edge", so 53 of 64 came back changed (14% "rgba
  noise", "pixel data modified").
- CreepJS's 2x2 arc value (`imageDataLowEntropy`) was not in its list of
  Blink values ("suspicious pixel data").
- `putImageData` of random opaque data, at an offset or not, did not read
  back exactly.
- Opaque pixels of one-colour text had 8 colours, not the fill colour.
- A `toDataURL` image drawn at an offset, `drawImage` of a canvas at an
  offset and the same scene drawn 3,4 px further did not match the source
  (6807 channels differed); a 0-255 gradient was neither monotonic nor
  grey.
- WebGL `drawImage` of the canvas and its `toDataURL` did not match
  `readPixels` (two noise keys).
- Unrelated to noise but found by the same probe: translucent
  `putImageData` round trips differed from Chrome's rounding in 962 of the
  65536 (value, alpha) pairs, and `createImageBitmap` and
  `transferToImageBitmap` threw `NotSupportedError`.

Already equal to Chrome: FingerprintJS's double `toDataURL`, the blank
canvas after `clearRect`, integer `measureText('')` boxes, solid fills,
`OffscreenCanvas` and a worker's `OffscreenCanvas` against an element.

Real machines do not differ in what readback returns; they differ in how
they rasterize: glyph rasterizers (hinting, outline placement, gamma) and
GPUs (sub-pixel precision, sample coverage). Any change made at readback
that does not follow the geometry is detectable by comparing two readback
paths or a draw with a known result.

## Decision

- **No readback noise.** Every readback path returns the bitmap's values:
  `getImageData`, `toDataURL`, `toBlob`, `convertToBlob`, `drawImage` of a
  canvas, `createImageBitmap`, `transferToImageBitmap`, WebGL `readPixels`,
  `toDataURL` and `drawImage`, page screenshots, workers. `lb-paint`
  `noise.rs` is gone.
- **Chrome's unpremultiply.** `getImageData` of a translucent pixel
  unpremultiplies as Skia's raster pipeline does (in `f32`:
  `c/255 * (1/(a/255))`, clamped, times 255, rounded half to even); all
  65536 `putImageData` round trips and 256 fill alphas equal Chrome 153
  (the same on arm64 and x86_64).
- **Chrome's glyph grid.** Canvas glyph origins snap as in Skia's glyph
  cache, recorded in Chrome: an axis-aligned glyph to the nearest quarter
  pixel in x (`floor(4x + 1/2)/4`) and whole pixel in y
  (`floor(y + 1/2)`), in device space (under the canvas transform), for
  fills and strokes; rotated or skewed text is not snapped
  (`lb_paint::GlyphGrid`). A colour emoji's x goes to a whole pixel too,
  per OS (`EmojiSnap`): on macOS from the quarter-pixel position (it moves
  on at a fraction of 3/8), on Linux straight from x (at 1/2). Windows was
  not recorded and uses the macOS rule.
- **Persona variation in rasterization** (`lb_paint::variation`):
  - Text: after snapping, every glyph and emoji of a canvas moves by the
    persona's device-space offset, from `canvas_seed` (first SFC32 output;
    x and y each in [-1/8, 1/8) px in 1/256 px steps: 4096 offsets).
    Whole-pixel moves and moves within one snapping step keep giving
    identical pixels; opaque glyph interiors keep the exact fill colour;
    `measureText` and DOM text are unchanged. Page screenshots draw page
    text without the offset.
  - WebGL: lb-gl decides which samples a triangle, line or point covers as
    if it lay the persona's offset away, from `webgl_noise_seed` (x and y
    each in [-1/16, 1/16) px in 1/256 px steps: 1024 offsets;
    `lb_gl::raster::Fixed::offset`). Depth, varyings and `gl_PointCoord`
    use the unmoved positions, so shader output inside a primitive is
    unchanged; every multisample position is at least 1/8 px from a pixel
    edge, so pixel-aligned geometry covers the same samples. Only slanted
    edges change.
- **`ImageBitmap`** (`lb-webapi` `rt/canvas.rs`), so the bitmap paths
  exist and agree: `createImageBitmap` from a canvas, image,
  `OffscreenCanvas`, `ImageBitmap`, `ImageData` or `Blob`, with crop
  (negative sizes counted from the far edge, outside transparent),
  `resizeWidth`/`resizeHeight` (the other side `ceil`-scaled),
  `resizeQuality: 'pixelated'` as Chrome picks pixels, `imageOrientation:
  'flipY'`; Chrome's errors, all as rejections; resolved in a task (after
  microtasks queued meanwhile, before timers). `transferToImageBitmap`
  (a 2D context keeps its state, a WebGL drawing buffer is cleared),
  `close()`, and `drawImage`, `createPattern` and `texImage2D` take it.

## Alternatives considered

- **Keep noise but make it consistent** (noise the bitmap when drawing,
  or only anti-aliased pixels): a per-pixel change that does not follow
  the geometry still breaks whole-pixel translation, decode-at-offset and
  the fill colour of opaque glyph pixels, and changes `putImageData`
  data unless it is skipped, which is itself a tell.
- **A per-persona gamma or contrast on glyph coverage**, as text
  rasterizers differ: realistic, but needs mask-based text rendering; the
  offset alone already separates the reference personas. Can be added
  later on top.
- **Moving WebGL vertices** (instead of coverage only): also changes
  varyings at pixel centres; the shader_math scene's escape counts then
  differed from SwiftShader in 56 pixels.

## Consequences

- `cargo test -p lb-page --test canvas` (`tests/canvas_chrome/`, recorded
  with `chrome.mjs` into `chrome_linux.json` and `chrome_mac.json`): the
  probe's lines equal Chrome 153 for all 8 reference personas, except
  CreepJS's 2x2 arc value, which depends on the rasterizer (ours
  `255255255255192192192255240240240255484848255`, one of CreepJS's own
  Blink values; Chrome 153 on both hosts:
  `...192192192255244244244255535353255`, also in the list); the CreepJS
  reconstruction finds no lie, no "rgba noise", no modified and no
  suspicious pixel data, as in Chrome; `ImageBitmap` behaviour and errors
  equal Chrome's; glyph snapping classes equal Chrome's on the persona's
  OS; the FingerprintJS text, CreepJS text, emoji and stroke images and a
  WebGL triangle fan differ between all 8 personas and repeat per
  persona.
- Canvases without text (FingerprintJS geometry, CreepJS "paint") and
  WebGL scenes without slanted edges are the same for every persona, as
  on real machines that share a rasterizer. Two personas with the same
  offset (1 in 4096 for text, 1 in 1024 for WebGL) render alike.
- Our rasterizer is not Skia, SwiftShader or a GPU: image hashes are
  LightBrowser's own and match no real machine.
- Not done: transferring an `ImageBitmap` (`postMessage` transfer list;
  structured clone copies it), `ImageBitmapRenderingContext`,
  non-`pixelated` resize compared with Chrome pixel by pixel,
  `premultiplyAlpha` and `colorSpaceConversion` (accepted, ignored).
  Chrome on Windows was not recorded.

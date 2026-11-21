# ADR 0030: Software WebGL: own GLSL compiler, lane-parallel interpreter and rasterizer

Status: Accepted (2026-11-04). Replaces the stub renderer of the M4 WebGL
work (ARCHITECTURE "WebGL" row); strings, limits, extensions and
precision still come from the persona as before.

## Context

WebGL 1/2 contexts answered every query from the persona but ran no
shaders: draws filled the first attribute's triangle with one color, and
textures, framebuffers, blending and depth did nothing. Fingerprint
scripts draw a gradient triangle (or a textured or shaded scene) and hash
`readPixels`/`toDataURL`; pages and libraries check compile and link
results, info logs, uniforms and framebuffer status. Chrome in this
container renders WebGL with SwiftShader (ANGLE on Vulkan), which is the
plausible software reference.

## Decision

- **Own pure-Rust crate `lb-gl`** (no dependencies). naga's GLSL frontend
  does not read GLSL ES 1.00 and would not give ANGLE's info logs; a
  dedicated front end is ~4k lines and costs +0.54 MB in the release
  binary (42,184,568 → 42,721,144 bytes).
- **Front end** (`glsl/`): ANGLE's preprocessor directives and messages,
  a recursive-descent parser that types and checks in one pass like
  ANGLE's parse context (same recovery, so follow-on errors match),
  constant folding, built-ins with exact overloads (GLSL ES has no implicit
  conversions), ES 1.00 Appendix A loop rules, uniform blocks. 72 info logs
  recorded in Chrome 153 match byte for byte (`tests/chrome/logs.json`).
- **Code generation** (`glsl/codegen.rs`): user functions are inlined
  (no recursion in GLSL ES); values are lists of 32-bit cells, so swizzles,
  constant indices and fields cost nothing; dynamic indices are clamped as
  ANGLE's robustness pass does.
- **Interpreter** (`exec.rs`): every operation runs on 16 lanes (16
  vertices, or a 4x4 pixel tile of four 2x2 quads). Operations compute all
  lanes and only stores to variables are masked; `if`, loops, `break`,
  `continue`, `return` and `discard` move lanes between masks as a GPU
  does. Helper lanes give `dFdx`/`dFdy`/`fwidth` and implicit texture LOD.
  A per-draw operation budget stops runaway shaders (the context is then
  lost, like Chrome's GPU watchdog).
- **Pipeline** (`raster.rs`, `texture.rs`, `image.rs`, `program.rs`):
  attribute fetch with all vertex formats and divisors, clipping against
  the six planes, culling, 8-bit sub-pixel edge functions with a top-left
  rule, 4x multisampling when `antialias` (multisampled lines are 1-pixel
  rectangles), perspective-correct, flat and `noperspective` varyings,
  points with `gl_PointCoord`, polygon offset, depth (16/24/32F),
  stencil, blending (all factors and equations), sRGB, MRT, integer and
  float targets; textures 2D/cube/3D/2D array with completeness rules,
  wrap modes, nearest/linear/mipmap filtering, shadow compare, sampler
  objects; linking with ANGLE's messages, ANGLE's uniform order (bool
  uniforms first, samplers last) and std140 block layout.
- **Bindings** (`lb-webapi` `rt/webgl_pipe.rs`): programs, uniforms,
  vertex arrays (VAO state), textures and all upload paths (typed arrays
  with unpack state, `ImageData`, canvas and image elements), renderbuffers,
  framebuffers, blits, clears and `readPixels` follow Chrome's errors; 88
  API results recorded in Chrome equal ours (`tests/chrome/chrome_api.json`).
- **Readback noise unchanged in kind** (ADR 0014's rule, keyed by
  `webgl_noise_seed`): applied to the drawing buffer and to framebuffers a
  draw rasterized into; uploaded or cleared contents read back exactly, as
  on a GPU. The same seed gives the same pixels; seeds differ per persona.

## Consequences

- Scenes from `tests/chrome/scenes.html` stay within a mean absolute
  difference of 0.1-1.7 per channel of SwiftShader's output (edges differ
  by sample pattern; `sin` of large arguments differs by libm), checked by
  `cargo test -p lb-page --test webgl`.
- One thread: a 1024x1024 full-screen fractal takes 266 ms against
  SwiftShader's 38 ms (JIT, 4 threads); 256x256 is on par (17 vs 16 ms).
- Not done: transform feedback and queries (accepted, no effect),
  compressed texture decoding (levels read black), `getTranslatedShaderSource`
  (empty), `preserveDrawingBuffer: false` clearing after compositing,
  `WEBGL_multi_draw`, line widths other than 1.

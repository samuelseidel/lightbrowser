//! CPU raster for Canvas 2D (tiny-skia), canvas text, image codecs and
//! the persona's rasterization variation.
//!
//! - [`Canvas2d`]: a 2D context's bitmap, state stack and current path.
//!   [`PathData`] is `Path2D`; [`Gradient`] and [`CanvasPattern`] are the
//!   paint objects. See the `canvas` module docs for compositing.
//! - Text ([`FontSet`]): widths and vertical metrics from lb-layout's
//!   persona font tables (so `measureText` matches DOM measurement),
//!   glyph outlines from small bundled packs (`profiles/fonts/outlines/`).
//! - Per-persona variation ([`variation`], ADR 0038): text glyphs are
//!   snapped to the device grid like Chrome's glyph cache, then moved by
//!   a sub-pixel offset from `canvas_seed` (WebGL vertices by one from
//!   `webgl_noise_seed`). Readback is exact; nothing is noised.
//! - Codecs: [`encode`] (PNG, JPEG, lossless WebP; data URLs) and
//!   [`decode`] (PNG, JPEG, GIF, WebP, ICO/BMP; SVG sized only).
//!
//! - Page screenshots ([`page`]): an lb-layout display list painted with
//!   backgrounds, borders, text, replaced content, opacity and clips.
//!
//! Everything here is engine-neutral and single-threaded per window
//! ([`PaintEnv`] is shared through `Rc`).

pub mod canvas;
pub mod color;
pub mod decode;
mod decor;
pub mod encode;
pub mod font;
pub mod page;
pub mod path;
pub mod variation;

pub use canvas::{Canvas2d, CanvasPattern, Gradient, PaintEnv, Repetition, Style, unpremultiply};
pub use color::{parse_color, serialize_color};
pub use encode::Bitmap;
pub use font::{
    CanvasFont, EmojiSnap, FontSet, GlyphGrid, TextAlign, TextBaseline, TextMetrics, TextStyle,
};
pub use page::{
    Capture, ImageFormat, ReplacedContent, ReplacedSource, encode_screenshot, paint_page,
};
pub use path::{PathData, parse_svg_path};
pub use tiny_skia;
pub use tiny_skia::{FillRule, Pixmap};

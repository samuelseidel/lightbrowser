//! CPU raster for Canvas 2D (tiny-skia), canvas text, image codecs and
//! seeded readback noise.
//!
//! - [`Canvas2d`]: a 2D context's bitmap, state stack and current path.
//!   [`PathData`] is `Path2D`; [`Gradient`] and [`CanvasPattern`] are the
//!   paint objects. See the `canvas` module docs for compositing.
//! - Text ([`FontSet`]): widths and vertical metrics from lb-layout's
//!   persona font tables (so `measureText` matches DOM measurement),
//!   glyph outlines from small bundled packs (`profiles/fonts/outlines/`).
//! - Readback ([`CanvasNoise`]): ±1 per channel on edge pixels, keyed by
//!   the persona's `canvas_seed` and the pixel position; the bitmap itself
//!   is never noised. See the `noise` module docs.
//! - Codecs: [`encode`] (PNG, JPEG, lossless WebP; data URLs) and
//!   [`decode`] (PNG, JPEG, GIF, WebP, ICO/BMP; SVG sized only).
//!
//! - Page screenshots ([`page`]): an lb-layout display list painted with
//!   backgrounds, borders, text, replaced content, opacity and clips;
//!   never noised.
//!
//! Everything here is engine-neutral and single-threaded per window
//! ([`PaintEnv`] is shared through `Rc`).

pub mod canvas;
pub mod color;
pub mod decode;
pub mod encode;
pub mod font;
pub mod noise;
pub mod page;
pub mod path;

pub use canvas::{Canvas2d, CanvasPattern, Gradient, PaintEnv, Repetition, Style};
pub use color::{parse_color, serialize_color};
pub use encode::Bitmap;
pub use font::{CanvasFont, FontSet, TextAlign, TextBaseline, TextMetrics, TextStyle};
pub use noise::CanvasNoise;
pub use page::{
    Capture, ImageFormat, ReplacedContent, ReplacedSource, encode_screenshot, paint_page,
};
pub use path::{PathData, parse_svg_path};
pub use tiny_skia;
pub use tiny_skia::{FillRule, Pixmap};

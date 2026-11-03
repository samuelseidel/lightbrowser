//! Layout for LightBrowser, computed only when geometry is requested.
//!
//! - [`LayoutEngine`]: owns the document's [`lb_css::StyleEngine`], builds a
//!   box tree from the DOM and computed styles, runs `taffy` (block, flex,
//!   grid, floats, absolute/fixed) and answers geometry queries:
//!   `getBoundingClientRect`, `offset*`, `client*`, `scroll*`,
//!   `elementFromPoint` (z-order, clips, `pointer-events`), visibility,
//!   `scrollIntoView`, box models for CDP, and resolved
//!   `getComputedStyle` values. Results are cached until the document's
//!   render generation, the styles or the viewport change; scrolling only
//!   re-runs placement.
//! - Text is measured from persona font metric tables
//!   (`profiles/fonts/<os>/*.json`, see [`FontDb`]): sum of advances plus
//!   spacing, greedy line breaking at spaces, Chrome-rounded line metrics.
//!   No shaping or kerning.
//! - Inline formatting is approximated: a run of inline content is a
//!   wrapping flex row of text runs, inline boxes and atomic inlines.
//!   Tables are blocks with flex rows. Transforms are ignored for layout.
//! - [`AxTree`]: roles, accessible names, states for CDP Accessibility.
//! - [`DisplayList`]: boxes in paint order with page geometry, clips,
//!   opacity groups and text lines, for lb-paint's page screenshots.

mod ax;
mod build;
mod display;
mod engine;
mod fonts;
mod text;
mod tree;

pub use ax::{AxNode, AxStates, AxTree, Checked};
pub use display::{DisplayItem, DisplayKind, DisplayList, DisplayStep, TextLine};
pub use engine::{BoxModel, LayoutBase, LayoutEngine, LayoutStats, Rect, ScrollAlign};
pub use fonts::{
    FontAvailability, FontDb, FontFace, FontFamily, LineMetrics, TextFont, bundled_table_count,
    emoji_advance_em, is_emoji, resolve_font,
};

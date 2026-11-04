//! CSS for LightBrowser: stylesheet parsing with `cssparser`, selector
//! matching through `lb-dom` (Servo `selectors`), and a lazy cascade.
//!
//! - [`Stylesheet`]: style rules flattened out of `@media` / `@supports` /
//!   `@layer`; `@import` is recorded, not fetched.
//! - [`StyleEngine`]: UA + author + inline cascade with specificity, source
//!   order and `!important`; [`ComputedStyle`] per element, computed on
//!   demand and cached until [`lb_dom::Document::render_generation`] or the
//!   engine's sheets/media change.
//! - [`MediaEnv`] / [`MediaQueryList`] / [`match_media`]: media queries
//!   against the persona's screen (`window.matchMedia`).
//! - [`computed_value`]: `getComputedStyle` serialization in Chrome's format.
//!
//! Supported: the box, flex, grid-basics, font, color, visibility and
//! positioning properties listed in [`LonghandId`]; shorthands and logical
//! properties map onto them; `var()` with fallbacks; `calc()`, `min()`,
//! `max()`, `clamp()`; units px/em/rem/ex/ch/%/vw/vh/vmin/vmax and absolute
//! units. No pseudo-elements, `@font-face`, animations or transitions.
//!
//! No JS here; bindings call these APIs. Fingerprint inputs (screen, OS)
//! come from the persona via [`MediaEnv::from_persona`].

mod computed;
pub mod cssom;
mod engine;
mod media;
mod props;
mod serialize;
mod sheet;
mod ua;
mod values;

pub use computed::{ComputedStyle, CustomMap, Defaults, Transform};
pub use engine::{SheetId, SheetTree, StyleEngine, StyleStats, in_display_none};
pub use media::{MediaEnv, MediaQueryList, MediaQueryResult, OsKind, PointerKind, match_media};
pub use props::{
    CustomDeclaration, Decl, Declaration, DeclarationBlock, FamilyName, FontSizeSpec,
    FontWeightSpec, LonghandId, Sv, SystemFont, declaration_longhands, is_valid_declaration,
};
pub use serialize::{computed_property_names, computed_value, longhand_value};
pub use sheet::{ImportRule, Origin, StyleRule, Stylesheet};
pub use ua::ua_stylesheet;
pub use values::{
    Align, BorderStyle, BoxSizing, Calc, Clear, Color, ColorValue, Direction, Display,
    FlexDirection, FlexWrap, Float, FontStyle, GridLine, LineHeight, Lp, LpAuto, Matrix, Overflow,
    PointerEvents, Position, TextAlign, TrackListEntry, TrackSize, Unit, Visibility, WhiteSpace,
    fmt_num, fmt_px,
};

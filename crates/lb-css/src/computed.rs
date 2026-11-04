//! Computed style: the per-element result of the cascade after
//! inheritance, `var()` substitution and unit resolution.

use std::collections::HashMap;
use std::rc::Rc;

use cssparser::{Parser, Token};

use crate::media::{MediaEnv, OsKind};
use crate::props::{
    CustomDeclaration, Decl, FamilyName, FontSizeSpec, FontWeightSpec, LonghandId, Pending, Sv,
    SystemFont, parse_property,
};
use crate::values::{
    Align, BorderStyle, BoxSizing, Calc, Clear, Color, ColorValue, Direction, Display,
    FlexDirection, FlexWrap, Float, FontStyle, GridLine, LineHeight, Lp, LpAuto, Matrix, Overflow,
    PointerEvents, Position, TextAlign, TrackListEntry, Unit, Visibility, WhiteSpace,
};

/// Custom properties visible on an element (inherited).
pub type CustomMap = HashMap<Rc<str>, Rc<str>>;

/// A `transform` value: the declared text and its 2D matrix when every
/// function could be resolved without layout. Layout treats every transform
/// as identity for now.
#[derive(Clone, Debug, PartialEq)]
pub struct Transform {
    pub css: Rc<str>,
    pub matrix: Option<Matrix>,
}

/// Per-persona defaults the UA uses (Chrome's default font settings).
#[derive(Clone, Debug, PartialEq)]
pub struct Defaults {
    /// Family of the initial `font-family` (Chrome's "standard" font).
    pub standard_family: String,
    /// Default font size in px (`medium`).
    pub font_size: f32,
    /// Default monospace size in px.
    pub monospace_size: f32,
    /// Family and size in px of the system font keywords (`font: caption`,
    /// `icon`, `menu`, `message-box`, `small-caption`, `status-bar`).
    pub system_family: String,
    pub system_size: f32,
    /// Family of the form-control keywords (`-webkit-small-control`, ...),
    /// as the UA stylesheet uses for controls.
    pub control_family: String,
}

impl Defaults {
    /// Defaults of an OS without a persona.
    pub fn for_os(os: OsKind) -> Defaults {
        Self::from_fonts(os, &os.font_defaults())
    }

    /// Defaults of a media environment (the persona's fonts).
    pub fn for_env(env: &MediaEnv) -> Defaults {
        Self::from_fonts(env.os, &env.fonts)
    }

    fn from_fonts(os: OsKind, f: &lb_persona::FontDefaults) -> Defaults {
        Defaults {
            standard_family: f.standard.clone(),
            font_size: 16.0,
            monospace_size: 13.0,
            system_family: f.system_font.clone(),
            system_size: f.system_font_px as f32,
            control_family: crate::ua::control_family(os).into(),
        }
    }

    /// Family and size of a system font keyword.
    pub fn system_font(&self, k: SystemFont) -> (&str, f32) {
        match k {
            SystemFont::WebkitSmallControl | SystemFont::WebkitControl => {
                (&self.control_family, self.font_size * 5.0 / 6.0)
            }
            // `x-small`.
            SystemFont::WebkitMiniControl => (&self.control_family, self.font_size * 10.0 / 16.0),
            _ => (&self.system_family, self.system_size),
        }
    }
}

/// Computed values for one element. Lengths are in px except where a
/// percentage can only be resolved by layout ([`Lp`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ComputedStyle {
    pub display: Display,
    pub position: Position,
    pub float: Float,
    pub clear: Clear,
    pub visibility: Visibility,
    pub opacity: f32,
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    pub width: LpAuto,
    pub height: LpAuto,
    pub min_width: LpAuto,
    pub min_height: LpAuto,
    /// `None` = `none`.
    pub max_width: Option<Lp>,
    pub max_height: Option<Lp>,
    /// Top, right, bottom, left.
    pub margin: [LpAuto; 4],
    pub padding: [Lp; 4],
    /// Used border widths (0 when the style is `none`/`hidden`).
    pub border_width: [f32; 4],
    pub border_style: [BorderStyle; 4],
    pub border_color: [Color; 4],
    /// `top`, `right`, `bottom`, `left`.
    pub inset: [LpAuto; 4],
    pub box_sizing: BoxSizing,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: LpAuto,
    pub order: i32,
    /// `None` = `normal`.
    pub row_gap: Option<Lp>,
    pub column_gap: Option<Lp>,
    /// `None` = `none`.
    pub grid_template_rows: Option<Rc<[TrackListEntry]>>,
    pub grid_template_columns: Option<Rc<[TrackListEntry]>>,
    pub grid_row_start: GridLine,
    pub grid_row_end: GridLine,
    pub grid_column_start: GridLine,
    pub grid_column_end: GridLine,
    pub align_items: Align,
    pub align_self: Align,
    pub align_content: Align,
    pub justify_content: Align,
    pub justify_items: Align,
    pub justify_self: Align,
    pub font_family: Rc<[FamilyName]>,
    pub font_size: f32,
    /// Ratio to `medium` when the size came from a keyword; lets a change
    /// to/from `monospace` rescale like Chrome does (16px vs 13px).
    pub font_size_keyword: Option<f32>,
    pub font_weight: f32,
    pub font_style: FontStyle,
    pub line_height: LineHeight,
    pub color: Color,
    pub background_color: Color,
    pub text_align: TextAlign,
    pub white_space: WhiteSpace,
    /// `None` = `auto`.
    pub z_index: Option<i32>,
    pub transform: Option<Transform>,
    pub pointer_events: PointerEvents,
    pub cursor: Rc<str>,
    /// `None` = `normal`.
    pub letter_spacing: Option<f32>,
    pub word_spacing: f32,
    pub direction: Direction,
    /// `None` = `auto`.
    pub aspect_ratio: Option<f32>,
    /// Background layers (shared until changed).
    pub background: Rc<crate::bg::Background>,
    /// Corner radii: top-left, top-right, bottom-right, bottom-left, each
    /// (horizontal, vertical).
    pub radius: [(Lp, Lp); 4],
    pub box_shadow: Rc<[crate::bg::BoxShadow]>,
    pub custom: Rc<CustomMap>,
}

impl ComputedStyle {
    /// Initial values of every property.
    pub fn initial(defaults: &Defaults) -> ComputedStyle {
        ComputedStyle {
            display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
            visibility: Visibility::Visible,
            opacity: 1.0,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            width: LpAuto::Auto,
            height: LpAuto::Auto,
            min_width: LpAuto::Auto,
            min_height: LpAuto::Auto,
            max_width: None,
            max_height: None,
            margin: [
                LpAuto::Lp(Lp::ZERO),
                LpAuto::Lp(Lp::ZERO),
                LpAuto::Lp(Lp::ZERO),
                LpAuto::Lp(Lp::ZERO),
            ],
            padding: [Lp::ZERO, Lp::ZERO, Lp::ZERO, Lp::ZERO],
            border_width: [0.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [Color::BLACK; 4],
            inset: [LpAuto::Auto, LpAuto::Auto, LpAuto::Auto, LpAuto::Auto],
            box_sizing: BoxSizing::ContentBox,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Nowrap,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: LpAuto::Auto,
            order: 0,
            row_gap: None,
            column_gap: None,
            grid_template_rows: None,
            grid_template_columns: None,
            grid_row_start: GridLine::Auto,
            grid_row_end: GridLine::Auto,
            grid_column_start: GridLine::Auto,
            grid_column_end: GridLine::Auto,
            align_items: Align::Normal,
            align_self: Align::Auto,
            align_content: Align::Normal,
            justify_content: Align::Normal,
            justify_items: Align::Normal,
            justify_self: Align::Auto,
            font_family: Rc::from(vec![FamilyName {
                name: defaults.standard_family.clone(),
                generic: false,
            }]),
            font_size: defaults.font_size,
            font_size_keyword: Some(1.0),
            font_weight: 400.0,
            font_style: FontStyle::Normal,
            line_height: LineHeight::Normal,
            color: Color::BLACK,
            background_color: Color::TRANSPARENT,
            text_align: TextAlign::Start,
            white_space: WhiteSpace::Normal,
            z_index: None,
            transform: None,
            pointer_events: PointerEvents::Auto,
            cursor: "auto".into(),
            letter_spacing: None,
            word_spacing: 0.0,
            direction: Direction::Ltr,
            aspect_ratio: None,
            background: Rc::new(crate::bg::Background::default()),
            radius: [
                (Lp::ZERO, Lp::ZERO),
                (Lp::ZERO, Lp::ZERO),
                (Lp::ZERO, Lp::ZERO),
                (Lp::ZERO, Lp::ZERO),
            ],
            box_shadow: Rc::from(Vec::new()),
            custom: Rc::new(CustomMap::new()),
        }
    }

    /// Style for an element: inherited properties from `parent`, the rest
    /// initial.
    pub fn inherit_from(parent: &ComputedStyle, initial: &ComputedStyle) -> ComputedStyle {
        let mut s = initial.clone();
        for &id in LonghandId::ALL {
            if id.inherited() {
                copy_field(id, parent, &mut s);
            }
        }
        s.font_size_keyword = parent.font_size_keyword;
        s.custom = parent.custom.clone();
        s
    }

    /// Used line height in px. `normal` needs font metrics, so the caller
    /// passes the font's normal line-height factor (e.g. 1.15 for Arial).
    pub fn line_height_px(&self, normal_factor: f32) -> f32 {
        match self.line_height {
            LineHeight::Normal => self.font_size * normal_factor,
            LineHeight::Number(n) => self.font_size * n,
            LineHeight::Px(v) => v,
        }
    }

    /// `display: none`.
    pub fn is_display_none(&self) -> bool {
        self.display == Display::None
    }

    /// The element is a scroll container in either axis.
    pub fn is_scroll_container(&self) -> bool {
        self.overflow_x.is_scrollable() || self.overflow_y.is_scrollable()
    }

    /// Value of a custom property, if set.
    pub fn custom_property(&self, name: &str) -> Option<&str> {
        self.custom.get(name).map(|v| &**v)
    }
}

/// Copies one longhand's computed value.
pub(crate) fn copy_field(id: LonghandId, from: &ComputedStyle, to: &mut ComputedStyle) {
    use LonghandId as L;
    match id {
        L::Display => to.display = from.display,
        L::Position => to.position = from.position,
        L::Float => to.float = from.float,
        L::Clear => to.clear = from.clear,
        L::Visibility => to.visibility = from.visibility,
        L::Opacity => to.opacity = from.opacity,
        L::OverflowX => to.overflow_x = from.overflow_x,
        L::OverflowY => to.overflow_y = from.overflow_y,
        L::Width => to.width = from.width.clone(),
        L::Height => to.height = from.height.clone(),
        L::MinWidth => to.min_width = from.min_width.clone(),
        L::MinHeight => to.min_height = from.min_height.clone(),
        L::MaxWidth => to.max_width = from.max_width.clone(),
        L::MaxHeight => to.max_height = from.max_height.clone(),
        L::MarginTop => to.margin[0] = from.margin[0].clone(),
        L::MarginRight => to.margin[1] = from.margin[1].clone(),
        L::MarginBottom => to.margin[2] = from.margin[2].clone(),
        L::MarginLeft => to.margin[3] = from.margin[3].clone(),
        L::PaddingTop => to.padding[0] = from.padding[0].clone(),
        L::PaddingRight => to.padding[1] = from.padding[1].clone(),
        L::PaddingBottom => to.padding[2] = from.padding[2].clone(),
        L::PaddingLeft => to.padding[3] = from.padding[3].clone(),
        L::BorderTopWidth => to.border_width[0] = from.border_width[0],
        L::BorderRightWidth => to.border_width[1] = from.border_width[1],
        L::BorderBottomWidth => to.border_width[2] = from.border_width[2],
        L::BorderLeftWidth => to.border_width[3] = from.border_width[3],
        L::BorderTopStyle => to.border_style[0] = from.border_style[0],
        L::BorderRightStyle => to.border_style[1] = from.border_style[1],
        L::BorderBottomStyle => to.border_style[2] = from.border_style[2],
        L::BorderLeftStyle => to.border_style[3] = from.border_style[3],
        L::BorderTopColor => to.border_color[0] = from.border_color[0],
        L::BorderRightColor => to.border_color[1] = from.border_color[1],
        L::BorderBottomColor => to.border_color[2] = from.border_color[2],
        L::BorderLeftColor => to.border_color[3] = from.border_color[3],
        L::Top => to.inset[0] = from.inset[0].clone(),
        L::Right => to.inset[1] = from.inset[1].clone(),
        L::Bottom => to.inset[2] = from.inset[2].clone(),
        L::Left => to.inset[3] = from.inset[3].clone(),
        L::BoxSizing => to.box_sizing = from.box_sizing,
        L::FlexDirection => to.flex_direction = from.flex_direction,
        L::FlexWrap => to.flex_wrap = from.flex_wrap,
        L::FlexGrow => to.flex_grow = from.flex_grow,
        L::FlexShrink => to.flex_shrink = from.flex_shrink,
        L::FlexBasis => to.flex_basis = from.flex_basis.clone(),
        L::Order => to.order = from.order,
        L::RowGap => to.row_gap = from.row_gap.clone(),
        L::ColumnGap => to.column_gap = from.column_gap.clone(),
        L::GridTemplateRows => to.grid_template_rows = from.grid_template_rows.clone(),
        L::GridTemplateColumns => to.grid_template_columns = from.grid_template_columns.clone(),
        L::GridRowStart => to.grid_row_start = from.grid_row_start.clone(),
        L::GridRowEnd => to.grid_row_end = from.grid_row_end.clone(),
        L::GridColumnStart => to.grid_column_start = from.grid_column_start.clone(),
        L::GridColumnEnd => to.grid_column_end = from.grid_column_end.clone(),
        L::AlignItems => to.align_items = from.align_items,
        L::AlignSelf => to.align_self = from.align_self,
        L::AlignContent => to.align_content = from.align_content,
        L::JustifyContent => to.justify_content = from.justify_content,
        L::JustifyItems => to.justify_items = from.justify_items,
        L::JustifySelf => to.justify_self = from.justify_self,
        L::FontFamily => to.font_family = from.font_family.clone(),
        L::FontSize => {
            to.font_size = from.font_size;
            to.font_size_keyword = from.font_size_keyword;
        }
        L::FontWeight => to.font_weight = from.font_weight,
        L::FontStyle => to.font_style = from.font_style,
        L::LineHeight => to.line_height = from.line_height,
        L::Color => to.color = from.color,
        L::BackgroundColor => to.background_color = from.background_color,
        L::TextAlign => to.text_align = from.text_align,
        L::WhiteSpace => to.white_space = from.white_space,
        L::ZIndex => to.z_index = from.z_index,
        L::Transform => to.transform = from.transform.clone(),
        L::PointerEvents => to.pointer_events = from.pointer_events,
        L::Cursor => to.cursor = from.cursor.clone(),
        L::LetterSpacing => to.letter_spacing = from.letter_spacing,
        L::WordSpacing => to.word_spacing = from.word_spacing,
        L::Direction => to.direction = from.direction,
        L::AspectRatio => to.aspect_ratio = from.aspect_ratio,
        L::BackgroundImage => {
            Rc::make_mut(&mut to.background).images = from.background.images.clone()
        }
        L::BackgroundPositionX => {
            Rc::make_mut(&mut to.background).pos_x = from.background.pos_x.clone()
        }
        L::BackgroundPositionY => {
            Rc::make_mut(&mut to.background).pos_y = from.background.pos_y.clone()
        }
        L::BackgroundSize => Rc::make_mut(&mut to.background).size = from.background.size.clone(),
        L::BackgroundRepeat => {
            Rc::make_mut(&mut to.background).repeat = from.background.repeat.clone()
        }
        L::BackgroundAttachment => {
            Rc::make_mut(&mut to.background).attachment = from.background.attachment.clone()
        }
        L::BackgroundOrigin => {
            Rc::make_mut(&mut to.background).origin = from.background.origin.clone()
        }
        L::BackgroundClip => Rc::make_mut(&mut to.background).clip = from.background.clip.clone(),
        L::BorderTopLeftRadius => to.radius[0] = from.radius[0].clone(),
        L::BorderTopRightRadius => to.radius[1] = from.radius[1].clone(),
        L::BorderBottomRightRadius => to.radius[2] = from.radius[2].clone(),
        L::BorderBottomLeftRadius => to.radius[3] = from.radius[3].clone(),
        L::BoxShadow => to.box_shadow = from.box_shadow.clone(),
    }
}

/// Inputs to value computation besides the declarations.
pub(crate) struct ComputeCtx<'a> {
    pub env: &'a MediaEnv,
    pub defaults: &'a Defaults,
    pub initial: &'a ComputedStyle,
    /// Root element font size (for `rem`); `None` while computing the root.
    pub root_font_size: Option<f32>,
    /// The element is the root element.
    pub is_root: bool,
    /// Base URL for relative `url()`s (the document's).
    pub base: Option<&'a url::Url>,
}

/// What the cascade found for each longhand (`None` = not declared).
pub(crate) type Cascaded<'a> = Vec<Option<&'a Decl>>;

/// Longhand values parsed from one declaration.
type Parsed = Vec<(LonghandId, Sv)>;

/// Order in which longhands are computed: font properties first (for
/// `em`), then `color` (for `currentcolor`), then everything else.
fn compute_order() -> impl Iterator<Item = LonghandId> {
    use LonghandId as L;
    [L::FontFamily, L::FontSize, L::Color].into_iter().chain(
        LonghandId::ALL
            .iter()
            .copied()
            .filter(|id| !matches!(id, L::FontFamily | L::FontSize | L::Color)),
    )
}

/// Builds the computed style for one element.
pub(crate) fn compute(
    cascaded: &Cascaded<'_>,
    customs: &[&CustomDeclaration],
    parent: Option<&ComputedStyle>,
    parent_display: Option<Display>,
    ctx: &ComputeCtx<'_>,
) -> ComputedStyle {
    let mut s = match parent {
        Some(p) => ComputedStyle::inherit_from(p, ctx.initial),
        None => ctx.initial.clone(),
    };
    // Custom properties first: they feed var().
    if !customs.is_empty() {
        let mut map: CustomMap = (*s.custom).clone();
        for c in customs {
            map.insert(c.name.clone(), c.value.clone());
        }
        let raw = map.clone();
        for c in customs {
            if c.value.to_ascii_lowercase().contains("var(") {
                match substitute(&c.value, &raw, 0) {
                    Some(v) => {
                        map.insert(c.name.clone(), v.into());
                    }
                    None => {
                        map.remove(&c.name);
                    }
                }
            }
        }
        s.custom = Rc::new(map);
    }
    let mut pending_cache: Vec<(*const Pending, Option<Parsed>)> = Vec::new();
    let font_family_declared = cascaded[LonghandId::FontFamily as usize].is_some();
    let mut font_size_declared = false;
    let mut border_color_set = [false; 4];
    for id in compute_order() {
        let Some(decl) = cascaded[id as usize] else {
            continue;
        };
        if id == LonghandId::FontSize {
            font_size_declared = true;
        }
        match decl {
            Decl::Inherit => match parent {
                Some(p) => copy_field(id, p, &mut s),
                None => copy_field(id, ctx.initial, &mut s),
            },
            Decl::Initial => copy_field(id, ctx.initial, &mut s),
            Decl::Unset => {}
            Decl::Value(v) => apply(id, v, &mut s, parent, ctx, &mut border_color_set),
            Decl::Pending(pend) => {
                let key = Rc::as_ptr(pend);
                let idx = match pending_cache.iter().position(|(k, _)| *k == key) {
                    Some(i) => i,
                    None => {
                        let parsed = substitute(&pend.css, &s.custom, 0)
                            .and_then(|text| parse_property(&pend.property, &text));
                        pending_cache.push((key, parsed));
                        pending_cache.len() - 1
                    }
                };
                let value = pending_cache[idx]
                    .1
                    .as_ref()
                    .and_then(|vals| vals.iter().find(|(i, _)| *i == id))
                    .map(|(_, v)| v.clone());
                match value {
                    Some(v) => apply(id, &v, &mut s, parent, ctx, &mut border_color_set),
                    None => {
                        // Invalid at computed-value time: behaves as unset.
                    }
                }
            }
        }
    }
    // `currentcolor` (the initial border color) follows the final color.
    for (i, set) in border_color_set.iter().enumerate() {
        if !set {
            s.border_color[i] = s.color;
        }
    }
    // A family change rescales keyword sizes (monospace 13px vs 16px).
    if font_family_declared
        && !font_size_declared
        && let Some(k) = s.font_size_keyword
    {
        s.font_size = keyword_base(&s.font_family, ctx.defaults) * k;
    }
    // Borders with no style have no width.
    for i in 0..4 {
        if matches!(s.border_style[i], BorderStyle::None | BorderStyle::Hidden) {
            s.border_width[i] = 0.0;
        }
    }
    fixup(&mut s, parent_display, ctx);
    s
}

/// Value fixups that depend on several properties (CSS 2.1 §9.7 and
/// friends).
fn fixup(s: &mut ComputedStyle, parent_display: Option<Display>, ctx: &ComputeCtx<'_>) {
    let out_of_flow = matches!(s.position, Position::Absolute | Position::Fixed);
    if out_of_flow {
        s.float = Float::None;
    }
    let parent_is_fg = parent_display.is_some_and(|d| d.is_flex() || d.is_grid());
    if s.display != Display::None
        && (out_of_flow || s.float != Float::None || ctx.is_root || parent_is_fg)
    {
        if s.display == Display::Contents && ctx.is_root {
            s.display = Display::Block;
        } else if s.display != Display::Contents {
            s.display = s.display.blockified();
        }
    }
    // `overflow`: visible/clip pairs with a scrolling axis.
    let fix = |o: Overflow| match o {
        Overflow::Visible => Overflow::Auto,
        Overflow::Clip => Overflow::Hidden,
        o => o,
    };
    if s.overflow_x.is_scrollable() && !s.overflow_y.is_scrollable() {
        s.overflow_y = fix(s.overflow_y);
    } else if s.overflow_y.is_scrollable() && !s.overflow_x.is_scrollable() {
        s.overflow_x = fix(s.overflow_x);
    }
}

/// `medium` for this family list: Chrome's monospace default is 13px.
fn keyword_base(families: &[FamilyName], defaults: &Defaults) -> f32 {
    if families.len() == 1 && families[0].generic && families[0].name == "monospace" {
        defaults.monospace_size
    } else {
        defaults.font_size
    }
}

/// Resolves a specified length to px/% for `font_size` (em basis).
fn to_lp(c: &Calc, em: f32, ctx: &ComputeCtx<'_>) -> Lp {
    let rem = ctx.root_font_size.unwrap_or(ctx.defaults.font_size);
    let env = ctx.env;
    let vw = env.viewport_width / 100.0;
    let vh = env.viewport_height / 100.0;
    Lp::from_calc(c.map_units(&|v, u| match u {
        Unit::Px => Calc::Dim(v, Unit::Px),
        Unit::Pct => Calc::Dim(v, Unit::Pct),
        Unit::Em => Calc::Dim(v * em, Unit::Px),
        Unit::Rem => Calc::Dim(v * rem, Unit::Px),
        Unit::Ex | Unit::Ch => Calc::Dim(v * em * 0.5, Unit::Px),
        Unit::Vw => Calc::Dim(v * vw, Unit::Px),
        Unit::Vh => Calc::Dim(v * vh, Unit::Px),
        Unit::Vmin => Calc::Dim(v * vw.min(vh), Unit::Px),
        Unit::Vmax => Calc::Dim(v * vw.max(vh), Unit::Px),
    }))
}

fn snap_border(v: f32, dpr: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    let dev = v * dpr;
    if dev < 1.0 {
        1.0 / dpr
    } else {
        dev.floor() / dpr
    }
}

fn apply(
    id: LonghandId,
    v: &Sv,
    s: &mut ComputedStyle,
    parent: Option<&ComputedStyle>,
    ctx: &ComputeCtx<'_>,
    border_color_set: &mut [bool; 4],
) {
    use LonghandId as L;
    let em = s.font_size;
    let lp = |c: &Calc| to_lp(c, em, ctx);
    let lp_auto = |v: &Sv| match v {
        Sv::Length(c) => LpAuto::Lp(lp(c)),
        _ => LpAuto::Auto,
    };
    let px = |v: &Sv| match v {
        Sv::Length(c) => lp(c).resolve(0.0),
        _ => 0.0,
    };
    match (id, v) {
        (L::Display, Sv::Display(d)) => s.display = *d,
        (L::Position, Sv::Position(p)) => s.position = *p,
        (L::Float, Sv::Float(f)) => s.float = *f,
        (L::Clear, Sv::Clear(c)) => s.clear = *c,
        (L::Visibility, Sv::Visibility(x)) => s.visibility = *x,
        (L::Opacity, Sv::Number(n)) => s.opacity = *n,
        (L::OverflowX, Sv::Overflow(o)) => s.overflow_x = *o,
        (L::OverflowY, Sv::Overflow(o)) => s.overflow_y = *o,
        (L::Width, v) => s.width = lp_auto(v),
        (L::Height, v) => s.height = lp_auto(v),
        (L::MinWidth, v) => s.min_width = lp_auto(v),
        (L::MinHeight, v) => s.min_height = lp_auto(v),
        (L::MaxWidth, v) => s.max_width = lp_auto(v).lp().cloned(),
        (L::MaxHeight, v) => s.max_height = lp_auto(v).lp().cloned(),
        (L::MarginTop, v) => s.margin[0] = lp_auto(v),
        (L::MarginRight, v) => s.margin[1] = lp_auto(v),
        (L::MarginBottom, v) => s.margin[2] = lp_auto(v),
        (L::MarginLeft, v) => s.margin[3] = lp_auto(v),
        (L::PaddingTop, Sv::Length(c)) => s.padding[0] = lp(c),
        (L::PaddingRight, Sv::Length(c)) => s.padding[1] = lp(c),
        (L::PaddingBottom, Sv::Length(c)) => s.padding[2] = lp(c),
        (L::PaddingLeft, Sv::Length(c)) => s.padding[3] = lp(c),
        (L::BorderTopWidth, v) => {
            s.border_width[0] = snap_border(px(v), ctx.env.device_pixel_ratio)
        }
        (L::BorderRightWidth, v) => {
            s.border_width[1] = snap_border(px(v), ctx.env.device_pixel_ratio)
        }
        (L::BorderBottomWidth, v) => {
            s.border_width[2] = snap_border(px(v), ctx.env.device_pixel_ratio)
        }
        (L::BorderLeftWidth, v) => {
            s.border_width[3] = snap_border(px(v), ctx.env.device_pixel_ratio)
        }
        (L::BorderTopStyle, Sv::BorderStyle(b)) => s.border_style[0] = *b,
        (L::BorderRightStyle, Sv::BorderStyle(b)) => s.border_style[1] = *b,
        (L::BorderBottomStyle, Sv::BorderStyle(b)) => s.border_style[2] = *b,
        (L::BorderLeftStyle, Sv::BorderStyle(b)) => s.border_style[3] = *b,
        (
            L::BorderTopColor | L::BorderRightColor | L::BorderBottomColor | L::BorderLeftColor,
            Sv::Color(c),
        ) => {
            let i = match id {
                L::BorderTopColor => 0,
                L::BorderRightColor => 1,
                L::BorderBottomColor => 2,
                _ => 3,
            };
            match c {
                ColorValue::Rgba(c) => {
                    s.border_color[i] = *c;
                    border_color_set[i] = true;
                }
                ColorValue::CurrentColor => {
                    s.border_color[i] = s.color;
                    border_color_set[i] = false;
                }
            }
        }
        (L::Top, v) => s.inset[0] = lp_auto(v),
        (L::Right, v) => s.inset[1] = lp_auto(v),
        (L::Bottom, v) => s.inset[2] = lp_auto(v),
        (L::Left, v) => s.inset[3] = lp_auto(v),
        (L::BoxSizing, Sv::BoxSizing(b)) => s.box_sizing = *b,
        (L::FlexDirection, Sv::FlexDirection(d)) => s.flex_direction = *d,
        (L::FlexWrap, Sv::FlexWrap(w)) => s.flex_wrap = *w,
        (L::FlexGrow, Sv::Number(n)) => s.flex_grow = *n,
        (L::FlexShrink, Sv::Number(n)) => s.flex_shrink = *n,
        (L::FlexBasis, v) => s.flex_basis = lp_auto(v),
        (L::Order, Sv::Integer(n)) => s.order = *n,
        (L::RowGap, v) => s.row_gap = lp_auto(v).lp().cloned(),
        (L::ColumnGap, v) => s.column_gap = lp_auto(v).lp().cloned(),
        (L::GridTemplateRows | L::GridTemplateColumns, v) => {
            let tracks = match v {
                Sv::Tracks(t) => Some(Rc::from(
                    t.iter().map(|e| e.map(&|c| lp(c))).collect::<Vec<_>>(),
                )),
                _ => None,
            };
            if id == L::GridTemplateRows {
                s.grid_template_rows = tracks;
            } else {
                s.grid_template_columns = tracks;
            }
        }
        (L::GridRowStart, Sv::GridLine(g)) => s.grid_row_start = g.clone(),
        (L::GridRowEnd, Sv::GridLine(g)) => s.grid_row_end = g.clone(),
        (L::GridColumnStart, Sv::GridLine(g)) => s.grid_column_start = g.clone(),
        (L::GridColumnEnd, Sv::GridLine(g)) => s.grid_column_end = g.clone(),
        (L::AlignItems, Sv::Align(a)) => s.align_items = *a,
        (L::AlignSelf, Sv::Align(a)) => s.align_self = *a,
        (L::AlignContent, Sv::Align(a)) => s.align_content = *a,
        (L::JustifyContent, Sv::Align(a)) => s.justify_content = *a,
        (L::JustifyItems, Sv::Align(a)) => s.justify_items = *a,
        (L::JustifySelf, Sv::Align(a)) => s.justify_self = *a,
        (L::FontFamily, Sv::FontFamily(f)) => s.font_family = f.clone(),
        (L::FontFamily, Sv::SystemFont(k)) => {
            s.font_family = Rc::from(vec![FamilyName {
                name: ctx.defaults.system_font(*k).0.to_owned(),
                generic: false,
            }]);
        }
        (L::FontSize, Sv::FontSize(spec)) => {
            let parent_size = parent.map_or(ctx.defaults.font_size, |p| p.font_size);
            let parent_kw = parent.map_or(Some(1.0), |p| p.font_size_keyword);
            match spec {
                FontSizeSpec::Keyword(r) => {
                    s.font_size = keyword_base(&s.font_family, ctx.defaults) * r;
                    s.font_size_keyword = Some(*r);
                }
                FontSizeSpec::System(k) => {
                    s.font_size = ctx.defaults.system_font(*k).1;
                    s.font_size_keyword = None;
                }
                FontSizeSpec::Larger => {
                    s.font_size = parent_size * 1.2;
                    s.font_size_keyword = parent_kw.map(|k| k * 1.2);
                }
                FontSizeSpec::Smaller => {
                    s.font_size = parent_size / 1.2;
                    s.font_size_keyword = parent_kw.map(|k| k / 1.2);
                }
                FontSizeSpec::Length(c) => {
                    // em and % refer to the parent's font size.
                    let is_relative = matches!(c, Calc::Dim(_, Unit::Em | Unit::Pct));
                    let v = to_lp(c, parent_size, ctx).resolve(parent_size).max(0.0);
                    s.font_size = v;
                    s.font_size_keyword = if is_relative {
                        parent_kw.map(|k| k * v / parent_size.max(f32::MIN_POSITIVE))
                    } else {
                        None
                    };
                }
            }
        }
        (L::FontWeight, Sv::FontWeight(w)) => {
            let pw = parent.map_or(400.0, |p| p.font_weight);
            s.font_weight = match w {
                FontWeightSpec::Absolute(n) => *n,
                FontWeightSpec::Bolder => {
                    if pw < 350.0 {
                        400.0
                    } else if pw < 550.0 {
                        700.0
                    } else {
                        pw.max(900.0)
                    }
                }
                FontWeightSpec::Lighter => {
                    if pw < 550.0 {
                        100.0
                    } else if pw < 750.0 {
                        400.0
                    } else {
                        700.0
                    }
                }
            };
        }
        (L::FontStyle, Sv::FontStyle(f)) => s.font_style = *f,
        (L::LineHeight, v) => {
            s.line_height = match v {
                Sv::Number(n) => LineHeight::Number(*n),
                Sv::Length(c) => LineHeight::Px(lp(c).resolve(em)),
                _ => LineHeight::Normal,
            }
        }
        (L::Color, Sv::Color(c)) => {
            s.color = c.resolve(parent.map_or(Color::BLACK, |p| p.color));
        }
        (L::BackgroundColor, Sv::Color(c)) => s.background_color = c.resolve(s.color),
        (L::TextAlign, Sv::TextAlign(t)) => {
            s.text_align = if *t == TextAlign::MatchParent {
                parent.map_or(TextAlign::Start, |p| p.text_align)
            } else {
                *t
            }
        }
        (L::WhiteSpace, Sv::WhiteSpace(w)) => s.white_space = *w,
        (L::ZIndex, v) => {
            s.z_index = match v {
                Sv::Integer(n) => Some(*n),
                _ => None,
            }
        }
        (L::Transform, v) => {
            s.transform = match v {
                Sv::Transform(css) => Some(Transform {
                    css: css.clone(),
                    matrix: transform_matrix(css, em, ctx),
                }),
                _ => None,
            }
        }
        (L::PointerEvents, Sv::PointerEvents(p)) => s.pointer_events = *p,
        (L::Cursor, Sv::Cursor(c)) => s.cursor = c.clone(),
        (L::LetterSpacing, v) => {
            s.letter_spacing = match v {
                Sv::Length(c) => Some(lp(c).resolve(em)),
                _ => None,
            }
        }
        (L::WordSpacing, v) => {
            s.word_spacing = match v {
                Sv::Length(c) => lp(c).resolve(em),
                _ => 0.0,
            }
        }
        (L::Direction, Sv::Direction(d)) => s.direction = *d,
        (L::AspectRatio, v) => {
            s.aspect_ratio = match v {
                Sv::Ratio(a, b) => Some(a / b),
                _ => None,
            }
        }
        (L::BackgroundImage, Sv::Images(list)) => {
            let current = s.color;
            let images = list
                .iter()
                .map(|i| crate::bg::compute_image(i, &lp, current, ctx.base))
                .collect();
            Rc::make_mut(&mut s.background).images = images;
        }
        (L::BackgroundPositionX, Sv::Positions(list)) => {
            Rc::make_mut(&mut s.background).pos_x = list.iter().map(lp).collect();
        }
        (L::BackgroundPositionY, Sv::Positions(list)) => {
            Rc::make_mut(&mut s.background).pos_y = list.iter().map(lp).collect();
        }
        (L::BackgroundSize, Sv::Sizes(list)) => {
            Rc::make_mut(&mut s.background).size = list
                .iter()
                .map(|z| crate::bg::compute_size(z, &lp))
                .collect();
        }
        (L::BackgroundRepeat, Sv::Repeats(list)) => {
            Rc::make_mut(&mut s.background).repeat = list.to_vec();
        }
        (L::BackgroundAttachment, Sv::Attachments(list)) => {
            Rc::make_mut(&mut s.background).attachment = list.to_vec();
        }
        (L::BackgroundOrigin, Sv::Boxes(list)) => {
            Rc::make_mut(&mut s.background).origin = list.to_vec();
        }
        (L::BackgroundClip, Sv::Boxes(list)) => {
            Rc::make_mut(&mut s.background).clip = list.to_vec();
        }
        (L::BorderTopLeftRadius, Sv::Radius(a, b)) => s.radius[0] = (lp(a), lp(b)),
        (L::BorderTopRightRadius, Sv::Radius(a, b)) => s.radius[1] = (lp(a), lp(b)),
        (L::BorderBottomRightRadius, Sv::Radius(a, b)) => s.radius[2] = (lp(a), lp(b)),
        (L::BorderBottomLeftRadius, Sv::Radius(a, b)) => s.radius[3] = (lp(a), lp(b)),
        (L::BoxShadow, Sv::Shadows(list)) => {
            let current = s.color;
            let px = |c: &Calc| lp(c).resolve(0.0);
            s.box_shadow = list
                .iter()
                .map(|x| crate::bg::compute_shadow(x, &px, current))
                .collect();
        }
        _ => {}
    }
}

/// Computes a 2D matrix for the common transform functions. `None` when a
/// value needs layout (percentages) or a function is not supported.
fn transform_matrix(css: &str, em: f32, ctx: &ComputeCtx<'_>) -> Option<Matrix> {
    let mut p = Parser::new(css);
    let mut m: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    while !p.is_exhausted() {
        let name = p.expect_function().ok()?.to_ascii_lowercase();
        let args: Vec<Arg> = p
            .parse_nested_block(|p| {
                let mut out = Vec::new();
                while !p.is_exhausted() {
                    let _ = p.try_parse(|p| p.expect_comma());
                    out.push(parse_arg(p, em, ctx)?);
                }
                Ok::<_, cssparser::ParseError<()>>(out)
            })
            .ok()?;
        let num = |i: usize| match args.get(i) {
            Some(Arg::Num(v)) => Some(*v),
            _ => None,
        };
        let len = |i: usize| match args.get(i) {
            Some(Arg::Px(v)) => Some(*v),
            Some(Arg::Num(v)) if *v == 0.0 => Some(0.0),
            None => Some(0.0),
            _ => None,
        };
        let ang = |i: usize| match args.get(i) {
            Some(Arg::Rad(v)) => Some(*v),
            Some(Arg::Num(v)) if *v == 0.0 => Some(0.0),
            None => Some(0.0),
            _ => None,
        };
        let t: Matrix = match name.as_str() {
            "matrix" => [num(0)?, num(1)?, num(2)?, num(3)?, num(4)?, num(5)?],
            "translate" => [1.0, 0.0, 0.0, 1.0, len(0)?, len(1)?],
            "translatex" => [1.0, 0.0, 0.0, 1.0, len(0)?, 0.0],
            "translatey" => [1.0, 0.0, 0.0, 1.0, 0.0, len(0)?],
            "scale" => {
                let x = num(0)?;
                let y = num(1).unwrap_or(x);
                [x, 0.0, 0.0, y, 0.0, 0.0]
            }
            "scalex" => [num(0)?, 0.0, 0.0, 1.0, 0.0, 0.0],
            "scaley" => [1.0, 0.0, 0.0, num(0)?, 0.0, 0.0],
            "rotate" | "rotatez" => {
                let a = ang(0)?;
                [a.cos(), a.sin(), -a.sin(), a.cos(), 0.0, 0.0]
            }
            "skew" => [1.0, ang(1)?.tan(), ang(0)?.tan(), 1.0, 0.0, 0.0],
            "skewx" => [1.0, 0.0, ang(0)?.tan(), 1.0, 0.0, 0.0],
            "skewy" => [1.0, ang(0)?.tan(), 0.0, 1.0, 0.0, 0.0],
            _ => return None,
        };
        m = multiply(m, t);
    }
    Some(m)
}

enum Arg {
    Num(f32),
    Px(f32),
    Rad(f32),
    Other,
}

fn parse_arg<'i>(
    p: &mut Parser<'i>,
    em: f32,
    ctx: &ComputeCtx<'_>,
) -> Result<Arg, cssparser::ParseError<()>> {
    let state = p.state();
    if let Ok(Token::Dimension { value, unit, .. }) = p.next().cloned() {
        let rad = match unit.to_ascii_lowercase().as_str() {
            "deg" => Some(value.to_radians()),
            "rad" => Some(value),
            "grad" => Some(value * std::f32::consts::PI / 200.0),
            "turn" => Some(value * std::f32::consts::TAU),
            _ => None,
        };
        if let Some(r) = rad {
            return Ok(Arg::Rad(r));
        }
    }
    p.reset(&state);
    if let Ok(n) = p.try_parse(crate::values::parse_number) {
        return Ok(Arg::Num(n));
    }
    if let Ok(c) = p.try_parse(|p| crate::values::parse_length(p, true, true)) {
        let l = to_lp(&c, em, ctx);
        return Ok(match l.as_px() {
            Some(v) => Arg::Px(v),
            None => Arg::Other,
        });
    }
    p.next()?;
    Ok(Arg::Other)
}

fn multiply(a: Matrix, b: Matrix) -> Matrix {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}

/// Replaces `var()` references (and `env()`, with its fallback) in `css`.
/// `None` if a reference has no value and no fallback, or on a cycle.
pub(crate) fn substitute(css: &str, customs: &CustomMap, depth: u32) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let mut p = Parser::new(css);
    let mut out = String::with_capacity(css.len());
    substitute_into(&mut p, customs, depth, &mut out).ok()?;
    Some(out)
}

fn substitute_into<'i>(
    p: &mut Parser<'i>,
    customs: &CustomMap,
    depth: u32,
    out: &mut String,
) -> Result<(), ()> {
    loop {
        let start = p.position();
        let Ok(tok) = p.next_including_whitespace_and_comments().cloned() else {
            return Ok(());
        };
        match tok {
            Token::Function(name)
                if name.eq_ignore_ascii_case("var") || name.eq_ignore_ascii_case("env") =>
            {
                let is_env = name.eq_ignore_ascii_case("env");
                let (var, fallback) = p
                    .parse_nested_block(|p| {
                        let n = p.expect_ident_cloned()?.to_string();
                        let fb = if p.try_parse(|p| p.expect_comma()).is_ok() {
                            let s = p.position();
                            while p.next_including_whitespace_and_comments().is_ok() {}
                            Some(p.slice_from(s).to_owned())
                        } else {
                            None
                        };
                        Ok::<_, cssparser::ParseError<()>>((n, fb))
                    })
                    .map_err(|_| ())?;
                let value = if is_env {
                    None
                } else {
                    customs.get(var.as_str()).map(|v| v.to_string())
                };
                let text = match (value, fallback) {
                    (Some(v), _) => v,
                    (None, Some(f)) => f,
                    (None, None) => return Err(()),
                };
                let resolved = substitute(&text, customs, depth + 1).ok_or(())?;
                out.push_str(resolved.trim());
            }
            Token::Function(_) | Token::ParenthesisBlock => {
                out.push_str(p.slice_from(start));
                p.parse_nested_block(|p| {
                    substitute_into(p, customs, depth, out)
                        .map_err(|_| cssparser::ParseError::<()>::custom(()))
                })
                .map_err(|_| ())?;
                out.push(')');
            }
            Token::SquareBracketBlock => {
                out.push('[');
                p.parse_nested_block(|p| {
                    substitute_into(p, customs, depth, out)
                        .map_err(|_| cssparser::ParseError::<()>::custom(()))
                })
                .map_err(|_| ())?;
                out.push(']');
            }
            Token::CurlyBracketBlock => {
                out.push('{');
                p.parse_nested_block(|p| {
                    substitute_into(p, customs, depth, out)
                        .map_err(|_| cssparser::ParseError::<()>::custom(()))
                })
                .map_err(|_| ())?;
                out.push('}');
            }
            _ => out.push_str(p.slice_from(start)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_vars_with_fallback() {
        let mut m = CustomMap::new();
        m.insert("--a".into(), "10px".into());
        m.insert("--b".into(), "var(--a) 2px".into());
        assert_eq!(substitute("var(--a)", &m, 0).unwrap(), "10px");
        assert_eq!(
            substitute("calc(var(--a) * 2)", &m, 0).unwrap(),
            "calc(10px * 2)"
        );
        assert_eq!(substitute("var(--b)", &m, 0).unwrap(), "10px 2px");
        assert_eq!(substitute("var(--x, 3px)", &m, 0).unwrap(), "3px");
        assert!(substitute("var(--x)", &m, 0).is_none());
        m.insert("--c".into(), "var(--c)".into());
        assert!(substitute("var(--c)", &m, 0).is_none());
    }

    #[test]
    fn matrix_math() {
        let env = MediaEnv::default();
        let d = Defaults::for_os(OsKind::Windows);
        let init = ComputedStyle::initial(&d);
        let ctx = ComputeCtx {
            env: &env,
            defaults: &d,
            initial: &init,
            root_font_size: None,
            is_root: false,
            base: None,
        };
        let m = transform_matrix("translate(10px, 5px) scale(2)", 16.0, &ctx).unwrap();
        assert_eq!(m, [2.0, 0.0, 0.0, 2.0, 10.0, 5.0]);
        assert!(transform_matrix("translate(50%)", 16.0, &ctx).is_none());
        let r = transform_matrix("rotate(90deg)", 16.0, &ctx).unwrap();
        assert!((r[1] - 1.0).abs() < 1e-6);
    }
}

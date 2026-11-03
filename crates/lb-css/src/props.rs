//! Property ids, specified values and declaration parsing (longhands,
//! shorthands, logical properties, CSS-wide keywords and `var()`).

use std::rc::Rc;

use cssparser::{Parser, Token};

use crate::values::{
    Align, BorderStyle, BoxSizing, Calc, Clear, ColorValue, Direction, Display, FlexDirection,
    FlexWrap, Float, FontStyle, GridLine, Overflow, PResult, PointerEvents, Position, TextAlign,
    TrackListEntry, TrackSize, Visibility, WhiteSpace, bad, parse_color, parse_display,
    parse_integer, parse_length, parse_number,
};

macro_rules! longhands {
    ($($id:ident = $name:literal, $inh:expr;)*) => {
        /// Longhand properties the cascade knows about.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum LonghandId { $($id),* }

        impl LonghandId {
            /// Every longhand, in declaration order.
            pub const ALL: &'static [LonghandId] = &[$(LonghandId::$id),*];
            /// Number of longhands.
            pub const COUNT: usize = Self::ALL.len();

            /// CSS property name.
            pub fn name(self) -> &'static str {
                match self { $(LonghandId::$id => $name),* }
            }

            /// Inherited by default.
            pub fn inherited(self) -> bool {
                match self { $(LonghandId::$id => $inh),* }
            }

            /// Looks up a longhand by (lowercase) name.
            pub fn from_name(name: &str) -> Option<LonghandId> {
                match name { $($name => Some(LonghandId::$id),)* _ => None }
            }
        }
    };
}

longhands! {
    Display = "display", false;
    Position = "position", false;
    Float = "float", false;
    Clear = "clear", false;
    Visibility = "visibility", true;
    Opacity = "opacity", false;
    OverflowX = "overflow-x", false;
    OverflowY = "overflow-y", false;
    Width = "width", false;
    Height = "height", false;
    MinWidth = "min-width", false;
    MinHeight = "min-height", false;
    MaxWidth = "max-width", false;
    MaxHeight = "max-height", false;
    MarginTop = "margin-top", false;
    MarginRight = "margin-right", false;
    MarginBottom = "margin-bottom", false;
    MarginLeft = "margin-left", false;
    PaddingTop = "padding-top", false;
    PaddingRight = "padding-right", false;
    PaddingBottom = "padding-bottom", false;
    PaddingLeft = "padding-left", false;
    BorderTopWidth = "border-top-width", false;
    BorderRightWidth = "border-right-width", false;
    BorderBottomWidth = "border-bottom-width", false;
    BorderLeftWidth = "border-left-width", false;
    BorderTopStyle = "border-top-style", false;
    BorderRightStyle = "border-right-style", false;
    BorderBottomStyle = "border-bottom-style", false;
    BorderLeftStyle = "border-left-style", false;
    BorderTopColor = "border-top-color", false;
    BorderRightColor = "border-right-color", false;
    BorderBottomColor = "border-bottom-color", false;
    BorderLeftColor = "border-left-color", false;
    Top = "top", false;
    Right = "right", false;
    Bottom = "bottom", false;
    Left = "left", false;
    BoxSizing = "box-sizing", false;
    FlexDirection = "flex-direction", false;
    FlexWrap = "flex-wrap", false;
    FlexGrow = "flex-grow", false;
    FlexShrink = "flex-shrink", false;
    FlexBasis = "flex-basis", false;
    Order = "order", false;
    RowGap = "row-gap", false;
    ColumnGap = "column-gap", false;
    GridTemplateRows = "grid-template-rows", false;
    GridTemplateColumns = "grid-template-columns", false;
    GridRowStart = "grid-row-start", false;
    GridRowEnd = "grid-row-end", false;
    GridColumnStart = "grid-column-start", false;
    GridColumnEnd = "grid-column-end", false;
    AlignItems = "align-items", false;
    AlignSelf = "align-self", false;
    AlignContent = "align-content", false;
    JustifyContent = "justify-content", false;
    JustifyItems = "justify-items", false;
    JustifySelf = "justify-self", false;
    FontFamily = "font-family", true;
    FontSize = "font-size", true;
    FontWeight = "font-weight", true;
    FontStyle = "font-style", true;
    LineHeight = "line-height", true;
    Color = "color", true;
    BackgroundColor = "background-color", false;
    TextAlign = "text-align", true;
    WhiteSpace = "white-space", true;
    ZIndex = "z-index", false;
    Transform = "transform", false;
    PointerEvents = "pointer-events", true;
    Cursor = "cursor", true;
    LetterSpacing = "letter-spacing", true;
    WordSpacing = "word-spacing", true;
    Direction = "direction", true;
    AspectRatio = "aspect-ratio", false;
}

/// One `font-family` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FamilyName {
    pub name: String,
    /// A generic family keyword (`serif`, `sans-serif`, `monospace`, ...).
    pub generic: bool,
}

impl FamilyName {
    /// Serialization as Chrome's computed `font-family` prints it.
    pub fn to_css(&self) -> String {
        if self.generic || is_plain_ident_seq(&self.name) && !self.name.contains(' ') {
            self.name.clone()
        } else {
            let mut s = String::new();
            let _ = cssparser::serialize_string(&self.name, &mut s);
            s
        }
    }
}

fn is_plain_ident_seq(s: &str) -> bool {
    !s.is_empty()
        && s.split(' ').all(|w| {
            let mut c = w.chars();
            c.next()
                .is_some_and(|f| f.is_ascii_alphabetic() || f == '_' || f == '-' || !f.is_ascii())
                && c.all(|ch| {
                    ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || !ch.is_ascii()
                })
        })
}

const GENERIC_FAMILIES: &[&str] = &[
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "math",
    "emoji",
    "fangsong",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
    "-webkit-body",
    "-webkit-pictograph",
];

/// Specified `font-size`.
#[derive(Clone, Debug, PartialEq)]
pub enum FontSizeSpec {
    /// Absolute keyword as a ratio of `medium` (e.g. `large` = 1.125).
    Keyword(f32),
    /// Size of a system font keyword (`font: caption`), per persona.
    System(SystemFont),
    Larger,
    Smaller,
    /// Length or percentage of the parent font size.
    Length(Calc),
}

/// Specified `font-weight`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FontWeightSpec {
    Absolute(f32),
    Bolder,
    Lighter,
}

/// A specified value for one longhand.
#[derive(Clone, Debug, PartialEq)]
pub enum Sv {
    Display(Display),
    Position(Position),
    Float(Float),
    Clear(Clear),
    Visibility(Visibility),
    Overflow(Overflow),
    BoxSizing(BoxSizing),
    BorderStyle(BorderStyle),
    FlexDirection(FlexDirection),
    FlexWrap(FlexWrap),
    Align(Align),
    FontStyle(FontStyle),
    TextAlign(TextAlign),
    WhiteSpace(WhiteSpace),
    PointerEvents(PointerEvents),
    Direction(Direction),
    Number(f32),
    Integer(i32),
    Length(Calc),
    Auto,
    None,
    Normal,
    Color(ColorValue),
    FontFamily(Rc<[FamilyName]>),
    FontSize(FontSizeSpec),
    FontWeight(FontWeightSpec),
    Tracks(Rc<[TrackListEntry<Calc>]>),
    GridLine(GridLine),
    /// Raw `transform` text (computed into a matrix later).
    Transform(Rc<str>),
    Cursor(Rc<str>),
    Ratio(f32, f32),
    /// `font-family` of a system font keyword (`font: menu`), per persona.
    SystemFont(SystemFont),
}

/// The system font keywords of the `font` shorthand. Family and size come
/// from the persona's font defaults (`lb_css::Defaults::system_font`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SystemFont {
    Caption,
    Icon,
    Menu,
    MessageBox,
    SmallCaption,
    StatusBar,
    WebkitControl,
    WebkitSmallControl,
    WebkitMiniControl,
}

impl SystemFont {
    pub fn from_ident(s: &str) -> Option<SystemFont> {
        Some(match s.to_ascii_lowercase().as_str() {
            "caption" => SystemFont::Caption,
            "icon" => SystemFont::Icon,
            "menu" => SystemFont::Menu,
            "message-box" => SystemFont::MessageBox,
            "small-caption" => SystemFont::SmallCaption,
            "status-bar" => SystemFont::StatusBar,
            "-webkit-control" => SystemFont::WebkitControl,
            "-webkit-small-control" => SystemFont::WebkitSmallControl,
            "-webkit-mini-control" => SystemFont::WebkitMiniControl,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SystemFont::Caption => "caption",
            SystemFont::Icon => "icon",
            SystemFont::Menu => "menu",
            SystemFont::MessageBox => "message-box",
            SystemFont::SmallCaption => "small-caption",
            SystemFont::StatusBar => "status-bar",
            SystemFont::WebkitControl => "-webkit-control",
            SystemFont::WebkitSmallControl => "-webkit-small-control",
            SystemFont::WebkitMiniControl => "-webkit-mini-control",
        }
    }
}

/// A `var()`-dependent declaration, parsed after substitution.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    /// Property as declared (may be a shorthand).
    pub property: Rc<str>,
    /// Value text with `var()` references.
    pub css: Rc<str>,
}

/// What a declaration sets a longhand to.
#[derive(Clone, Debug, PartialEq)]
pub enum Decl {
    Value(Sv),
    Inherit,
    Initial,
    Unset,
    /// Contains `var()`: substituted and parsed at compute time.
    Pending(Rc<Pending>),
}

/// One longhand declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct Declaration {
    pub id: LonghandId,
    pub value: Decl,
    pub important: bool,
}

/// A custom property declaration (`--name: value`).
#[derive(Clone, Debug, PartialEq)]
pub struct CustomDeclaration {
    pub name: Rc<str>,
    pub value: Rc<str>,
    pub important: bool,
}

/// The declarations of one rule or `style` attribute.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeclarationBlock {
    pub decls: Vec<Declaration>,
    pub customs: Vec<CustomDeclaration>,
}

impl DeclarationBlock {
    /// Parses a declaration list (the inside of `{}` or a `style`
    /// attribute). Invalid declarations are dropped.
    pub fn parse(css: &str) -> DeclarationBlock {
        let mut p = Parser::new(css);
        Self::parse_from(&mut p)
    }

    pub(crate) fn parse_from(p: &mut Parser<'_>) -> DeclarationBlock {
        let mut block = DeclarationBlock::default();
        let mut dp = DeclParser { block: &mut block };
        let iter = cssparser::RuleBodyParser::new(p, &mut dp);
        for _ in iter {}
        block
    }

    /// Appends one `name: value` declaration (with optional `!important`
    /// in `value`). Returns false if it was invalid.
    pub fn push_text(&mut self, name: &str, value_with_priority: &str) -> bool {
        let (value, important) = split_important(value_with_priority);
        push_declaration(self, name, value, important)
    }
}

/// Splits a trailing `!important` off a value.
pub(crate) fn split_important(text: &str) -> (&str, bool) {
    let t = text.trim_end();
    if t.len() >= 9 && t[t.len() - 9..].eq_ignore_ascii_case("important") {
        let rest = t[..t.len() - 9].trim_end();
        if let Some(r) = rest.strip_suffix('!') {
            return (r.trim(), true);
        }
    }
    (t.trim(), false)
}

struct DeclParser<'a> {
    block: &'a mut DeclarationBlock,
}

impl<'i> cssparser::DeclarationParser<'i> for DeclParser<'_> {
    type Declaration = ();
    type Error = ();

    fn parse_value(
        &mut self,
        name: cssparser::CowRcStr<'i>,
        input: &mut Parser<'i>,
        _start: &cssparser::ParserState,
    ) -> PResult<()> {
        let start = input.position();
        while input.next_including_whitespace_and_comments().is_ok() {}
        let (value, important) = split_important(input.slice_from(start));
        if push_declaration(self.block, &name, value, important) {
            Ok(())
        } else {
            bad(input)
        }
    }
}

impl<'i> cssparser::AtRuleParser<'i> for DeclParser<'_> {
    type Prelude = ();
    type AtRule = ();
    type Error = ();
}

impl<'i> cssparser::QualifiedRuleParser<'i> for DeclParser<'_> {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = ();
}

impl<'i> cssparser::RuleBodyItemParser<'i, (), ()> for DeclParser<'_> {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}

/// Lowercases, strips `-webkit-` from known names and maps logical
/// properties to physical ones (horizontal LTR writing mode).
pub(crate) fn normalize_name(name: &str) -> String {
    let mut n = name.to_ascii_lowercase();
    if let Some(rest) = n.strip_prefix("-webkit-")
        && (LonghandId::from_name(rest).is_some() || shorthand_longhands(rest).is_some())
        && rest != "box"
    {
        n = rest.to_owned();
    }
    let logical: &[(&str, &str)] = &[
        ("inline-size", "width"),
        ("block-size", "height"),
        ("min-inline-size", "min-width"),
        ("min-block-size", "min-height"),
        ("max-inline-size", "max-width"),
        ("max-block-size", "max-height"),
        ("margin-inline-start", "margin-left"),
        ("margin-inline-end", "margin-right"),
        ("margin-block-start", "margin-top"),
        ("margin-block-end", "margin-bottom"),
        ("padding-inline-start", "padding-left"),
        ("padding-inline-end", "padding-right"),
        ("padding-block-start", "padding-top"),
        ("padding-block-end", "padding-bottom"),
        ("inset-inline-start", "left"),
        ("inset-inline-end", "right"),
        ("inset-block-start", "top"),
        ("inset-block-end", "bottom"),
        ("border-inline-start-width", "border-left-width"),
        ("border-inline-end-width", "border-right-width"),
        ("border-block-start-width", "border-top-width"),
        ("border-block-end-width", "border-bottom-width"),
        ("border-inline-start", "border-left"),
        ("border-inline-end", "border-right"),
        ("border-block-start", "border-top"),
        ("border-block-end", "border-bottom"),
        ("grid-row-gap", "row-gap"),
        ("grid-column-gap", "column-gap"),
        ("grid-gap", "gap"),
        ("word-wrap", "overflow-wrap"),
    ];
    for (from, to) in logical {
        if n == *from {
            return (*to).to_owned();
        }
    }
    n
}

use LonghandId as L;

/// Longhands set by a shorthand (or logical pair shorthand).
pub(crate) fn shorthand_longhands(name: &str) -> Option<&'static [LonghandId]> {
    Some(match name {
        "margin" => &[L::MarginTop, L::MarginRight, L::MarginBottom, L::MarginLeft],
        "padding" => &[
            L::PaddingTop,
            L::PaddingRight,
            L::PaddingBottom,
            L::PaddingLeft,
        ],
        "inset" => &[L::Top, L::Right, L::Bottom, L::Left],
        "border-width" => &[
            L::BorderTopWidth,
            L::BorderRightWidth,
            L::BorderBottomWidth,
            L::BorderLeftWidth,
        ],
        "border-style" => &[
            L::BorderTopStyle,
            L::BorderRightStyle,
            L::BorderBottomStyle,
            L::BorderLeftStyle,
        ],
        "border-color" => &[
            L::BorderTopColor,
            L::BorderRightColor,
            L::BorderBottomColor,
            L::BorderLeftColor,
        ],
        "border" => &[
            L::BorderTopWidth,
            L::BorderRightWidth,
            L::BorderBottomWidth,
            L::BorderLeftWidth,
            L::BorderTopStyle,
            L::BorderRightStyle,
            L::BorderBottomStyle,
            L::BorderLeftStyle,
            L::BorderTopColor,
            L::BorderRightColor,
            L::BorderBottomColor,
            L::BorderLeftColor,
        ],
        "border-top" => &[L::BorderTopWidth, L::BorderTopStyle, L::BorderTopColor],
        "border-right" => &[
            L::BorderRightWidth,
            L::BorderRightStyle,
            L::BorderRightColor,
        ],
        "border-bottom" => &[
            L::BorderBottomWidth,
            L::BorderBottomStyle,
            L::BorderBottomColor,
        ],
        "border-left" => &[L::BorderLeftWidth, L::BorderLeftStyle, L::BorderLeftColor],
        "margin-inline" => &[L::MarginLeft, L::MarginRight],
        "margin-block" => &[L::MarginTop, L::MarginBottom],
        "padding-inline" => &[L::PaddingLeft, L::PaddingRight],
        "padding-block" => &[L::PaddingTop, L::PaddingBottom],
        "inset-inline" => &[L::Left, L::Right],
        "inset-block" => &[L::Top, L::Bottom],
        "flex" => &[L::FlexGrow, L::FlexShrink, L::FlexBasis],
        "flex-flow" => &[L::FlexDirection, L::FlexWrap],
        "gap" => &[L::RowGap, L::ColumnGap],
        "overflow" => &[L::OverflowX, L::OverflowY],
        "place-items" => &[L::AlignItems, L::JustifyItems],
        "place-content" => &[L::AlignContent, L::JustifyContent],
        "place-self" => &[L::AlignSelf, L::JustifySelf],
        "font" => &[
            L::FontStyle,
            L::FontWeight,
            L::FontSize,
            L::LineHeight,
            L::FontFamily,
        ],
        "background" => &[L::BackgroundColor],
        "grid-row" => &[L::GridRowStart, L::GridRowEnd],
        "grid-column" => &[L::GridColumnStart, L::GridColumnEnd],
        "grid-area" => &[
            L::GridRowStart,
            L::GridColumnStart,
            L::GridRowEnd,
            L::GridColumnEnd,
        ],
        _ => return None,
    })
}

fn css_wide(value: &str) -> Option<Decl> {
    let v = value.trim();
    Some(match v.to_ascii_lowercase().as_str() {
        "inherit" => Decl::Inherit,
        "initial" => Decl::Initial,
        "unset" | "revert" | "revert-layer" => Decl::Unset,
        _ => return None,
    })
}

fn contains_var(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("var(") || lower.contains("env(")
}

/// Parses and appends one declaration. Returns false if invalid.
pub(crate) fn push_declaration(
    block: &mut DeclarationBlock,
    name: &str,
    value: &str,
    important: bool,
) -> bool {
    if let Some(custom) = name.strip_prefix("--") {
        let _ = custom;
        block.customs.push(CustomDeclaration {
            name: name.into(),
            value: value.trim().into(),
            important,
        });
        return true;
    }
    let name = normalize_name(name);
    let ids: Vec<LonghandId> = match LonghandId::from_name(&name) {
        Some(id) => vec![id],
        None => match shorthand_longhands(&name) {
            Some(ids) => ids.to_vec(),
            None => return false,
        },
    };
    if let Some(d) = css_wide(value) {
        for id in ids {
            block.decls.push(Declaration {
                id,
                value: d.clone(),
                important,
            });
        }
        return true;
    }
    if contains_var(value) {
        let pending = Rc::new(Pending {
            property: name.as_str().into(),
            css: value.into(),
        });
        for id in ids {
            block.decls.push(Declaration {
                id,
                value: Decl::Pending(pending.clone()),
                important,
            });
        }
        return true;
    }
    match parse_property(&name, value) {
        Some(values) => {
            for (id, v) in values {
                block.decls.push(Declaration {
                    id,
                    value: Decl::Value(v),
                    important,
                });
            }
            true
        }
        None => false,
    }
}

/// Parses `value` for (normalized) property `name` into longhand values.
pub(crate) fn parse_property(name: &str, value: &str) -> Option<Vec<(LonghandId, Sv)>> {
    let mut p = Parser::new(value);
    let r = p.parse_entirely(|p| parse_property_inner(name, p)).ok()?;
    Some(r)
}

fn ident_of<'i, T>(p: &mut Parser<'i>, f: impl Fn(&str) -> Option<T>) -> PResult<T> {
    let i = p.expect_ident()?;
    f(i).ok_or_else(|| cssparser::ParseError::custom(()))
}

fn lp<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    parse_length(p, true, true).map(Sv::Length)
}

fn lp_nonneg<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    parse_length(p, true, false).map(Sv::Length)
}

fn lp_auto<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
        return Ok(Sv::Auto);
    }
    lp(p)
}

fn size<'i>(p: &mut Parser<'i>, none_ok: bool, auto_ok: bool) -> PResult<Sv> {
    if let Ok(i) = p.try_parse(|p| p.expect_ident_cloned()) {
        let i = i.to_ascii_lowercase();
        return match i.as_str() {
            "auto" if auto_ok => Ok(Sv::Auto),
            "none" if none_ok => Ok(Sv::None),
            // Intrinsic keywords are approximated as auto.
            "min-content"
            | "max-content"
            | "fit-content"
            | "-webkit-fit-content"
            | "-webkit-min-content"
            | "-webkit-max-content"
            | "stretch"
            | "-webkit-fill-available" => Ok(if none_ok && !auto_ok {
                Sv::None
            } else {
                Sv::Auto
            }),
            _ => bad(p),
        };
    }
    lp_nonneg(p)
}

fn border_width<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    if let Ok(i) = p.try_parse(|p| p.expect_ident_cloned()) {
        let v = match i.to_ascii_lowercase().as_str() {
            "thin" => 1.0,
            "medium" => 3.0,
            "thick" => 5.0,
            _ => return bad(p),
        };
        return Ok(Sv::Length(Calc::Dim(v, crate::values::Unit::Px)));
    }
    parse_length(p, false, false).map(Sv::Length)
}

fn border_style<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    ident_of(p, BorderStyle::from_ident).map(Sv::BorderStyle)
}

fn color<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    parse_color(p).map(Sv::Color)
}

/// 1-4 box values (top, right, bottom, left).
fn four<'i>(p: &mut Parser<'i>, f: impl Fn(&mut Parser<'i>) -> PResult<Sv>) -> PResult<[Sv; 4]> {
    let a = f(p)?;
    let b = p.try_parse(&f).ok();
    let c = if b.is_some() {
        p.try_parse(&f).ok()
    } else {
        None
    };
    let d = if c.is_some() {
        p.try_parse(&f).ok()
    } else {
        None
    };
    Ok(match (b, c, d) {
        (None, _, _) => [a.clone(), a.clone(), a.clone(), a],
        (Some(b), None, _) => [a.clone(), b.clone(), a, b],
        (Some(b), Some(c), None) => [a, b.clone(), c, b],
        (Some(b), Some(c), Some(d)) => [a, b, c, d],
    })
}

fn two<'i>(p: &mut Parser<'i>, f: impl Fn(&mut Parser<'i>) -> PResult<Sv>) -> PResult<[Sv; 2]> {
    let a = f(p)?;
    let b = p.try_parse(&f).ok().unwrap_or_else(|| a.clone());
    Ok([a, b])
}

fn zip(ids: &[LonghandId], vals: impl IntoIterator<Item = Sv>) -> Vec<(LonghandId, Sv)> {
    ids.iter().copied().zip(vals).collect()
}

fn align<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    let _ = p.try_parse(|p| {
        let i = p.expect_ident()?;
        if i.eq_ignore_ascii_case("safe") || i.eq_ignore_ascii_case("unsafe") {
            Ok(())
        } else {
            bad(p)
        }
    });
    let i = p.expect_ident()?.to_ascii_lowercase();
    let a = match i.as_str() {
        "first" => {
            p.expect_ident_matching("baseline")?;
            Align::Baseline
        }
        "last" => {
            p.expect_ident_matching("baseline")?;
            Align::LastBaseline
        }
        "legacy" => {
            let _ = p.try_parse(|p| p.expect_ident_cloned());
            Align::Legacy
        }
        other => match Align::from_ident(other) {
            Some(a) => {
                if matches!(a, Align::Left | Align::Right | Align::Center) {
                    let _ = p.try_parse(|p| p.expect_ident_matching("legacy"));
                }
                a
            }
            None => return bad(p),
        },
    };
    Ok(Sv::Align(a))
}

fn number_nonneg<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    let n = parse_number(p)?;
    if n < 0.0 {
        return bad(p);
    }
    Ok(Sv::Number(n))
}

fn font_family<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    let list = p.parse_comma_separated(|p| {
        if let Ok(s) = p.try_parse(|p| p.expect_string_cloned()) {
            return Ok(FamilyName {
                name: s.to_string(),
                generic: false,
            });
        }
        let first = p.expect_ident_cloned()?;
        let lower = first.to_ascii_lowercase();
        let mut words = vec![first.to_string()];
        while let Ok(w) = p.try_parse(|p| p.expect_ident_cloned()) {
            words.push(w.to_string());
        }
        if words.len() == 1 && GENERIC_FAMILIES.contains(&lower.as_str()) {
            return Ok(FamilyName {
                name: lower,
                generic: true,
            });
        }
        if words.len() == 1
            && matches!(
                lower.as_str(),
                "inherit" | "initial" | "unset" | "default" | "revert"
            )
        {
            return bad(p);
        }
        Ok(FamilyName {
            name: words.join(" "),
            generic: false,
        })
    })?;
    Ok(Sv::FontFamily(list.into()))
}

/// `medium`-relative ratios of the absolute size keywords (Chrome's table
/// for a 16px default).
fn font_size_keyword(i: &str) -> Option<f32> {
    Some(match i {
        "xx-small" => 9.0 / 16.0,
        "x-small" => 10.0 / 16.0,
        "small" => 13.0 / 16.0,
        "medium" => 1.0,
        "large" => 18.0 / 16.0,
        "x-large" => 24.0 / 16.0,
        "xx-large" => 32.0 / 16.0,
        "xxx-large" => 48.0 / 16.0,
        _ => return None,
    })
}

fn font_size<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    if let Ok(i) = p.try_parse(|p| p.expect_ident_cloned()) {
        let i = i.to_ascii_lowercase();
        return match i.as_str() {
            "larger" => Ok(Sv::FontSize(FontSizeSpec::Larger)),
            "smaller" => Ok(Sv::FontSize(FontSizeSpec::Smaller)),
            k => font_size_keyword(k)
                .map(|r| Sv::FontSize(FontSizeSpec::Keyword(r)))
                .map_or_else(|| bad(p), Ok),
        };
    }
    parse_length(p, true, false).map(|c| Sv::FontSize(FontSizeSpec::Length(c)))
}

fn font_weight<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    if let Ok(i) = p.try_parse(|p| p.expect_ident_cloned()) {
        let w = match i.to_ascii_lowercase().as_str() {
            "normal" => FontWeightSpec::Absolute(400.0),
            "bold" => FontWeightSpec::Absolute(700.0),
            "bolder" => FontWeightSpec::Bolder,
            "lighter" => FontWeightSpec::Lighter,
            _ => return bad(p),
        };
        return Ok(Sv::FontWeight(w));
    }
    let n = parse_number(p)?;
    if !(1.0..=1000.0).contains(&n) {
        return bad(p);
    }
    Ok(Sv::FontWeight(FontWeightSpec::Absolute(n)))
}

fn line_height<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    if p.try_parse(|p| p.expect_ident_matching("normal")).is_ok() {
        return Ok(Sv::Normal);
    }
    if let Ok(n) = p.try_parse(parse_number) {
        if n < 0.0 {
            return bad(p);
        }
        return Ok(Sv::Number(n));
    }
    lp_nonneg(p)
}

fn track_size<'i>(p: &mut Parser<'i>) -> PResult<TrackSize<Calc>> {
    if let Ok(i) = p.try_parse(|p| p.expect_ident_cloned()) {
        return Ok(match i.to_ascii_lowercase().as_str() {
            "auto" => TrackSize::Auto,
            "min-content" => TrackSize::MinContent,
            "max-content" => TrackSize::MaxContent,
            _ => return bad(p),
        });
    }
    if let Ok(v) = p.try_parse(|p| match p.next()? {
        Token::Dimension { value, unit, .. }
            if unit.eq_ignore_ascii_case("fr") && *value >= 0.0 =>
        {
            Ok(*value)
        }
        _ => bad(p),
    }) {
        return Ok(TrackSize::Fr(v));
    }
    if let Ok(name) = p.try_parse(|p| p.expect_function().map(|f| f.to_ascii_lowercase())) {
        return p.parse_nested_block(|p| match name.as_str() {
            "minmax" => {
                let a = track_size(p)?;
                p.expect_comma()?;
                let b = track_size(p)?;
                Ok(TrackSize::MinMax(Box::new(a), Box::new(b)))
            }
            "fit-content" => Ok(TrackSize::FitContent(parse_length(p, true, false)?)),
            _ => bad(p),
        });
    }
    parse_length(p, true, false).map(TrackSize::Lp)
}

fn skip_line_names(p: &mut Parser<'_>) {
    while p
        .try_parse(|p| match p.next()? {
            Token::SquareBracketBlock => Ok(()),
            _ => bad(p),
        })
        .is_ok()
    {}
}

fn track_list<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(Sv::None);
    }
    let mut out = Vec::new();
    loop {
        skip_line_names(p);
        if p.is_exhausted() {
            break;
        }
        if p.try_parse(|p| p.expect_function_matching("repeat"))
            .is_ok()
        {
            let entry = p.parse_nested_block(|p| {
                let count = if let Ok(i) = p.try_parse(|p| p.expect_ident_cloned()) {
                    match i.to_ascii_lowercase().as_str() {
                        "auto-fill" => 0,
                        "auto-fit" => -1,
                        _ => return bad(p),
                    }
                } else {
                    let n = parse_integer(p)?;
                    if n < 1 {
                        return bad(p);
                    }
                    n
                };
                p.expect_comma()?;
                let mut tracks = Vec::new();
                loop {
                    skip_line_names(p);
                    if p.is_exhausted() {
                        break;
                    }
                    tracks.push(track_size(p)?);
                }
                if tracks.is_empty() {
                    return bad(p);
                }
                Ok(TrackListEntry::Repeat(count, tracks))
            })?;
            out.push(entry);
            continue;
        }
        out.push(TrackListEntry::Track(track_size(p)?));
    }
    if out.is_empty() {
        return bad(p);
    }
    Ok(Sv::Tracks(out.into()))
}

fn grid_line<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
        return Ok(Sv::GridLine(GridLine::Auto));
    }
    if p.try_parse(|p| p.expect_ident_matching("span")).is_ok() {
        let n = p.try_parse(parse_integer).unwrap_or(1);
        let _ = p.try_parse(|p| p.expect_ident_cloned());
        if n < 1 {
            return bad(p);
        }
        return Ok(Sv::GridLine(GridLine::Span(n.min(u16::MAX as i32) as u16)));
    }
    if let Ok(n) = p.try_parse(parse_integer) {
        if n == 0 {
            return bad(p);
        }
        let _ = p.try_parse(|p| p.expect_ident_cloned());
        return Ok(Sv::GridLine(GridLine::Line(n)));
    }
    let name = p.expect_ident()?.to_string();
    Ok(Sv::GridLine(GridLine::Named(name)))
}

fn grid_pair<'i>(p: &mut Parser<'i>) -> PResult<Vec<Sv>> {
    let a = grid_line(p)?;
    let b = if p.try_parse(|p| p.expect_delim('/')).is_ok() {
        grid_line(p)?
    } else if let Sv::GridLine(GridLine::Named(n)) = &a {
        Sv::GridLine(GridLine::Named(n.clone()))
    } else {
        Sv::GridLine(GridLine::Auto)
    };
    Ok(vec![a, b])
}

fn rest_text<'i>(p: &mut Parser<'i>) -> PResult<Rc<str>> {
    let start = p.position();
    while p.next().is_ok() {}
    Ok(p.slice_from(start).trim().into())
}

/// `border` / `border-top` etc.: `<width> || <style> || <color>`.
fn border_side<'i>(p: &mut Parser<'i>) -> PResult<[Sv; 3]> {
    let mut w = None;
    let mut s = None;
    let mut c = None;
    for _ in 0..3 {
        if w.is_none()
            && let Ok(v) = p.try_parse(border_width)
        {
            w = Some(v);
            continue;
        }
        if s.is_none()
            && let Ok(v) = p.try_parse(border_style)
        {
            s = Some(v);
            continue;
        }
        if c.is_none()
            && let Ok(v) = p.try_parse(color)
        {
            c = Some(v);
            continue;
        }
        break;
    }
    if w.is_none() && s.is_none() && c.is_none() {
        return bad(p);
    }
    Ok([
        w.unwrap_or(Sv::Length(Calc::Dim(3.0, crate::values::Unit::Px))),
        s.unwrap_or(Sv::BorderStyle(BorderStyle::None)),
        c.unwrap_or(Sv::Color(ColorValue::CurrentColor)),
    ])
}

fn flex_shorthand<'i>(p: &mut Parser<'i>) -> PResult<Vec<Sv>> {
    if let Ok(i) = p.try_parse(|p| p.expect_ident_cloned()) {
        return match i.to_ascii_lowercase().as_str() {
            "none" => Ok(vec![Sv::Number(0.0), Sv::Number(0.0), Sv::Auto]),
            "auto" => Ok(vec![Sv::Number(1.0), Sv::Number(1.0), Sv::Auto]),
            "content" => Ok(vec![Sv::Number(1.0), Sv::Number(1.0), Sv::Auto]),
            _ => bad(p),
        };
    }
    let mut grow = None;
    let mut shrink = None;
    let mut basis = None;
    for _ in 0..3 {
        if grow.is_none()
            && let Ok(n) = p.try_parse(parse_number)
        {
            grow = Some(n);
            shrink = p.try_parse(parse_number).ok();
            continue;
        }
        if basis.is_none()
            && let Ok(b) = p.try_parse(|p| size(p, false, true))
        {
            basis = Some(b);
            continue;
        }
        break;
    }
    if grow.is_none() && basis.is_none() {
        return bad(p);
    }
    Ok(vec![
        Sv::Number(grow.unwrap_or(1.0)),
        Sv::Number(shrink.unwrap_or(1.0)),
        basis.unwrap_or(Sv::Length(Calc::Dim(0.0, crate::values::Unit::Pct))),
    ])
}

fn font_shorthand<'i>(p: &mut Parser<'i>) -> PResult<Vec<Sv>> {
    // A system font keyword stands alone and sets everything.
    if let Ok(k) = p.try_parse(|p| {
        let i = p.expect_ident_cloned()?;
        let k = SystemFont::from_ident(&i).map_or_else(|| bad(p), Ok)?;
        p.expect_exhausted()?;
        PResult::Ok(k)
    }) {
        return Ok(vec![
            Sv::FontStyle(FontStyle::Normal),
            Sv::FontWeight(FontWeightSpec::Absolute(400.0)),
            Sv::FontSize(FontSizeSpec::System(k)),
            Sv::Normal,
            Sv::SystemFont(k),
        ]);
    }
    let mut style = None;
    let mut weight = None;
    for _ in 0..4 {
        if p.try_parse(|p| p.expect_ident_matching("normal")).is_ok() {
            continue;
        }
        if style.is_none()
            && let Ok(s) = p.try_parse(|p| ident_of(p, FontStyle::from_ident))
        {
            style = Some(s);
            continue;
        }
        if weight.is_none()
            && let Ok(w) = p.try_parse(font_weight)
        {
            weight = Some(w);
            continue;
        }
        if p.try_parse(|p| p.expect_ident_matching("small-caps"))
            .is_ok()
        {
            continue;
        }
        let stretch = p.try_parse(|p| {
            let i = p.expect_ident()?;
            if i.ends_with("condensed") || i.ends_with("expanded") {
                Ok(())
            } else {
                bad(p)
            }
        });
        if stretch.is_ok() {
            continue;
        }
        break;
    }
    let size = font_size(p)?;
    let lh = if p.try_parse(|p| p.expect_delim('/')).is_ok() {
        line_height(p)?
    } else {
        Sv::Normal
    };
    let family = font_family(p)?;
    Ok(vec![
        Sv::FontStyle(style.unwrap_or(FontStyle::Normal)),
        weight.unwrap_or(Sv::FontWeight(FontWeightSpec::Absolute(400.0))),
        size,
        lh,
        family,
    ])
}

/// `background`: only the color matters to us.
fn background<'i>(p: &mut Parser<'i>) -> PResult<Vec<Sv>> {
    let mut found = None;
    while !p.is_exhausted() {
        if let Ok(c) = p.try_parse(parse_color) {
            found = Some(c);
            continue;
        }
        if p.try_parse(|p| p.expect_comma()).is_ok() {
            // Only the final layer may hold a color.
            found = None;
            continue;
        }
        p.next()?;
    }
    Ok(vec![Sv::Color(found.unwrap_or(ColorValue::Rgba(
        crate::values::Color::TRANSPARENT,
    )))])
}

fn overflow_kw<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    ident_of(p, Overflow::from_ident).map(Sv::Overflow)
}

fn gap_value<'i>(p: &mut Parser<'i>) -> PResult<Sv> {
    if p.try_parse(|p| p.expect_ident_matching("normal")).is_ok() {
        return Ok(Sv::Normal);
    }
    lp_nonneg(p)
}

fn parse_property_inner<'i>(name: &str, p: &mut Parser<'i>) -> PResult<Vec<(LonghandId, Sv)>> {
    if let Some(id) = LonghandId::from_name(name) {
        let v = parse_longhand(id, p)?;
        return Ok(vec![(id, v)]);
    }
    let ids = shorthand_longhands(name).map_or_else(|| bad(p), Ok)?;
    let vals: Vec<Sv> = match name {
        "margin" => four(p, lp_auto)?.into(),
        "padding" => four(p, lp_nonneg)?.into(),
        "inset" => four(p, lp_auto)?.into(),
        "border-width" => four(p, border_width)?.into(),
        "border-style" => four(p, border_style)?.into(),
        "border-color" => four(p, color)?.into(),
        "border" => {
            let [w, s, c] = border_side(p)?;
            let mut v = vec![w; 4];
            v.extend(std::iter::repeat_n(s, 4));
            v.extend(std::iter::repeat_n(c, 4));
            v
        }
        "border-top" | "border-right" | "border-bottom" | "border-left" => border_side(p)?.into(),
        "margin-inline" | "margin-block" | "inset-inline" | "inset-block" => {
            two(p, lp_auto)?.into()
        }
        "padding-inline" | "padding-block" => two(p, lp_nonneg)?.into(),
        "flex" => flex_shorthand(p)?,
        "flex-flow" => {
            let mut dir = None;
            let mut wrap = None;
            for _ in 0..2 {
                if dir.is_none()
                    && let Ok(d) = p.try_parse(|p| ident_of(p, FlexDirection::from_ident))
                {
                    dir = Some(d);
                    continue;
                }
                if wrap.is_none()
                    && let Ok(w) = p.try_parse(|p| ident_of(p, FlexWrap::from_ident))
                {
                    wrap = Some(w);
                    continue;
                }
                break;
            }
            if dir.is_none() && wrap.is_none() {
                return bad(p);
            }
            vec![
                Sv::FlexDirection(dir.unwrap_or(FlexDirection::Row)),
                Sv::FlexWrap(wrap.unwrap_or(FlexWrap::Nowrap)),
            ]
        }
        "gap" => two(p, gap_value)?.into(),
        "overflow" => two(p, overflow_kw)?.into(),
        "place-items" | "place-content" | "place-self" => two(p, align)?.into(),
        "font" => font_shorthand(p)?,
        "background" => background(p)?,
        "grid-row" | "grid-column" => grid_pair(p)?,
        "grid-area" => {
            let mut v = vec![grid_line(p)?];
            while p.try_parse(|p| p.expect_delim('/')).is_ok() {
                v.push(grid_line(p)?);
            }
            while v.len() < 4 {
                v.push(Sv::GridLine(GridLine::Auto));
            }
            v.truncate(4);
            v
        }
        _ => return bad(p),
    };
    Ok(zip(ids, vals))
}

/// Parses one longhand's value.
pub(crate) fn parse_longhand<'i>(id: LonghandId, p: &mut Parser<'i>) -> PResult<Sv> {
    Ok(match id {
        L::Display => Sv::Display(parse_display(p)?),
        L::Position => Sv::Position(ident_of(p, Position::from_ident)?),
        L::Float => Sv::Float(ident_of(p, Float::from_ident)?),
        L::Clear => Sv::Clear(ident_of(p, Clear::from_ident)?),
        L::Visibility => Sv::Visibility(ident_of(p, Visibility::from_ident)?),
        L::Opacity => {
            let pct = p.try_parse(|p| match p.next()? {
                Token::Percentage { unit_value, .. } => Ok(*unit_value),
                _ => bad(p),
            });
            let v = if let Ok(v) = pct { v } else { parse_number(p)? };
            Sv::Number(v.clamp(0.0, 1.0))
        }
        L::OverflowX | L::OverflowY => overflow_kw(p)?,
        L::Width | L::Height => size(p, false, true)?,
        L::MinWidth | L::MinHeight => size(p, false, true)?,
        L::MaxWidth | L::MaxHeight => size(p, true, false)?,
        L::MarginTop | L::MarginRight | L::MarginBottom | L::MarginLeft => lp_auto(p)?,
        L::PaddingTop | L::PaddingRight | L::PaddingBottom | L::PaddingLeft => lp_nonneg(p)?,
        L::BorderTopWidth | L::BorderRightWidth | L::BorderBottomWidth | L::BorderLeftWidth => {
            border_width(p)?
        }
        L::BorderTopStyle | L::BorderRightStyle | L::BorderBottomStyle | L::BorderLeftStyle => {
            border_style(p)?
        }
        L::BorderTopColor | L::BorderRightColor | L::BorderBottomColor | L::BorderLeftColor => {
            color(p)?
        }
        L::Top | L::Right | L::Bottom | L::Left => lp_auto(p)?,
        L::BoxSizing => Sv::BoxSizing(ident_of(p, BoxSizing::from_ident)?),
        L::FlexDirection => Sv::FlexDirection(ident_of(p, FlexDirection::from_ident)?),
        L::FlexWrap => Sv::FlexWrap(ident_of(p, FlexWrap::from_ident)?),
        L::FlexGrow | L::FlexShrink => number_nonneg(p)?,
        L::FlexBasis => {
            if p.try_parse(|p| p.expect_ident_matching("content")).is_ok() {
                Sv::Auto
            } else {
                size(p, false, true)?
            }
        }
        L::Order => Sv::Integer(parse_integer(p)?),
        L::RowGap | L::ColumnGap => gap_value(p)?,
        L::GridTemplateRows | L::GridTemplateColumns => track_list(p)?,
        L::GridRowStart | L::GridRowEnd | L::GridColumnStart | L::GridColumnEnd => grid_line(p)?,
        L::AlignItems
        | L::AlignSelf
        | L::AlignContent
        | L::JustifyContent
        | L::JustifyItems
        | L::JustifySelf => align(p)?,
        L::FontFamily => font_family(p)?,
        L::FontSize => font_size(p)?,
        L::FontWeight => font_weight(p)?,
        L::FontStyle => {
            let s = ident_of(p, FontStyle::from_ident)?;
            // `oblique <angle>`.
            let _ = p.try_parse(|p| match p.next()? {
                Token::Dimension { .. } => Ok(()),
                _ => bad(p),
            });
            Sv::FontStyle(s)
        }
        L::LineHeight => line_height(p)?,
        L::Color | L::BackgroundColor => color(p)?,
        L::TextAlign => Sv::TextAlign(ident_of(p, TextAlign::from_ident)?),
        L::WhiteSpace => Sv::WhiteSpace(ident_of(p, WhiteSpace::from_ident)?),
        L::ZIndex => {
            if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
                Sv::Auto
            } else {
                Sv::Integer(parse_integer(p)?)
            }
        }
        L::Transform => {
            if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
                Sv::None
            } else {
                // Validate that it is a list of functions.
                let start = p.state();
                let mut any = false;
                while !p.is_exhausted() {
                    p.expect_function()?;
                    p.parse_nested_block(|p| {
                        while p.next().is_ok() {}
                        Ok::<_, cssparser::ParseError<()>>(())
                    })?;
                    any = true;
                }
                if !any {
                    return bad(p);
                }
                p.reset(&start);
                Sv::Transform(rest_text(p)?)
            }
        }
        L::PointerEvents => Sv::PointerEvents(ident_of(p, PointerEvents::from_ident)?),
        L::Cursor => {
            // `url(...) x y, ..., keyword`: keep the final keyword.
            let mut last = None;
            while !p.is_exhausted() {
                match p.next()?.clone() {
                    Token::Ident(i) => last = Some(i.to_ascii_lowercase()),
                    _ => last = None,
                }
            }
            Sv::Cursor(last.map_or_else(|| bad(p), |s| Ok(Rc::from(s.as_str())))?)
        }
        L::LetterSpacing | L::WordSpacing => {
            if p.try_parse(|p| p.expect_ident_matching("normal")).is_ok() {
                Sv::Normal
            } else {
                lp(p)?
            }
        }
        L::Direction => Sv::Direction(ident_of(p, Direction::from_ident)?),
        L::AspectRatio => {
            if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
                Sv::Auto
            } else {
                let a = parse_number(p)?;
                let b = if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                    parse_number(p)?
                } else {
                    1.0
                };
                let _ = p.try_parse(|p| p.expect_ident_matching("auto"));
                if a <= 0.0 || b <= 0.0 {
                    return bad(p);
                }
                Sv::Ratio(a, b)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decls(css: &str) -> Vec<Declaration> {
        DeclarationBlock::parse(css).decls
    }

    #[test]
    fn longhands_and_important() {
        let d = decls("display: flex; color: red !important; bogus: 1; width: 10px");
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].value, Decl::Value(Sv::Display(Display::Flex)));
        assert!(d[1].important);
        assert_eq!(d[2].id, LonghandId::Width);
    }

    #[test]
    fn shorthands_expand() {
        let d = decls("margin: 1px 2px; border: 1px solid red; flex: 1");
        let ids: Vec<_> = d.iter().map(|d| d.id).collect();
        assert_eq!(&ids[..4], shorthand_longhands("margin").unwrap());
        assert_eq!(d.len(), 4 + 12 + 3);
        assert_eq!(
            d[1].value,
            Decl::Value(Sv::Length(Calc::Dim(2.0, crate::values::Unit::Px)))
        );
        assert_eq!(d[16].value, Decl::Value(Sv::Number(1.0)));
    }

    #[test]
    fn var_and_wide_keywords() {
        let d = decls("--gap: 4px; margin: var(--gap) 0; color: inherit");
        assert_eq!(d.len(), 5);
        assert!(matches!(d[0].value, Decl::Pending(_)));
        assert_eq!(d[4].value, Decl::Inherit);
        let b = DeclarationBlock::parse("--gap: 4px");
        assert_eq!(&*b.customs[0].value, "4px");
    }

    #[test]
    fn logical_and_prefixed_names() {
        let d = decls("padding-inline-start: 40px; -webkit-flex-direction: column");
        assert_eq!(d[0].id, LonghandId::PaddingLeft);
        assert_eq!(d[1].id, LonghandId::FlexDirection);
    }

    #[test]
    fn font_shorthand() {
        let d = decls("font: italic bold 12px/30px Georgia, serif");
        assert_eq!(d.len(), 5);
        assert_eq!(d[0].value, Decl::Value(Sv::FontStyle(FontStyle::Italic)));
        match &d[4].value {
            Decl::Value(Sv::FontFamily(f)) => {
                assert_eq!(f[0].name, "Georgia");
                assert!(f[1].generic);
            }
            other => panic!("{other:?}"),
        }
    }
}

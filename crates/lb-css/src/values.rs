//! Value types: colors, lengths with `calc()`, keyword enums, number
//! formatting in Chrome's `getComputedStyle` style.

use std::rc::Rc;

use cssparser::{ParseError, Parser, Token, match_ignore_ascii_case};

/// Parse result with no custom error type.
pub(crate) type PResult<T> = Result<T, ParseError<()>>;

pub(crate) fn bad<T>(_input: &Parser<'_>) -> PResult<T> {
    Err(ParseError::custom(()))
}

// ----- numbers -----

/// Formats a number the way Chrome serializes computed values: at most six
/// significant digits, no trailing zeros, `-0` as `0`.
pub fn fmt_num(v: f32) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let v = v as f64;
    if v.abs() < 1e-7 {
        return "0".into();
    }
    let int_digits = if v.abs() >= 1.0 {
        (v.abs().log10().floor() as i32) + 1
    } else {
        1
    };
    let decimals = (6 - int_digits).max(0) as usize;
    let mut s = format!("{v:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".into() } else { s }
}

/// `fmt_num` plus `px`.
pub fn fmt_px(v: f32) -> String {
    format!("{}px", fmt_num(v))
}

// ----- colors -----

/// An sRGB color with 8-bit channels and alpha in `0..=1`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: f32,
}

impl Color {
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0.0,
    };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 1.0 }
    }

    pub fn is_transparent(&self) -> bool {
        self.a <= 0.0
    }

    /// Chrome's serialization: `rgb(0, 0, 0)` or `rgba(0, 0, 0, 0.5)`.
    pub fn to_css(&self) -> String {
        if self.a >= 1.0 {
            format!("rgb({}, {}, {})", self.r, self.g, self.b)
        } else {
            // Chrome keeps alpha as a float; round to 3 places like Blink's
            // serializer does for 8-bit alpha.
            let a = (self.a * 1000.0).round() / 1000.0;
            format!("rgba({}, {}, {}, {})", self.r, self.g, self.b, fmt_num(a))
        }
    }
}

/// A specified or computed color that may be `currentcolor`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ColorValue {
    Rgba(Color),
    CurrentColor,
}

impl ColorValue {
    /// Resolves `currentcolor` against `color`.
    pub fn resolve(self, current: Color) -> Color {
        match self {
            ColorValue::Rgba(c) => c,
            ColorValue::CurrentColor => current,
        }
    }
}

/// CSS system colors as Chrome renders them in light mode.
fn system_color(name: &str) -> Option<Color> {
    Some(match_ignore_ascii_case! { name,
        "canvas" | "window" | "field" | "background" => Color::WHITE,
        "canvastext" | "windowtext" | "fieldtext" | "buttontext" | "captiontext"
            | "infotext" | "menutext" | "-webkit-text" => Color::BLACK,
        "buttonface" | "buttonhighlight" | "threedface" | "threedlightshadow" => Color::rgb(239, 239, 239),
        "buttonborder" | "buttonshadow" | "threedshadow" => Color::rgb(118, 118, 118),
        "graytext" => Color::rgb(109, 109, 109),
        "linktext" | "-webkit-link" => Color::rgb(0, 0, 238),
        "visitedtext" => Color::rgb(85, 26, 139),
        "activetext" | "-webkit-activelink" => Color::rgb(255, 0, 0),
        "highlight" => Color::rgb(181, 213, 255),
        "highlighttext" => Color::BLACK,
        "mark" => Color::rgb(255, 255, 0),
        "marktext" => Color::BLACK,
        "accentcolor" => Color::rgb(0, 117, 255),
        "accentcolortext" => Color::WHITE,
        "activeborder" | "inactiveborder" | "windowframe" => Color::WHITE,
        "activecaption" | "inactivecaption" | "appworkspace" | "infobackground" | "menu" | "scrollbar" => Color::WHITE,
        "inactivecaptiontext" => Color::rgb(128, 128, 128),
        "threeddarkshadow" => Color::BLACK,
        _ => return None,
    })
}

fn clamp_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Parses `<color>`.
pub(crate) fn parse_color<'i>(input: &mut Parser<'i>) -> PResult<ColorValue> {
    let token = input.next()?.clone();
    match &token {
        Token::Hash(h) | Token::IDHash(h) => cssparser::color::parse_hash_color(h.as_bytes())
            .map(|(r, g, b, a)| ColorValue::Rgba(Color { r, g, b, a }))
            .map_err(|_| ParseError::custom(())),
        Token::Ident(name) => {
            if name.eq_ignore_ascii_case("currentcolor") {
                return Ok(ColorValue::CurrentColor);
            }
            if name.eq_ignore_ascii_case("transparent") {
                return Ok(ColorValue::Rgba(Color::TRANSPARENT));
            }
            if let Ok((r, g, b)) = cssparser::color::parse_named_color(&name.to_ascii_lowercase()) {
                return Ok(ColorValue::Rgba(Color::rgb(r, g, b)));
            }
            system_color(name)
                .map(ColorValue::Rgba)
                .ok_or_else(|| ParseError::custom(()))
        }
        Token::Function(name) => {
            let name = name.to_ascii_lowercase();
            input.parse_nested_block(|p| match name.as_str() {
                "rgb" | "rgba" => parse_rgb_args(p),
                "hsl" | "hsla" => parse_hsl_args(p),
                _ => bad(p),
            })
        }
        _ => Err(ParseError::custom(())),
    }
}

/// One color component: number or percentage (`pct_scale` maps 100%).
fn component<'i>(p: &mut Parser<'i>, pct_scale: f32) -> PResult<f32> {
    match p.next()?.clone() {
        Token::Number { value, .. } => Ok(value),
        Token::Percentage { unit_value, .. } => Ok(unit_value * pct_scale),
        Token::Ident(i) if i.eq_ignore_ascii_case("none") => Ok(0.0),
        _ => bad(p),
    }
}

fn alpha_sep<'i>(p: &mut Parser<'i>, legacy: bool) -> PResult<f32> {
    if p.is_exhausted() {
        return Ok(1.0);
    }
    if legacy {
        p.expect_comma()?;
    } else {
        p.expect_delim('/')?;
    }
    let a = component(p, 1.0)?;
    Ok(a.clamp(0.0, 1.0))
}

fn parse_rgb_args<'i>(p: &mut Parser<'i>) -> PResult<ColorValue> {
    let r = component(p, 255.0)?;
    let legacy = p.try_parse(|p| p.expect_comma()).is_ok();
    let g = component(p, 255.0)?;
    if legacy {
        p.expect_comma()?;
    }
    let b = component(p, 255.0)?;
    let a = alpha_sep(p, legacy)?;
    Ok(ColorValue::Rgba(Color {
        r: clamp_u8(r),
        g: clamp_u8(g),
        b: clamp_u8(b),
        a,
    }))
}

fn parse_hsl_args<'i>(p: &mut Parser<'i>) -> PResult<ColorValue> {
    let h = match p.next()?.clone() {
        Token::Number { value, .. } => value,
        Token::Dimension { value, unit, .. } => match_ignore_ascii_case! { &unit,
            "deg" => value,
            "rad" => value.to_degrees(),
            "grad" => value * 0.9,
            "turn" => value * 360.0,
            _ => return bad(p),
        },
        _ => return bad(p),
    };
    let legacy = p.try_parse(|p| p.expect_comma()).is_ok();
    let s = component(p, 100.0)? / 100.0;
    if legacy {
        p.expect_comma()?;
    }
    let l = component(p, 100.0)? / 100.0;
    let a = alpha_sep(p, legacy)?;
    let (r, g, b) = hsl_to_rgb(h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
    Ok(ColorValue::Rgba(Color {
        r: clamp_u8(r * 255.0),
        g: clamp_u8(g * 255.0),
        b: clamp_u8(b * 255.0),
        a,
    }))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 360.0;
    let t2 = if l <= 0.5 {
        l * (s + 1.0)
    } else {
        l + s - l * s
    };
    let t1 = l * 2.0 - t2;
    let hue = |mut h: f32| {
        if h < 0.0 {
            h += 1.0;
        }
        if h > 1.0 {
            h -= 1.0;
        }
        if h * 6.0 < 1.0 {
            t1 + (t2 - t1) * h * 6.0
        } else if h * 2.0 < 1.0 {
            t2
        } else if h * 3.0 < 2.0 {
            t1 + (t2 - t1) * (2.0 / 3.0 - h) * 6.0
        } else {
            t1
        }
    };
    (hue(h + 1.0 / 3.0), hue(h), hue(h - 1.0 / 3.0))
}

// ----- lengths and calc() -----

/// Length units after parsing. Absolute units are converted to `Px` at
/// parse time.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Unit {
    Px,
    Pct,
    Em,
    Rem,
    Ex,
    Ch,
    Vw,
    Vh,
    Vmin,
    Vmax,
}

/// A length, percentage or `calc()` / `min()` / `max()` / `clamp()`
/// expression. Specified values may use any [`Unit`]; computed values only
/// hold `Px` and `Pct` leaves.
#[derive(Clone, Debug, PartialEq)]
pub enum Calc {
    Dim(f32, Unit),
    Num(f32),
    Sum(Vec<Calc>),
    Mul(Box<Calc>, f32),
    Min(Vec<Calc>),
    Max(Vec<Calc>),
    Clamp(Box<Calc>, Box<Calc>, Box<Calc>),
}

impl Calc {
    /// Evaluates a computed expression with `%` resolved against `basis`.
    pub fn eval(&self, basis: f32) -> f32 {
        match self {
            Calc::Dim(v, Unit::Pct) => v * basis / 100.0,
            Calc::Dim(v, _) | Calc::Num(v) => *v,
            Calc::Sum(xs) => xs.iter().map(|x| x.eval(basis)).sum(),
            Calc::Mul(x, k) => x.eval(basis) * k,
            Calc::Min(xs) => xs
                .iter()
                .map(|x| x.eval(basis))
                .fold(f32::INFINITY, f32::min),
            Calc::Max(xs) => xs
                .iter()
                .map(|x| x.eval(basis))
                .fold(f32::NEG_INFINITY, f32::max),
            Calc::Clamp(lo, v, hi) => {
                let lo = lo.eval(basis);
                v.eval(basis).min(hi.eval(basis)).max(lo)
            }
        }
    }

    fn is_number(&self) -> bool {
        match self {
            Calc::Num(_) => true,
            Calc::Dim(..) => false,
            Calc::Sum(xs) | Calc::Min(xs) | Calc::Max(xs) => xs.iter().all(Calc::is_number),
            Calc::Mul(x, _) => x.is_number(),
            Calc::Clamp(a, b, c) => a.is_number() && b.is_number() && c.is_number(),
        }
    }

    fn has_pct(&self) -> bool {
        match self {
            Calc::Dim(_, u) => *u == Unit::Pct,
            Calc::Num(_) => false,
            Calc::Sum(xs) | Calc::Min(xs) | Calc::Max(xs) => xs.iter().any(Calc::has_pct),
            Calc::Mul(x, _) => x.has_pct(),
            Calc::Clamp(a, b, c) => a.has_pct() || b.has_pct() || c.has_pct(),
        }
    }

    /// Maps every leaf unit to px/% with `f`.
    pub(crate) fn map_units(&self, f: &impl Fn(f32, Unit) -> Calc) -> Calc {
        match self {
            Calc::Dim(v, u) => f(*v, *u),
            Calc::Num(v) => Calc::Num(*v),
            Calc::Sum(xs) => Calc::Sum(xs.iter().map(|x| x.map_units(f)).collect()),
            Calc::Mul(x, k) => Calc::Mul(Box::new(x.map_units(f)), *k),
            Calc::Min(xs) => Calc::Min(xs.iter().map(|x| x.map_units(f)).collect()),
            Calc::Max(xs) => Calc::Max(xs.iter().map(|x| x.map_units(f)).collect()),
            Calc::Clamp(a, b, c) => Calc::Clamp(
                Box::new(a.map_units(f)),
                Box::new(b.map_units(f)),
                Box::new(c.map_units(f)),
            ),
        }
    }

    /// Linear form `(px, pct)` if the expression has no min/max/clamp.
    fn linear(&self) -> Option<(f32, f32)> {
        match self {
            Calc::Dim(v, Unit::Pct) => Some((0.0, *v)),
            Calc::Dim(v, _) => Some((*v, 0.0)),
            Calc::Num(_) => None,
            Calc::Sum(xs) => xs.iter().try_fold((0.0, 0.0), |(a, b), x| {
                let (c, d) = x.linear()?;
                Some((a + c, b + d))
            }),
            Calc::Mul(x, k) => x.linear().map(|(a, b)| (a * k, b * k)),
            _ if !self.has_pct() => Some((self.eval(0.0), 0.0)),
            _ => None,
        }
    }
}

/// Computed `<length-percentage>`.
#[derive(Clone, Debug, PartialEq)]
pub enum Lp {
    Px(f32),
    Pct(f32),
    /// Mixed px and %, or `min()`/`max()` with %: resolved at layout time.
    Calc(Rc<Calc>),
}

impl Default for Lp {
    fn default() -> Self {
        Lp::Px(0.0)
    }
}

impl Lp {
    pub const ZERO: Lp = Lp::Px(0.0);

    /// Builds the simplest form of a computed (px/% only) expression.
    pub fn from_calc(c: Calc) -> Lp {
        let has_pct = c.has_pct();
        match c.linear() {
            Some((px, _)) if !has_pct => Lp::Px(px),
            Some((0.0, pct)) => Lp::Pct(pct),
            _ => Lp::Calc(Rc::new(c)),
        }
    }

    /// Resolves against `basis` (the percentage basis).
    pub fn resolve(&self, basis: f32) -> f32 {
        match self {
            Lp::Px(v) => *v,
            Lp::Pct(p) => p * basis / 100.0,
            Lp::Calc(c) => c.eval(basis),
        }
    }

    /// The px value if no percentage is involved.
    pub fn as_px(&self) -> Option<f32> {
        match self {
            Lp::Px(v) => Some(*v),
            _ => None,
        }
    }

    pub fn has_pct(&self) -> bool {
        !matches!(self, Lp::Px(_))
    }

    /// Serialization as Chrome prints an unresolved computed value.
    pub fn to_css(&self) -> String {
        match self {
            Lp::Px(v) => fmt_px(*v),
            Lp::Pct(p) => format!("{}%", fmt_num(*p)),
            Lp::Calc(c) => match c.linear() {
                Some((px, pct)) => {
                    let sign = if px < 0.0 { '-' } else { '+' };
                    format!("calc({}% {} {})", fmt_num(pct), sign, fmt_px(px.abs()))
                }
                None => "calc(0px)".into(),
            },
        }
    }
}

/// Computed `<length-percentage> | auto`.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum LpAuto {
    #[default]
    Auto,
    Lp(Lp),
}

impl LpAuto {
    pub fn to_css(&self) -> String {
        match self {
            LpAuto::Auto => "auto".into(),
            LpAuto::Lp(l) => l.to_css(),
        }
    }

    pub fn lp(&self) -> Option<&Lp> {
        match self {
            LpAuto::Auto => None,
            LpAuto::Lp(l) => Some(l),
        }
    }
}

/// Parses `<length-percentage>` (or `<length>` when `allow_pct` is false).
pub(crate) fn parse_length<'i>(
    input: &mut Parser<'i>,
    allow_pct: bool,
    allow_negative: bool,
) -> PResult<Calc> {
    let c = parse_calc_leaf(input)?;
    if c.is_number() {
        // Only a literal 0 is a valid length.
        if c != Calc::Num(0.0) {
            return bad(input);
        }
        return Ok(Calc::Dim(0.0, Unit::Px));
    }
    if !allow_pct && c.has_pct() {
        return bad(input);
    }
    if !allow_negative
        && let Calc::Dim(v, _) = c
        && v < 0.0
    {
        return bad(input);
    }
    Ok(c)
}

fn dim_unit(value: f32, unit: &str) -> Option<Calc> {
    let u = unit.to_ascii_lowercase();
    Some(match u.as_str() {
        "px" => Calc::Dim(value, Unit::Px),
        "em" => Calc::Dim(value, Unit::Em),
        "rem" => Calc::Dim(value, Unit::Rem),
        "ex" => Calc::Dim(value, Unit::Ex),
        "ch" => Calc::Dim(value, Unit::Ch),
        "vw" | "svw" | "lvw" | "dvw" | "vi" => Calc::Dim(value, Unit::Vw),
        "vh" | "svh" | "lvh" | "dvh" | "vb" => Calc::Dim(value, Unit::Vh),
        "vmin" | "svmin" | "lvmin" | "dvmin" => Calc::Dim(value, Unit::Vmin),
        "vmax" | "svmax" | "lvmax" | "dvmax" => Calc::Dim(value, Unit::Vmax),
        "pt" => Calc::Dim(value * 4.0 / 3.0, Unit::Px),
        "pc" => Calc::Dim(value * 16.0, Unit::Px),
        "in" => Calc::Dim(value * 96.0, Unit::Px),
        "cm" => Calc::Dim(value * 96.0 / 2.54, Unit::Px),
        "mm" => Calc::Dim(value * 96.0 / 25.4, Unit::Px),
        "q" => Calc::Dim(value * 96.0 / 101.6, Unit::Px),
        _ => return None,
    })
}

/// One calc operand: number, dimension, percentage, or a math function.
fn parse_calc_leaf<'i>(input: &mut Parser<'i>) -> PResult<Calc> {
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok(Calc::Num(value)),
        Token::Percentage { unit_value, .. } => Ok(Calc::Dim(unit_value * 100.0, Unit::Pct)),
        Token::Dimension { value, unit, .. } => {
            dim_unit(value, &unit).ok_or_else(|| ParseError::custom(()))
        }
        Token::Function(name) => {
            let name = name.to_ascii_lowercase();
            input.parse_nested_block(|p| match name.as_str() {
                "calc" | "-webkit-calc" => parse_sum(p),
                "min" | "max" => {
                    let args = p.parse_comma_separated(parse_sum)?;
                    Ok(if name == "min" {
                        Calc::Min(args)
                    } else {
                        Calc::Max(args)
                    })
                }
                "clamp" => {
                    let lo = parse_sum(p)?;
                    p.expect_comma()?;
                    let v = parse_sum(p)?;
                    p.expect_comma()?;
                    let hi = parse_sum(p)?;
                    Ok(Calc::Clamp(Box::new(lo), Box::new(v), Box::new(hi)))
                }
                _ => bad(p),
            })
        }
        Token::ParenthesisBlock => input.parse_nested_block(parse_sum),
        _ => Err(ParseError::custom(())),
    }
}

fn parse_sum<'i>(p: &mut Parser<'i>) -> PResult<Calc> {
    let mut terms = vec![parse_product(p)?];
    loop {
        let sign = p.try_parse(|p| match p.next()? {
            Token::Delim('+') => Ok(1.0),
            Token::Delim('-') => Ok(-1.0),
            _ => Err(ParseError::<()>::custom(())),
        });
        let Ok(sign) = sign else { break };
        let t = parse_product(p)?;
        terms.push(if sign < 0.0 {
            Calc::Mul(Box::new(t), -1.0)
        } else {
            t
        });
    }
    if terms.len() == 1 {
        return Ok(terms.pop().expect("one term"));
    }
    // Mixing numbers and lengths in a sum is invalid.
    let numbers = terms.iter().filter(|t| t.is_number()).count();
    if numbers != 0 && numbers != terms.len() {
        return bad(p);
    }
    if numbers == terms.len() {
        return Ok(Calc::Num(terms.iter().map(|t| t.eval(0.0)).sum()));
    }
    Ok(Calc::Sum(terms))
}

fn parse_product<'i>(p: &mut Parser<'i>) -> PResult<Calc> {
    let mut acc = parse_calc_leaf(p)?;
    loop {
        let op = p.try_parse(|p| match p.next()? {
            Token::Delim('*') => Ok('*'),
            Token::Delim('/') => Ok('/'),
            _ => Err(ParseError::<()>::custom(())),
        });
        let Ok(op) = op else { break };
        let rhs = parse_calc_leaf(p)?;
        acc = match (op, acc.is_number(), rhs.is_number()) {
            ('*', true, true) => Calc::Num(acc.eval(0.0) * rhs.eval(0.0)),
            ('*', true, false) => Calc::Mul(Box::new(rhs), acc.eval(0.0)),
            ('*', false, true) => Calc::Mul(Box::new(acc), rhs.eval(0.0)),
            ('/', _, true) => {
                let d = rhs.eval(0.0);
                if d == 0.0 {
                    return bad(p);
                }
                if acc.is_number() {
                    Calc::Num(acc.eval(0.0) / d)
                } else {
                    Calc::Mul(Box::new(acc), 1.0 / d)
                }
            }
            _ => return bad(p),
        };
    }
    Ok(acc)
}

/// Parses a plain `<number>` (also inside `calc()`).
pub(crate) fn parse_number<'i>(input: &mut Parser<'i>) -> PResult<f32> {
    let c = parse_calc_leaf(input)?;
    if c.is_number() {
        Ok(c.eval(0.0))
    } else {
        bad(input)
    }
}

/// Parses an `<integer>`.
pub(crate) fn parse_integer<'i>(input: &mut Parser<'i>) -> PResult<i32> {
    match input.next()?.clone() {
        Token::Number {
            int_value: Some(v), ..
        } => Ok(v),
        _ => bad(input),
    }
}

// ----- keyword enums -----

macro_rules! keyword_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($var),+ }

        impl $name {
            /// Parses the keyword (ASCII case-insensitive).
            pub fn from_ident(s: &str) -> Option<Self> {
                $( if s.eq_ignore_ascii_case($s) { return Some(Self::$var); } )+
                None
            }

            /// The CSS keyword.
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$var => $s),+ }
            }
        }
    };
}

keyword_enum!(
    /// `display`. Two-keyword forms map onto these.
    Display {
        None = "none",
        Contents = "contents",
        Block = "block",
        Inline = "inline",
        InlineBlock = "inline-block",
        FlowRoot = "flow-root",
        ListItem = "list-item",
        Flex = "flex",
        InlineFlex = "inline-flex",
        Grid = "grid",
        InlineGrid = "inline-grid",
        Table = "table",
        InlineTable = "inline-table",
        TableRowGroup = "table-row-group",
        TableHeaderGroup = "table-header-group",
        TableFooterGroup = "table-footer-group",
        TableRow = "table-row",
        TableCell = "table-cell",
        TableColumnGroup = "table-column-group",
        TableColumn = "table-column",
        TableCaption = "table-caption",
        WebkitBox = "-webkit-box",
        WebkitInlineBox = "-webkit-inline-box",
    }
);

impl Display {
    /// Inline-level (takes part in an inline formatting context).
    pub fn is_inline_level(self) -> bool {
        matches!(
            self,
            Display::Inline
                | Display::InlineBlock
                | Display::InlineFlex
                | Display::InlineGrid
                | Display::InlineTable
                | Display::WebkitInlineBox
        )
    }

    /// The block-level equivalent (CSS "blockification").
    pub fn blockified(self) -> Display {
        match self {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::InlineTable => Display::Table,
            Display::WebkitInlineBox => Display::WebkitBox,
            Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableRow
            | Display::TableCell
            | Display::TableColumnGroup
            | Display::TableColumn
            | Display::TableCaption => Display::Block,
            d => d,
        }
    }

    /// Flex container (incl. legacy `-webkit-box`).
    pub fn is_flex(self) -> bool {
        matches!(
            self,
            Display::Flex | Display::InlineFlex | Display::WebkitBox | Display::WebkitInlineBox
        )
    }

    pub fn is_grid(self) -> bool {
        matches!(self, Display::Grid | Display::InlineGrid)
    }
}

/// Parses `display`, including two-keyword forms.
pub(crate) fn parse_display<'i>(input: &mut Parser<'i>) -> PResult<Display> {
    let first = input.expect_ident()?.to_ascii_lowercase();
    let second = input
        .try_parse(|p| p.expect_ident().map(|s| s.to_ascii_lowercase()))
        .ok();
    let d = match (first.as_str(), second.as_deref()) {
        (one, None) => Display::from_ident(one),
        ("block", Some("flow")) | ("flow", Some("block")) => Some(Display::Block),
        ("inline", Some("flow")) | ("flow", Some("inline")) => Some(Display::Inline),
        ("block", Some("flow-root")) | ("flow-root", Some("block")) => Some(Display::FlowRoot),
        ("inline", Some("flow-root")) | ("flow-root", Some("inline")) => Some(Display::InlineBlock),
        ("block", Some("flex")) | ("flex", Some("block")) => Some(Display::Flex),
        ("inline", Some("flex")) | ("flex", Some("inline")) => Some(Display::InlineFlex),
        ("block", Some("grid")) | ("grid", Some("block")) => Some(Display::Grid),
        ("inline", Some("grid")) | ("grid", Some("inline")) => Some(Display::InlineGrid),
        ("block", Some("table")) | ("table", Some("block")) => Some(Display::Table),
        ("inline", Some("table")) | ("table", Some("inline")) => Some(Display::InlineTable),
        ("block", Some("list-item")) | ("list-item", Some("block")) => Some(Display::ListItem),
        _ => None,
    };
    d.map_or_else(|| bad(input), Ok)
}

keyword_enum!(Position {
    Static = "static",
    Relative = "relative",
    Absolute = "absolute",
    Fixed = "fixed",
    Sticky = "sticky",
});

keyword_enum!(Float {
    None = "none",
    Left = "left",
    Right = "right",
    InlineStart = "inline-start",
    InlineEnd = "inline-end",
});

keyword_enum!(Clear {
    None = "none",
    Left = "left",
    Right = "right",
    Both = "both",
    InlineStart = "inline-start",
    InlineEnd = "inline-end",
});

keyword_enum!(Visibility {
    Visible = "visible",
    Hidden = "hidden",
    Collapse = "collapse",
});

keyword_enum!(Overflow {
    Visible = "visible",
    Hidden = "hidden",
    Clip = "clip",
    Scroll = "scroll",
    Auto = "auto",
    Overlay = "overlay",
});

impl Overflow {
    /// Creates a scroll container.
    pub fn is_scrollable(self) -> bool {
        matches!(
            self,
            Overflow::Hidden | Overflow::Scroll | Overflow::Auto | Overflow::Overlay
        )
    }
}

keyword_enum!(BoxSizing {
    ContentBox = "content-box",
    BorderBox = "border-box",
});

keyword_enum!(BorderStyle {
    None = "none",
    Hidden = "hidden",
    Solid = "solid",
    Dashed = "dashed",
    Dotted = "dotted",
    Double = "double",
    Groove = "groove",
    Ridge = "ridge",
    Inset = "inset",
    Outset = "outset",
});

keyword_enum!(FlexDirection {
    Row = "row",
    RowReverse = "row-reverse",
    Column = "column",
    ColumnReverse = "column-reverse",
});

keyword_enum!(FlexWrap {
    Nowrap = "nowrap",
    Wrap = "wrap",
    WrapReverse = "wrap-reverse",
});

keyword_enum!(
    /// One keyword of the box-alignment properties (`align-*`,
    /// `justify-*`). Which ones a property accepts is checked at parse time.
    Align {
        Auto = "auto",
        Normal = "normal",
        Stretch = "stretch",
        Start = "start",
        End = "end",
        FlexStart = "flex-start",
        FlexEnd = "flex-end",
        Center = "center",
        Baseline = "baseline",
        LastBaseline = "last baseline",
        SpaceBetween = "space-between",
        SpaceAround = "space-around",
        SpaceEvenly = "space-evenly",
        Left = "left",
        Right = "right",
        SelfStart = "self-start",
        SelfEnd = "self-end",
        Legacy = "legacy",
        AnchorCenter = "anchor-center",
    }
);

keyword_enum!(FontStyle {
    Normal = "normal",
    Italic = "italic",
    Oblique = "oblique",
});

keyword_enum!(TextAlign {
    Start = "start",
    End = "end",
    Left = "left",
    Right = "right",
    Center = "center",
    Justify = "justify",
    MatchParent = "match-parent",
    WebkitLeft = "-webkit-left",
    WebkitRight = "-webkit-right",
    WebkitCenter = "-webkit-center",
    InternalCenter = "-internal-center",
});

keyword_enum!(WhiteSpace {
    Normal = "normal",
    Pre = "pre",
    Nowrap = "nowrap",
    PreWrap = "pre-wrap",
    PreLine = "pre-line",
    BreakSpaces = "break-spaces",
});

impl WhiteSpace {
    /// Collapses runs of spaces and tabs.
    pub fn collapses_spaces(self) -> bool {
        matches!(
            self,
            WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine
        )
    }

    /// Keeps line feeds as forced breaks.
    pub fn preserves_newlines(self) -> bool {
        !matches!(self, WhiteSpace::Normal | WhiteSpace::Nowrap)
    }

    /// Lines may wrap at spaces.
    pub fn wraps(self) -> bool {
        !matches!(self, WhiteSpace::Nowrap | WhiteSpace::Pre)
    }
}

keyword_enum!(PointerEvents {
    Auto = "auto",
    None = "none",
    VisiblePainted = "visiblepainted",
    VisibleFill = "visiblefill",
    VisibleStroke = "visiblestroke",
    Visible = "visible",
    Painted = "painted",
    Fill = "fill",
    Stroke = "stroke",
    All = "all",
    BoundingBox = "bounding-box",
});

keyword_enum!(Direction {
    Ltr = "ltr",
    Rtl = "rtl",
});

/// Computed `line-height`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum LineHeight {
    Normal,
    Number(f32),
    Px(f32),
}

/// One grid track size (a simple subset of `<track-size>`). `L` is the
/// length type: [`Calc`] when specified, [`Lp`] when computed.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackSize<L = Lp> {
    Lp(L),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    MinMax(Box<TrackSize<L>>, Box<TrackSize<L>>),
    FitContent(L),
}

impl<L> TrackSize<L> {
    /// Maps the length type.
    pub fn map<M>(&self, f: &impl Fn(&L) -> M) -> TrackSize<M> {
        match self {
            TrackSize::Lp(l) => TrackSize::Lp(f(l)),
            TrackSize::Fr(v) => TrackSize::Fr(*v),
            TrackSize::Auto => TrackSize::Auto,
            TrackSize::MinContent => TrackSize::MinContent,
            TrackSize::MaxContent => TrackSize::MaxContent,
            TrackSize::MinMax(a, b) => TrackSize::MinMax(Box::new(a.map(f)), Box::new(b.map(f))),
            TrackSize::FitContent(l) => TrackSize::FitContent(f(l)),
        }
    }
}

impl TrackSize<Lp> {
    pub fn to_css(&self) -> String {
        match self {
            TrackSize::Lp(l) => l.to_css(),
            TrackSize::Fr(v) => format!("{}fr", fmt_num(*v)),
            TrackSize::Auto => "auto".into(),
            TrackSize::MinContent => "min-content".into(),
            TrackSize::MaxContent => "max-content".into(),
            TrackSize::MinMax(a, b) => format!("minmax({}, {})", a.to_css(), b.to_css()),
            TrackSize::FitContent(l) => format!("fit-content({})", l.to_css()),
        }
    }
}

/// One entry of `grid-template-rows/columns`.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackListEntry<L = Lp> {
    Track(TrackSize<L>),
    /// `repeat(n | auto-fill | auto-fit, tracks)`; `count` 0 = auto-fill,
    /// -1 = auto-fit.
    Repeat(i32, Vec<TrackSize<L>>),
}

impl<L> TrackListEntry<L> {
    /// Maps the length type.
    pub fn map<M>(&self, f: &impl Fn(&L) -> M) -> TrackListEntry<M> {
        match self {
            TrackListEntry::Track(t) => TrackListEntry::Track(t.map(f)),
            TrackListEntry::Repeat(n, ts) => {
                TrackListEntry::Repeat(*n, ts.iter().map(|t| t.map(f)).collect())
            }
        }
    }
}

impl TrackListEntry<Lp> {
    pub fn to_css(&self) -> String {
        match self {
            TrackListEntry::Track(t) => t.to_css(),
            TrackListEntry::Repeat(n, ts) => {
                let count = match n {
                    0 => "auto-fill".to_owned(),
                    -1 => "auto-fit".to_owned(),
                    n => n.to_string(),
                };
                let inner = ts
                    .iter()
                    .map(TrackSize::to_css)
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("repeat({count}, {inner})")
            }
        }
    }
}

/// `grid-row-start` and friends.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum GridLine {
    #[default]
    Auto,
    Line(i32),
    Span(u16),
    Named(String),
}

impl GridLine {
    pub fn to_css(&self) -> String {
        match self {
            GridLine::Auto => "auto".into(),
            GridLine::Line(n) => n.to_string(),
            GridLine::Span(n) => format!("span {n}"),
            GridLine::Named(s) => s.clone(),
        }
    }
}

/// 2D transform matrix `[a, b, c, d, e, f]`.
pub type Matrix = [f32; 6];

#[cfg(test)]
mod tests {
    use super::*;

    fn color(s: &str) -> Option<ColorValue> {
        let mut p = Parser::new(s);
        parse_color(&mut p).ok()
    }

    #[test]
    fn numbers_format_like_chrome() {
        assert_eq!(fmt_num(16.0), "16");
        assert_eq!(fmt_num(13.333333), "13.3333");
        assert_eq!(fmt_num(18.4), "18.4");
        assert_eq!(fmt_num(-0.0), "0");
        assert_eq!(fmt_num(0.5), "0.5");
    }

    #[test]
    fn colors_parse_and_serialize() {
        let c = |s| color(s).unwrap().resolve(Color::BLACK).to_css();
        assert_eq!(c("red"), "rgb(255, 0, 0)");
        assert_eq!(c("#0f08"), "rgba(0, 255, 0, 0.533)");
        assert_eq!(c("rgb(1 2 3 / 50%)"), "rgba(1, 2, 3, 0.5)");
        assert_eq!(c("rgba(10, 20, 30, 0.25)"), "rgba(10, 20, 30, 0.25)");
        assert_eq!(c("hsl(120, 100%, 50%)"), "rgb(0, 255, 0)");
        assert_eq!(c("transparent"), "rgba(0, 0, 0, 0)");
        assert_eq!(color("currentColor"), Some(ColorValue::CurrentColor));
        assert!(color("nope").is_none());
    }

    #[test]
    fn calc_parses_and_simplifies() {
        let parse = |s: &str| {
            let mut p = Parser::new(s);
            parse_length(&mut p, true, true).unwrap()
        };
        let px = |c: Calc| {
            Lp::from_calc(c.map_units(&|v, u| match u {
                Unit::Em => Calc::Dim(v * 10.0, Unit::Px),
                u => Calc::Dim(v, u),
            }))
        };
        assert_eq!(px(parse("calc(10px + 2em)")), Lp::Px(30.0));
        assert_eq!(px(parse("calc(50% - 10px)")).resolve(200.0), 90.0);
        assert_eq!(px(parse("calc(100% / 4)")), Lp::Pct(25.0));
        assert_eq!(px(parse("min(100%, 600px)")).resolve(1000.0), 600.0);
        assert_eq!(px(parse("max(10px, 2em)")), Lp::Px(20.0));
        assert_eq!(px(parse("0")), Lp::Px(0.0));
    }
}

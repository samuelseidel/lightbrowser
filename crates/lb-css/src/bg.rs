//! Backgrounds and box decorations: `background-*` layers (images,
//! gradients, position, size, repeat, origin, clip, attachment),
//! `border-*-radius` and `box-shadow`. Parsing, computing and Chrome's
//! `getComputedStyle` serialization; painting is in lb-paint.

use std::rc::Rc;

use cssparser::{Parser, Token};

use crate::values::{
    Calc, Color, ColorValue, Lp, PResult, Unit, bad, fmt_num, fmt_px, parse_color, parse_length,
};

// ---------------------------------------------------------------------------
// Keywords
// ---------------------------------------------------------------------------

/// One axis of `background-repeat`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Repeat {
    Repeat,
    NoRepeat,
    Space,
    Round,
}

impl Repeat {
    fn from_ident(s: &str) -> Option<Repeat> {
        Some(match s.to_ascii_lowercase().as_str() {
            "repeat" => Repeat::Repeat,
            "no-repeat" => Repeat::NoRepeat,
            "space" => Repeat::Space,
            "round" => Repeat::Round,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Repeat::Repeat => "repeat",
            Repeat::NoRepeat => "no-repeat",
            Repeat::Space => "space",
            Repeat::Round => "round",
        }
    }
}

/// `background-origin` / `background-clip` boxes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BgBox {
    BorderBox,
    PaddingBox,
    ContentBox,
    /// `background-clip: text` (painted like `border-box`).
    Text,
}

impl BgBox {
    fn from_ident(s: &str) -> Option<BgBox> {
        Some(match s.to_ascii_lowercase().as_str() {
            "border-box" => BgBox::BorderBox,
            "padding-box" => BgBox::PaddingBox,
            "content-box" => BgBox::ContentBox,
            "text" => BgBox::Text,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BgBox::BorderBox => "border-box",
            BgBox::PaddingBox => "padding-box",
            BgBox::ContentBox => "content-box",
            BgBox::Text => "text",
        }
    }
}

/// `background-attachment`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Attachment {
    Scroll,
    Fixed,
    Local,
}

impl Attachment {
    fn from_ident(s: &str) -> Option<Attachment> {
        Some(match s.to_ascii_lowercase().as_str() {
            "scroll" => Attachment::Scroll,
            "fixed" => Attachment::Fixed,
            "local" => Attachment::Local,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Attachment::Scroll => "scroll",
            Attachment::Fixed => "fixed",
            Attachment::Local => "local",
        }
    }
}

// ---------------------------------------------------------------------------
// Images and gradients
// ---------------------------------------------------------------------------

/// Direction of a linear gradient.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum LinearDir {
    /// No direction given (`to bottom`).
    Default,
    /// Degrees, as Chrome serializes any angle unit.
    Angle(f32),
    /// `to <side-or-corner>`: horizontal (-1 left, 1 right, 0 none) and
    /// vertical (-1 top, 1 bottom, 0 none).
    To(i8, i8),
}

/// Ending shape size of a radial gradient.
#[derive(Clone, Debug, PartialEq)]
pub enum RadialSize<L> {
    ClosestSide,
    ClosestCorner,
    FarthestSide,
    FarthestCorner,
    /// Radius (circle) or radii (ellipse).
    Explicit(L, Option<L>),
}

/// A color stop or hint.
#[derive(Clone, Debug, PartialEq)]
pub struct Stop<C, L> {
    /// `None` for a transition hint.
    pub color: Option<C>,
    pub pos: Option<L>,
}

/// A background image layer, generic over specified / computed values.
#[derive(Clone, Debug, PartialEq)]
pub enum Image<C, L> {
    None,
    Url(Rc<str>),
    Linear {
        repeating: bool,
        dir: LinearDir,
        stops: Vec<Stop<C, L>>,
    },
    Radial {
        repeating: bool,
        circle: bool,
        size: RadialSize<L>,
        at: Option<(L, L)>,
        stops: Vec<Stop<C, L>>,
    },
}

/// Specified image.
pub type ImageSpec = Image<ColorValue, Calc>;
/// Computed image.
pub type BgImage = Image<Color, Lp>;

/// `background-size` of one layer; `None` sides are `auto`.
#[derive(Clone, Debug, PartialEq)]
pub enum BgSize<L> {
    Cover,
    Contain,
    Explicit(Option<L>, Option<L>),
}

/// One `box-shadow`, generic over specified / computed values.
#[derive(Clone, Debug, PartialEq)]
pub struct Shadow<C, L> {
    pub color: C,
    pub x: L,
    pub y: L,
    pub blur: L,
    pub spread: L,
    pub inset: bool,
}

pub type ShadowSpec = Shadow<Option<ColorValue>, Calc>;
pub type BoxShadow = Shadow<Color, f32>;

/// The computed background layers of an element (lists as specified;
/// painting and serialization repeat or truncate them to the image count).
#[derive(Clone, Debug, PartialEq)]
pub struct Background {
    pub images: Vec<BgImage>,
    pub pos_x: Vec<Lp>,
    pub pos_y: Vec<Lp>,
    pub size: Vec<BgSize<Lp>>,
    pub repeat: Vec<(Repeat, Repeat)>,
    pub attachment: Vec<Attachment>,
    pub origin: Vec<BgBox>,
    pub clip: Vec<BgBox>,
}

impl Default for Background {
    fn default() -> Self {
        Background {
            images: vec![Image::None],
            pos_x: vec![Lp::Pct(0.0)],
            pos_y: vec![Lp::Pct(0.0)],
            size: vec![BgSize::Explicit(None, None)],
            repeat: vec![(Repeat::Repeat, Repeat::Repeat)],
            attachment: vec![Attachment::Scroll],
            origin: vec![BgBox::PaddingBox],
            clip: vec![BgBox::BorderBox],
        }
    }
}

/// One layer with every list resolved.
#[derive(Clone, Debug)]
pub struct Layer<'a> {
    pub image: &'a BgImage,
    pub pos: (&'a Lp, &'a Lp),
    pub size: &'a BgSize<Lp>,
    pub repeat: (Repeat, Repeat),
    pub attachment: Attachment,
    pub origin: BgBox,
    pub clip: BgBox,
}

fn nth<T>(v: &[T], i: usize) -> &T {
    &v[i % v.len()]
}

impl Background {
    /// True when no layer has an image.
    pub fn is_none(&self) -> bool {
        self.images.iter().all(|i| matches!(i, Image::None))
    }

    /// The layers, top first.
    pub fn layers(&self) -> Vec<Layer<'_>> {
        (0..self.images.len())
            .map(|i| Layer {
                image: &self.images[i],
                pos: (nth(&self.pos_x, i), nth(&self.pos_y, i)),
                size: nth(&self.size, i),
                repeat: *nth(&self.repeat, i),
                attachment: *nth(&self.attachment, i),
                origin: *nth(&self.origin, i),
                clip: *nth(&self.clip, i),
            })
            .collect()
    }

    /// The bottom layer's clip (where the background color paints).
    pub fn color_clip(&self) -> BgBox {
        *nth(&self.clip, self.images.len().saturating_sub(1))
    }

    /// Image URLs of the layers.
    pub fn urls(&self) -> impl Iterator<Item = &str> {
        self.images.iter().filter_map(|i| match i {
            Image::Url(u) => Some(&**u),
            _ => None,
        })
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn comma_list<'i, T>(
    p: &mut Parser<'i>,
    f: impl Fn(&mut Parser<'i>) -> PResult<T>,
) -> PResult<Vec<T>> {
    let mut out = vec![f(p)?];
    while p.try_parse(|p| p.expect_comma()).is_ok() {
        out.push(f(p)?);
    }
    Ok(out)
}

/// `none | <image>`.
pub(crate) fn parse_image<'i>(p: &mut Parser<'i>) -> PResult<ImageSpec> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(Image::None);
    }
    let t = p.next()?.clone();
    match t {
        Token::UnquotedUrl(u) => Ok(Image::Url(Rc::from(u.as_ref()))),
        Token::Function(f) => {
            let f = f.to_ascii_lowercase();
            match f.as_str() {
                "url" => p.parse_nested_block(|p| {
                    let s = p.expect_string()?.as_ref().to_owned();
                    Ok(Image::Url(s.into()))
                }),
                "linear-gradient" | "-webkit-linear-gradient" | "repeating-linear-gradient" => {
                    let repeating = f.starts_with("repeating");
                    p.parse_nested_block(|p| linear(p, repeating))
                }
                "radial-gradient" | "-webkit-radial-gradient" | "repeating-radial-gradient" => {
                    let repeating = f.starts_with("repeating");
                    p.parse_nested_block(|p| radial(p, repeating))
                }
                _ => bad(p),
            }
        }
        _ => bad(p),
    }
}

fn angle<'i>(p: &mut Parser<'i>) -> PResult<f32> {
    match p.next()?.clone() {
        Token::Dimension { value, unit, .. } => {
            let u = unit.to_ascii_lowercase();
            Ok(match u.as_str() {
                "deg" => value,
                "rad" => value.to_degrees(),
                "grad" => value * 0.9,
                "turn" => value * 360.0,
                _ => return bad(p),
            })
        }
        Token::Number { value: 0.0, .. } => Ok(0.0),
        _ => bad(p),
    }
}

fn linear<'i>(p: &mut Parser<'i>, repeating: bool) -> PResult<ImageSpec> {
    let mut dir = LinearDir::Default;
    if let Ok(a) = p.try_parse(angle) {
        dir = LinearDir::Angle(a);
        p.expect_comma()?;
    } else if p.try_parse(|p| p.expect_ident_matching("to")).is_ok() {
        let (mut h, mut v) = (0i8, 0i8);
        for _ in 0..2 {
            let Ok(k) = p.try_parse(|p| p.expect_ident_cloned()) else {
                break;
            };
            match k.to_ascii_lowercase().as_str() {
                "left" if h == 0 => h = -1,
                "right" if h == 0 => h = 1,
                "top" if v == 0 => v = -1,
                "bottom" if v == 0 => v = 1,
                _ => return bad(p),
            }
        }
        if h == 0 && v == 0 {
            return bad(p);
        }
        // `to bottom` is the default direction.
        dir = if (h, v) == (0, 1) {
            LinearDir::Default
        } else {
            LinearDir::To(h, v)
        };
        p.expect_comma()?;
    }
    let stops = stops(p)?;
    Ok(Image::Linear {
        repeating,
        dir,
        stops,
    })
}

fn radial<'i>(p: &mut Parser<'i>, repeating: bool) -> PResult<ImageSpec> {
    let mut circle = None;
    let mut size: Option<RadialSize<Calc>> = None;
    let mut at = None;
    let mut any = false;
    for _ in 0..3 {
        if let Ok(k) = p.try_parse(|p| p.expect_ident_cloned()) {
            match k.to_ascii_lowercase().as_str() {
                "circle" if circle.is_none() => circle = Some(true),
                "ellipse" if circle.is_none() => circle = Some(false),
                "closest-side" if size.is_none() => size = Some(RadialSize::ClosestSide),
                "closest-corner" if size.is_none() => size = Some(RadialSize::ClosestCorner),
                "farthest-side" if size.is_none() => size = Some(RadialSize::FarthestSide),
                "farthest-corner" if size.is_none() => size = Some(RadialSize::FarthestCorner),
                "at" => {
                    at = Some(position(p)?);
                    any = true;
                    break;
                }
                _ => return bad(p),
            }
            any = true;
            continue;
        }
        if size.is_none()
            && let Ok(a) = p.try_parse(|p| parse_length(p, true, false))
        {
            let b = p.try_parse(|p| parse_length(p, true, false)).ok();
            size = Some(RadialSize::Explicit(a, b));
            any = true;
            continue;
        }
        break;
    }
    if any {
        p.expect_comma()?;
    }
    let circle = match (circle, &size) {
        (Some(c), _) => c,
        (None, Some(RadialSize::Explicit(_, None))) => true,
        _ => false,
    };
    let stops = stops(p)?;
    Ok(Image::Radial {
        repeating,
        circle,
        size: size.unwrap_or(RadialSize::FarthestCorner),
        at,
        stops,
    })
}

fn stops<'i>(p: &mut Parser<'i>) -> PResult<Vec<Stop<ColorValue, Calc>>> {
    let mut out = Vec::new();
    loop {
        if let Ok(c) = p.try_parse(parse_color) {
            let a = p.try_parse(|p| parse_length(p, true, true)).ok();
            let b = a
                .as_ref()
                .and_then(|_| p.try_parse(|p| parse_length(p, true, true)).ok());
            out.push(Stop {
                color: Some(c),
                pos: a,
            });
            if let Some(b) = b {
                out.push(Stop {
                    color: Some(c),
                    pos: Some(b),
                });
            }
        } else {
            let h = parse_length(p, true, true)?;
            out.push(Stop {
                color: None,
                pos: Some(h),
            });
        }
        if p.try_parse(|p| p.expect_comma()).is_err() {
            break;
        }
    }
    let colors = out.iter().filter(|s| s.color.is_some()).count();
    if colors < 2
        || out.first().is_some_and(|s| s.color.is_none())
        || out.last().is_some_and(|s| s.color.is_none())
    {
        return bad(p);
    }
    Ok(out)
}

fn pct(v: f32) -> Calc {
    Calc::Dim(v, Unit::Pct)
}

/// `100% - len`.
fn from_end(len: Calc) -> Calc {
    Calc::Sum(vec![pct(100.0), Calc::Mul(Box::new(len), -1.0)])
}

/// A `<position>` (1 to 4 values) as x and y.
pub(crate) fn position<'i>(p: &mut Parser<'i>) -> PResult<(Calc, Calc)> {
    #[derive(Clone)]
    enum Part {
        Kw(String),
        Len(Calc),
    }
    let mut parts: Vec<Part> = Vec::new();
    for _ in 0..4 {
        if let Ok(k) = p.try_parse(|p| {
            let k = p.expect_ident_cloned()?.to_ascii_lowercase();
            if matches!(k.as_str(), "left" | "right" | "top" | "bottom" | "center") {
                Ok(k)
            } else {
                bad(p)
            }
        }) {
            parts.push(Part::Kw(k));
        } else if let Ok(l) = p.try_parse(|p| parse_length(p, true, true)) {
            parts.push(Part::Len(l));
        } else {
            break;
        }
    }
    let kw_x = |k: &str| match k {
        "left" => Some(pct(0.0)),
        "center" => Some(pct(50.0)),
        "right" => Some(pct(100.0)),
        _ => None,
    };
    let kw_y = |k: &str| match k {
        "top" => Some(pct(0.0)),
        "center" => Some(pct(50.0)),
        "bottom" => Some(pct(100.0)),
        _ => None,
    };
    let is_y = |k: &str| matches!(k, "top" | "bottom");
    let is_x = |k: &str| matches!(k, "left" | "right");
    let r = match parts.as_slice() {
        [Part::Len(a)] => Some((a.clone(), pct(50.0))),
        [Part::Kw(k)] => {
            if is_y(k) {
                Some((pct(50.0), kw_y(k).unwrap()))
            } else {
                kw_x(k).map(|x| (x, pct(50.0)))
            }
        }
        [a, b] => {
            let (a, b) = match (a, b) {
                (Part::Kw(x), Part::Kw(y)) if is_y(x) || is_x(y) => {
                    (Part::Kw(y.clone()), Part::Kw(x.clone()))
                }
                _ => (a.clone(), b.clone()),
            };
            let x = match &a {
                Part::Len(l) => Some(l.clone()),
                Part::Kw(k) => kw_x(k),
            };
            let y = match &b {
                Part::Len(l) => Some(l.clone()),
                Part::Kw(k) => kw_y(k),
            };
            x.zip(y)
        }
        [_, _, _] | [_, _, _, _] => {
            // Edge offsets: `right 5px bottom`, `left 10px top 20px`.
            let mut x = None;
            let mut y = None;
            let mut i = 0;
            while i < parts.len() {
                let Part::Kw(k) = &parts[i] else {
                    return bad(p);
                };
                let off = match parts.get(i + 1) {
                    Some(Part::Len(l)) => {
                        i += 1;
                        Some(l.clone())
                    }
                    _ => None,
                };
                let v = match (k.as_str(), off) {
                    ("left" | "top", Some(o)) => o,
                    ("right" | "bottom", Some(o)) => from_end(o),
                    (_, Some(_)) => return bad(p),
                    (k, None) => kw_x(k).or_else(|| kw_y(k)).unwrap_or(pct(50.0)),
                };
                if is_x(k) && x.is_none() {
                    x = Some(v);
                } else if is_y(k) && y.is_none() {
                    y = Some(v);
                } else if k == "center" && x.is_none() {
                    x = Some(v);
                } else if k == "center" && y.is_none() {
                    y = Some(v);
                } else {
                    return bad(p);
                }
                i += 1;
            }
            x.zip(y)
        }
        _ => None,
    };
    r.map_or_else(|| bad(p), Ok)
}

/// One `background-position-x` / `-y` value.
fn position_axis<'i>(p: &mut Parser<'i>, x: bool) -> PResult<Calc> {
    if let Ok(k) = p.try_parse(|p| p.expect_ident_cloned()) {
        let k = k.to_ascii_lowercase();
        let base = match (k.as_str(), x) {
            ("left", true) | ("top", false) => 0.0,
            ("center", _) => 50.0,
            ("right", true) | ("bottom", false) => 100.0,
            _ => return bad(p),
        };
        if let Ok(o) = p.try_parse(|p| parse_length(p, true, true)) {
            return Ok(if base == 0.0 { o } else { from_end(o) });
        }
        return Ok(pct(base));
    }
    parse_length(p, true, true)
}

fn size_value<'i>(p: &mut Parser<'i>) -> PResult<BgSize<Calc>> {
    if let Ok(k) = p.try_parse(|p| p.expect_ident_cloned()) {
        match k.to_ascii_lowercase().as_str() {
            "cover" => return Ok(BgSize::Cover),
            "contain" => return Ok(BgSize::Contain),
            "auto" => {}
            _ => return bad(p),
        }
        let b = p.try_parse(size_side).ok().flatten();
        return Ok(BgSize::Explicit(None, b));
    }
    let a = parse_length(p, true, false)?;
    let b = p.try_parse(size_side).ok().flatten();
    Ok(BgSize::Explicit(Some(a), b))
}

fn size_side<'i>(p: &mut Parser<'i>) -> PResult<Option<Calc>> {
    if p.try_parse(|p| p.expect_ident_matching("auto")).is_ok() {
        return Ok(None);
    }
    parse_length(p, true, false).map(Some)
}

fn repeat_value<'i>(p: &mut Parser<'i>) -> PResult<(Repeat, Repeat)> {
    let k = p.expect_ident_cloned()?.to_ascii_lowercase();
    match k.as_str() {
        "repeat-x" => return Ok((Repeat::Repeat, Repeat::NoRepeat)),
        "repeat-y" => return Ok((Repeat::NoRepeat, Repeat::Repeat)),
        _ => {}
    }
    let a = Repeat::from_ident(&k).map_or_else(|| bad(p), Ok)?;
    let b = p
        .try_parse(|p| {
            let k = p.expect_ident_cloned()?;
            Repeat::from_ident(&k).map_or_else(|| bad(p), Ok)
        })
        .unwrap_or(a);
    Ok((a, b))
}

fn box_value<'i>(p: &mut Parser<'i>) -> PResult<BgBox> {
    let k = p.expect_ident_cloned()?;
    BgBox::from_ident(&k).map_or_else(|| bad(p), Ok)
}

fn attachment_value<'i>(p: &mut Parser<'i>) -> PResult<Attachment> {
    let k = p.expect_ident_cloned()?;
    Attachment::from_ident(&k).map_or_else(|| bad(p), Ok)
}

/// Longhand list parsers.
pub(crate) fn images<'i>(p: &mut Parser<'i>) -> PResult<Vec<ImageSpec>> {
    comma_list(p, parse_image)
}
pub(crate) fn positions<'i>(p: &mut Parser<'i>, x: bool) -> PResult<Vec<Calc>> {
    comma_list(p, |p| position_axis(p, x))
}
pub(crate) fn sizes<'i>(p: &mut Parser<'i>) -> PResult<Vec<BgSize<Calc>>> {
    comma_list(p, size_value)
}
pub(crate) fn repeats<'i>(p: &mut Parser<'i>) -> PResult<Vec<(Repeat, Repeat)>> {
    comma_list(p, repeat_value)
}
pub(crate) fn boxes<'i>(p: &mut Parser<'i>) -> PResult<Vec<BgBox>> {
    comma_list(p, box_value)
}
pub(crate) fn attachments<'i>(p: &mut Parser<'i>) -> PResult<Vec<Attachment>> {
    comma_list(p, attachment_value)
}

/// `background-position`: x and y lists.
pub(crate) fn position_lists<'i>(p: &mut Parser<'i>) -> PResult<(Vec<Calc>, Vec<Calc>)> {
    let list = comma_list(p, position)?;
    Ok(list.into_iter().unzip())
}

/// Parsed `background` shorthand.
pub(crate) struct BgShorthand {
    pub color: ColorValue,
    pub images: Vec<ImageSpec>,
    pub pos_x: Vec<Calc>,
    pub pos_y: Vec<Calc>,
    pub size: Vec<BgSize<Calc>>,
    pub repeat: Vec<(Repeat, Repeat)>,
    pub attachment: Vec<Attachment>,
    pub origin: Vec<BgBox>,
    pub clip: Vec<BgBox>,
}

/// The `background` shorthand (color only in the last layer).
pub(crate) fn shorthand<'i>(p: &mut Parser<'i>) -> PResult<BgShorthand> {
    let mut out = BgShorthand {
        color: ColorValue::Rgba(Color::TRANSPARENT),
        images: Vec::new(),
        pos_x: Vec::new(),
        pos_y: Vec::new(),
        size: Vec::new(),
        repeat: Vec::new(),
        attachment: Vec::new(),
        origin: Vec::new(),
        clip: Vec::new(),
    };
    loop {
        let mut color = None;
        let mut image = None;
        let mut pos = None;
        let mut size = None;
        let mut repeat = None;
        let mut attach = None;
        let mut boxes: Vec<BgBox> = Vec::new();
        let mut any = false;
        loop {
            if color.is_none()
                && let Ok(c) = p.try_parse(parse_color)
            {
                color = Some(c);
            } else if image.is_none()
                && let Ok(i) = p.try_parse(parse_image)
            {
                image = Some(i);
            } else if pos.is_none()
                && let Ok(xy) = p.try_parse(position)
            {
                pos = Some(xy);
                if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                    size = Some(size_value(p)?);
                }
            } else if repeat.is_none()
                && let Ok(r) = p.try_parse(repeat_value)
            {
                repeat = Some(r);
            } else if attach.is_none()
                && let Ok(a) = p.try_parse(attachment_value)
            {
                attach = Some(a);
            } else if boxes.len() < 2
                && let Ok(b) = p.try_parse(box_value)
            {
                boxes.push(b);
            } else {
                break;
            }
            any = true;
        }
        if !any {
            return bad(p);
        }
        let last = p.try_parse(|p| p.expect_comma()).is_err();
        if color.is_some() && !last {
            return bad(p);
        }
        if let Some(c) = color {
            out.color = c;
        }
        out.images.push(image.unwrap_or(Image::None));
        let (x, y) = pos.unwrap_or((pct(0.0), pct(0.0)));
        out.pos_x.push(x);
        out.pos_y.push(y);
        out.size.push(size.unwrap_or(BgSize::Explicit(None, None)));
        out.repeat
            .push(repeat.unwrap_or((Repeat::Repeat, Repeat::Repeat)));
        out.attachment.push(attach.unwrap_or(Attachment::Scroll));
        let (o, c) = match boxes.as_slice() {
            [] => (BgBox::PaddingBox, BgBox::BorderBox),
            [b] => (*b, *b),
            [a, b, ..] => (*a, *b),
        };
        out.origin.push(o);
        out.clip.push(c);
        if last {
            break;
        }
    }
    Ok(out)
}

/// One `border-*-radius` value: horizontal and vertical radius.
pub(crate) fn radius<'i>(p: &mut Parser<'i>) -> PResult<(Calc, Calc)> {
    let a = parse_length(p, true, false)?;
    let b = p
        .try_parse(|p| parse_length(p, true, false))
        .unwrap_or_else(|_| a.clone());
    Ok((a, b))
}

/// `border-radius`: four corners (top-left, top-right, bottom-right,
/// bottom-left).
pub(crate) fn radius_shorthand<'i>(p: &mut Parser<'i>) -> PResult<[(Calc, Calc); 4]> {
    let side = |p: &mut Parser<'i>| -> PResult<[Calc; 4]> {
        let mut v = vec![parse_length(p, true, false)?];
        while v.len() < 4 {
            match p.try_parse(|p| parse_length(p, true, false)) {
                Ok(l) => v.push(l),
                Err(_) => break,
            }
        }
        let g = |i: usize| v[i].clone();
        Ok(match v.len() {
            1 => [g(0), g(0), g(0), g(0)],
            2 => [g(0), g(1), g(0), g(1)],
            3 => [g(0), g(1), g(2), g(1)],
            _ => [g(0), g(1), g(2), g(3)],
        })
    };
    let h = side(p)?;
    let v = if p.try_parse(|p| p.expect_delim('/')).is_ok() {
        side(p)?
    } else {
        h.clone()
    };
    let [h0, h1, h2, h3] = h;
    let [v0, v1, v2, v3] = v;
    Ok([(h0, v0), (h1, v1), (h2, v2), (h3, v3)])
}

/// `box-shadow`: `none` (empty) or a list.
pub(crate) fn shadows<'i>(p: &mut Parser<'i>) -> PResult<Vec<ShadowSpec>> {
    if p.try_parse(|p| p.expect_ident_matching("none")).is_ok() {
        return Ok(Vec::new());
    }
    comma_list(p, |p| {
        let mut color = None;
        let mut inset = false;
        let mut lens: Vec<Calc> = Vec::new();
        loop {
            if !inset && p.try_parse(|p| p.expect_ident_matching("inset")).is_ok() {
                inset = true;
            } else if color.is_none()
                && let Ok(c) = p.try_parse(parse_color)
            {
                color = Some(c);
            } else if lens.is_empty() {
                let Ok(a) = p.try_parse(|p| parse_length(p, false, true)) else {
                    break;
                };
                lens.push(a);
                lens.push(parse_length(p, false, true)?);
                if let Ok(b) = p.try_parse(|p| parse_length(p, false, false)) {
                    lens.push(b);
                    if let Ok(s) = p.try_parse(|p| parse_length(p, false, true)) {
                        lens.push(s);
                    }
                }
            } else {
                break;
            }
        }
        if lens.len() < 2 {
            return bad(p);
        }
        let z = || Calc::Dim(0.0, Unit::Px);
        Ok(Shadow {
            color,
            x: lens[0].clone(),
            y: lens[1].clone(),
            blur: lens.get(2).cloned().unwrap_or_else(z),
            spread: lens.get(3).cloned().unwrap_or_else(z),
            inset,
        })
    })
}

// ---------------------------------------------------------------------------
// Computing
// ---------------------------------------------------------------------------

/// Computes a specified image: lengths to px/%, colors resolved, URLs
/// made absolute against `base` (when given).
pub(crate) fn compute_image(
    i: &ImageSpec,
    lp: &dyn Fn(&Calc) -> Lp,
    current: Color,
    base: Option<&url::Url>,
) -> BgImage {
    let stops = |s: &[Stop<ColorValue, Calc>]| -> Vec<Stop<Color, Lp>> {
        s.iter()
            .map(|s| Stop {
                color: s.color.map(|c| c.resolve(current)),
                pos: s.pos.as_ref().map(lp),
            })
            .collect()
    };
    match i {
        Image::None => Image::None,
        Image::Url(u) => {
            let abs = base
                .and_then(|b| b.join(u.trim()).ok())
                .map(|u| Rc::from(u.as_str()))
                .unwrap_or_else(|| u.clone());
            Image::Url(abs)
        }
        Image::Linear {
            repeating,
            dir,
            stops: s,
        } => Image::Linear {
            repeating: *repeating,
            dir: *dir,
            stops: stops(s),
        },
        Image::Radial {
            repeating,
            circle,
            size,
            at,
            stops: s,
        } => Image::Radial {
            repeating: *repeating,
            circle: *circle,
            size: match size {
                RadialSize::ClosestSide => RadialSize::ClosestSide,
                RadialSize::ClosestCorner => RadialSize::ClosestCorner,
                RadialSize::FarthestSide => RadialSize::FarthestSide,
                RadialSize::FarthestCorner => RadialSize::FarthestCorner,
                RadialSize::Explicit(a, b) => RadialSize::Explicit(lp(a), b.as_ref().map(lp)),
            },
            at: at.as_ref().map(|(x, y)| (lp(x), lp(y))),
            stops: stops(s),
        },
    }
}

pub(crate) fn compute_size(s: &BgSize<Calc>, lp: &dyn Fn(&Calc) -> Lp) -> BgSize<Lp> {
    match s {
        BgSize::Cover => BgSize::Cover,
        BgSize::Contain => BgSize::Contain,
        BgSize::Explicit(a, b) => BgSize::Explicit(a.as_ref().map(lp), b.as_ref().map(lp)),
    }
}

pub(crate) fn compute_shadow(
    s: &ShadowSpec,
    px: &dyn Fn(&Calc) -> f32,
    current: Color,
) -> BoxShadow {
    Shadow {
        color: s.color.map_or(current, |c| c.resolve(current)),
        x: px(&s.x),
        y: px(&s.y),
        blur: px(&s.blur).max(0.0),
        spread: px(&s.spread),
        inset: s.inset,
    }
}

// ---------------------------------------------------------------------------
// Serialization (Chrome's computed form)
// ---------------------------------------------------------------------------

fn css_url(u: &str) -> String {
    let mut s = String::from("url(\"");
    for c in u.chars() {
        if c == '"' || c == '\\' {
            s.push('\\');
        }
        s.push(c);
    }
    s.push_str("\")");
    s
}

fn stops_css(stops: &[Stop<Color, Lp>]) -> String {
    stops
        .iter()
        .map(|s| match (&s.color, &s.pos) {
            (Some(c), Some(p)) => format!("{} {}", c.to_css(), p.to_css()),
            (Some(c), None) => c.to_css(),
            (None, Some(p)) => p.to_css(),
            (None, None) => String::new(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl BgImage {
    pub fn to_css(&self) -> String {
        match self {
            Image::None => "none".into(),
            Image::Url(u) => css_url(u),
            Image::Linear {
                repeating,
                dir,
                stops,
            } => {
                let name = if *repeating {
                    "repeating-linear-gradient"
                } else {
                    "linear-gradient"
                };
                let d = match dir {
                    LinearDir::Default => None,
                    LinearDir::Angle(a) => Some(format!("{}deg", fmt_num(*a))),
                    LinearDir::To(h, v) => {
                        let mut s = String::from("to");
                        if *v != 0 {
                            s.push_str(if *v < 0 { " top" } else { " bottom" });
                        }
                        if *h != 0 {
                            s.push_str(if *h < 0 { " left" } else { " right" });
                        }
                        Some(s)
                    }
                };
                match d {
                    Some(d) => format!("{name}({d}, {})", stops_css(stops)),
                    None => format!("{name}({})", stops_css(stops)),
                }
            }
            Image::Radial {
                repeating,
                circle,
                size,
                at,
                stops,
            } => {
                let name = if *repeating {
                    "repeating-radial-gradient"
                } else {
                    "radial-gradient"
                };
                let mut head: Vec<String> = Vec::new();
                if *circle {
                    head.push("circle".into());
                }
                match size {
                    RadialSize::FarthestCorner => {}
                    RadialSize::ClosestSide => head.push("closest-side".into()),
                    RadialSize::ClosestCorner => head.push("closest-corner".into()),
                    RadialSize::FarthestSide => head.push("farthest-side".into()),
                    RadialSize::Explicit(a, b) => {
                        head.push(a.to_css());
                        if let Some(b) = b {
                            head.push(b.to_css());
                        }
                    }
                }
                if let Some((x, y)) = at {
                    head.push(format!("at {} {}", x.to_css(), y.to_css()));
                }
                if head.is_empty() {
                    format!("{name}({})", stops_css(stops))
                } else {
                    format!("{name}({}, {})", head.join(" "), stops_css(stops))
                }
            }
        }
    }
}

impl BgSize<Lp> {
    pub fn to_css(&self) -> String {
        match self {
            BgSize::Cover => "cover".into(),
            BgSize::Contain => "contain".into(),
            BgSize::Explicit(None, None) => "auto".into(),
            BgSize::Explicit(a, b) => {
                let f = |v: &Option<Lp>| v.as_ref().map_or("auto".to_owned(), Lp::to_css);
                format!("{} {}", f(a), f(b))
            }
        }
    }
}

fn repeat_css(r: (Repeat, Repeat)) -> String {
    match r {
        (Repeat::Repeat, Repeat::NoRepeat) => "repeat-x".into(),
        (Repeat::NoRepeat, Repeat::Repeat) => "repeat-y".into(),
        (a, b) if a == b => a.as_str().into(),
        (a, b) => format!("{} {}", a.as_str(), b.as_str()),
    }
}

/// Serializes one background list longhand, repeated or truncated to the
/// number of layers.
pub fn list_css(bg: &Background, name: &str) -> Option<String> {
    let n = bg.images.len();
    let join = |f: &dyn Fn(usize) -> String| (0..n).map(f).collect::<Vec<_>>().join(", ");
    Some(match name {
        "background-image" => join(&|i| bg.images[i].to_css()),
        "background-position-x" => join(&|i| nth(&bg.pos_x, i).to_css()),
        "background-position-y" => join(&|i| nth(&bg.pos_y, i).to_css()),
        "background-position" => join(&|i| {
            format!(
                "{} {}",
                nth(&bg.pos_x, i).to_css(),
                nth(&bg.pos_y, i).to_css()
            )
        }),
        "background-size" => join(&|i| nth(&bg.size, i).to_css()),
        "background-repeat" => join(&|i| repeat_css(*nth(&bg.repeat, i))),
        "background-attachment" => join(&|i| nth(&bg.attachment, i).as_str().into()),
        "background-origin" => join(&|i| nth(&bg.origin, i).as_str().into()),
        "background-clip" => join(&|i| nth(&bg.clip, i).as_str().into()),
        _ => return None,
    })
}

/// The `background` shorthand as Chrome serializes it.
pub fn shorthand_css(bg: &Background, color: Color) -> String {
    let n = bg.images.len();
    (0..n)
        .map(|i| {
            let layer = format!(
                "{} {} {} {} {} / {} {} {}",
                bg.images[i].to_css(),
                repeat_css(*nth(&bg.repeat, i)),
                nth(&bg.attachment, i).as_str(),
                nth(&bg.pos_x, i).to_css(),
                nth(&bg.pos_y, i).to_css(),
                nth(&bg.size, i).to_css(),
                nth(&bg.origin, i).as_str(),
                nth(&bg.clip, i).as_str(),
            );
            if i + 1 == n {
                format!("{} {layer}", color.to_css())
            } else {
                layer
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One corner radius: `10px` or `10px 5px`.
pub fn radius_css(r: &(Lp, Lp)) -> String {
    let (a, b) = (r.0.to_css(), r.1.to_css());
    if a == b { a } else { format!("{a} {b}") }
}

/// The `border-radius` shorthand (corners top-left first).
pub fn radius_shorthand_css(r: &[(Lp, Lp); 4]) -> String {
    let collapse = |v: [String; 4]| -> String {
        if v[1] == v[3] {
            if v[0] == v[2] {
                if v[0] == v[1] {
                    return v[0].clone();
                }
                return format!("{} {}", v[0], v[1]);
            }
            return format!("{} {} {}", v[0], v[1], v[2]);
        }
        v.join(" ")
    };
    let h = collapse([0, 1, 2, 3].map(|i| r[i].0.to_css()));
    let v = collapse([0, 1, 2, 3].map(|i| r[i].1.to_css()));
    if h == v { h } else { format!("{h} / {v}") }
}

/// `box-shadow` as Chrome serializes it.
pub fn shadows_css(s: &[BoxShadow]) -> String {
    if s.is_empty() {
        return "none".into();
    }
    s.iter()
        .map(|s| {
            let mut out = format!(
                "{} {} {} {} {}",
                s.color.to_css(),
                fmt_px(s.x),
                fmt_px(s.y),
                fmt_px(s.blur),
                fmt_px(s.spread)
            );
            if s.inset {
                out.push_str(" inset");
            }
            out
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lp(c: &Calc) -> Lp {
        Lp::from_calc(c.clone())
    }

    fn img(css: &str) -> String {
        let mut p = Parser::new(css);
        let i = p.parse_entirely(images).unwrap();
        i.iter()
            .map(|i| compute_image(i, &lp, Color::BLACK, None).to_css())
            .collect::<Vec<_>>()
            .join(", ")
    }

    #[test]
    fn images_serialize_like_chrome() {
        assert_eq!(
            img("linear-gradient(to right, red 10%, rgba(0,0,255,0.5) 80%)"),
            "linear-gradient(to right, rgb(255, 0, 0) 10%, rgba(0, 0, 255, 0.5) 80%)"
        );
        assert_eq!(
            img("radial-gradient(circle, red, blue 50px)"),
            "radial-gradient(circle, rgb(255, 0, 0), rgb(0, 0, 255) 50px)"
        );
        assert_eq!(
            img(
                "radial-gradient(ellipse at top left, red, blue), repeating-linear-gradient(45deg, red 0 10px, blue 10px 20px)"
            ),
            "radial-gradient(at 0% 0%, rgb(255, 0, 0), rgb(0, 0, 255)), repeating-linear-gradient(45deg, rgb(255, 0, 0) 0px, rgb(255, 0, 0) 10px, rgb(0, 0, 255) 10px, rgb(0, 0, 255) 20px)"
        );
        assert_eq!(
            img("radial-gradient(farthest-side at 20px 30px, red, blue)"),
            "radial-gradient(farthest-side at 20px 30px, rgb(255, 0, 0), rgb(0, 0, 255))"
        );
        assert_eq!(
            img("linear-gradient(0.25turn, red, 30%, blue)"),
            "linear-gradient(90deg, rgb(255, 0, 0), 30%, rgb(0, 0, 255))"
        );
        assert_eq!(img("url(a.png)"), "url(\"a.png\")");
    }

    #[test]
    fn positions_and_shadows() {
        let mut p = Parser::new("right 5px bottom");
        let (x, y) = p.parse_entirely(position).unwrap();
        assert_eq!(lp(&x).to_css(), "calc(100% - 5px)");
        assert_eq!(lp(&y).to_css(), "100%");
        let mut p = Parser::new("2px 3px 4px red, inset 0 0 5px 1px rgba(0,0,0,.5)");
        let s = p.parse_entirely(shadows).unwrap();
        let c: Vec<BoxShadow> = s
            .iter()
            .map(|s| compute_shadow(s, &|c| lp(c).resolve(0.0), Color::BLACK))
            .collect();
        assert_eq!(
            shadows_css(&c),
            "rgb(255, 0, 0) 2px 3px 4px 0px, rgba(0, 0, 0, 0.5) 0px 0px 5px 1px inset"
        );
        let mut p = Parser::new("10px 20px / 5px");
        let r = p.parse_entirely(radius_shorthand).unwrap();
        let r = r.map(|(a, b)| (lp(&a), lp(&b)));
        assert_eq!(radius_shorthand_css(&r), "10px 20px / 5px");
        assert_eq!(radius_css(&r[0]), "10px 5px");
    }
}

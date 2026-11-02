//! Media queries: parsing, evaluation against a [`MediaEnv`] built from the
//! persona, and serialization for `MediaQueryList.media`.

use cssparser::{Parser, ParserInput, Token};
use lb_persona::PersonaProfile;

use crate::values::{PResult, bad, fmt_num};

/// Primary pointing device (`pointer` / `any-pointer`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PointerKind {
    None,
    Coarse,
    Fine,
}

/// Operating system of the persona. Picks UA stylesheet details (form
/// control fonts) and default fonts.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OsKind {
    Windows,
    MacOs,
    Linux,
    Other,
}

impl OsKind {
    /// From the persona's `os.name`.
    pub fn from_name(name: &str) -> OsKind {
        let n = name.to_ascii_lowercase();
        if n.starts_with("win") {
            OsKind::Windows
        } else if n.starts_with("mac") || n.contains("os x") {
            OsKind::MacOs
        } else if n.contains("linux") || n.contains("ubuntu") {
            OsKind::Linux
        } else {
            OsKind::Other
        }
    }

    /// Directory name under `profiles/fonts/`.
    pub fn as_str(self) -> &'static str {
        match self {
            OsKind::Windows => "windows",
            OsKind::MacOs => "macos",
            OsKind::Linux => "linux",
            OsKind::Other => "other",
        }
    }
}

/// Everything media queries and viewport units read. Built from the
/// persona; the viewport can be overridden (e.g. CDP device metrics).
#[derive(Clone, Debug, PartialEq)]
pub struct MediaEnv {
    /// Layout viewport (`innerWidth` / `innerHeight`) in CSS px.
    pub viewport_width: f32,
    pub viewport_height: f32,
    /// `screen.width` / `screen.height`.
    pub device_width: f32,
    pub device_height: f32,
    pub device_pixel_ratio: f32,
    /// Bits per color component (`color` feature).
    pub color_bits: u32,
    pub prefers_dark: bool,
    pub prefers_reduced_motion: bool,
    /// The primary pointer can hover.
    pub hover: bool,
    pub pointer: PointerKind,
    pub os: OsKind,
}

impl Default for MediaEnv {
    /// A 1920x1080 Windows desktop with a 1920x947 viewport.
    fn default() -> Self {
        MediaEnv {
            viewport_width: 1920.0,
            viewport_height: 947.0,
            device_width: 1920.0,
            device_height: 1080.0,
            device_pixel_ratio: 1.0,
            color_bits: 8,
            prefers_dark: false,
            prefers_reduced_motion: false,
            hover: true,
            pointer: PointerKind::Fine,
            os: OsKind::Windows,
        }
    }
}

impl MediaEnv {
    /// Media environment of a persona: viewport from `screen.inner_*`,
    /// device size and DPR from `screen`, pointer from `client_hints.mobile`.
    pub fn from_persona(p: &PersonaProfile) -> MediaEnv {
        let mobile = p.client_hints.as_ref().is_some_and(|h| h.mobile);
        MediaEnv {
            viewport_width: p.screen.inner_width as f32,
            viewport_height: p.screen.inner_height as f32,
            device_width: p.screen.width as f32,
            device_height: p.screen.height as f32,
            device_pixel_ratio: p.screen.device_pixel_ratio as f32,
            color_bits: (p.screen.color_depth / 3).clamp(1, 16),
            prefers_dark: false,
            prefers_reduced_motion: false,
            hover: !mobile,
            pointer: if mobile {
                PointerKind::Coarse
            } else {
                PointerKind::Fine
            },
            os: OsKind::from_name(&p.os.name),
        }
    }
}

/// A feature value with the unit it was written in (for serialization).
#[derive(Clone, Debug, PartialEq)]
enum MfValue {
    Num(f32),
    /// Length in px, plus original number and unit.
    Len(f32, f32, String),
    /// Resolution in dppx, plus original number and unit.
    Res(f32, f32, String),
    Ratio(f32, f32),
    Ident(String),
}

impl MfValue {
    fn to_css(&self) -> String {
        match self {
            MfValue::Num(v) => fmt_num(*v),
            MfValue::Len(_, v, u) | MfValue::Res(_, v, u) => format!("{}{}", fmt_num(*v), u),
            MfValue::Ratio(a, b) => format!("{} / {}", fmt_num(*a), fmt_num(*b)),
            MfValue::Ident(s) => s.clone(),
        }
    }

    fn number(&self) -> Option<f32> {
        match self {
            MfValue::Num(v) | MfValue::Len(v, ..) | MfValue::Res(v, ..) => Some(*v),
            MfValue::Ratio(a, b) => Some(a / b),
            MfValue::Ident(_) => None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Cmp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

impl Cmp {
    fn as_str(self) -> &'static str {
        match self {
            Cmp::Lt => "<",
            Cmp::Le => "<=",
            Cmp::Eq => "=",
            Cmp::Ge => ">=",
            Cmp::Gt => ">",
        }
    }

    fn test(self, a: f32, b: f32) -> bool {
        match self {
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
            Cmp::Eq => (a - b).abs() < 1e-4,
            Cmp::Ge => a >= b,
            Cmp::Gt => a > b,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Feature {
    /// `(name)` or `(name: value)`, `name` may carry `min-`/`max-`.
    Plain(String, Option<MfValue>),
    /// `(name op value)`, plus an optional second bound
    /// (`value op name op value`, stored as name-relative comparisons).
    Range {
        name: String,
        first: (Cmp, MfValue),
        second: Option<(Cmp, MfValue)>,
        /// Written as `value op name ...`.
        value_first: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
enum Cond {
    Feature(Feature),
    Not(Box<Cond>),
    And(Vec<Cond>),
    Or(Vec<Cond>),
    /// Unknown `<general-enclosed>`: never matches.
    Unknown(String),
}

#[derive(Clone, Debug, PartialEq)]
struct MediaQuery {
    not: bool,
    only: bool,
    /// Lowercased media type, `None` when omitted.
    media_type: Option<String>,
    cond: Option<Cond>,
    /// Failed to parse: serializes as `not all`, never matches.
    invalid: bool,
}

/// A parsed media query list. An empty list matches everything.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MediaQueryList {
    queries: Vec<MediaQuery>,
}

impl MediaQueryList {
    /// Parses a media query list (e.g. a `media` attribute or the text
    /// passed to `matchMedia`). Never fails: bad queries become `not all`.
    pub fn parse(text: &str) -> MediaQueryList {
        let mut input = ParserInput::new(text);
        let mut p = Parser::new(&mut input);
        Self::parse_from(&mut p)
    }

    pub(crate) fn parse_from(p: &mut Parser<'_, '_>) -> MediaQueryList {
        let mut queries = Vec::new();
        if p.is_exhausted() {
            return MediaQueryList { queries };
        }
        loop {
            let q = p
                .parse_until_before(cssparser::Delimiter::Comma, |p| {
                    let q = parse_query(p)?;
                    p.expect_exhausted()?;
                    Ok::<_, cssparser::ParseError<'_, ()>>(q)
                })
                .unwrap_or(MediaQuery {
                    not: false,
                    only: false,
                    media_type: None,
                    cond: None,
                    invalid: true,
                });
            queries.push(q);
            match p.next() {
                Ok(Token::Comma) => continue,
                _ => break,
            }
        }
        MediaQueryList { queries }
    }

    /// True for an empty list (matches all media).
    pub fn is_empty(&self) -> bool {
        self.queries.is_empty()
    }

    /// Evaluates the list: true if any query matches.
    pub fn matches(&self, env: &MediaEnv) -> bool {
        self.queries.is_empty() || self.queries.iter().any(|q| q.matches(env))
    }

    /// Serialization as `MediaList.mediaText` / `MediaQueryList.media`.
    pub fn to_css(&self) -> String {
        self.queries
            .iter()
            .map(MediaQuery::to_css)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl MediaQuery {
    fn matches(&self, env: &MediaEnv) -> bool {
        if self.invalid {
            return false;
        }
        let type_ok = match self.media_type.as_deref() {
            None | Some("all") | Some("screen") => true,
            _ => false,
        };
        let r = type_ok && self.cond.as_ref().is_none_or(|c| c.eval(env));
        r != self.not
    }

    fn to_css(&self) -> String {
        if self.invalid {
            return "not all".into();
        }
        let mut out = String::new();
        if self.not {
            out.push_str("not ");
        } else if self.only {
            out.push_str("only ");
        }
        match (&self.media_type, &self.cond) {
            (Some(t), Some(c)) => {
                out.push_str(t);
                out.push_str(" and ");
                out.push_str(&c.to_css(true));
            }
            (Some(t), None) => out.push_str(t),
            (None, Some(c)) => {
                if self.not {
                    out.push_str("all and ");
                }
                out.push_str(&c.to_css(true));
            }
            (None, None) => out.push_str("all"),
        }
        out
    }
}

impl Cond {
    fn eval(&self, env: &MediaEnv) -> bool {
        match self {
            Cond::Feature(f) => f.eval(env),
            Cond::Not(c) => !c.eval(env),
            Cond::And(cs) => cs.iter().all(|c| c.eval(env)),
            Cond::Or(cs) => cs.iter().any(|c| c.eval(env)),
            Cond::Unknown(_) => false,
        }
    }

    fn to_css(&self, top: bool) -> String {
        match self {
            Cond::Feature(f) => f.to_css(),
            Cond::Not(c) => format!("not {}", c.to_css(false)),
            Cond::And(cs) | Cond::Or(cs) => {
                let sep = if matches!(self, Cond::And(_)) {
                    " and "
                } else {
                    " or "
                };
                let s = cs
                    .iter()
                    .map(|c| c.to_css(false))
                    .collect::<Vec<_>>()
                    .join(sep);
                if top { s } else { format!("({s})") }
            }
            Cond::Unknown(s) => s.clone(),
        }
    }
}

/// Current value of a range feature, in the feature's canonical unit.
fn env_number(name: &str, env: &MediaEnv) -> Option<f32> {
    Some(match name {
        "width" => env.viewport_width,
        "height" => env.viewport_height,
        "device-width" => env.device_width,
        "device-height" => env.device_height,
        "aspect-ratio" => env.viewport_width / env.viewport_height.max(1.0),
        "device-aspect-ratio" => env.device_width / env.device_height.max(1.0),
        "resolution" | "-webkit-device-pixel-ratio" => env.device_pixel_ratio,
        "color" => env.color_bits as f32,
        "color-index" | "monochrome" | "grid" => 0.0,
        _ => return None,
    })
}

/// Value of a discrete feature as a keyword.
fn env_ident(name: &str, env: &MediaEnv) -> Option<&'static str> {
    Some(match name {
        "orientation" => {
            if env.viewport_height >= env.viewport_width {
                "portrait"
            } else {
                "landscape"
            }
        }
        "prefers-color-scheme" => {
            if env.prefers_dark {
                "dark"
            } else {
                "light"
            }
        }
        "prefers-reduced-motion" => {
            if env.prefers_reduced_motion {
                "reduce"
            } else {
                "no-preference"
            }
        }
        "hover" | "any-hover" => {
            if env.hover {
                "hover"
            } else {
                "none"
            }
        }
        "pointer" | "any-pointer" => match env.pointer {
            PointerKind::None => "none",
            PointerKind::Coarse => "coarse",
            PointerKind::Fine => "fine",
        },
        "display-mode" => "browser",
        "forced-colors" => "none",
        "inverted-colors" => "none",
        "prefers-contrast" => "no-preference",
        "prefers-reduced-transparency" => "no-preference",
        "prefers-reduced-data" => "no-preference",
        "scripting" => "enabled",
        "update" => "fast",
        "overflow-block" => "scroll",
        "overflow-inline" => "scroll",
        "color-gamut" => "srgb",
        "dynamic-range" | "video-dynamic-range" => "standard",
        "-webkit-transform-3d" => "1",
        _ => return None,
    })
}

impl Feature {
    fn eval(&self, env: &MediaEnv) -> bool {
        match self {
            Feature::Plain(name, value) => {
                let (prefix, base) = split_min_max(name);
                let base = base.as_str();
                if let Some(cur) = env_number(base, env) {
                    match value {
                        None => prefix.is_none() && cur != 0.0,
                        Some(v) => match v.number() {
                            Some(n) => prefix.unwrap_or(Cmp::Eq).test(cur, n),
                            None => false,
                        },
                    }
                } else if prefix.is_none()
                    && let Some(cur) = env_ident(base, env)
                {
                    match value {
                        None => !matches!(cur, "none" | "no-preference"),
                        Some(MfValue::Ident(v)) => v == cur,
                        Some(MfValue::Num(n)) => cur == fmt_num(*n),
                        _ => false,
                    }
                } else {
                    false
                }
            }
            Feature::Range {
                name,
                first,
                second,
                value_first,
            } => {
                let Some(cur) = env_number(name, env) else {
                    return false;
                };
                let check = |(cmp, v): &(Cmp, MfValue), value_first: bool| {
                    let Some(n) = v.number() else { return false };
                    if value_first {
                        cmp.test(n, cur)
                    } else {
                        cmp.test(cur, n)
                    }
                };
                check(first, *value_first)
                    && second.as_ref().is_none_or(|s| check(s, false))
            }
        }
    }

    fn to_css(&self) -> String {
        match self {
            Feature::Plain(name, None) => format!("({name})"),
            Feature::Plain(name, Some(v)) => format!("({name}: {})", v.to_css()),
            Feature::Range {
                name,
                first,
                second,
                value_first,
            } => {
                let mut s = if *value_first {
                    format!("({} {} {name}", first.1.to_css(), first.0.as_str())
                } else {
                    format!("({name} {} {}", first.0.as_str(), first.1.to_css())
                };
                if let Some((c, v)) = second {
                    s.push_str(&format!(" {} {}", c.as_str(), v.to_css()));
                }
                s.push(')');
                s
            }
        }
    }
}

fn parse_query<'i>(p: &mut Parser<'i, '_>) -> PResult<'i, MediaQuery> {
    // A query is either `[not|only]? type [and condition]?` or a condition.
    let start = p.state();
    if let Ok(ident) = p.expect_ident_cloned() {
        let lower = ident.to_ascii_lowercase();
        let (not, only, media_type) = match lower.as_str() {
            "not" | "only" => {
                let t = p.expect_ident()?.to_ascii_lowercase();
                (lower == "not", lower == "only", t)
            }
            _ => (false, false, lower),
        };
        if matches!(media_type.as_str(), "and" | "or" | "not" | "only" | "layer") {
            return bad(p);
        }
        let cond = if p.try_parse(|p| p.expect_ident_matching("and")).is_ok() {
            Some(parse_condition(p, false)?)
        } else {
            None
        };
        return Ok(MediaQuery {
            not,
            only,
            media_type: Some(media_type),
            cond,
            invalid: false,
        });
    }
    p.reset(&start);
    let cond = parse_condition(p, true)?;
    Ok(MediaQuery {
        not: false,
        only: false,
        media_type: None,
        cond: Some(cond),
        invalid: false,
    })
}

/// `<media-condition>`; `allow_or` is false after `type and`.
fn parse_condition<'i>(p: &mut Parser<'i, '_>, allow_or: bool) -> PResult<'i, Cond> {
    if p.try_parse(|p| p.expect_ident_matching("not")).is_ok() {
        return Ok(Cond::Not(Box::new(parse_in_parens(p)?)));
    }
    let first = parse_in_parens(p)?;
    let mut items = vec![first];
    let mut op: Option<&str> = None;
    loop {
        let next = p.try_parse(|p| {
            let i = p.expect_ident()?.to_ascii_lowercase();
            match i.as_str() {
                "and" => Ok("and"),
                "or" => Ok("or"),
                _ => bad(p),
            }
        });
        let Ok(kw) = next else { break };
        if kw == "or" && !allow_or {
            return bad(p);
        }
        if op.is_some_and(|o| o != kw) {
            return bad(p);
        }
        op = Some(kw);
        items.push(parse_in_parens(p)?);
    }
    Ok(match (op, items.len()) {
        (_, 1) => items.pop().expect("one"),
        (Some("or"), _) => Cond::Or(items),
        _ => Cond::And(items),
    })
}

fn parse_in_parens<'i>(p: &mut Parser<'i, '_>) -> PResult<'i, Cond> {
    let start = p.position();
    match p.next()? {
        Token::ParenthesisBlock => {}
        Token::Function(_) => {
            p.parse_nested_block(|p| {
                while p.next().is_ok() {}
                Ok::<_, cssparser::ParseError<'_, ()>>(())
            })?;
            return Ok(Cond::Unknown(p.slice_from(start).to_owned()));
        }
        _ => return bad(p),
    }
    let r = p.parse_nested_block(|p| {
        if let Ok(c) = p.try_parse(|p| {
            let c = parse_condition(p, true)?;
            p.expect_exhausted()?;
            Ok::<_, cssparser::ParseError<'_, ()>>(c)
        }) {
            return Ok(c);
        }
        if let Ok(f) = p.try_parse(|p| {
            let f = parse_feature(p)?;
            p.expect_exhausted()?;
            Ok::<_, cssparser::ParseError<'_, ()>>(f)
        }) {
            return Ok(Cond::Feature(f));
        }
        while p.next().is_ok() {}
        Ok(Cond::Unknown(String::new()))
    })?;
    if let Cond::Unknown(_) = r {
        return Ok(Cond::Unknown(p.slice_from(start).to_owned()));
    }
    Ok(r)
}

fn parse_cmp<'i>(p: &mut Parser<'i, '_>) -> PResult<'i, Cmp> {
    let first = match p.next()? {
        Token::Delim('<') => Cmp::Lt,
        Token::Delim('>') => Cmp::Gt,
        Token::Delim('=') => return Ok(Cmp::Eq),
        _ => return bad(p),
    };
    // `<=` / `>=` must have no whitespace between the two delims.
    let eq = p.try_parse(|p| match p.next_including_whitespace()? {
        Token::Delim('=') => Ok(()),
        _ => bad(p),
    });
    Ok(match (first, eq.is_ok()) {
        (Cmp::Lt, true) => Cmp::Le,
        (Cmp::Gt, true) => Cmp::Ge,
        (c, _) => c,
    })
}

fn parse_value<'i>(p: &mut Parser<'i, '_>) -> PResult<'i, MfValue> {
    let v = match p.next()?.clone() {
        Token::Number { value, .. } => {
            // `a / b` ratio.
            if p.try_parse(|p| p.expect_delim('/')).is_ok() {
                let b = p.expect_number()?;
                return Ok(MfValue::Ratio(value, b));
            }
            MfValue::Num(value)
        }
        Token::Dimension { value, unit, .. } => {
            let u = unit.to_ascii_lowercase();
            match u.as_str() {
                "px" => MfValue::Len(value, value, u),
                "em" | "rem" => MfValue::Len(value * 16.0, value, u),
                "pt" => MfValue::Len(value * 4.0 / 3.0, value, u),
                "in" => MfValue::Len(value * 96.0, value, u),
                "cm" => MfValue::Len(value * 96.0 / 2.54, value, u),
                "mm" => MfValue::Len(value * 96.0 / 25.4, value, u),
                "dppx" | "x" => MfValue::Res(value, value, u),
                "dpi" => MfValue::Res(value / 96.0, value, u),
                "dpcm" => MfValue::Res(value * 2.54 / 96.0, value, u),
                _ => return bad(p),
            }
        }
        Token::Ident(i) => MfValue::Ident(i.to_ascii_lowercase()),
        _ => return bad(p),
    };
    Ok(v)
}

fn parse_feature<'i>(p: &mut Parser<'i, '_>) -> PResult<'i, Feature> {
    // `name`, `name: value`, `name op value`, `value op name [op value]`.
    if let Ok(name) = p.try_parse(|p| p.expect_ident().map(|s| s.to_ascii_lowercase())) {
        if p.is_exhausted() {
            return Ok(Feature::Plain(name, None));
        }
        if p.try_parse(|p| p.expect_colon()).is_ok() {
            let v = parse_value(p)?;
            return Ok(Feature::Plain(name, Some(v)));
        }
        let cmp = parse_cmp(p)?;
        let v = parse_value(p)?;
        return Ok(Feature::Range {
            name,
            first: (cmp, v),
            second: None,
            value_first: false,
        });
    }
    let v1 = parse_value(p)?;
    let c1 = parse_cmp(p)?;
    let name = p.expect_ident()?.to_ascii_lowercase();
    let second = if p.is_exhausted() {
        None
    } else {
        let c2 = parse_cmp(p)?;
        let v2 = parse_value(p)?;
        Some((c2, v2))
    };
    Ok(Feature::Range {
        name,
        first: (c1, v1),
        second,
        value_first: true,
    })
}

/// Result of `window.matchMedia(query)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaQueryResult {
    /// Normalized query text (`MediaQueryList.media`).
    pub media: String,
    pub matches: bool,
}

/// Evaluates a `matchMedia` query.
pub fn match_media(query: &str, env: &MediaEnv) -> MediaQueryResult {
    let list = MediaQueryList::parse(query);
    MediaQueryResult {
        media: list.to_css(),
        matches: list.matches(env),
    }
}

/// Splits `min-`/`max-` (also `-webkit-min-`/`-webkit-max-`) off a feature
/// name.
fn split_min_max(name: &str) -> (Option<Cmp>, String) {
    for (pre, cmp, keep) in [
        ("min-", Cmp::Ge, ""),
        ("max-", Cmp::Le, ""),
        ("-webkit-min-", Cmp::Ge, "-webkit-"),
        ("-webkit-max-", Cmp::Le, "-webkit-"),
    ] {
        if let Some(b) = name.strip_prefix(pre) {
            return (Some(cmp), format!("{keep}{b}"));
        }
    }
    (None, name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> MediaEnv {
        MediaEnv {
            viewport_width: 1280.0,
            viewport_height: 720.0,
            device_pixel_ratio: 2.0,
            ..MediaEnv::default()
        }
    }

    fn m(q: &str) -> bool {
        match_media(q, &env()).matches
    }

    #[test]
    fn evaluates_features() {
        assert!(m("(min-width: 1000px)"));
        assert!(!m("(max-width: 1000px)"));
        assert!(m("screen and (min-width: 600px) and (orientation: landscape)"));
        assert!(!m("print"));
        assert!(m("not print"));
        assert!(m("(width >= 1280px)"));
        assert!(!m("(width > 1280px)"));
        assert!(m("(400px < width < 1300px)"));
        assert!(m("(min-resolution: 2dppx)"));
        assert!(m("(-webkit-min-device-pixel-ratio: 1.5)"));
        assert!(m("(prefers-color-scheme: light)"));
        assert!(m("(hover: hover) and (pointer: fine)"));
        assert!(!m("(pointer: coarse)"));
        assert!(m("(max-width: 100px), (min-width: 100px)"));
        assert!(m("(min-width: 50em)"));
        assert!(!m("(unknown-feature)"));
        assert!(m(""));
        assert!(m("all"));
        assert!(m("not (max-width: 100px)"));
        assert!(m("(max-width: 100px) or (color)"));
    }

    #[test]
    fn serializes_like_chrome() {
        let s = |q| match_media(q, &env()).media;
        assert_eq!(s("screen and (min-width:600px)"), "screen and (min-width: 600px)");
        assert_eq!(s("(MIN-WIDTH: 40EM)"), "(min-width: 40em)");
        assert_eq!(s("(width>=600px)"), "(width >= 600px)");
        assert_eq!(s("foo bar baz"), "not all");
        assert_eq!(s("(aspect-ratio: 16/9)"), "(aspect-ratio: 16 / 9)");
    }
}

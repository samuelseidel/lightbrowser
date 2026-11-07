//! `CSS.supports()` and `@supports` conditions as in Chrome 153 (CSS
//! Conditional 3/4 and Blink's parser).
//!
//! - Declarations: the property must be one Chrome knows
//!   ([`supports_data::PROPERTIES`], recorded from Chrome 153 by
//!   `tools/css_supports/record.py`); custom properties take any value.
//!   A value is supported when lb-css parses it for the property, or when
//!   every top-level component (keyword, number, length, color, function,
//!   ...) is of a kind Chrome accepts for that property (an approximation:
//!   the parts are checked one by one, not their order or count). CSS-wide
//!   keywords alone and any value with `var()` / `env()` pass.
//! - Conditions: `not`, `and`, `or` (not mixed), nesting,
//!   `selector()` (one complex selector; pseudo-classes and elements as
//!   Chrome lists them; no nested `:has()`, no pseudo-elements inside
//!   `:has()`), `font-tech()`, `font-format()`, `at-rule()`; other
//!   functions and blocks (general-enclosed) are false. Text that is not
//!   a condition is tried again in parentheses, as the spec says.

use cssparser::{ParseError, Parser, Token};

use crate::supports_data::{CLASSES, COLOR_BIT, FN_SETS, FUNCS, KEYWORDS, KW_SETS, PROPERTIES};
use crate::values::{PResult, bad};

/// `CSS.supports(property, value)`.
pub fn supports_declaration(name: &str, value: &str) -> bool {
    declaration(name, value, false)
}

/// `CSS.supports(conditionText)`.
pub fn supports_condition(text: &str) -> bool {
    let whole = |t: &str| {
        let mut p = Parser::new(t);
        p.parse_entirely(condition).ok()
    };
    whole(text)
        .or_else(|| whole(&format!("({text})")))
        .unwrap_or(false)
}

/// An `@supports` prelude (the rule applies when true).
pub(crate) fn supports_prelude(p: &mut Parser<'_>) -> PResult<bool> {
    condition(p)
}

// ---------------------------------------------------------------------------
// Conditions
// ---------------------------------------------------------------------------

fn condition(p: &mut Parser<'_>) -> PResult<bool> {
    if p.try_parse(|p| p.expect_ident_matching("not")).is_ok() {
        return Ok(!in_parens(p)?);
    }
    let mut result = in_parens(p)?;
    let mut op: Option<bool> = None;
    loop {
        if p.is_exhausted() {
            return Ok(result);
        }
        let and = match p.next()? {
            Token::Ident(i) if i.eq_ignore_ascii_case("and") => true,
            Token::Ident(i) if i.eq_ignore_ascii_case("or") => false,
            _ => return bad(p),
        };
        if op.is_some_and(|o| o != and) {
            return bad(p);
        }
        op = Some(and);
        let rhs = in_parens(p)?;
        result = if and { result && rhs } else { result || rhs };
    }
}

fn in_parens(p: &mut Parser<'_>) -> PResult<bool> {
    match p.next()?.clone() {
        Token::ParenthesisBlock => p.parse_nested_block(|p| {
            if let Ok(r) = p.try_parse(|p| p.parse_entirely(condition)) {
                return Ok(r);
            }
            if let Ok(r) = p.try_parse(|p| p.parse_entirely(feature_declaration)) {
                return Ok(r);
            }
            general_enclosed(p)
        }),
        Token::Function(f) => {
            let f = f.to_ascii_lowercase();
            p.parse_nested_block(|p| {
                let r = match f.as_str() {
                    "selector" => p
                        .try_parse(|p| p.parse_entirely(|p| complex(p, false, false)))
                        .is_ok(),
                    "font-tech" => one_ident(p).is_some_and(|i| FONT_TECH.contains(&i.as_str())),
                    "font-format" => {
                        one_ident(p).is_some_and(|i| FONT_FORMAT.contains(&i.as_str()))
                    }
                    "at-rule" => p
                        .try_parse(|p| {
                            p.parse_entirely(|p| match p.next()? {
                                Token::AtKeyword(a) => Ok(a.to_ascii_lowercase()),
                                _ => bad(p),
                            })
                        })
                        .is_ok_and(|a| AT_RULES.contains(&a.as_str())),
                    _ => false,
                };
                general_enclosed(p)?;
                Ok(r)
            })
        }
        _ => bad(p),
    }
}

/// `( <declaration> )` inside a condition (`!important` is allowed).
fn feature_declaration(p: &mut Parser<'_>) -> PResult<bool> {
    let name = p.expect_ident()?.to_string();
    p.expect_colon()?;
    let start = p.position();
    while p.next_including_whitespace_and_comments().is_ok() {}
    Ok(declaration(&name, p.slice_from(start), true))
}

/// Consume the rest of a block (`<any-value>`): false, or an error on a
/// bad token.
fn general_enclosed(p: &mut Parser<'_>) -> PResult<bool> {
    if well_formed(p) { Ok(false) } else { bad(p) }
}

fn one_ident(p: &mut Parser<'_>) -> Option<String> {
    p.try_parse(|p| {
        p.parse_entirely(|p| {
            p.expect_ident()
                .map(|i| i.to_ascii_lowercase())
                .map_err(ParseError::<()>::from)
        })
    })
    .ok()
}

const FONT_TECH: &[&str] = &[
    "features-opentype",
    "features-aat",
    "color-colrv0",
    "color-colrv1",
    "color-sbix",
    "color-cbdt",
    "variations",
    "palettes",
];

const FONT_FORMAT: &[&str] = &["collection", "opentype", "truetype", "woff", "woff2"];

const AT_RULES: &[&str] = &[
    "container",
    "counter-style",
    "font-face",
    "font-feature-values",
    "font-palette-values",
    "function",
    "import",
    "keyframes",
    "-webkit-keyframes",
    "layer",
    "media",
    "namespace",
    "page",
    "position-try",
    "property",
    "scope",
    "starting-style",
    "supports",
    "view-transition",
    // `@font-feature-values` blocks.
    "stylistic",
    "historical-forms",
    "styleset",
    "character-variant",
    "swash",
    "ornaments",
    "annotation",
];

/// Every token is well formed (no bad strings or URLs, no unmatched
/// closing brackets), nested blocks included. Consumes the input.
fn well_formed(p: &mut Parser<'_>) -> bool {
    loop {
        let t = match p.next_including_whitespace_and_comments() {
            Ok(t) => t.clone(),
            Err(_) => return true,
        };
        match t {
            Token::BadString(_)
            | Token::BadUrl(_)
            | Token::CloseParenthesis
            | Token::CloseSquareBracket
            | Token::CloseCurlyBracket => return false,
            Token::Function(_)
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock
            | Token::CurlyBracketBlock => {
                let ok = p
                    .parse_nested_block(|p| if well_formed(p) { Ok(()) } else { bad(p) })
                    .is_ok();
                if !ok {
                    return false;
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Declarations
// ---------------------------------------------------------------------------

fn declaration(name: &str, value: &str, allow_important: bool) -> bool {
    // `!important`: allowed in a condition, never in the two-argument form.
    let (value, important) = crate::props::split_important(value);
    // Chrome takes `initial !important` in both forms, `red !important`
    // only in a condition.
    let wide = WIDE.iter().any(|w| value.eq_ignore_ascii_case(w));
    if important && !allow_important && !wide {
        return false;
    }
    if let Some(custom) = name.strip_prefix("--") {
        if custom.is_empty() {
            return false;
        }
        if value.is_empty() && !allow_important {
            return false;
        }
        return !has_bang(value) && well_formed(&mut Parser::new(value));
    }
    let lower = name.to_ascii_lowercase();
    let Ok(i) = PROPERTIES.binary_search_by(|r| r.0.cmp(lower.as_str())) else {
        return false;
    };
    if value.is_empty() || has_bang(value) {
        return false;
    }
    if wide {
        return true;
    }
    if !generic_value(PROPERTIES[i], value) {
        // The per-component check misses combinations in shorthands
        // (`font: 12px/1.5 Arial`); lb-css's parser knows its shorthands.
        let n = crate::props::normalize_name(&lower);
        return crate::props::shorthand_longhands(&n).is_some()
            && !has_unknown_function(value)
            && crate::props::is_valid_declaration(&lower, value);
    }
    // For the properties lb-css implements, its parser also decides values
    // made only of numbers and lengths (it counts them, which the check
    // above does not: `margin: 1px 2px 3px 4px 5px`). Keywords and
    // functions it may not know are left to the check above.
    lb_css_unknown(&lower)
        || !only_numeric(value)
        || crate::props::is_valid_declaration(&lower, value)
}

fn lb_css_unknown(name: &str) -> bool {
    let n = crate::props::normalize_name(name);
    crate::props::LonghandId::from_name(&n).is_none()
        && crate::props::shorthand_longhands(&n).is_none()
}

/// Only numbers, percentages and the length units lb-css parses.
fn only_numeric(value: &str) -> bool {
    let mut p = Parser::new(value);
    while let Ok(t) = p.next() {
        match t {
            Token::Number { .. } | Token::Percentage { .. } => {}
            Token::Dimension { unit, .. } => {
                let u = unit.to_ascii_lowercase();
                if !matches!(
                    u.as_str(),
                    "px" | "em"
                        | "rem"
                        | "ex"
                        | "ch"
                        | "vw"
                        | "vh"
                        | "vmin"
                        | "vmax"
                        | "cm"
                        | "mm"
                        | "q"
                        | "in"
                        | "pt"
                        | "pc"
                ) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

/// A function no property takes (`transform: foo(1)`).
fn has_unknown_function(value: &str) -> bool {
    let mut p = Parser::new(value);
    let mut unknown = false;
    while let Ok(t) = p.next() {
        let name = match t {
            Token::Function(f) => f.to_ascii_lowercase(),
            Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock => {
                String::new()
            }
            _ => continue,
        };
        if !name.is_empty() {
            let n = name.as_str();
            unknown |= !(MATH.contains(&n)
                || COLOR_FNS.contains(&n)
                || GRADIENTS.contains(&n)
                || FUNCS.binary_search(&n).is_ok());
        }
        unknown |= p
            .parse_nested_block(|p| {
                let start = p.position();
                while p.next().is_ok() {}
                Ok::<_, ParseError<()>>(has_unknown_function(p.slice_from(start)))
            })
            .unwrap_or(false);
    }
    unknown
}

/// A top-level `!` (or `;`): never part of a valid value.
fn has_bang(value: &str) -> bool {
    let mut p = Parser::new(value);
    while let Ok(t) = p.next() {
        if matches!(t, Token::Delim('!') | Token::Semicolon) {
            return true;
        }
    }
    false
}

fn has_var(p: &mut Parser<'_>) -> bool {
    let mut found = false;
    while let Ok(t) = p.next() {
        match t.clone() {
            Token::Function(f)
                if f.eq_ignore_ascii_case("var") || f.eq_ignore_ascii_case("env") =>
            {
                found = true;
                let _ = p.parse_nested_block(|p| {
                    while p.next().is_ok() {}
                    Ok::<_, ParseError<()>>(())
                });
            }
            Token::Function(_)
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock
            | Token::CurlyBracketBlock => {
                let inner = p
                    .parse_nested_block(|p| Ok::<_, ParseError<()>>(has_var(p)))
                    .unwrap_or(false);
                found |= inner;
            }
            _ => {}
        }
    }
    found
}

const WIDE: &[&str] = &["inherit", "initial", "unset", "revert", "revert-layer"];

const MATH: &[&str] = &[
    "calc",
    "-webkit-calc",
    "min",
    "max",
    "clamp",
    "round",
    "mod",
    "rem",
    "abs",
    "sign",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "atan2",
    "pow",
    "sqrt",
    "hypot",
    "log",
    "exp",
    "sibling-index",
    "sibling-count",
    "progress",
];

const COLOR_FNS: &[&str] = &[
    "rgb",
    "rgba",
    "hsl",
    "hsla",
    "hwb",
    "lab",
    "lch",
    "oklab",
    "oklch",
    "color",
    "color-mix",
    "light-dark",
    "contrast-color",
];

const GRADIENTS: &[&str] = &[
    "linear-gradient",
    "radial-gradient",
    "conic-gradient",
    "repeating-linear-gradient",
    "repeating-radial-gradient",
    "repeating-conic-gradient",
    "-webkit-linear-gradient",
    "-webkit-radial-gradient",
    "-webkit-repeating-linear-gradient",
    "-webkit-repeating-radial-gradient",
];

const SYSTEM_COLORS: &[&str] = &[
    "accentcolor",
    "accentcolortext",
    "activeborder",
    "activecaption",
    "activetext",
    "appworkspace",
    "background",
    "buttonborder",
    "buttonface",
    "buttonhighlight",
    "buttonshadow",
    "buttontext",
    "canvas",
    "canvastext",
    "captiontext",
    "currentcolor",
    "field",
    "fieldtext",
    "graytext",
    "highlight",
    "highlighttext",
    "inactiveborder",
    "inactivecaption",
    "inactivecaptiontext",
    "infobackground",
    "infotext",
    "linktext",
    "mark",
    "marktext",
    "menu",
    "menutext",
    "scrollbar",
    "selecteditem",
    "selecteditemtext",
    "threeddarkshadow",
    "threedface",
    "threedhighlight",
    "threedlightshadow",
    "threedshadow",
    "transparent",
    "visitedtext",
    "window",
    "windowframe",
    "windowtext",
    "-webkit-link",
    "-webkit-text",
    "-webkit-activelink",
];

const LENGTH_UNITS: &[&str] = &[
    "px", "em", "rem", "ex", "rex", "ch", "rch", "ic", "ric", "cap", "rcap", "lh", "rlh", "vw",
    "vh", "vi", "vb", "vmin", "vmax", "svw", "svh", "svi", "svb", "svmin", "svmax", "lvw", "lvh",
    "lvi", "lvb", "lvmin", "lvmax", "dvw", "dvh", "dvi", "dvb", "dvmin", "dvmax", "cqw", "cqh",
    "cqi", "cqb", "cqmin", "cqmax", "cm", "mm", "q", "in", "pt", "pc",
];

fn class(mask: u32, name: &str) -> bool {
    CLASSES
        .iter()
        .position(|c| *c == name)
        .is_some_and(|i| mask & (1 << i) != 0)
}

fn has_fn(set: u16, name: &str) -> bool {
    let Ok(i) = FUNCS.binary_search(&name) else {
        return false;
    };
    FN_SETS[set as usize].contains(&(i as u8))
}

fn ident_ok((_, mask, kw, _): (&str, u32, u16, u16), ident: &str) -> bool {
    let lower = ident.to_ascii_lowercase();
    if lower.starts_with("--") {
        return class(mask, "--a");
    }
    if class(mask, "foo") {
        return true;
    }
    if mask & (1 << COLOR_BIT) != 0
        && (cssparser::color::parse_named_color(&lower).is_ok()
            || SYSTEM_COLORS.contains(&lower.as_str()))
    {
        return true;
    }
    KEYWORDS
        .binary_search(&lower.as_str())
        .is_ok_and(|i| KW_SETS[kw as usize].binary_search(&(i as u16)).is_ok())
}

fn function_ok((_, _, _, fns): (&str, u32, u16, u16), name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let n = name.as_str();
    if MATH.contains(&n) {
        return ["math", "mathn", "mathp", "matht", "matha"]
            .iter()
            .any(|m| has_fn(fns, m));
    }
    if COLOR_FNS.contains(&n) {
        return has_fn(fns, "color");
    }
    if GRADIENTS.contains(&n) {
        return has_fn(fns, "gradient");
    }
    has_fn(fns, n)
}

/// The component-by-component check of a value (see the module docs).
fn generic_value(row: (&str, u32, u16, u16), value: &str) -> bool {
    if !well_formed(&mut Parser::new(value)) {
        return false;
    }
    if has_var(&mut Parser::new(value)) {
        return true;
    }
    let mask = row.1;
    let mut p = Parser::new(value);
    let mut count = 0;
    let mut wide = false;
    while let Ok(t) = p.next() {
        let t = t.clone();
        let ok = match &t {
            Token::Ident(i) if WIDE.iter().any(|w| i.eq_ignore_ascii_case(w)) => {
                wide = true;
                true
            }
            Token::Ident(i) => ident_ok(row, i),
            Token::Number { value, .. } if *value == 0.0 => {
                class(mask, "0") || class(mask, "1") || class(mask, "1px")
            }
            Token::Number { .. } => class(mask, "1"),
            Token::Percentage { .. } => class(mask, "50%"),
            Token::Dimension { unit, .. } => {
                let u = unit.to_ascii_lowercase();
                let u = u.as_str();
                if LENGTH_UNITS.contains(&u) {
                    class(mask, "1px")
                } else if matches!(u, "s" | "ms") {
                    class(mask, "1s")
                } else if matches!(u, "deg" | "rad" | "grad" | "turn") {
                    class(mask, "45deg")
                } else if u == "fr" {
                    class(mask, "1fr")
                } else if matches!(u, "dpi" | "dpcm" | "dppx" | "x") {
                    class(mask, "2dppx")
                } else if matches!(u, "hz" | "khz") {
                    class(mask, "10hz")
                } else {
                    false
                }
            }
            Token::Hash(h) | Token::IDHash(h) => {
                class(mask, "#f00") && cssparser::color::parse_hash_color(h.as_bytes()).is_ok()
            }
            Token::QuotedString(_) => class(mask, "\"abcd\""),
            Token::UnquotedUrl(_) => function_ok(row, "url"),
            Token::Function(f) => function_ok(row, f),
            Token::SquareBracketBlock => class(mask, "[a]"),
            Token::Comma | Token::Delim('/') => {
                continue;
            }
            _ => false,
        };
        if !ok {
            return false;
        }
        if matches!(
            t,
            Token::Function(_) | Token::SquareBracketBlock | Token::ParenthesisBlock
        ) {
            let _ = p.parse_nested_block(|p| {
                while p.next().is_ok() {}
                Ok::<_, ParseError<()>>(())
            });
        }
        count += 1;
    }
    count > 0 && !(wide && count > 1)
}

// ---------------------------------------------------------------------------
// selector()
// ---------------------------------------------------------------------------

/// Pseudo-classes Chrome 153 accepts in `selector()` (no arguments).
const PSEUDO_CLASSES: &[&str] = &[
    "active",
    "active-view-transition",
    "any-link",
    "autofill",
    "checked",
    "corner-present",
    "current",
    "decrement",
    "default",
    "defined",
    "disabled",
    "double-button",
    "empty",
    "enabled",
    "end",
    "first-child",
    "first-of-type",
    "focus",
    "focus-visible",
    "focus-within",
    "fullscreen",
    "future",
    "horizontal",
    "host",
    "hover",
    "in-range",
    "increment",
    "indeterminate",
    "invalid",
    "last-child",
    "last-of-type",
    "link",
    "modal",
    "no-button",
    "only-child",
    "only-of-type",
    "open",
    "optional",
    "out-of-range",
    "past",
    "picture-in-picture",
    "placeholder-shown",
    "popover-open",
    "read-only",
    "read-write",
    "required",
    "root",
    "scope",
    "single-button",
    "start",
    "target",
    "target-current",
    "user-invalid",
    "user-valid",
    "valid",
    "vertical",
    "visited",
    "window-inactive",
    "xr-overlay",
    "-internal-autofill-selected",
    "-webkit-any-link",
    "-webkit-autofill",
    "-webkit-drag",
    "-webkit-full-page-media",
    "-webkit-full-screen",
    "-webkit-full-screen-ancestor",
];

/// Pseudo-elements Chrome 153 accepts in `selector()` (no arguments).
const PSEUDO_ELEMENTS: &[&str] = &[
    "after",
    "backdrop",
    "before",
    "checkmark",
    "column",
    "cue",
    "details-content",
    "file-selector-button",
    "first-letter",
    "first-line",
    "grammar-error",
    "marker",
    "picker-icon",
    "placeholder",
    "scroll-marker",
    "scroll-marker-group",
    "search-text",
    "selection",
    "spelling-error",
    "target-text",
    "view-transition",
    "-webkit-calendar-picker-indicator",
    "-webkit-clear-button",
    "-webkit-color-swatch",
    "-webkit-color-swatch-wrapper",
    "-webkit-datetime-edit",
    "-webkit-datetime-edit-fields-wrapper",
    "-webkit-datetime-edit-text",
    "-webkit-datetime-edit-year-field",
    "-webkit-datetime-edit-month-field",
    "-webkit-datetime-edit-day-field",
    "-webkit-datetime-edit-hour-field",
    "-webkit-datetime-edit-minute-field",
    "-webkit-datetime-edit-second-field",
    "-webkit-datetime-edit-millisecond-field",
    "-webkit-datetime-edit-ampm-field",
    "-webkit-datetime-edit-week-field",
    "-webkit-file-upload-button",
    "-webkit-inner-spin-button",
    "-webkit-input-placeholder",
    "-webkit-media-controls",
    "-webkit-media-controls-panel",
    "-webkit-media-slider-container",
    "-webkit-media-text-track-container",
    "-webkit-meter-bar",
    "-webkit-meter-even-less-good-value",
    "-webkit-meter-inner-element",
    "-webkit-meter-optimum-value",
    "-webkit-meter-suboptimum-value",
    "-webkit-progress-bar",
    "-webkit-progress-inner-element",
    "-webkit-progress-value",
    "-webkit-resizer",
    "-webkit-scrollbar",
    "-webkit-scrollbar-button",
    "-webkit-scrollbar-corner",
    "-webkit-scrollbar-thumb",
    "-webkit-scrollbar-track",
    "-webkit-scrollbar-track-piece",
    "-webkit-search-cancel-button",
    "-webkit-slider-runnable-track",
    "-webkit-slider-thumb",
    "-webkit-textfield-decoration-container",
];

/// One complex selector (`rel`: may start with a combinator, as in
/// `:has()`). `in_has`: inside `:has()`.
fn complex(p: &mut Parser<'_>, rel: bool, in_has: bool) -> PResult<()> {
    p.skip_whitespace();
    if rel {
        let _ = p.try_parse(|p| match p.next()? {
            Token::Delim('>' | '+' | '~') => Ok(()),
            _ => bad(p),
        });
        p.skip_whitespace();
    }
    compound(p, in_has)?;
    loop {
        let mut space = false;
        loop {
            let st = p.state();
            match p.next_including_whitespace() {
                Ok(Token::WhiteSpace(_)) => space = true,
                Ok(_) => {
                    p.reset(&st);
                    break;
                }
                Err(_) => return Ok(()),
            }
        }
        let st = p.state();
        match p.next_including_whitespace()?.clone() {
            Token::Delim('>' | '+' | '~') => {
                p.skip_whitespace();
            }
            _ if space => p.reset(&st),
            _ => return bad(p),
        }
        compound(p, in_has)?;
    }
}

/// A selector list (`:is()`, `:not()`, ...): one or more, comma separated.
fn list(p: &mut Parser<'_>, rel: bool, in_has: bool) -> PResult<()> {
    p.parse_comma_separated(|p| complex(p, rel, in_has))
        .map(|_| ())
}

fn compound(p: &mut Parser<'_>, in_has: bool) -> PResult<()> {
    // Type selector with an optional namespace: `*|a` and `|a`; no
    // namespace is declared, so `ns|a` fails.
    let mut any = false;
    let st = p.state();
    match p.next_including_whitespace()?.clone() {
        first @ (Token::Ident(_) | Token::Delim('*')) => {
            any = true;
            let st2 = p.state();
            if matches!(p.next_including_whitespace(), Ok(Token::Delim('|'))) {
                if !matches!(first, Token::Delim('*')) {
                    return bad(p);
                }
                match p.next_including_whitespace()? {
                    Token::Ident(_) | Token::Delim('*') => {}
                    _ => return bad(p),
                }
            } else {
                p.reset(&st2);
            }
        }
        Token::Delim('|') => match p.next_including_whitespace()? {
            Token::Ident(_) | Token::Delim('*') => any = true,
            _ => return bad(p),
        },
        Token::Delim('&') => any = true,
        _ => p.reset(&st),
    }
    rest_of_compound(p, in_has, any)
}

fn rest_of_compound(p: &mut Parser<'_>, in_has: bool, mut any: bool) -> PResult<()> {
    let mut after_element = false;
    loop {
        let st = p.state();
        let t = match p.next_including_whitespace() {
            Ok(t) => t.clone(),
            Err(_) => break,
        };
        match t {
            Token::IDHash(_) if !after_element => {}
            Token::Delim('.') if !after_element => match p.next_including_whitespace()? {
                Token::Ident(_) => {}
                _ => return bad(p),
            },
            Token::Delim('&') if !after_element => {}
            Token::SquareBracketBlock if !after_element => p.parse_nested_block(attribute)?,
            Token::Colon => {
                if pseudo(p, in_has)? {
                    after_element = true;
                }
            }
            _ => {
                p.reset(&st);
                break;
            }
        }
        any = true;
    }
    if any { Ok(()) } else { bad(p) }
}

fn attribute(p: &mut Parser<'_>) -> PResult<()> {
    p.skip_whitespace();
    match p.next_including_whitespace()?.clone() {
        Token::Ident(_) => {
            let st = p.state();
            if matches!(p.next_including_whitespace(), Ok(Token::Delim('|'))) {
                return bad(p);
            }
            p.reset(&st);
        }
        Token::Delim('*') => {
            p.expect_delim('|')?;
            p.expect_ident()?;
        }
        Token::Delim('|') => {
            p.expect_ident()?;
        }
        _ => return bad(p),
    }
    p.skip_whitespace();
    if p.is_exhausted() {
        return Ok(());
    }
    match p.next_including_whitespace()?.clone() {
        Token::Delim('=')
        | Token::IncludeMatch
        | Token::DashMatch
        | Token::PrefixMatch
        | Token::SuffixMatch
        | Token::SubstringMatch => {}
        _ => return bad(p),
    }
    match p.next()? {
        Token::Ident(_) | Token::QuotedString(_) => {}
        _ => return bad(p),
    }
    if p.is_exhausted() {
        return Ok(());
    }
    match p.next()? {
        Token::Ident(i) if i.eq_ignore_ascii_case("i") || i.eq_ignore_ascii_case("s") => {}
        _ => return bad(p),
    }
    p.expect_exhausted().map_err(ParseError::from)
}

/// After `:`. True for a pseudo-element.
fn pseudo(p: &mut Parser<'_>, in_has: bool) -> PResult<bool> {
    match p.next_including_whitespace()?.clone() {
        Token::Colon => {
            if in_has {
                return bad(p);
            }
            match p.next_including_whitespace()?.clone() {
                Token::Ident(n) => {
                    let n = n.to_ascii_lowercase();
                    if PSEUDO_ELEMENTS.contains(&n.as_str()) {
                        Ok(true)
                    } else {
                        bad(p)
                    }
                }
                Token::Function(n) => {
                    let n = n.to_ascii_lowercase();
                    p.parse_nested_block(|p| {
                        match n.as_str() {
                            "highlight" | "picker" => {
                                p.expect_ident()?;
                            }
                            "part" => {
                                p.expect_ident()?;
                                while p.try_parse(|p| p.expect_ident().map(|_| ())).is_ok() {}
                            }
                            "slotted" => {
                                p.skip_whitespace();
                                compound(p, false)?;
                                p.skip_whitespace();
                            }
                            "cue" => list(p, false, false)?,
                            "view-transition-group"
                            | "view-transition-image-pair"
                            | "view-transition-old"
                            | "view-transition-new"
                            | "scroll-button" => match p.next()? {
                                Token::Ident(_) | Token::Delim('*') => {}
                                _ => return bad(p),
                            },
                            _ => return bad(p),
                        }
                        p.expect_exhausted().map_err(ParseError::from)
                    })?;
                    Ok(true)
                }
                _ => bad(p),
            }
        }
        Token::Ident(n) => {
            let n = n.to_ascii_lowercase();
            if PSEUDO_CLASSES.contains(&n.as_str()) {
                Ok(false)
            } else if matches!(
                n.as_str(),
                "before" | "after" | "first-letter" | "first-line"
            ) {
                if in_has { bad(p) } else { Ok(true) }
            } else {
                bad(p)
            }
        }
        Token::Function(n) => {
            let n = n.to_ascii_lowercase();
            p.parse_nested_block(|p| {
                match n.as_str() {
                    "is" | "where" | "not" => list(p, false, in_has)?,
                    "-webkit-any" => p
                        .parse_comma_separated(|p| {
                            p.skip_whitespace();
                            compound(p, in_has)?;
                            p.skip_whitespace();
                            Ok(())
                        })
                        .map(|_| ())?,
                    "has" if !in_has => list(p, true, true)?,
                    "nth-child" | "nth-last-child" => {
                        cssparser::parse_nth(p)?;
                        p.skip_whitespace();
                        if !p.is_exhausted() {
                            p.expect_ident_matching("of")?;
                            list(p, false, in_has)?;
                        }
                    }
                    "nth-of-type" | "nth-last-of-type" => {
                        cssparser::parse_nth(p)?;
                    }
                    "dir" | "state" => {
                        p.expect_ident()?;
                    }
                    "lang" => {
                        p.parse_comma_separated(|p| {
                            p.expect_ident_or_string()
                                .map(|_| ())
                                .map_err(ParseError::<()>::from)
                        })?;
                    }
                    "active-view-transition-type" => {
                        p.parse_comma_separated(|p| {
                            p.expect_ident().map(|_| ()).map_err(ParseError::from)
                        })?;
                    }
                    "host" | "host-context" => {
                        p.skip_whitespace();
                        compound(p, in_has)?;
                        p.skip_whitespace();
                    }
                    _ => return bad(p),
                }
                p.expect_exhausted().map_err(ParseError::from)
            })?;
            Ok(false)
        }
        _ => bad(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Chrome 153's answers; the full table is lb-page's `css_supports` test.
    #[test]
    fn declarations_and_conditions() {
        assert!(supports_declaration("container-type", "inline-size"));
        assert!(!supports_declaration("container-type", "foo"));
        assert!(supports_declaration("color", "oklch(70% 0.1 200)"));
        assert!(!supports_declaration("color", "red !important"));
        assert!(supports_declaration("width", "initial !important"));
        assert!(!supports_declaration(
            "-webkit-backdrop-filter",
            "blur(2px)"
        ));
        assert!(supports_declaration("--x", "{a}"));
        assert!(!supports_declaration("--x", ""));
        assert!(!supports_declaration("margin", "1px 2px 3px 4px 5px"));
        assert!(supports_condition("display: grid"));
        assert!(supports_condition("not foo(bar)"));
        assert!(!supports_condition("(color: red) and not (color: foo)"));
        assert!(!supports_condition("(a: b) and (c: d) or (e: f)"));
        assert!(supports_condition("selector(:has(a))"));
        assert!(!supports_condition("selector(:has(:is(:has(a))))"));
        assert!(!supports_condition("selector(a, b)"));
        assert!(supports_condition("font-tech(color-COLRv1)"));
        assert!(supports_condition("at-rule(@container)"));
    }

    #[test]
    fn supports_rules_use_the_same_check() {
        let s = crate::Stylesheet::parse(
            "@supports (container-type: inline-size) { .a { color: red } }\
             @supports (foo: bar) { .b { color: red } }",
            crate::Origin::Author,
        );
        assert_eq!(s.rules.len(), 1);
    }
}

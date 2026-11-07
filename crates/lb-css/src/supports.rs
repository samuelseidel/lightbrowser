//! `CSS.supports()` and `@supports` conditions as in Chrome 153 (CSS
//! Conditional 3/4 and Blink's parser).
//!
//! - Declarations: the property must be one Chrome knows and the value
//!   must match its grammar (`supports.grammar`, matched by
//!   [`crate::supports_value`]); custom properties take any well-formed
//!   value. CSS-wide keywords alone pass; a value with an arbitrary
//!   substitution function (`var()`, `env()`, `attr()`, `if()`) passes when
//!   those functions are well formed, as Chrome defers the rest.
//! - Conditions: `not`, `and`, `or` (not mixed), nesting,
//!   `selector()` (one complex selector; pseudo-classes and elements as
//!   Chrome lists them; no nested `:has()`, no pseudo-elements inside
//!   `:has()`), `font-tech()`, `font-format()`, `at-rule()`; other
//!   functions and blocks (general-enclosed) are false. Text that is not
//!   a condition is tried again in parentheses, as the spec says.

use cssparser::{ParseError, Parser, Token};

use crate::supports_value::{Comp, Cv, components, grammar, matches_property, matches_type};
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
                        .try_parse(|p| p.parse_entirely(|p| complex(p, false, Sx::default())))
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
    // `@page` margin boxes.
    "top-left-corner",
    "top-left",
    "top-center",
    "top-right",
    "top-right-corner",
    "bottom-left-corner",
    "bottom-left",
    "bottom-center",
    "bottom-right",
    "bottom-right-corner",
    "left-top",
    "left-middle",
    "left-bottom",
    "right-top",
    "right-middle",
    "right-bottom",
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

fn declaration(name: &str, raw: &str, allow_important: bool) -> bool {
    // `!important`: allowed in a condition, never in the two-argument form
    // (except after a CSS-wide keyword or with a substitution function).
    let (value, important) = crate::props::split_important(raw);
    let wide = WIDE.iter().any(|w| value.eq_ignore_ascii_case(w));
    let comps = components(value);
    let subst = comps.as_deref().and_then(substitution);
    if important && !allow_important && !wide && subst.is_none() {
        return false;
    }
    if let Some(custom) = name.strip_prefix("--") {
        if custom.is_empty() || (important && !allow_important) {
            return false;
        }
        // A value of only white space is valid, an empty one is not.
        if raw.is_empty() && !allow_important {
            return false;
        }
        return !has_bang(value) && well_formed(&mut Parser::new(value)) && subst != Some(false);
    }
    let lower = name.to_ascii_lowercase();
    if !grammar().has_property(&lower) || has_bang(value) {
        return false;
    }
    // Only white space or comments: valid where the grammar takes an empty
    // value (Chrome 153: `timeline-trigger`), never for `""` in the
    // two-argument form.
    if comps.as_ref().is_some_and(|c| c.is_empty()) {
        return (allow_important || !raw.is_empty()) && matches_property(&lower, &[]);
    }
    if wide {
        return true;
    }
    let Some(comps) = comps else {
        return false;
    };
    if let Some(ok) = subst {
        return ok;
    }
    matches_property(&lower, &comps)
        && (!matches!(
            lower.as_str(),
            "grid-template-areas" | "grid-template" | "grid"
        ) || grid_areas_ok(&comps))
}

/// The strings of `grid-template-areas` (and the `grid` shorthands): rows
/// of the same number of cells, each named area a rectangle.
fn grid_areas_ok(comps: &[Comp]) -> bool {
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    for c in comps {
        let Cv::Str(s) = &c.cv else { continue };
        let mut row = Vec::new();
        let mut chars = s.chars().peekable();
        while let Some(&ch) = chars.peek() {
            if ch.is_whitespace() {
                chars.next();
            } else if ch == '.' {
                while chars.peek() == Some(&'.') {
                    chars.next();
                }
                row.push(None);
            } else if ch.is_alphanumeric() || ch == '-' || ch == '_' || !ch.is_ascii() {
                let mut name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() {
                        name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                row.push(Some(name));
            } else {
                return false;
            }
        }
        if row.is_empty() || rows.first().is_some_and(|r| r.len() != row.len()) {
            return false;
        }
        rows.push(row);
    }
    // Each name: its cells fill their bounding box.
    let mut names: Vec<&str> = rows
        .iter()
        .flatten()
        .flatten()
        .map(|s| s.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    names.iter().all(|n| {
        let cells: Vec<(usize, usize)> = rows
            .iter()
            .enumerate()
            .flat_map(|(r, row)| {
                row.iter()
                    .enumerate()
                    .filter(|(_, c)| c.as_deref() == Some(*n))
                    .map(move |(col, _)| (r, col))
            })
            .collect();
        let (r0, r1) = (
            cells.iter().map(|c| c.0).min(),
            cells.iter().map(|c| c.0).max(),
        );
        let (c0, c1) = (
            cells.iter().map(|c| c.1).min(),
            cells.iter().map(|c| c.1).max(),
        );
        let (Some(r0), Some(r1), Some(c0), Some(c1)) = (r0, r1, c0, c1) else {
            return false;
        };
        cells.len() == (r1 - r0 + 1) * (c1 - c0 + 1)
    })
}

/// Arbitrary substitution functions (`var()`, `env()`, `attr()`, `if()`,
/// custom functions `--name()`): a value with one is valid when all of
/// them are well formed, Chrome checks the rest after substitution.
/// `None` when the value has none.
fn substitution(c: &[Comp]) -> Option<bool> {
    let mut found: Option<bool> = None;
    for x in c {
        let r = match &x.cv {
            Cv::Func(f, args) if is_substitution(f) => Some(substitution_ok(f, args)),
            Cv::Func(_, args) | Cv::Block(_, args) => substitution(args),
            _ => None,
        };
        if let Some(r) = r {
            found = Some(found.unwrap_or(true) && r);
        }
    }
    found
}

fn is_substitution(f: &str) -> bool {
    matches!(f, "var" | "env" | "attr" | "if" | "inherit") || (f.starts_with("--") && f.len() > 2)
}

fn ident_is(c: Option<&Comp>, k: &str) -> bool {
    matches!(c, Some(Comp { cv: Cv::Ident(i), .. }) if i == k)
}

/// Any tokens without a top-level `;` or `!`, nested substitution
/// functions valid (a fallback, a custom function's arguments).
fn any_value(c: &[Comp]) -> bool {
    !c.iter()
        .any(|x| matches!(x.cv, Cv::Semicolon | Cv::Delim('!')))
        && substitution(c) != Some(false)
}

/// Splits at the first top-level comma: before, and the fallback after it.
fn fallback(args: &[Comp]) -> (&[Comp], Option<&[Comp]>) {
    match args.iter().position(|c| matches!(c.cv, Cv::Comma)) {
        Some(i) => (&args[..i], Some(&args[i + 1..])),
        None => (args, None),
    }
}

fn substitution_ok(f: &str, args: &[Comp]) -> bool {
    if f.starts_with("--") {
        return any_value(args);
    }
    if f == "if" {
        return if_ok(args);
    }
    // Chrome 153 rejects `inherit()` everywhere.
    if f == "inherit" {
        return false;
    }
    let (head, fb) = fallback(args);
    if fb.is_some_and(|fb| !any_value(fb)) {
        return false;
    }
    let ident = |c: &Comp| matches!(c.cv, Cv::Ident(_));
    match f {
        "var" => {
            matches!(head, [Comp { cv: Cv::Ident(i), .. }] if i.starts_with("--") && i.len() > 2)
        }
        "env" => {
            head.first().is_some_and(ident)
                && head[1..]
                    .iter()
                    .all(|c| matches!(c.cv, Cv::Num(v, true) if v >= 0.0))
        }
        // `attr( <attr-name> <attr-type>? , <fallback>? )`, no namespaces.
        _ => match head {
            [n] => ident(n),
            [n, t] => {
                ident(n)
                    && match &t.cv {
                        Cv::Ident(_) | Cv::Delim('%') => true,
                        Cv::Func(f, s) if f == "type" => matches_type("syntax", s),
                        _ => false,
                    }
            }
            _ => false,
        },
    }
}

/// `if( [ <if-branch> ; ]* <if-branch> ;? )`, a branch
/// `<if-condition> : <declaration-value>?`.
fn if_ok(args: &[Comp]) -> bool {
    let mut parts: Vec<&[Comp]> = args.split(|c| matches!(c.cv, Cv::Semicolon)).collect();
    if parts.last().is_some_and(|p| p.is_empty()) && parts.len() > 1 {
        parts.pop();
    }
    parts.iter().all(|b| {
        let Some(i) = b.iter().position(|c| matches!(c.cv, Cv::Colon)) else {
            return false;
        };
        let (cond, value) = (&b[..i], &b[i + 1..]);
        let cond_ok = match cond {
            [c] if ident_is(Some(c), "else") => true,
            _ => if_condition(cond),
        };
        cond_ok && any_value(value)
    })
}

/// A boolean expression of `style()`, `media()`, `supports()` tests
/// (other functions and blocks are unknown tests, still valid).
fn if_condition(c: &[Comp]) -> bool {
    let test = |x: &Comp| match &x.cv {
        Cv::Func(..) => true,
        Cv::Block('(', inner) => if_condition(inner) || !inner.is_empty(),
        _ => false,
    };
    match c {
        [] => false,
        [n, rest @ ..] if ident_is(Some(n), "not") => matches!(rest, [t] if test(t)),
        [first, rest @ ..] => {
            if !test(first) || rest.len() % 2 != 0 {
                return false;
            }
            let op = match rest.first() {
                Some(Comp {
                    cv: Cv::Ident(i), ..
                }) => i.as_str(),
                _ => return rest.is_empty(),
            };
            (op == "and" || op == "or")
                && rest
                    .chunks(2)
                    .all(|p| ident_is(p.first(), op) && test(&p[1]))
        }
    }
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

const WIDE: &[&str] = &["inherit", "initial", "unset", "revert", "revert-layer"];

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
    "interest-source",
    "interest-target",
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
    "target-after",
    "target-before",
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

/// Where a selector is parsed: inside `:has()` (no nested `:has()`, no
/// pseudo-elements), inside `:is()` / `:where()` / `:not()` (no
/// pseudo-elements), inside `:host()` (no `:has()`).
#[derive(Clone, Copy, Default)]
struct Sx {
    in_has: bool,
    no_pe: bool,
    no_has: bool,
}

/// One complex selector (`rel`: may start with a combinator, as in
/// `:has()`). Nothing may follow a compound with a pseudo-element.
fn complex(p: &mut Parser<'_>, rel: bool, sx: Sx) -> PResult<()> {
    p.skip_whitespace();
    if rel {
        let _ = p.try_parse(|p| match p.next()? {
            Token::Delim('>' | '+' | '~') => Ok(()),
            _ => bad(p),
        });
        p.skip_whitespace();
    }
    let mut elem = compound(p, sx)?;
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
        if elem.is_some() {
            return bad(p);
        }
        let st = p.state();
        match p.next_including_whitespace()?.clone() {
            Token::Delim('>' | '+' | '~') => {
                p.skip_whitespace();
            }
            _ if space => p.reset(&st),
            _ => return bad(p),
        }
        elem = compound(p, sx)?;
    }
}

/// A selector list (`:is()`, `:not()`, ...): one or more, comma separated.
fn list(p: &mut Parser<'_>, rel: bool, sx: Sx) -> PResult<()> {
    p.parse_comma_separated(|p| complex(p, rel, sx)).map(|_| ())
}

/// A compound selector; the pseudo-element it ends in, if any.
fn compound(p: &mut Parser<'_>, sx: Sx) -> PResult<Option<String>> {
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
    rest_of_compound(p, sx, any)
}

fn rest_of_compound(p: &mut Parser<'_>, sx: Sx, mut any: bool) -> PResult<Option<String>> {
    let mut elem: Option<String> = None;
    loop {
        let st = p.state();
        let t = match p.next_including_whitespace() {
            Ok(t) => t.clone(),
            Err(_) => break,
        };
        match t {
            Token::IDHash(_) if elem.is_none() => {}
            Token::Delim('.') if elem.is_none() => match p.next_including_whitespace()? {
                Token::Ident(_) => {}
                _ => return bad(p),
            },
            Token::Delim('&') if elem.is_none() => {}
            Token::SquareBracketBlock if elem.is_none() => p.parse_nested_block(attribute)?,
            Token::Colon => match pseudo(p, sx)? {
                Simple::Element(e) => {
                    if let Some(prev) = &elem
                        && !element_after_element(&e, prev)
                    {
                        return bad(p);
                    }
                    elem = Some(e);
                }
                Simple::Class(c) => {
                    if let Some(prev) = &elem
                        && !class_after_element(&c, prev)
                    {
                        return bad(p);
                    }
                }
            },
            _ => {
                p.reset(&st);
                break;
            }
        }
        any = true;
    }
    if any { Ok(elem) } else { bad(p) }
}

const SCROLLBAR_ELEMENTS: &[&str] = &[
    "-webkit-resizer",
    "-webkit-scrollbar",
    "-webkit-scrollbar-corner",
    "-webkit-scrollbar-button",
    "-webkit-scrollbar-thumb",
    "-webkit-scrollbar-track",
    "-webkit-scrollbar-track-piece",
];

const SCROLLBAR_CLASSES: &[&str] = &[
    "enabled",
    "disabled",
    "hover",
    "active",
    "horizontal",
    "vertical",
    "decrement",
    "increment",
    "start",
    "end",
    "double-button",
    "single-button",
    "no-button",
    "corner-present",
    "window-inactive",
];

const USER_ACTION: &[&str] = &["hover", "focus", "focus-visible", "focus-within", "active"];

/// Pseudo-classes that depend on the tree, not allowed after `::part()`.
const STRUCTURAL: &[&str] = &[
    "first-child",
    "last-child",
    "only-child",
    "first-of-type",
    "last-of-type",
    "only-of-type",
    "nth-child",
    "nth-last-child",
    "nth-of-type",
    "nth-last-of-type",
    "empty",
    "root",
    "has",
    "host",
    "host-context",
    "scope",
];

/// A pseudo-class after pseudo-element `e` (Blink's
/// `IsPseudoClassValidAfterPseudoElement`).
fn class_after_element(c: &str, e: &str) -> bool {
    if matches!(c, "is" | "where" | "not") {
        return true;
    }
    match e {
        "part" => !STRUCTURAL.contains(&c),
        _ if SCROLLBAR_ELEMENTS.contains(&e) => SCROLLBAR_CLASSES.contains(&c),
        "selection" => c == "window-inactive",
        "file-selector-button" => USER_ACTION.contains(&c),
        _ if e.starts_with("-webkit-") => USER_ACTION.contains(&c),
        "view-transition-group"
        | "view-transition-image-pair"
        | "view-transition-old"
        | "view-transition-new" => c == "only-child",
        "search-text" => c == "current",
        "scroll-marker-group" => matches!(c, "focus-within" | "hover"),
        "scroll-marker" => {
            matches!(c, "target-current" | "target-before" | "target-after")
                || USER_ACTION.contains(&c)
        }
        "scroll-button" => matches!(c, "disabled" | "enabled") || USER_ACTION.contains(&c),
        _ => false,
    }
}

/// A pseudo-element after pseudo-element `e`.
fn element_after_element(s: &str, e: &str) -> bool {
    match e {
        "column" => s == "scroll-marker",
        "before" | "after" => s == "marker",
        "slotted" => matches!(
            s,
            "before"
                | "after"
                | "marker"
                | "placeholder"
                | "file-selector-button"
                | "details-content"
        ),
        "part" => !matches!(s, "part" | "slotted"),
        _ => false,
    }
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
    // Chrome 153 has the `i` flag, not `s`.
    match p.next()? {
        Token::Ident(i) if i.eq_ignore_ascii_case("i") => {}
        _ => return bad(p),
    }
    p.expect_exhausted().map_err(ParseError::from)
}

enum Simple {
    Class(String),
    Element(String),
}

/// After `:`: a pseudo-class or pseudo-element, by name.
fn pseudo(p: &mut Parser<'_>, sx: Sx) -> PResult<Simple> {
    let no_pe = sx.in_has || sx.no_pe;
    match p.next_including_whitespace()?.clone() {
        Token::Colon => {
            if no_pe {
                return bad(p);
            }
            match p.next_including_whitespace()?.clone() {
                Token::Ident(n) => {
                    let n = n.to_ascii_lowercase();
                    if PSEUDO_ELEMENTS.contains(&n.as_str()) {
                        Ok(Simple::Element(n))
                    } else {
                        bad(p)
                    }
                }
                Token::Function(n) => {
                    let n = n.to_ascii_lowercase();
                    p.parse_nested_block(|p| {
                        match n.as_str() {
                            "highlight" => {
                                p.expect_ident()?;
                            }
                            "picker" => {
                                p.expect_ident_matching("select")?;
                            }
                            "part" => {
                                p.expect_ident()?;
                                while p.try_parse(|p| p.expect_ident().map(|_| ())).is_ok() {}
                            }
                            "slotted" => {
                                p.skip_whitespace();
                                compound(p, Sx::default())?;
                                p.skip_whitespace();
                            }
                            "cue" => list(p, false, Sx::default())?,
                            "view-transition-group"
                            | "view-transition-image-pair"
                            | "view-transition-old"
                            | "view-transition-new" => view_transition_arg(p)?,
                            "scroll-button" => match p.next()? {
                                Token::Delim('*') => {}
                                Token::Ident(i)
                                    if matches!(
                                        i.to_ascii_lowercase().as_str(),
                                        "up" | "down"
                                            | "left"
                                            | "right"
                                            | "block-start"
                                            | "block-end"
                                            | "inline-start"
                                            | "inline-end"
                                    ) => {}
                                _ => return bad(p),
                            },
                            _ => return bad(p),
                        }
                        p.expect_exhausted().map_err(ParseError::from)
                    })?;
                    Ok(Simple::Element(n))
                }
                _ => bad(p),
            }
        }
        Token::Ident(n) => {
            let n = n.to_ascii_lowercase();
            if PSEUDO_CLASSES.contains(&n.as_str()) {
                Ok(Simple::Class(n))
            } else if matches!(
                n.as_str(),
                "before" | "after" | "first-letter" | "first-line"
            ) {
                if no_pe {
                    bad(p)
                } else {
                    Ok(Simple::Element(n))
                }
            } else {
                bad(p)
            }
        }
        Token::Function(n) => {
            let n = n.to_ascii_lowercase();
            if n == "has" && (sx.in_has || sx.no_has) {
                return bad(p);
            }
            p.parse_nested_block(|p| {
                match n.as_str() {
                    "is" | "where" | "not" => list(p, false, Sx { no_pe: true, ..sx })?,
                    "-webkit-any" => p
                        .parse_comma_separated(|p| {
                            p.skip_whitespace();
                            compound(p, Sx { no_pe: true, ..sx })?;
                            p.skip_whitespace();
                            Ok(())
                        })
                        .map(|_| ())?,
                    "has" => list(p, true, Sx { in_has: true, ..sx })?,
                    "nth-child" | "nth-last-child" => {
                        cssparser::parse_nth(p)?;
                        p.skip_whitespace();
                        if !p.is_exhausted() {
                            p.expect_ident_matching("of")?;
                            list(p, false, Sx { no_pe: true, ..sx })?;
                        }
                    }
                    "nth-of-type" | "nth-last-of-type" => {
                        cssparser::parse_nth(p)?;
                    }
                    "dir" | "state" | "lang" => {
                        p.expect_ident()?;
                    }
                    "active-view-transition-type" => {
                        p.parse_comma_separated(|p| {
                            p.expect_ident().map(|_| ()).map_err(ParseError::from)
                        })?;
                    }
                    "host" | "host-context" => {
                        p.skip_whitespace();
                        compound(
                            p,
                            Sx {
                                no_has: true,
                                no_pe: true,
                                ..sx
                            },
                        )?;
                        p.skip_whitespace();
                    }
                    _ => return bad(p),
                }
                p.expect_exhausted().map_err(ParseError::from)
            })?;
            Ok(Simple::Class(n))
        }
        _ => bad(p),
    }
}

/// `::view-transition-group()` and the others: `*`, a name, or a name
/// and classes (`a.b`, `.b`, `*.b`).
fn view_transition_arg(p: &mut Parser<'_>) -> PResult<()> {
    let mut any = false;
    let st = p.state();
    match p.next()? {
        Token::Ident(_) | Token::Delim('*') => any = true,
        _ => p.reset(&st),
    }
    loop {
        let st = p.state();
        match p.next_including_whitespace() {
            Ok(Token::Delim('.')) => match p.next_including_whitespace()? {
                Token::Ident(_) => any = true,
                _ => return bad(p),
            },
            _ => {
                p.reset(&st);
                break;
            }
        }
    }
    p.skip_whitespace();
    if any { Ok(()) } else { bad(p) }
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

    // Chrome 153's answers, recorded on loopback pages.
    #[test]
    fn grammar_details() {
        let t = |p, v| supports_declaration(p, v);
        // Typed arithmetic, whitespace around `+`, percentages in mixes.
        assert!(t("width", "calc(1px / 1px * 1px)"));
        assert!(!t("width", "calc(1px+1px)"));
        assert!(!t("width", "calc(1px + 1)"));
        assert!(t("width", "min(1px, 2%)"));
        assert!(!t("width", "round(1.5px)"));
        assert!(t("z-index", "calc(1.5)"));
        assert!(!t("z-index", "1e1"));
        assert!(t("width", "calc-size(auto, size * 2 + 1px)"));
        assert!(!t("max-width", "calc-size(auto, size)"));
        // Shorthands need their required parts.
        assert!(!t("font", "bold"));
        assert!(!t("text-shadow", "red"));
        assert!(t("font", "italic bold 12px/1.5 Arial, sans-serif"));
        // Substitution functions defer the check, even with `!important`.
        assert!(t("width", "var(--x) !important"));
        assert!(!t("width", "1px !important"));
        assert!(!t("width", "var(x)"));
        assert!(t("width", "attr(x type(<length>), 1px)"));
        assert!(!t("width", "if(foo: 1px)"));
        assert!(t("width", "--foo(1px, 2px)"));
        assert!(t("--x", " "));
        assert!(!t("--x", "var(--y, var(z))"));
        // Strings Chrome checks.
        assert!(t("d", "path(\"M0 0 A1 1 0 112 2\")"));
        assert!(!t("d", "path(\"M0 0 L\")"));
        assert!(!t("grid-template-areas", "\"a b\" \"b a\""));
        assert!(t("grid-template-areas", "\"a a\" \"b b\""));
        assert!(!t("font-feature-settings", "\"lig\""));
        assert!(t("font-family", "initial a"));
        assert!(!t("font-family", "serif a"));
        // Colors.
        assert!(t("color", "rgb(from red calc(r + 1) g b / alpha)"));
        assert!(!t("color", "rgb(1, none, 3)"));
        assert!(!t("color", "color-mix(in srgb, red 0%, blue 0%)"));
        assert!(!t("color", "device-cmyk(0 0 0 1)"));
        // Blink's parser quirks: commas in `border`, `first` dropped.
        assert!(t("border-inline", "groove, 1q red"));
        assert!(t("align-content", "first end"));
        assert!(t("timeline-trigger", " ") && !t("timeline-trigger", ""));
        assert!(supports_condition("(timeline-trigger:)"));
        assert!(t("transition-timing-function", "linear(1 2 50%, 1)"));
        assert!(!t("transition-timing-function", "linear(1 2, 1)"));
        assert!(t("width", "calc-size(1px, size size)"));
        assert!(!t("width", "calc-size(1px, size + 1s)"));
        assert!(!t("align-content", "last baseline"));
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

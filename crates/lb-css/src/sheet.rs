//! Stylesheet parsing: style rules, `@media`, `@supports`, `@layer`,
//! `@import` (recorded, not fetched). Other at-rules are skipped.

use std::rc::Rc;

use cssparser::{CowRcStr, ParseError, Parser, ParserState, Token};
use lb_dom::SelectorList;

use crate::media::{MediaEnv, MediaQueryList};
use crate::props::{DeclarationBlock, parse_property};
use crate::values::{PResult, bad};

/// Where a stylesheet comes from, for cascade order.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    UserAgent,
    Author,
}

/// One style rule, flattened out of any `@media` / `@supports` blocks.
#[derive(Clone, Debug)]
pub struct StyleRule {
    pub selectors: SelectorList,
    pub block: Rc<DeclarationBlock>,
    /// Enclosing `@media` lists; all must match.
    pub media: Rc<[Rc<MediaQueryList>]>,
}

impl StyleRule {
    /// True if every enclosing `@media` matches `env`.
    pub fn media_matches(&self, env: &MediaEnv) -> bool {
        self.media.iter().all(|m| m.matches(env))
    }
}

/// An `@import` rule. Imports are recorded but not fetched yet.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportRule {
    pub url: String,
    pub media: MediaQueryList,
}

/// A parsed stylesheet.
#[derive(Clone, Debug)]
pub struct Stylesheet {
    pub origin: Origin,
    pub rules: Vec<StyleRule>,
    pub imports: Vec<ImportRule>,
}

impl Stylesheet {
    /// Parses a stylesheet. Invalid rules are skipped, as browsers do.
    pub fn parse(css: &str, origin: Origin) -> Stylesheet {
        let mut sheet = Stylesheet {
            origin,
            rules: Vec::new(),
            imports: Vec::new(),
        };
        let mut p = Parser::new(css);
        let mut rp = RuleParser {
            sheet: &mut sheet,
            media: Vec::new(),
            top_level: true,
        };
        let iter = cssparser::StyleSheetParser::new(&mut p, &mut rp);
        for _ in iter {}
        sheet
    }
}

impl Stylesheet {
    /// Makes relative `url()` images absolute against `base` (the sheet's
    /// URL, for linked and imported sheets).
    pub fn resolve_urls(&mut self, base: &url::Url) {
        use crate::bg::Image;
        use crate::props::{Decl, Sv};
        for rule in &mut self.rules {
            let has = rule
                .block
                .decls
                .iter()
                .any(|d| matches!(d.value, Decl::Value(Sv::Images(_))));
            if !has {
                continue;
            }
            let block = Rc::make_mut(&mut rule.block);
            for d in &mut block.decls {
                if let Decl::Value(Sv::Images(list)) = &mut d.value {
                    let fixed: Vec<_> = list
                        .iter()
                        .map(|i| match i {
                            Image::Url(u) => match base.join(u.trim()) {
                                Ok(abs) => Image::Url(Rc::from(abs.as_str())),
                                Err(_) => i.clone(),
                            },
                            other => other.clone(),
                        })
                        .collect();
                    *list = fixed.into();
                }
            }
        }
    }

    /// True if a rule sets a `url()` background image.
    pub fn has_url_images(&self) -> bool {
        use crate::props::{Decl, Sv};
        self.rules.iter().any(|r| {
            r.block.decls.iter().any(|d| match &d.value {
                Decl::Value(Sv::Images(l)) => {
                    l.iter().any(|i| matches!(i, crate::bg::Image::Url(_)))
                }
                Decl::Pending(p) => p.css.to_ascii_lowercase().contains("url("),
                _ => false,
            })
        })
    }
}

struct RuleParser<'a> {
    sheet: &'a mut Stylesheet,
    media: Vec<Rc<MediaQueryList>>,
    top_level: bool,
}

enum AtPrelude {
    Media(MediaQueryList),
    Supports(bool),
    /// `@layer`, `@container`, `@scope`, `@starting-style`: contents apply.
    Transparent,
    Import(String, MediaQueryList),
    /// Rules we skip (`@font-face`, `@keyframes`, `@page`, ...).
    Skip,
}

impl<'i> cssparser::AtRuleParser<'i> for RuleParser<'_> {
    type Prelude = AtPrelude;
    type AtRule = ();
    type Error = ();

    fn parse_prelude(&mut self, name: CowRcStr<'i>, input: &mut Parser<'i>) -> PResult<AtPrelude> {
        let name = name.to_ascii_lowercase();
        Ok(match name.as_str() {
            "media" => AtPrelude::Media(MediaQueryList::parse_from(input)),
            "supports" => AtPrelude::Supports(supports_condition(input).unwrap_or(false)),
            "layer" | "container" | "scope" | "starting-style" | "document" | "-moz-document" => {
                while input.next().is_ok() {}
                AtPrelude::Transparent
            }
            "import" if self.top_level => {
                let url = match input.next()?.clone() {
                    Token::UnquotedUrl(u) | Token::QuotedString(u) => u.to_string(),
                    Token::Function(f) if f.eq_ignore_ascii_case("url") => input
                        .parse_nested_block(|p| {
                            p.expect_string()
                                .map(|s| s.to_string())
                                .map_err(ParseError::<()>::from)
                        })?,
                    _ => return bad(input),
                };
                // Skip `layer(...)` / `supports(...)`.
                let _ = input.try_parse(|p| {
                    let t = p.next()?.clone();
                    match t {
                        Token::Function(_) => p.parse_nested_block(|p| {
                            while p.next().is_ok() {}
                            Ok::<_, ParseError<()>>(())
                        }),
                        Token::Ident(i) if i.eq_ignore_ascii_case("layer") => Ok(()),
                        _ => bad(p),
                    }
                });
                AtPrelude::Import(url, MediaQueryList::parse_from(input))
            }
            _ => {
                while input.next().is_ok() {}
                AtPrelude::Skip
            }
        })
    }

    fn rule_without_block(&mut self, prelude: AtPrelude, _start: &ParserState) -> Result<(), ()> {
        if let AtPrelude::Import(url, media) = prelude {
            self.sheet.imports.push(ImportRule { url, media });
        }
        Ok(())
    }

    fn parse_block(
        &mut self,
        prelude: AtPrelude,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> PResult<()> {
        let pushed = match prelude {
            AtPrelude::Media(m) => {
                self.media.push(Rc::new(m));
                true
            }
            AtPrelude::Supports(true) | AtPrelude::Transparent => false,
            AtPrelude::Supports(false) | AtPrelude::Skip | AtPrelude::Import(..) => {
                while input.next().is_ok() {}
                return Ok(());
            }
        };
        let was_top = std::mem::replace(&mut self.top_level, false);
        {
            let iter = cssparser::StyleSheetParser::new(input, self);
            for _ in iter {}
        }
        self.top_level = was_top;
        if pushed {
            self.media.pop();
        }
        Ok(())
    }
}

impl<'i> cssparser::QualifiedRuleParser<'i> for RuleParser<'_> {
    type Prelude = SelectorList;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> PResult<SelectorList> {
        let start = input.position();
        while input.next().is_ok() {}
        let text = input.slice_from(start);
        SelectorList::parse(text.trim()).map_err(|_| ParseError::custom(()))
    }

    fn parse_block(
        &mut self,
        selectors: SelectorList,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> PResult<()> {
        let block = DeclarationBlock::parse_from(input);
        if !block.decls.is_empty() || !block.customs.is_empty() {
            self.sheet.rules.push(StyleRule {
                selectors,
                block: Rc::new(block),
                media: self.media.clone().into(),
            });
        }
        Ok(())
    }
}

/// Evaluates an `@supports` condition: declarations are supported if we
/// parse them; `selector()` is assumed supported.
fn supports_condition<'i>(p: &mut Parser<'i>) -> PResult<bool> {
    if p.try_parse(|p| p.expect_ident_matching("not")).is_ok() {
        return Ok(!supports_in_parens(p)?);
    }
    let mut result = supports_in_parens(p)?;
    while let Ok(op) = p.try_parse(|p| p.expect_ident().map(|i| i.to_ascii_lowercase())) {
        let rhs = supports_in_parens(p)?;
        match op.as_str() {
            "and" => result = result && rhs,
            "or" => result = result || rhs,
            _ => return bad(p),
        }
    }
    Ok(result)
}

fn supports_in_parens<'i>(p: &mut Parser<'i>) -> PResult<bool> {
    match p.next()?.clone() {
        Token::ParenthesisBlock => p.parse_nested_block(|p| {
            if let Ok(r) = p.try_parse(supports_condition) {
                return Ok(r);
            }
            let name = p.expect_ident()?.to_ascii_lowercase();
            p.expect_colon()?;
            let start = p.position();
            while p.next().is_ok() {}
            let value = p.slice_from(start).trim();
            let name = crate::props::normalize_name(&name);
            Ok(name.starts_with("--") || parse_property(&name, value).is_some())
        }),
        Token::Function(f) => {
            let f = f.to_ascii_lowercase();
            p.parse_nested_block(|p| {
                while p.next().is_ok() {}
                Ok::<_, ParseError<()>>(())
            })?;
            Ok(f == "selector")
        }
        _ => bad(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rules_media_supports_imports() {
        let css = r#"
            @import url("a.css") screen;
            @charset "utf-8";
            p, div > span { color: red }
            @media (min-width: 100px) { .a { display: none } @media print { .b { color: blue } } }
            @supports (display: grid) { .g { display: grid } }
            @supports (display: nonsense) { .n { display: block } }
            @font-face { font-family: X; src: url(x.woff) }
            @keyframes k { from { opacity: 0 } }
            @layer base { .l { width: 1px } }
            bad! { color: red }
            .after { color: green }
        "#;
        let s = Stylesheet::parse(css, Origin::Author);
        assert_eq!(s.imports.len(), 1);
        assert_eq!(s.imports[0].url, "a.css");
        let n = s.rules.len();
        assert_eq!(n, 6, "{:?}", s.rules);
        assert_eq!(s.rules[1].media.len(), 1);
        assert_eq!(s.rules[2].media.len(), 2);
        let env = MediaEnv::default();
        assert!(s.rules[1].media_matches(&env));
        assert!(!s.rules[2].media_matches(&env));
    }
}

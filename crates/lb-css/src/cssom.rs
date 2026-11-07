//! Rule lists for the CSSOM (`CSSStyleSheet.cssRules`): the top-level
//! rules of a sheet as Chrome keeps them, with their source text. The
//! cascade does not use this; it is parsed only when script asks.

use cssparser::{CowRcStr, ParseError, Parser, ParserState, Token};
use lb_dom::SelectorList;

use crate::values::{PResult, bad};

/// The kind of a CSSOM rule (`CSSRule` subclass).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RuleKind {
    Style,
    Import,
    Media,
    Supports,
    FontFace,
    Keyframes,
    Page,
    LayerBlock,
    LayerStatement,
    Container,
    Property,
    CounterStyle,
    Scope,
    StartingStyle,
    Namespace,
    FontFeatureValues,
}

impl RuleKind {
    /// `CSSRule.type` (0 for rule kinds newer than the constants).
    pub fn type_num(self) -> u16 {
        match self {
            RuleKind::Style => 1,
            RuleKind::Import => 3,
            RuleKind::Media => 4,
            RuleKind::FontFace => 5,
            RuleKind::Page => 6,
            RuleKind::Keyframes => 7,
            RuleKind::Namespace => 10,
            RuleKind::CounterStyle => 11,
            RuleKind::Supports => 12,
            RuleKind::FontFeatureValues => 14,
            _ => 0,
        }
    }

    /// The interface name of the rule object.
    pub fn iface(self) -> &'static str {
        match self {
            RuleKind::Style => "CSSStyleRule",
            RuleKind::Import => "CSSImportRule",
            RuleKind::Media => "CSSMediaRule",
            RuleKind::Supports => "CSSSupportsRule",
            RuleKind::FontFace => "CSSFontFaceRule",
            RuleKind::Keyframes => "CSSKeyframesRule",
            RuleKind::Page => "CSSPageRule",
            RuleKind::LayerBlock => "CSSLayerBlockRule",
            RuleKind::LayerStatement => "CSSLayerStatementRule",
            RuleKind::Container => "CSSContainerRule",
            RuleKind::Property => "CSSPropertyRule",
            RuleKind::CounterStyle => "CSSCounterStyleRule",
            RuleKind::Scope => "CSSScopeRule",
            RuleKind::StartingStyle => "CSSStartingStyleRule",
            RuleKind::Namespace => "CSSNamespaceRule",
            RuleKind::FontFeatureValues => "CSSFontFeatureValuesRule",
        }
    }

    /// Rules holding a nested rule list (`CSSGroupingRule`).
    pub fn is_grouping(self) -> bool {
        matches!(
            self,
            RuleKind::Media
                | RuleKind::Supports
                | RuleKind::LayerBlock
                | RuleKind::Container
                | RuleKind::Scope
                | RuleKind::StartingStyle
        )
    }

    /// Rules whose block is a declaration list.
    pub fn has_declarations(self) -> bool {
        matches!(
            self,
            RuleKind::Style
                | RuleKind::FontFace
                | RuleKind::Page
                | RuleKind::Property
                | RuleKind::CounterStyle
        )
    }
}

/// One rule of a rule list.
#[derive(Clone, Debug, PartialEq)]
pub struct RawRule {
    pub kind: RuleKind,
    /// Selector text, media / condition text, or name, whitespace
    /// collapsed.
    pub prelude: String,
    /// The block's contents (declarations or nested rules); `None` for
    /// statements (`@import`, `@namespace`, `@layer a, b;`).
    pub body: Option<String>,
    /// `@import` URL.
    pub url: Option<String>,
}

/// Collapse runs of whitespace into one space and trim.
fn collapse(s: &str) -> String {
    s.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

/// The rules of a sheet (or of a grouping rule's block) as the CSSOM lists
/// them. Invalid style rules and unknown at-rules are dropped; `@charset`
/// is dropped too, as Chrome does.
pub fn split_rules(css: &str) -> Vec<RawRule> {
    let mut p = Parser::new(css);
    let mut rp = Splitter { out: Vec::new() };
    let iter = cssparser::StyleSheetParser::new(&mut p, &mut rp);
    for _ in iter {}
    rp.out
}

/// Parse exactly one rule (`insertRule`). `None` on a syntax error or more
/// than one rule.
pub fn parse_one_rule(css: &str) -> Option<RawRule> {
    let mut rules = split_rules(css);
    // Text after the rule other than whitespace is a syntax error.
    if rules.len() != 1 {
        return None;
    }
    rules.pop()
}

/// Serialize a rule back to CSS text (also its `cssText`, approximately
/// Chrome's form).
pub fn rule_text(r: &RawRule, decls: Option<&str>) -> String {
    match r.kind {
        RuleKind::Import => {
            let url = r.url.as_deref().unwrap_or("");
            let mut s = String::from("@import url(\"");
            for c in url.chars() {
                if c == '"' || c == '\\' {
                    s.push('\\');
                }
                s.push(c);
            }
            s.push_str("\")");
            if !r.prelude.is_empty() {
                s.push(' ');
                s.push_str(&r.prelude);
            }
            s.push(';');
            s
        }
        RuleKind::Namespace => format!("@namespace {};", r.prelude),
        RuleKind::LayerStatement => format!("@layer {};", r.prelude),
        _ => {
            let head = match r.kind {
                RuleKind::Style => r.prelude.clone(),
                RuleKind::Media => format!("@media {}", r.prelude),
                RuleKind::Supports => format!("@supports {}", r.prelude),
                RuleKind::FontFace => "@font-face".into(),
                RuleKind::Keyframes => format!("@keyframes {}", r.prelude),
                RuleKind::Page if r.prelude.is_empty() => "@page".into(),
                RuleKind::Page => format!("@page {}", r.prelude),
                RuleKind::LayerBlock if r.prelude.is_empty() => "@layer".into(),
                RuleKind::LayerBlock => format!("@layer {}", r.prelude),
                RuleKind::Container => format!("@container {}", r.prelude),
                RuleKind::Property => format!("@property {}", r.prelude),
                RuleKind::CounterStyle => format!("@counter-style {}", r.prelude),
                RuleKind::Scope if r.prelude.is_empty() => "@scope".into(),
                RuleKind::Scope => format!("@scope {}", r.prelude),
                RuleKind::StartingStyle => "@starting-style".into(),
                RuleKind::FontFeatureValues => format!("@font-feature-values {}", r.prelude),
                _ => String::new(),
            };
            let body = r.body.as_deref().unwrap_or("");
            if r.kind.has_declarations() {
                let d = decls.unwrap_or(body).trim();
                if d.is_empty() {
                    format!("{head} {{ }}")
                } else {
                    format!("{head} {{ {d} }}")
                }
            } else if r.kind.is_grouping() {
                let inner: Vec<String> = split_rules(body)
                    .iter()
                    .map(|x| format!("  {}", rule_text(x, None)))
                    .collect();
                if inner.is_empty() {
                    format!("{head} {{\n}}")
                } else {
                    format!("{head} {{\n{}\n}}", inner.join("\n"))
                }
            } else {
                format!("{head} {{ {} }}", collapse(body))
            }
        }
    }
}

struct Splitter {
    out: Vec<RawRule>,
}

enum Prelude {
    Kind(RuleKind, String, Option<String>),
    Skip,
}

impl<'i> cssparser::AtRuleParser<'i> for Splitter {
    type Prelude = Prelude;
    type AtRule = ();
    type Error = ();

    fn parse_prelude(&mut self, name: CowRcStr<'i>, input: &mut Parser<'i>) -> PResult<Prelude> {
        let name = name.to_ascii_lowercase();
        let start = input.position();
        if name == "import" {
            let url = match input.next()?.clone() {
                Token::UnquotedUrl(u) | Token::QuotedString(u) => u.to_string(),
                Token::Function(f) if f.eq_ignore_ascii_case("url") => {
                    input.parse_nested_block(|p| {
                        p.expect_string()
                            .map(|s| s.to_string())
                            .map_err(ParseError::<()>::from)
                    })?
                }
                _ => return bad(input),
            };
            let rest = input.position();
            while input.next().is_ok() {}
            let media = collapse(input.slice_from(rest));
            return Ok(Prelude::Kind(RuleKind::Import, media, Some(url)));
        }
        while input.next().is_ok() {}
        let text = collapse(input.slice_from(start));
        let kind = match name.as_str() {
            "media" => RuleKind::Media,
            "supports" => RuleKind::Supports,
            "font-face" => RuleKind::FontFace,
            "keyframes" | "-webkit-keyframes" => RuleKind::Keyframes,
            "page" => RuleKind::Page,
            "layer" => RuleKind::LayerBlock,
            "container" => RuleKind::Container,
            "property" => RuleKind::Property,
            "counter-style" => RuleKind::CounterStyle,
            "scope" => RuleKind::Scope,
            "starting-style" => RuleKind::StartingStyle,
            "namespace" => RuleKind::Namespace,
            "font-feature-values" => RuleKind::FontFeatureValues,
            _ => return Ok(Prelude::Skip),
        };
        Ok(Prelude::Kind(kind, text, None))
    }

    fn rule_without_block(&mut self, prelude: Prelude, _start: &ParserState) -> Result<(), ()> {
        if let Prelude::Kind(kind, text, url) = prelude {
            let kind = match kind {
                RuleKind::LayerBlock if !text.is_empty() => RuleKind::LayerStatement,
                RuleKind::Import | RuleKind::Namespace => kind,
                _ => return Err(()),
            };
            self.out.push(RawRule {
                kind,
                prelude: text,
                body: None,
                url,
            });
        }
        Ok(())
    }

    fn parse_block(
        &mut self,
        prelude: Prelude,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> PResult<()> {
        let start = input.position();
        while input.next().is_ok() {}
        let body = input.slice_from(start).trim().to_owned();
        if let Prelude::Kind(kind, text, _) = prelude {
            if matches!(kind, RuleKind::Import | RuleKind::Namespace) {
                return Ok(());
            }
            self.out.push(RawRule {
                kind,
                prelude: text,
                body: Some(body),
                url: None,
            });
        }
        Ok(())
    }
}

impl<'i> cssparser::QualifiedRuleParser<'i> for Splitter {
    type Prelude = String;
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> PResult<String> {
        let start = input.position();
        while input.next().is_ok() {}
        let text = input.slice_from(start).trim();
        if SelectorList::parse(text).is_err() {
            return bad(input);
        }
        Ok(collapse(text))
    }

    fn parse_block(
        &mut self,
        prelude: String,
        _start: &ParserState,
        input: &mut Parser<'i>,
    ) -> PResult<()> {
        let start = input.position();
        while input.next().is_ok() {}
        let body = input.slice_from(start).trim().to_owned();
        self.out.push(RawRule {
            kind: RuleKind::Style,
            prelude,
            body: Some(body),
            url: None,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_top_level_rules() {
        let css = r#"@charset "utf-8"; @import url(a.css) screen; @import "b.css";
            #t { color: rgb(1, 1, 1) }  bad!! { x: y }
            @media (min-width: 1px) { p { color: red } .a { } }
            @font-face { font-family: X; src: url(x.woff) }
            @keyframes k { from { opacity: 0 } }
            @unknown foo { }
            @layer a, b;
            @layer c { div { color: blue } }"#;
        let r = split_rules(css);
        let kinds: Vec<RuleKind> = r.iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            [
                RuleKind::Import,
                RuleKind::Import,
                RuleKind::Style,
                RuleKind::Media,
                RuleKind::FontFace,
                RuleKind::Keyframes,
                RuleKind::LayerStatement,
                RuleKind::LayerBlock
            ]
        );
        assert_eq!(r[0].url.as_deref(), Some("a.css"));
        assert_eq!(r[0].prelude, "screen");
        assert_eq!(r[2].prelude, "#t");
        assert_eq!(r[2].body.as_deref(), Some("color: rgb(1, 1, 1)"));
        assert_eq!(split_rules(r[3].body.as_deref().unwrap()).len(), 2);
        assert_eq!(
            rule_text(&r[2], Some("color: rgb(1, 1, 1);")),
            "#t { color: rgb(1, 1, 1); }"
        );
        assert_eq!(rule_text(&r[0], None), "@import url(\"a.css\") screen;");
        assert!(parse_one_rule("p { }").is_some());
        assert!(parse_one_rule("p { } q { }").is_none());
        assert!(parse_one_rule("p {").is_some());
        assert!(parse_one_rule("!!").is_none());
    }
}

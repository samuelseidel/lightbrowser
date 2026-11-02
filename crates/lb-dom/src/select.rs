//! CSS selector matching over the arena with Servo's `selectors` crate:
//! `querySelector(All)`, `matches` and `closest`.
//!
//! Dynamic state (`:hover`, `:focus`, `:target`, form checkedness) is read
//! from [`NodeFlags`] that the page layer maintains. Pseudo-elements parse
//! but never match, as in `querySelector`.

use std::fmt;

use cssparser::{ToCss, match_ignore_ascii_case, serialize_identifier};
use html5ever::{LocalName, Namespace, local_name};
use precomputed_hash::PrecomputedHash;
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::context::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};
use selectors::matching::{ElementSelectorFlags, matches_selector_list};
use selectors::parser::{ParseRelative, SelectorParseErrorKind};
use selectors::{Element, OpaqueElement};

use crate::document::{Document, DomError};
use crate::node::{NodeFlags, NodeId, NodeType};

/// Selector implementation marker for `selectors`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LbSelectorImpl;

/// Attribute selector value.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CssString(pub String);

impl From<&str> for CssString {
    fn from(s: &str) -> Self {
        CssString(s.to_owned())
    }
}

impl AsRef<str> for CssString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl ToCss for CssString {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        cssparser::serialize_string(&self.0, dest)
    }
}

/// Interned identifier (local names, ids, classes, prefixes).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CssAtom(pub LocalName);

impl From<&str> for CssAtom {
    fn from(s: &str) -> Self {
        CssAtom(LocalName::from(s))
    }
}

impl ToCss for CssAtom {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        serialize_identifier(&self.0, dest)
    }
}

impl PrecomputedHash for CssAtom {
    fn precomputed_hash(&self) -> u32 {
        self.0.precomputed_hash()
    }
}

macro_rules! pseudo_classes {
    ($($variant:ident => $name:literal,)*) => {
        /// Non-functional pseudo-classes we parse (those the `selectors`
        /// crate does not handle itself).
        #[derive(Copy, Clone, Debug, PartialEq, Eq)]
        pub enum NamedPseudoClass { $($variant,)* }

        impl NamedPseudoClass {
            fn from_name(name: &str) -> Option<Self> {
                $(if name.eq_ignore_ascii_case($name) { return Some(Self::$variant); })*
                None
            }

            fn name(self) -> &'static str {
                match self { $(Self::$variant => $name,)* }
            }
        }
    };
}

pseudo_classes! {
    Active => "active",
    AnyLink => "any-link",
    WebkitAnyLink => "-webkit-any-link",
    Autofill => "autofill",
    WebkitAutofill => "-webkit-autofill",
    Checked => "checked",
    Default => "default",
    Defined => "defined",
    Disabled => "disabled",
    Enabled => "enabled",
    Focus => "focus",
    FocusVisible => "focus-visible",
    FocusWithin => "focus-within",
    Fullscreen => "fullscreen",
    WebkitFullScreen => "-webkit-full-screen",
    Future => "future",
    Hover => "hover",
    InRange => "in-range",
    Indeterminate => "indeterminate",
    Invalid => "invalid",
    Link => "link",
    Modal => "modal",
    Open => "open",
    Optional => "optional",
    OutOfRange => "out-of-range",
    Past => "past",
    Paused => "paused",
    PictureInPicture => "picture-in-picture",
    PlaceholderShown => "placeholder-shown",
    Playing => "playing",
    PopoverOpen => "popover-open",
    ReadOnly => "read-only",
    ReadWrite => "read-write",
    Required => "required",
    Target => "target",
    UserInvalid => "user-invalid",
    UserValid => "user-valid",
    Valid => "valid",
    Visited => "visited",
}

/// Pseudo-classes beyond the ones `selectors` implements itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PseudoClass {
    Named(NamedPseudoClass),
    Lang(Box<str>),
    Dir(Box<str>),
}

impl ToCss for PseudoClass {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        match self {
            PseudoClass::Named(n) => {
                dest.write_char(':')?;
                dest.write_str(n.name())
            }
            PseudoClass::Lang(l) => {
                dest.write_str(":lang(")?;
                serialize_identifier(l, dest)?;
                dest.write_char(')')
            }
            PseudoClass::Dir(d) => {
                dest.write_str(":dir(")?;
                serialize_identifier(d, dest)?;
                dest.write_char(')')
            }
        }
    }
}

impl selectors::parser::NonTSPseudoClass for PseudoClass {
    fn is_active_or_hover(&self) -> bool {
        matches!(
            self,
            PseudoClass::Named(NamedPseudoClass::Active | NamedPseudoClass::Hover)
        )
    }

    fn is_user_action_state(&self) -> bool {
        matches!(
            self,
            PseudoClass::Named(
                NamedPseudoClass::Active
                    | NamedPseudoClass::Hover
                    | NamedPseudoClass::Focus
                    | NamedPseudoClass::FocusVisible
                    | NamedPseudoClass::FocusWithin
            )
        )
    }
}

/// Pseudo-elements: parsed, never matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PseudoElement {
    Named(Box<str>),
    Highlight(Box<str>),
}

const PSEUDO_ELEMENTS: &[&str] = &[
    "after",
    "backdrop",
    "before",
    "cue",
    "details-content",
    "file-selector-button",
    "first-letter",
    "first-line",
    "grammar-error",
    "marker",
    "placeholder",
    "scroll-marker",
    "scroll-marker-group",
    "search-text",
    "selection",
    "spelling-error",
    "target-text",
    "view-transition",
];

impl ToCss for PseudoElement {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        match self {
            PseudoElement::Named(n) => {
                dest.write_str("::")?;
                dest.write_str(n)
            }
            PseudoElement::Highlight(n) => {
                dest.write_str("::highlight(")?;
                serialize_identifier(n, dest)?;
                dest.write_char(')')
            }
        }
    }
}

impl selectors::parser::PseudoElement for PseudoElement {
    fn accepts_state_pseudo_classes(&self) -> bool {
        true
    }

    fn valid_after_slotted(&self) -> bool {
        true
    }

    fn is_before_or_after(&self) -> bool {
        matches!(self, PseudoElement::Named(n) if &**n == "before" || &**n == "after")
    }
}

impl selectors::parser::SelectorImpl for LbSelectorImpl {
    type ExtraMatchingData<'a> = ();
    type AttrValue = CssString;
    type Identifier = CssAtom;
    type LocalName = CssAtom;
    type NamespaceUrl = Namespace;
    type NamespacePrefix = CssAtom;
    type BorrowedNamespaceUrl = Namespace;
    type BorrowedLocalName = CssAtom;
    type NonTSPseudoClass = PseudoClass;
    type PseudoElement = PseudoElement;
}

struct SelectorParser;

type ParseError = cssparser::ParseError<SelectorParseErrorKind>;

fn unsupported<T>() -> Result<T, ParseError> {
    Err(cssparser::ParseError::custom(
        SelectorParseErrorKind::UnsupportedPseudoClassOrElement,
    ))
}

impl<'i> selectors::parser::Parser<'i> for SelectorParser {
    type Impl = LbSelectorImpl;
    type Error = SelectorParseErrorKind;

    fn parse_slotted(&self) -> bool {
        true
    }

    fn parse_part(&self) -> bool {
        true
    }

    fn parse_nth_child_of(&self) -> bool {
        true
    }

    fn parse_is_and_where(&self) -> bool {
        true
    }

    fn parse_has(&self) -> bool {
        true
    }

    fn parse_parent_selector(&self) -> bool {
        true
    }

    fn parse_host(&self) -> bool {
        true
    }

    fn is_is_alias(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case("-webkit-any")
    }

    fn parse_non_ts_pseudo_class(&self, name: &str) -> Result<PseudoClass, ParseError> {
        match NamedPseudoClass::from_name(name) {
            Some(n) => Ok(PseudoClass::Named(n)),
            None => unsupported(),
        }
    }

    fn parse_non_ts_functional_pseudo_class(
        &self,
        name: &str,
        parser: &mut cssparser::Parser<'i>,
        _after_part: bool,
    ) -> Result<PseudoClass, ParseError> {
        match_ignore_ascii_case! { name,
            "lang" => {
                let v = parser.expect_ident_or_string()?.as_ref().to_ascii_lowercase();
                Ok(PseudoClass::Lang(v.into()))
            },
            "dir" => {
                let v = parser.expect_ident()?.as_ref().to_ascii_lowercase();
                Ok(PseudoClass::Dir(v.into()))
            },
            _ => unsupported(),
        }
    }

    fn parse_pseudo_element(&self, name: &str) -> Result<PseudoElement, ParseError> {
        let lower = name.to_ascii_lowercase();
        // Blink accepts any `-webkit-` pseudo-element without complaint.
        if PSEUDO_ELEMENTS.contains(&lower.as_str()) || lower.starts_with("-webkit-") {
            Ok(PseudoElement::Named(lower.into()))
        } else {
            unsupported()
        }
    }

    fn parse_functional_pseudo_element(
        &self,
        name: &str,
        parser: &mut cssparser::Parser<'i>,
    ) -> Result<PseudoElement, ParseError> {
        if name.eq_ignore_ascii_case("highlight") {
            let v = parser.expect_ident()?.as_ref().to_owned();
            return Ok(PseudoElement::Highlight(v.into()));
        }
        unsupported()
    }
}

/// A parsed, reusable selector list.
#[derive(Clone, Debug)]
pub struct SelectorList(selectors::SelectorList<LbSelectorImpl>);

impl SelectorList {
    /// Parse a selector list; errors map to `SyntaxError`.
    pub fn parse(text: &str) -> Result<Self, DomError> {
        let mut parser = cssparser::Parser::new(text);
        let list = selectors::SelectorList::parse(&SelectorParser, &mut parser, ParseRelative::No)
            .map_err(|e| {
                DomError::Syntax(format!("'{text}' is not a valid selector: {:?}", e.kind))
            })?;
        parser
            .expect_exhausted()
            .map_err(|_| DomError::Syntax(format!("'{text}' is not a valid selector")))?;
        Ok(SelectorList(list))
    }
}

impl ToCss for SelectorList {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        self.0.to_css(dest)
    }
}

/// An element borrowed from a document, as `selectors` sees it.
#[derive(Copy, Clone)]
pub struct ElementRef<'a> {
    doc: &'a Document,
    id: NodeId,
}

impl fmt::Debug for ElementRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ElementRef({:?} {:?})",
            self.id,
            self.doc.local_name(self.id)
        )
    }
}

impl<'a> ElementRef<'a> {
    fn new(doc: &'a Document, id: NodeId) -> Self {
        debug_assert!(doc.is_element(id));
        ElementRef { doc, id }
    }

    fn wrap(self, id: Option<NodeId>) -> Option<Self> {
        id.map(|id| ElementRef::new(self.doc, id))
    }

    fn data(&self) -> &'a crate::ElementData {
        self.doc.element(self.id).expect("element")
    }

    fn is_html_named(&self, name: &LocalName) -> bool {
        self.doc.is_html_element_named(self.id, name)
    }

    fn attr(&self, local: &str) -> Option<&'a str> {
        self.data().attr(local)
    }

    fn has_flag(&self, f: NodeFlags) -> bool {
        self.doc.flags(self.id).contains(f)
    }

    fn input_type(&self) -> Option<String> {
        if !self.is_html_named(&local_name!("input")) {
            return None;
        }
        Some(
            self.attr("type")
                .map(|t| t.trim().to_ascii_lowercase())
                .unwrap_or_else(|| "text".into()),
        )
    }

    /// HTML "actually disabled" for form controls, plus option/optgroup.
    fn is_disabled(&self) -> bool {
        let d = self.data();
        if !d.is_html() {
            return false;
        }
        let local = &d.name.local;
        let own = d.attr("disabled").is_some();
        match *local {
            local_name!("option") => {
                own || self.doc.parent_element(self.id).is_some_and(|p| {
                    ElementRef::new(self.doc, p).is_html_named(&local_name!("optgroup"))
                        && ElementRef::new(self.doc, p).attr("disabled").is_some()
                })
            }
            local_name!("optgroup") => own,
            local_name!("button")
            | local_name!("input")
            | local_name!("select")
            | local_name!("textarea")
            | local_name!("fieldset") => own || self.in_disabled_fieldset(),
            _ => false,
        }
    }

    /// Descendant of a disabled `<fieldset>` and not in its first `<legend>`.
    fn in_disabled_fieldset(&self) -> bool {
        let mut child = self.id;
        for anc in self.doc.ancestors(self.id) {
            let a = ElementRef::new_checked(self.doc, anc);
            if let Some(a) = a
                && a.is_html_named(&local_name!("fieldset"))
                && a.attr("disabled").is_some()
            {
                let first_legend = self
                    .doc
                    .element_children(anc)
                    .find(|&c| self.doc.is_html_element_named(c, &local_name!("legend")));
                if first_legend != Some(child) {
                    return true;
                }
            }
            child = anc;
        }
        false
    }

    fn new_checked(doc: &'a Document, id: NodeId) -> Option<Self> {
        doc.is_element(id).then(|| ElementRef::new(doc, id))
    }

    fn is_form_control(&self) -> bool {
        let d = self.data();
        d.is_html()
            && matches!(
                d.name.local,
                local_name!("button")
                    | local_name!("input")
                    | local_name!("select")
                    | local_name!("textarea")
                    | local_name!("optgroup")
                    | local_name!("option")
                    | local_name!("fieldset")
            )
    }

    fn is_checked(&self) -> bool {
        if self.has_flag(NodeFlags::CHECKED_DIRTY) {
            return self.has_flag(NodeFlags::CHECKED);
        }
        match self.input_type().as_deref() {
            Some("checkbox" | "radio") => self.attr("checked").is_some(),
            Some(_) => false,
            None => self.is_html_named(&local_name!("option")) && self.attr("selected").is_some(),
        }
    }

    fn is_read_write(&self) -> bool {
        if let Some(t) = self.input_type() {
            let editable = !matches!(
                t.as_str(),
                "hidden"
                    | "checkbox"
                    | "radio"
                    | "file"
                    | "submit"
                    | "reset"
                    | "button"
                    | "image"
                    | "color"
                    | "range"
            );
            return editable && self.attr("readonly").is_none() && !self.is_disabled();
        }
        if self.is_html_named(&local_name!("textarea")) {
            return self.attr("readonly").is_none() && !self.is_disabled();
        }
        // contenteditable, inherited.
        std::iter::once(self.id)
            .chain(self.doc.ancestors(self.id))
            .filter_map(|n| self.doc.element(n))
            .find_map(|e| e.attr("contenteditable"))
            .is_some_and(|v| {
                let v = v.to_ascii_lowercase();
                v.is_empty() || v == "true" || v == "plaintext-only"
            })
    }

    /// Value is empty for placeholder / required checks.
    fn value_is_empty(&self) -> bool {
        if self.is_html_named(&local_name!("textarea")) {
            return self.doc.child_text_content(self.id).is_empty();
        }
        self.attr("value").is_none_or(str::is_empty)
    }

    fn lang_matches(&self, range: &str) -> bool {
        let xml_lang = Namespace::from("http://www.w3.org/XML/1998/namespace");
        let lang = std::iter::once(self.id)
            .chain(self.doc.ancestors(self.id))
            .filter(|&n| self.doc.is_element(n))
            .find_map(|n| {
                self.doc
                    .get_attribute_ns(n, &xml_lang, "lang")
                    .or_else(|| self.doc.element(n).and_then(|e| e.attr("lang")))
            });
        let Some(lang) = lang else {
            return false;
        };
        let lang = lang.to_ascii_lowercase();
        range == "*" && !lang.is_empty()
            || lang == range
            || lang.strip_prefix(range).is_some_and(|r| r.starts_with('-'))
    }

    fn dir_matches(&self, dir: &str) -> bool {
        let found = std::iter::once(self.id)
            .chain(self.doc.ancestors(self.id))
            .filter_map(|n| self.doc.element(n))
            .find_map(|e| {
                e.attr("dir")
                    .map(str::to_ascii_lowercase)
                    .filter(|d| d == "ltr" || d == "rtl")
            });
        found.as_deref().unwrap_or("ltr") == dir
    }
}

impl<'a> Element for ElementRef<'a> {
    type Impl = LbSelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.doc.node(self.id))
    }

    fn parent_element(&self) -> Option<Self> {
        self.wrap(self.doc.parent_element(self.id))
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.wrap(self.doc.prev_element_sibling(self.id))
    }

    fn next_sibling_element(&self) -> Option<Self> {
        self.wrap(self.doc.next_element_sibling(self.id))
    }

    fn first_element_child(&self) -> Option<Self> {
        self.wrap(self.doc.first_element_child(self.id))
    }

    fn is_html_element_in_html_document(&self) -> bool {
        self.doc.is_html() && self.data().is_html()
    }

    fn has_local_name(&self, name: &CssAtom) -> bool {
        self.data().name.local == name.0
    }

    fn has_namespace(&self, ns: &Namespace) -> bool {
        self.data().name.ns == *ns
    }

    fn is_same_type(&self, other: &Self) -> bool {
        let (a, b) = (&self.data().name, &other.data().name);
        a.local == b.local && a.ns == b.ns
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&Namespace>,
        local_name: &CssAtom,
        operation: &AttrSelectorOperation<&CssString>,
    ) -> bool {
        self.data().attrs.iter().any(|a| {
            let ns_ok = match ns {
                NamespaceConstraint::Any => true,
                NamespaceConstraint::Specific(url) => a.name.ns == **url,
            };
            ns_ok && a.name.local == local_name.0 && operation.eval_str(&a.value)
        })
    }

    fn match_non_ts_pseudo_class(
        &self,
        pc: &PseudoClass,
        _context: &mut MatchingContext<'_, LbSelectorImpl>,
    ) -> bool {
        use NamedPseudoClass as P;
        let n = match pc {
            PseudoClass::Lang(l) => return self.lang_matches(l),
            PseudoClass::Dir(d) => return self.dir_matches(d),
            PseudoClass::Named(n) => *n,
        };
        match n {
            P::Link | P::AnyLink | P::WebkitAnyLink => self.is_link(),
            P::Visited => false,
            P::Hover => self.has_flag(NodeFlags::HOVER),
            P::Active => self.has_flag(NodeFlags::ACTIVE),
            P::Focus => self.has_flag(NodeFlags::FOCUS),
            P::FocusVisible => self.has_flag(NodeFlags::FOCUS_VISIBLE),
            P::FocusWithin => self.has_flag(NodeFlags::FOCUS_WITHIN),
            P::Target => self.has_flag(NodeFlags::TARGET),
            P::Checked => self.is_checked(),
            P::Default => match self.input_type().as_deref() {
                Some("checkbox" | "radio") => self.attr("checked").is_some(),
                Some(_) => false,
                None => {
                    self.is_html_named(&local_name!("option")) && self.attr("selected").is_some()
                }
            },
            P::Indeterminate => {
                self.is_html_named(&local_name!("progress")) && self.attr("value").is_none()
            }
            P::Disabled => self.is_form_control() && self.is_disabled(),
            P::Enabled => self.is_form_control() && !self.is_disabled(),
            P::Required | P::Optional => {
                let d = self.data();
                let applies = d.is_html()
                    && matches!(
                        d.name.local,
                        local_name!("input") | local_name!("select") | local_name!("textarea")
                    );
                applies && (d.attr("required").is_some() == (n == P::Required))
            }
            P::ReadWrite => self.is_read_write(),
            P::ReadOnly => !self.is_read_write(),
            P::PlaceholderShown => {
                (self.input_type().is_some() || self.is_html_named(&local_name!("textarea")))
                    && self.attr("placeholder").is_some()
                    && self.value_is_empty()
            }
            P::Valid | P::Invalid => {
                let candidate =
                    self.input_type().is_some() || self.is_html_named(&local_name!("textarea"));
                if !candidate {
                    return false;
                }
                let invalid = self.attr("required").is_some() && self.value_is_empty();
                invalid == (n == P::Invalid)
            }
            P::Defined => {
                let d = self.data();
                !(d.is_html() && d.name.local.contains('-'))
            }
            P::Open => {
                (self.is_html_named(&local_name!("details"))
                    || self.is_html_named(&local_name!("dialog")))
                    && self.attr("open").is_some()
            }
            P::Paused => {
                self.is_html_named(&local_name!("audio"))
                    || self.is_html_named(&local_name!("video"))
            }
            P::Autofill
            | P::WebkitAutofill
            | P::Fullscreen
            | P::WebkitFullScreen
            | P::Future
            | P::Past
            | P::InRange
            | P::OutOfRange
            | P::Modal
            | P::PictureInPicture
            | P::Playing
            | P::PopoverOpen
            | P::UserInvalid
            | P::UserValid => false,
        }
    }

    fn match_pseudo_element(
        &self,
        _pe: &PseudoElement,
        _context: &mut MatchingContext<'_, LbSelectorImpl>,
    ) -> bool {
        false
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        (self.is_html_named(&local_name!("a")) || self.is_html_named(&local_name!("area")))
            && self.attr("href").is_some()
    }

    fn is_html_slot_element(&self) -> bool {
        self.is_html_named(&local_name!("slot"))
    }

    fn has_id(&self, id: &CssAtom, case_sensitivity: CaseSensitivity) -> bool {
        self.attr("id")
            .is_some_and(|v| case_sensitivity.eq(v.as_bytes(), id.0.as_bytes()))
    }

    fn has_class(&self, name: &CssAtom, case_sensitivity: CaseSensitivity) -> bool {
        self.doc
            .classes(self.id)
            .any(|c| case_sensitivity.eq(c.as_bytes(), name.0.as_bytes()))
    }

    fn has_custom_state(&self, _name: &CssAtom) -> bool {
        false
    }

    fn imported_part(&self, _name: &CssAtom) -> Option<CssAtom> {
        None
    }

    fn is_part(&self, _name: &CssAtom) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.doc
            .children(self.id)
            .all(|c| match self.doc.node_type(c) {
                NodeType::Element => false,
                NodeType::Text => self.doc.character_data(c).is_some_and(str::is_empty),
                _ => true,
            })
    }

    fn is_root(&self) -> bool {
        self.doc
            .parent(self.id)
            .is_some_and(|p| self.doc.node_type(p) == NodeType::Document)
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        false
    }
}

impl Document {
    fn selector_quirks(&self) -> QuirksMode {
        match self.quirks_mode() {
            html5ever::tree_builder::QuirksMode::Quirks => QuirksMode::Quirks,
            html5ever::tree_builder::QuirksMode::LimitedQuirks => QuirksMode::LimitedQuirks,
            html5ever::tree_builder::QuirksMode::NoQuirks => QuirksMode::NoQuirks,
        }
    }

    /// Run `f` with a matching context scoped to `scope` (for `:scope`).
    fn with_context<R>(
        &self,
        scope: Option<NodeId>,
        f: impl FnOnce(&mut MatchingContext<'_, LbSelectorImpl>) -> R,
    ) -> R {
        let mut caches = SelectorCaches::default();
        let mut ctx = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut caches,
            self.selector_quirks(),
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        ctx.scope_element = scope
            .filter(|&s| self.is_element(s))
            .map(|s| ElementRef::new(self, s).opaque());
        f(&mut ctx)
    }

    /// True if element `id` matches `list` (`:scope` = `scope`).
    pub fn matches_list(&self, id: NodeId, list: &SelectorList, scope: Option<NodeId>) -> bool {
        if !self.is_element(id) {
            return false;
        }
        self.with_context(scope, |ctx| {
            matches_selector_list(&list.0, ElementRef::new(self, id), ctx)
        })
    }

    /// First matching descendant of `scope` in tree order.
    pub fn select_first(&self, scope: NodeId, list: &SelectorList) -> Option<NodeId> {
        self.with_context(Some(scope), |ctx| {
            self.descendants(scope).find(|&n| {
                self.is_element(n) && matches_selector_list(&list.0, ElementRef::new(self, n), ctx)
            })
        })
    }

    /// All matching descendants of `scope` in tree order.
    pub fn select_all(&self, scope: NodeId, list: &SelectorList) -> Vec<NodeId> {
        self.with_context(Some(scope), |ctx| {
            self.descendants(scope)
                .filter(|&n| {
                    self.is_element(n)
                        && matches_selector_list(&list.0, ElementRef::new(self, n), ctx)
                })
                .collect()
        })
    }

    /// `querySelector`.
    pub fn query_selector(
        &self,
        scope: NodeId,
        selectors: &str,
    ) -> Result<Option<NodeId>, DomError> {
        Ok(self.select_first(scope, &SelectorList::parse(selectors)?))
    }

    /// `querySelectorAll` (a static list).
    pub fn query_selector_all(
        &self,
        scope: NodeId,
        selectors: &str,
    ) -> Result<Vec<NodeId>, DomError> {
        Ok(self.select_all(scope, &SelectorList::parse(selectors)?))
    }

    /// `Element.matches`.
    pub fn matches(&self, id: NodeId, selectors: &str) -> Result<bool, DomError> {
        let list = SelectorList::parse(selectors)?;
        Ok(self.matches_list(id, &list, Some(id)))
    }

    /// `Element.closest`.
    pub fn closest(&self, id: NodeId, selectors: &str) -> Result<Option<NodeId>, DomError> {
        let list = SelectorList::parse(selectors)?;
        Ok(std::iter::once(id)
            .chain(self.ancestors(id))
            .filter(|&n| self.is_element(n))
            .find(|&n| self.matches_list(n, &list, Some(id))))
    }
}

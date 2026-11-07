//! Content extraction from a loaded document (ADR 0042): Markdown, links
//! and a JSON summary, behind `lightbrowser fetch --dump markdown | links
//! | json`.
//!
//! Neutral tooling: it reads the post-script DOM (the flat tree, so shadow
//! content and slotted nodes count) and, in the chrome build, the computed
//! style of the page's own layout engine ([`Page::extract`]). It never
//! changes the document and sends no request.
//!
//! The converter is engine-neutral: it takes a style probe
//! ([`NodeStyle`] per node) so the firefox build, which has no style
//! engine in `lb-page`, uses [`attribute_probe`] (the `hidden` attribute
//! and inline `style` only).
//!
//! Rules, in short (the golden files in `tests/extract/` pin them):
//!
//! - Skipped: `script style template noscript head iframe object embed
//!   canvas video audio select textarea input` and friends, non-HTML
//!   elements (SVG, MathML), `hidden` and `display: none` subtrees, text
//!   of `visibility: hidden` elements, closed `dialog`s.
//! - Whitespace collapses as rendering collapses it (NBSP stays);
//!   `pre` keeps its text. Text is escaped for Markdown; there are no
//!   trailing spaces; blocks are separated by one blank line; the output
//!   ends with one newline (an empty page gives an empty string).
//! - `<br>` is a backslash hard break, two in a row end the paragraph.
//! - A link that holds block content stays one inline link (its blocks
//!   are joined with spaces).

use std::collections::HashSet;

use lb_dom::{Document, NodeId, NodeType, ns};
use url::Url;

/// Marks a hard line break in an inline buffer (becomes `\` + newline).
const BR: char = '\u{1}';
/// Marks a paragraph break in an inline buffer (two `<br>` in a row).
const PARA: char = '\u{2}';

/// A heading of the page, in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub text: String,
}

/// An anchor with a usable `href`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Absolute URL, resolved against the document base.
    pub url: String,
    /// Whitespace-normalised text.
    pub text: String,
    /// The `rel` attribute, whitespace-normalised (empty when absent).
    pub rel: String,
}

/// Everything extracted from one document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
    pub title: String,
    /// `lang` of the root element.
    pub lang: Option<String>,
    /// `<meta name="description">`.
    pub description: Option<String>,
    /// `<link rel="canonical">`, absolute.
    pub canonical: Option<String>,
    pub headings: Vec<Heading>,
    pub links: Vec<Link>,
    pub markdown: String,
    /// The `--dump text` string.
    pub text: String,
}

/// What the converter needs to know about a node's computed style.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeStyle {
    pub display_none: bool,
    /// `visibility` other than `visible`.
    pub visibility_hidden: bool,
}

/// A style probe made from the `style` attribute only: `display: none` on
/// the node, `visibility` from the nearest ancestor that sets it. For
/// builds without a style engine and for tests.
pub fn attribute_probe(doc: &Document) -> impl FnMut(NodeId) -> NodeStyle + '_ {
    move |n| {
        let mut st = NodeStyle::default();
        if let Some(v) = inline_decl(doc.get_attribute(n, "style"), "display") {
            st.display_none = v == "none";
        }
        for a in std::iter::once(n).chain(doc.ancestors(n)) {
            if let Some(v) = inline_decl(doc.get_attribute(a, "style"), "visibility") {
                st.visibility_hidden = matches!(v.as_str(), "hidden" | "collapse");
                break;
            }
        }
        st
    }
}

/// The last value of `name` in an inline style, lowercased.
fn inline_decl(style: Option<&str>, name: &str) -> Option<String> {
    let mut found = None;
    for d in style?.split(';') {
        if let Some((k, v)) = d.split_once(':')
            && k.trim().eq_ignore_ascii_case(name)
        {
            let v = v.trim().to_ascii_lowercase();
            found = Some(v.trim_end_matches("!important").trim().to_owned());
        }
    }
    found
}

/// Extract from `doc`, whose URL is `doc_url`, with attribute-only style.
pub fn extract_document(doc: &Document, doc_url: &Url, text: String) -> Extracted {
    extract(doc, doc_url, text, attribute_probe(doc))
}

/// Extract from `doc`. `text` is the `--dump text` string (kept in the
/// result for the JSON); `probe` answers style questions per node.
pub fn extract<P: FnMut(NodeId) -> NodeStyle>(
    doc: &Document,
    doc_url: &Url,
    text: String,
    probe: P,
) -> Extracted {
    let base = document_base(doc, doc_url);
    let mut ex = Ex {
        doc,
        base: &base,
        probe,
        headings: Vec::new(),
        h1: false,
        depth: 0,
    };
    let title = doc.title();
    let markdown = ex.markdown(&title);
    let links = ex.links();
    let headings = std::mem::take(&mut ex.headings);
    Extracted {
        title,
        lang: doc
            .document_element()
            .and_then(|r| doc.get_attribute(r, "lang"))
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty()),
        description: meta_description(doc),
        canonical: canonical(doc, &base),
        headings,
        links,
        markdown,
        text,
    }
}

/// The document base URL: the first `<base href>` that resolves against
/// the document URL, else the document URL.
pub fn document_base(doc: &Document, doc_url: &Url) -> Url {
    for n in doc.descendants(Document::DOCUMENT) {
        if doc.is_html_element_named(n, &lb_dom::local_name!("base"))
            && let Some(h) = doc.get_attribute(n, "href")
            && let Ok(u) = doc_url.join(h.trim())
        {
            return u;
        }
    }
    doc_url.clone()
}

fn meta_description(doc: &Document) -> Option<String> {
    doc.descendants(Document::DOCUMENT).find_map(|n| {
        if !doc.is_html_element_named(n, &lb_dom::local_name!("meta")) {
            return None;
        }
        let name = doc.get_attribute(n, "name")?;
        if !name.trim().eq_ignore_ascii_case("description") {
            return None;
        }
        doc.get_attribute(n, "content").map(norm_ws)
    })
}

fn canonical(doc: &Document, base: &Url) -> Option<String> {
    doc.descendants(Document::DOCUMENT).find_map(|n| {
        if !doc.is_html_element_named(n, &lb_dom::local_name!("link")) {
            return None;
        }
        let rel = doc.get_attribute(n, "rel")?;
        if !rel
            .split(is_html_ws)
            .any(|t| t.eq_ignore_ascii_case("canonical"))
        {
            return None;
        }
        resolve(base, doc.get_attribute(n, "href")?).map(|u| u.to_string())
    })
}

/// The `links` dump: `URL<TAB>text` per line.
pub fn links_dump(e: &Extracted) -> String {
    let mut o = String::new();
    for l in &e.links {
        o.push_str(&l.url);
        o.push('\t');
        o.push_str(&l.text);
        o.push('\n');
    }
    o
}

/// The `json` dump: one object, fixed key order, two-space indent, ending
/// with a newline. `url` is the requested URL, `final_url` the URL after
/// redirects.
pub fn json_dump(e: &Extracted, url: &str, final_url: &str, status: u16) -> String {
    fn s(v: &str) -> String {
        serde_json::to_string(v).unwrap_or_else(|_| "\"\"".into())
    }
    fn opt(v: &Option<String>) -> String {
        v.as_deref().map_or_else(|| "null".into(), s)
    }
    let mut o = String::from("{\n");
    o.push_str(&format!("  \"url\": {},\n", s(url)));
    o.push_str(&format!("  \"final_url\": {},\n", s(final_url)));
    o.push_str(&format!("  \"status\": {status},\n"));
    o.push_str(&format!("  \"title\": {},\n", s(&e.title)));
    o.push_str(&format!("  \"lang\": {},\n", opt(&e.lang)));
    o.push_str(&format!("  \"description\": {},\n", opt(&e.description)));
    o.push_str(&format!("  \"canonical\": {},\n", opt(&e.canonical)));
    if e.headings.is_empty() {
        o.push_str("  \"headings\": [],\n");
    } else {
        let items: Vec<String> = e
            .headings
            .iter()
            .map(|h| {
                format!(
                    "    {{\n      \"level\": {},\n      \"text\": {}\n    }}",
                    h.level,
                    s(&h.text)
                )
            })
            .collect();
        o.push_str(&format!("  \"headings\": [\n{}\n  ],\n", items.join(",\n")));
    }
    if e.links.is_empty() {
        o.push_str("  \"links\": [],\n");
    } else {
        let items: Vec<String> = e
            .links
            .iter()
            .map(|l| {
                format!(
                    "    {{\n      \"url\": {},\n      \"text\": {},\n      \"rel\": {}\n    }}",
                    s(&l.url),
                    s(&l.text),
                    s(&l.rel)
                )
            })
            .collect();
        o.push_str(&format!("  \"links\": [\n{}\n  ],\n", items.join(",\n")));
    }
    o.push_str(&format!("  \"text\": {},\n", s(&e.text)));
    o.push_str(&format!("  \"markdown\": {}\n}}\n", s(&e.markdown)));
    o
}

#[cfg(feature = "chrome")]
impl crate::Page {
    /// Extract Markdown, links and metadata from the page's document, with
    /// the page's own computed style deciding what is hidden.
    pub async fn extract(&self) -> Result<Extracted, crate::PageError> {
        self.run(|h| {
            let url = h.window.url();
            let text = h.window.text();
            h.window.with_layout_engine(|eng, doc| {
                extract(doc, &url, text, |n| match eng.computed_style(doc, n) {
                    Some(s) => NodeStyle {
                        display_none: s.display == lb_css::Display::None,
                        visibility_hidden: s.visibility != lb_css::Visibility::Visible,
                    },
                    None => NodeStyle::default(),
                })
            })
        })
        .await
    }
}

// ----- helpers -----

/// HTML whitespace (not NBSP and other Unicode spaces).
fn is_html_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0c' | '\r')
}

fn norm_ws(s: &str) -> String {
    s.split(is_html_ws)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve `href` against `base`; `javascript:` and unparsable values give
/// `None`.
fn resolve(base: &Url, href: &str) -> Option<Url> {
    let u = base.join(href.trim_matches(is_html_ws)).ok()?;
    (u.scheme() != "javascript").then_some(u)
}

/// Elements that start and end a line (flow content). Everything else is
/// inline.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "menu"
            | "dir"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "table"
            | "caption"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "pre"
            | "blockquote"
            | "hr"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "main"
            | "search"
            | "form"
            | "fieldset"
            | "legend"
            | "figure"
            | "figcaption"
            | "address"
            | "details"
            | "summary"
            | "center"
            | "hgroup"
            | "body"
            | "html"
    )
}

/// Elements whose content is never part of the extracted page.
fn is_skipped_name(name: &str) -> bool {
    matches!(
        name,
        "script"
            | "style"
            | "template"
            | "noscript"
            | "head"
            | "iframe"
            | "frame"
            | "frameset"
            | "noframes"
            | "object"
            | "embed"
            | "canvas"
            | "video"
            | "audio"
            | "select"
            | "datalist"
            | "textarea"
            | "input"
            | "option"
            | "optgroup"
            | "meta"
            | "link"
            | "base"
            | "title"
            | "param"
            | "source"
            | "track"
            | "map"
            | "area"
    )
}

/// Collapsing, escaping text buffer (one paragraph's worth).
struct Inline {
    s: String,
    /// Escape block markers at the start of a line (off in table cells and
    /// headings).
    ls: bool,
    /// A buffer inside a wrapper (`**`, link text): a leading space is
    /// kept, [`emit`] moves it outside the wrapper.
    edge: bool,
}

impl Default for Inline {
    fn default() -> Self {
        Inline {
            s: String::new(),
            ls: true,
            edge: false,
        }
    }
}

impl Inline {
    /// A one-line buffer (headings, table cells): no line-start escapes.
    fn line() -> Inline {
        Inline {
            ls: false,
            ..Inline::default()
        }
    }

    fn sub(&self) -> Inline {
        Inline {
            s: String::new(),
            ls: self.ls,
            edge: true,
        }
    }

    fn at_start(&self) -> bool {
        matches!(self.s.chars().last(), None | Some(BR) | Some(PARA))
    }

    /// One collapsible space; none at the start, after a space or a break.
    fn space(&mut self) {
        match self.s.chars().last() {
            Some(' ') | Some(BR) | Some(PARA) => {}
            None if !self.edge => {}
            _ => self.s.push(' '),
        }
    }

    /// A hard break, or a paragraph break when one just happened.
    fn brk(&mut self, para: bool) {
        while self.s.ends_with(' ') {
            self.s.pop();
        }
        match self.s.chars().last() {
            None | Some(PARA) => {}
            Some(BR) => {
                self.s.pop();
                self.s.push(PARA);
            }
            _ => self.s.push(if para { PARA } else { BR }),
        }
    }

    /// Already escaped text, kept as it is.
    fn raw(&mut self, t: &str) {
        self.s.push_str(t);
    }

    /// Text of a text node: whitespace collapses, specials are escaped.
    fn text(&mut self, t: &str) {
        for (i, w) in t.split(is_html_ws).enumerate() {
            if i > 0 {
                self.space();
            }
            if w.is_empty() {
                continue;
            }
            let ls = self.ls && self.at_start();
            self.s.push_str(&escape_word(w, ls));
        }
    }

    /// Add a buffer made elsewhere (spaces and breaks merge with ours).
    fn append(&mut self, other: &str) {
        for ch in other.chars() {
            match ch {
                ' ' => self.space(),
                BR => self.brk(false),
                PARA => self.brk(true),
                c => self.s.push(c),
            }
        }
    }
}

/// Backslash-escape the Markdown specials of one whitespace-free word.
/// `ls`: the word starts a line, so block markers count.
fn escape_word(w: &str, ls: bool) -> String {
    let chars: Vec<char> = w.chars().filter(|&c| c != BR && c != PARA).collect();
    let n = chars.len();
    let all = |c: char| chars.iter().all(|&x| x == c);
    let mut o = String::with_capacity(w.len() + 4);
    for (i, &ch) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        let first = ls && i == 0;
        let esc = match ch {
            '\\' | '`' | '*' | '[' | ']' | '<' => true,
            '_' => {
                !(prev.is_some_and(char::is_alphanumeric)
                    && next.is_some_and(char::is_alphanumeric))
            }
            '~' => prev == Some('~') || next == Some('~'),
            '&' => entity_like(&chars[i + 1..]),
            '>' => first,
            '#' => first && n <= 6 && all('#'),
            '-' => first && (n == 1 || all('-')),
            '+' => first && n == 1,
            '=' => first && all('='),
            '.' | ')' => {
                ls && i == n - 1
                    && (2..=10).contains(&n)
                    && chars[..i].iter().all(char::is_ascii_digit)
            }
            _ => false,
        };
        if esc {
            o.push('\\');
        }
        o.push(ch);
    }
    o
}

/// `&name;`, `&#12;` and `&#x1f;` would be read as character references.
fn entity_like(rest: &[char]) -> bool {
    let mut j = usize::from(rest.first() == Some(&'#'));
    let start = j;
    while j < rest.len() && rest[j].is_ascii_alphanumeric() {
        j += 1;
    }
    j > start && rest.get(j) == Some(&';')
}

/// Escape plain text that is not at the start of a line (image alt).
fn escape_plain(s: &str) -> String {
    s.split(' ')
        .map(|w| escape_word(w, false))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A link destination: `\`, `(` and `)` escaped.
fn escape_dest(u: &str) -> String {
    let mut o = String::with_capacity(u.len());
    for c in u.chars() {
        if matches!(c, '\\' | '(' | ')') {
            o.push('\\');
        }
        o.push(c);
    }
    o
}

/// Breaks become spaces, spaces collapse, edges are trimmed (headings and
/// table cells are one line).
fn flatten(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        let c = if c == BR || c == PARA { ' ' } else { c };
        if c == ' ' {
            if !o.is_empty() && !o.ends_with(' ') {
                o.push(' ');
            }
        } else {
            o.push(c);
        }
    }
    while o.ends_with(' ') {
        o.pop();
    }
    o
}

/// Unescaped `|` becomes `\|` (table cells).
fn escape_pipes(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => {
                o.push(c);
                if let Some(n) = it.next() {
                    o.push(n);
                }
            }
            '|' => o.push_str("\\|"),
            _ => o.push(c),
        }
    }
    o
}

/// Put `inner` into `inl`: its edge spaces and breaks go outside, `f` wraps
/// the rest (nothing when the rest is empty).
fn emit(inl: &mut Inline, inner: &str, f: impl FnOnce(&str) -> String) {
    let edge = |c: char| c == ' ' || c == BR || c == PARA;
    let core = inner.trim_matches(edge);
    if core.is_empty() {
        inl.append(inner);
        return;
    }
    let lead = &inner[..inner.len() - inner.trim_start_matches(edge).len()];
    let trail = &inner[inner.trim_end_matches(edge).len()..];
    inl.append(lead);
    inl.raw(&f(core));
    inl.append(trail);
}

fn code_span(content: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in content.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat(longest + 1);
    let pad = if content.starts_with('`') || content.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{pad}{content}{pad}{fence}")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    List,
    Other,
}

struct Block {
    kind: Kind,
    text: String,
}

fn other(text: String) -> Block {
    Block {
        kind: Kind::Other,
        text,
    }
}

/// Deepest element nesting the converter follows.
const MAX_DEPTH: usize = 400;

struct Ex<'a, P: FnMut(NodeId) -> NodeStyle> {
    doc: &'a Document,
    base: &'a Url,
    probe: P,
    headings: Vec<Heading>,
    h1: bool,
    depth: usize,
}

impl<'a, P: FnMut(NodeId) -> NodeStyle> Ex<'a, P> {
    /// Local name of an HTML element.
    fn html_name(&self, n: NodeId) -> Option<&'a str> {
        let doc: &'a Document = self.doc;
        let q = doc.qual_name(n)?;
        (q.ns == ns!(html)).then(|| &*q.local)
    }

    fn style(&mut self, n: NodeId) -> NodeStyle {
        (self.probe)(n)
    }

    /// Not rendered: skipped by name, hidden, closed dialog or `display:
    /// none`.
    fn skipped(&mut self, n: NodeId, name: &str) -> bool {
        is_skipped_name(name)
            || self.doc.has_attribute(n, "hidden")
            || (name == "dialog" && !self.doc.has_attribute(n, "open"))
            || self.style(n).display_none
    }

    fn text_visible(&mut self, parent: NodeId) -> bool {
        !self.style(parent).visibility_hidden
    }

    /// Any block-level element below `c` in the flat tree.
    fn has_block(&self, c: NodeId) -> bool {
        let doc = self.doc;
        let mut stack = doc.flat_children(c);
        while let Some(n) = stack.pop() {
            if doc.node_type(n) != NodeType::Element {
                continue;
            }
            if let Some(name) = self.html_name(n) {
                if is_block(name) {
                    return true;
                }
                if !is_skipped_name(name) {
                    stack.extend(doc.flat_children(n));
                }
            }
        }
        false
    }

    // ----- Markdown -----

    fn markdown(&mut self, title: &str) -> String {
        let doc = self.doc;
        let root = doc
            .body()
            .or_else(|| doc.document_element())
            .unwrap_or(Document::DOCUMENT);
        let mut blocks = Vec::new();
        if !self.style(root).display_none {
            self.flow(root, &mut blocks);
        }
        let mut parts: Vec<String> = Vec::new();
        let title = norm_ws(title);
        if !self.h1 && !title.is_empty() {
            let mut t = Inline::line();
            t.text(&title);
            parts.push(format!("# {}", flatten(&t.s)));
        }
        parts.extend(blocks.into_iter().map(|b| b.text));
        let joined = parts.join("\n\n");
        let mut md = joined
            .split('\n')
            .map(|l| l.trim_end_matches([' ', '\t']))
            .collect::<Vec<_>>()
            .join("\n");
        if !md.is_empty() {
            md.push('\n');
        }
        md
    }

    fn flow(&mut self, parent: NodeId, out: &mut Vec<Block>) {
        let kids = self.doc.flat_children(parent);
        self.flow_nodes(parent, &kids, out);
    }

    /// Flow content: inline runs become paragraphs, block elements their
    /// own blocks.
    fn flow_nodes(&mut self, parent: NodeId, kids: &[NodeId], out: &mut Vec<Block>) {
        if self.depth >= MAX_DEPTH {
            return;
        }
        self.depth += 1;
        let doc = self.doc;
        let mut inl = Inline::default();
        for &c in kids {
            match doc.node_type(c) {
                NodeType::Text => {
                    if self.text_visible(parent) {
                        inl.text(doc.character_data(c).unwrap_or(""));
                    }
                }
                NodeType::Element => {
                    let Some(name) = self.html_name(c) else {
                        continue;
                    };
                    if self.skipped(c, name) {
                        continue;
                    }
                    if is_block(name) {
                        self.flush(&mut inl, out);
                        self.block(c, name, out);
                    } else if name != "a" && self.has_block(c) {
                        // An inline element around blocks: its content is
                        // flow content (a link stays one inline link).
                        self.flush(&mut inl, out);
                        self.flow(c, out);
                    } else {
                        self.inline_el(c, name, &mut inl);
                    }
                }
                _ => {}
            }
        }
        self.flush(&mut inl, out);
        self.depth -= 1;
    }

    fn flush(&mut self, inl: &mut Inline, out: &mut Vec<Block>) {
        let s = std::mem::take(&mut inl.s);
        for seg in s.split(PARA) {
            let t = seg.trim_matches(|c| c == ' ' || c == BR);
            if t.trim().is_empty() {
                continue;
            }
            out.push(Block {
                kind: Kind::Text,
                text: t.replace(BR, "\\\n"),
            });
        }
    }

    fn block(&mut self, c: NodeId, name: &str, out: &mut Vec<Block>) {
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name.as_bytes()[1] - b'0';
                self.heading(c, level, out);
            }
            "ul" | "ol" | "menu" | "dir" => self.list(c, name == "ol", out),
            "table" => self.table(c, out),
            "pre" => self.pre(c, out),
            "blockquote" => self.quote(c, out),
            "hr" => {
                if !self.style(c).visibility_hidden {
                    out.push(other("---".into()));
                }
            }
            _ => self.flow(c, out),
        }
    }

    fn heading(&mut self, c: NodeId, level: u8, out: &mut Vec<Block>) {
        let mut sub = Inline::line();
        self.children(c, &mut sub);
        let md = flatten(&sub.s);
        if md.trim().is_empty() {
            return;
        }
        let text = self.plain(c);
        if !text.is_empty() {
            self.headings.push(Heading { level, text });
        }
        if level == 1 {
            self.h1 = true;
        }
        out.push(other(format!("{} {md}", "#".repeat(level as usize))));
    }

    fn list(&mut self, c: NodeId, ordered: bool, out: &mut Vec<Block>) {
        let doc = self.doc;
        // A list right after another one needs the other marker, or
        // Markdown would join them.
        let alt = out.last().is_some_and(|b| b.kind == Kind::List);
        let number = |v: Option<&str>| {
            v.and_then(|v| v.trim().parse::<i64>().ok())
                .filter(|n| (0..=999_999_999).contains(n))
        };
        let mut num = if ordered {
            number(doc.get_attribute(c, "start")).unwrap_or(1)
        } else {
            1
        };
        let mut items = Vec::new();
        let mut loose = false;
        for k in doc.flat_children(c) {
            let mut blocks = Vec::new();
            match doc.node_type(k) {
                NodeType::Element => {
                    let Some(name) = self.html_name(k) else {
                        continue;
                    };
                    if self.skipped(k, name) {
                        continue;
                    }
                    if name == "li" {
                        if ordered && let Some(v) = number(doc.get_attribute(k, "value")) {
                            num = v;
                        }
                        self.flow(k, &mut blocks);
                    } else {
                        self.flow_nodes(c, &[k], &mut blocks);
                    }
                }
                NodeType::Text => {
                    let t = doc.character_data(k).unwrap_or("");
                    if t.trim_matches(is_html_ws).is_empty() || !self.text_visible(c) {
                        continue;
                    }
                    self.flow_nodes(c, &[k], &mut blocks);
                }
                _ => continue,
            }
            let mut body = String::new();
            for (i, b) in blocks.iter().enumerate() {
                if i > 0 {
                    // A nested list follows its item's text tightly.
                    if b.kind == Kind::List && blocks[i - 1].kind == Kind::Text {
                        body.push('\n');
                    } else {
                        body.push_str("\n\n");
                        loose = true;
                    }
                }
                body.push_str(&b.text);
            }
            let marker = if ordered {
                format!("{num}{} ", if alt { ')' } else { '.' })
            } else {
                (if alt { "* " } else { "- " }).to_owned()
            };
            num += 1;
            items.push(indent_item(&marker, &body));
        }
        if !items.is_empty() {
            out.push(Block {
                kind: Kind::List,
                text: items.join(if loose { "\n\n" } else { "\n" }),
            });
        }
    }

    fn quote(&mut self, c: NodeId, out: &mut Vec<Block>) {
        let mut inner = Vec::new();
        self.flow(c, &mut inner);
        if inner.is_empty() {
            return;
        }
        let text = inner
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let quoted = text
            .split('\n')
            .map(|l| {
                if l.is_empty() {
                    ">".to_owned()
                } else {
                    format!("> {l}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        out.push(other(quoted));
    }

    fn pre(&mut self, c: NodeId, out: &mut Vec<Block>) {
        let mut s = String::new();
        self.pre_text(c, &mut s, 0);
        let body = s.trim_matches('\n');
        if body.trim().is_empty() {
            return;
        }
        let mut longest = 0;
        let mut run = 0;
        for ch in body.chars() {
            if ch == '`' {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        let fence = "`".repeat((longest + 1).max(3));
        let lang = self.code_lang(c).unwrap_or_default();
        out.push(other(format!("{fence}{lang}\n{body}\n{fence}")));
    }

    fn pre_text(&mut self, c: NodeId, s: &mut String, depth: usize) {
        if depth >= MAX_DEPTH {
            return;
        }
        let doc = self.doc;
        for k in doc.flat_children(c) {
            match doc.node_type(k) {
                NodeType::Text => {
                    if self.text_visible(c) {
                        s.push_str(doc.character_data(k).unwrap_or(""));
                    }
                }
                NodeType::Element => {
                    let Some(name) = self.html_name(k) else {
                        continue;
                    };
                    if self.skipped(k, name) {
                        continue;
                    }
                    if name == "br" {
                        s.push('\n');
                    } else {
                        self.pre_text(k, s, depth + 1);
                    }
                }
                _ => {}
            }
        }
    }

    /// `language-x` of the first `<code>` child, else of the `<pre>`.
    fn code_lang(&self, pre: NodeId) -> Option<String> {
        let doc = self.doc;
        let code = doc
            .flat_children(pre)
            .into_iter()
            .find(|&k| self.html_name(k) == Some("code"));
        [code, Some(pre)].into_iter().flatten().find_map(|n| {
            doc.classes(n).find_map(|c| {
                let l = c.strip_prefix("language-")?;
                let ok = !l.is_empty()
                    && l.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_+#.-".contains(c));
                ok.then(|| l.to_owned())
            })
        })
    }

    fn table(&mut self, t: NodeId, out: &mut Vec<Block>) {
        struct Cell {
            text: String,
            th: bool,
            align: Option<&'static str>,
            span: usize,
        }
        let doc = self.doc;
        let mut caption = None;
        let mut head: Vec<Vec<Cell>> = Vec::new();
        let mut body: Vec<Vec<Cell>> = Vec::new();
        let mut foot: Vec<Vec<Cell>> = Vec::new();
        let mut sections: Vec<(NodeId, &str)> = Vec::new();
        for k in doc.flat_children(t) {
            let Some(name) = self.html_name(k) else {
                continue;
            };
            if self.skipped(k, name) {
                continue;
            }
            match name {
                "caption" => {
                    caption.get_or_insert(k);
                }
                "thead" | "tbody" | "tfoot" | "tr" => sections.push((k, name)),
                _ => {}
            }
        }
        for (k, name) in sections {
            let trs = if name == "tr" {
                vec![k]
            } else {
                doc.flat_children(k)
            };
            for tr in trs {
                if self.html_name(tr) != Some("tr") || self.skipped(tr, "tr") {
                    continue;
                }
                let mut cells = Vec::new();
                for td in doc.flat_children(tr) {
                    let Some(cn) = self.html_name(td) else {
                        continue;
                    };
                    if !matches!(cn, "td" | "th") || self.skipped(td, cn) {
                        continue;
                    }
                    let mut sub = Inline::line();
                    self.children(td, &mut sub);
                    let align = match doc
                        .get_attribute(td, "align")
                        .map(|a| a.trim().to_ascii_lowercase())
                        .or_else(|| inline_decl(doc.get_attribute(td, "style"), "text-align"))
                        .as_deref()
                    {
                        Some("left") => Some("left"),
                        Some("center") => Some("center"),
                        Some("right") => Some("right"),
                        _ => None,
                    };
                    let span = doc
                        .get_attribute(td, "colspan")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .map_or(1, |n| n.clamp(1, 100));
                    cells.push(Cell {
                        text: escape_pipes(&flatten(&sub.s)),
                        th: cn == "th",
                        align,
                        span,
                    });
                }
                if cells.is_empty() {
                    continue;
                }
                match name {
                    "thead" => head.push(cells),
                    "tfoot" => foot.push(cells),
                    _ => body.push(cells),
                }
            }
        }
        let has_head = !head.is_empty();
        let mut rows = head;
        rows.append(&mut body);
        rows.append(&mut foot);
        if rows.is_empty() {
            return;
        }
        let header = if has_head || rows[0].iter().all(|c| c.th) {
            Some(rows.remove(0))
        } else {
            None
        };
        let width = |r: &Vec<Cell>| r.iter().map(|c| c.span).sum::<usize>();
        let ncols = rows
            .iter()
            .chain(header.iter())
            .map(width)
            .max()
            .unwrap_or(0);
        if ncols == 0 {
            return;
        }
        let mut aligns: Vec<Option<&'static str>> = vec![None; ncols];
        let expand = |r: &Vec<Cell>, aligns: &mut Vec<Option<&'static str>>| {
            let mut cells = Vec::with_capacity(ncols);
            for c in r {
                if aligns[cells.len()].is_none() {
                    aligns[cells.len()] = c.align;
                }
                cells.push(c.text.clone());
                for _ in 1..c.span {
                    cells.push(String::new());
                }
            }
            cells.resize(ncols, String::new());
            cells
        };
        let fmt = |cells: &[String]| {
            let mut s = String::from("|");
            for c in cells {
                s.push(' ');
                s.push_str(c);
                s.push_str(" |");
            }
            s
        };
        let head_cells = match &header {
            Some(h) => expand(h, &mut aligns),
            None => vec![String::new(); ncols],
        };
        let body_cells: Vec<Vec<String>> = rows.iter().map(|r| expand(r, &mut aligns)).collect();
        let delim: Vec<String> = aligns
            .iter()
            .map(|a| {
                match a {
                    Some("left") => ":---",
                    Some("center") => ":---:",
                    Some("right") => "---:",
                    _ => "---",
                }
                .to_owned()
            })
            .collect();
        let mut lines = vec![fmt(&head_cells), fmt(&delim)];
        lines.extend(body_cells.iter().map(|r| fmt(r)));
        if let Some(cap) = caption {
            let mut sub = Inline::line();
            self.children(cap, &mut sub);
            let t = flatten(&sub.s);
            if !t.is_empty() {
                out.push(Block {
                    kind: Kind::Text,
                    text: t,
                });
            }
        }
        out.push(other(lines.join("\n")));
    }

    // ----- inline content -----

    fn children(&mut self, c: NodeId, inl: &mut Inline) {
        if self.depth >= MAX_DEPTH {
            return;
        }
        self.depth += 1;
        let doc = self.doc;
        for k in doc.flat_children(c) {
            match doc.node_type(k) {
                NodeType::Text => {
                    if self.text_visible(c) {
                        inl.text(doc.character_data(k).unwrap_or(""));
                    }
                }
                NodeType::Element => {
                    let Some(name) = self.html_name(k) else {
                        continue;
                    };
                    if self.skipped(k, name) {
                        continue;
                    }
                    self.inline_el(k, name, inl);
                }
                _ => {}
            }
        }
        self.depth -= 1;
    }

    fn inline_el(&mut self, c: NodeId, name: &str, inl: &mut Inline) {
        match name {
            "br" => inl.brk(false),
            "wbr" => {}
            "img" => self.image(c, inl),
            "a" => self.anchor(c, inl),
            "em" | "i" => self.wrap(c, "*", inl),
            "strong" | "b" => self.wrap(c, "**", inl),
            "del" | "s" | "strike" => self.wrap(c, "~~", inl),
            "code" | "kbd" | "samp" | "tt" => {
                let raw = self.plain_raw(c);
                emit(inl, &raw, code_span);
            }
            _ if is_block(name) => {
                inl.space();
                self.children(c, inl);
                inl.space();
            }
            _ => self.children(c, inl),
        }
    }

    fn wrap(&mut self, c: NodeId, mark: &str, inl: &mut Inline) {
        let mut sub = inl.sub();
        self.children(c, &mut sub);
        emit(inl, &sub.s, |core| format!("{mark}{core}{mark}"));
    }

    fn anchor(&mut self, c: NodeId, inl: &mut Inline) {
        let url = self
            .doc
            .get_attribute(c, "href")
            .and_then(|h| resolve(self.base, h));
        let mut sub = inl.sub();
        self.children(c, &mut sub);
        match url {
            Some(u) => {
                let inner = sub.s.replace(PARA, &BR.to_string());
                emit(inl, &inner, |core| {
                    format!("[{core}]({})", escape_dest(u.as_str()))
                });
            }
            None => inl.append(&sub.s),
        }
    }

    fn image(&mut self, c: NodeId, inl: &mut Inline) {
        if self.style(c).visibility_hidden {
            return;
        }
        let doc = self.doc;
        let Some(u) = doc
            .get_attribute(c, "src")
            .filter(|s| !s.trim_matches(is_html_ws).is_empty())
            .and_then(|s| resolve(self.base, s))
        else {
            return;
        };
        let alt = escape_plain(&norm_ws(doc.get_attribute(c, "alt").unwrap_or("")));
        inl.raw(&format!("![{alt}]({})", escape_dest(u.as_str())));
    }

    // ----- plain text -----

    /// Whitespace-normalised text of `c` (images give their `alt`); may
    /// start or end with one space.
    fn plain_raw(&mut self, c: NodeId) -> String {
        let mut s = String::new();
        self.plain_into(c, &mut s, 0);
        s
    }

    fn plain(&mut self, c: NodeId) -> String {
        self.plain_raw(c).trim_matches(' ').to_owned()
    }

    fn plain_into(&mut self, c: NodeId, s: &mut String, depth: usize) {
        fn space(s: &mut String) {
            if !s.ends_with(' ') {
                s.push(' ');
            }
        }
        if depth >= MAX_DEPTH {
            return;
        }
        let doc = self.doc;
        for k in doc.flat_children(c) {
            match doc.node_type(k) {
                NodeType::Text => {
                    if self.text_visible(c) {
                        let t = doc.character_data(k).unwrap_or("");
                        for (i, w) in t.split(is_html_ws).enumerate() {
                            if i > 0 {
                                space(s);
                            }
                            s.push_str(w);
                        }
                    }
                }
                NodeType::Element => {
                    let Some(name) = self.html_name(k) else {
                        continue;
                    };
                    if self.skipped(k, name) {
                        continue;
                    }
                    match name {
                        "br" => space(s),
                        "img" => {
                            if !self.style(k).visibility_hidden
                                && let Some(alt) = doc.get_attribute(k, "alt")
                            {
                                let alt = norm_ws(alt);
                                if !alt.is_empty() {
                                    s.push_str(&alt);
                                }
                            }
                        }
                        _ => {
                            let block = is_block(name);
                            if block {
                                space(s);
                            }
                            self.plain_into(k, s, depth + 1);
                            if block {
                                space(s);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ----- links -----

    /// Anchors with an `href` in document order (hidden ones too), minus
    /// fragment-only and `javascript:` targets and duplicates.
    fn links(&mut self) -> Vec<Link> {
        let doc = self.doc;
        let mut out = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut stack = vec![Document::DOCUMENT];
        while let Some(n) = stack.pop() {
            if doc.node_type(n) == NodeType::Element {
                let Some(name) = self.html_name(n) else {
                    continue;
                };
                if matches!(name, "script" | "style" | "template" | "noscript") {
                    continue;
                }
                if name == "a"
                    && let Some(href) = doc.get_attribute(n, "href")
                    && !href.trim_matches(is_html_ws).starts_with('#')
                    && let Some(u) = resolve(self.base, href)
                {
                    let text = self.plain(n);
                    if seen.insert((u.to_string(), text.clone())) {
                        out.push(Link {
                            url: u.to_string(),
                            text,
                            rel: doc.get_attribute(n, "rel").map(norm_ws).unwrap_or_default(),
                        });
                    }
                }
            }
            let mut kids = doc.flat_children(n);
            kids.reverse();
            stack.extend(kids);
        }
        out
    }
}

/// Marker on the first line, the rest indented under it.
fn indent_item(marker: &str, body: &str) -> String {
    let pad = " ".repeat(marker.len());
    let mut o = String::new();
    for (i, l) in body.split('\n').enumerate() {
        if i == 0 {
            o.push_str(marker);
            o.push_str(l);
        } else {
            o.push('\n');
            if !l.is_empty() {
                o.push_str(&pad);
                o.push_str(l);
            }
        }
    }
    o.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.com/dir/page.html";

    fn run(html: &str) -> Extracted {
        let d = Document::parse_html(html);
        extract_document(&d, &Url::parse(URL).unwrap(), String::new())
    }

    fn md(html: &str) -> String {
        run(html).markdown
    }

    #[test]
    fn escapes_markdown_specials_by_position() {
        assert_eq!(escape_word("*a*", false), "\\*a\\*");
        assert_eq!(escape_word("snake_case", false), "snake_case");
        assert_eq!(escape_word("_x_", false), "\\_x\\_");
        assert_eq!(escape_word("a~~b", false), "a\\~\\~b");
        assert_eq!(escape_word("a~b", false), "a~b");
        assert_eq!(escape_word("#", true), "\\#");
        assert_eq!(escape_word("#", false), "#");
        assert_eq!(escape_word("#tag", true), "#tag");
        assert_eq!(escape_word("1.", true), "1\\.");
        assert_eq!(escape_word("1.5", true), "1.5");
        assert_eq!(escape_word("-", true), "\\-");
        assert_eq!(escape_word("-3", true), "-3");
        assert_eq!(escape_word("&amp;", false), "\\&amp;");
        assert_eq!(escape_word("R&D", false), "R&D");
        assert_eq!(escape_word("<b>", false), "\\<b>");
        assert_eq!(escape_word("a>b", true), "a>b");
        assert_eq!(escape_word(">q", true), "\\>q");
    }

    #[test]
    fn title_only_without_a_visible_h1() {
        assert_eq!(md("<title>T</title><p>x</p>"), "# T\n\nx\n");
        assert_eq!(md("<title>T</title><h1>H</h1><p>x</p>"), "# H\n\nx\n");
        assert_eq!(
            md("<title>T</title><h1 hidden>H</h1><p>x</p>"),
            "# T\n\nx\n"
        );
        assert_eq!(md("<title>T</title><h2>H</h2>"), "# T\n\n## H\n");
    }

    #[test]
    fn empty_page_is_empty() {
        assert_eq!(md(""), "");
        assert_eq!(md("<body>  <div> </div>  </body>"), "");
    }

    #[test]
    fn first_base_with_href_decides() {
        let e = run(
            "<base target=x><base href='/a/'><base href='/b/'><a href='p'>p</a>\
             <img src='i.png'>",
        );
        assert_eq!(e.links[0].url, "https://example.com/a/p");
        assert!(e.markdown.contains("![](https://example.com/a/i.png)"));
        // An unusable base falls back to the document URL.
        let e = run("<base href='http://[bad'><a href='p'>p</a>");
        assert_eq!(e.links[0].url, "https://example.com/dir/p");
    }

    #[test]
    fn links_drop_fragments_scripts_and_duplicates() {
        let e = run(
            "<a href='#a'>f</a><a href=' javascript:x()'>j</a><a>none</a>\
             <a href='/x' rel='  a   b '>one</a><a href='/x'>one</a><a href='/x'>two</a>\
             <a href='/x#frag'>one</a><a href='http://[bad'>bad</a>",
        );
        let got: Vec<(&str, &str, &str)> = e
            .links
            .iter()
            .map(|l| (l.url.as_str(), l.text.as_str(), l.rel.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("https://example.com/x", "one", "a b"),
                ("https://example.com/x", "two", ""),
                ("https://example.com/x#frag", "one", ""),
            ]
        );
        assert_eq!(
            links_dump(&e),
            "https://example.com/x\tone\nhttps://example.com/x\ttwo\nhttps://example.com/x#frag\tone\n"
        );
    }

    #[test]
    fn ordered_lists_count_and_indent_by_marker() {
        assert_eq!(
            md(
                "<ol start=9><li>a<ol><li>x</li></ol></li><li>b</li><li value=100>c</li><li>d</li></ol>"
            ),
            "9. a\n   1. x\n10. b\n100. c\n101. d\n"
        );
        // Out-of-range or bad numbers fall back.
        assert_eq!(md("<ol start=-4><li>a</li></ol>"), "1. a\n");
        assert_eq!(md("<ol start=x><li>a</li><li>b</li></ol>"), "1. a\n2. b\n");
        // Text and elements directly inside the list still show up.
        assert_eq!(
            md("<ul>stray<li>a</li><div>d</div></ul>"),
            "- stray\n- a\n- d\n"
        );
    }

    #[test]
    fn emphasis_hugs_its_text() {
        assert_eq!(md("<p>a<b> b </b>c</p>"), "a **b** c\n");
        assert_eq!(md("<p><b> </b>x</p>"), "x\n");
        assert_eq!(md("<p>a<i></i>b</p>"), "ab\n");
        assert_eq!(md("<p><b>x<br></b>y</p>"), "**x**\\\ny\n");
    }

    #[test]
    fn breaks_and_paragraphs() {
        assert_eq!(md("<p>a<br>b</p>"), "a\\\nb\n");
        assert_eq!(md("<p>a<br><br>b</p>"), "a\n\nb\n");
        assert_eq!(md("<p>a<br> <br> <br>b</p>"), "a\n\nb\n");
        assert_eq!(md("<p><br>a<br></p>"), "a\n");
        assert_eq!(md("<h2>a<br>b</h2>"), "## a b\n");
    }

    #[test]
    fn tables_escape_and_pad() {
        assert_eq!(
            md("<table><tr><th>a|b<th align=right>c<tr><td>1<td>2<td>3</table>"),
            "| a\\|b | c |  |\n| --- | ---: | --- |\n| 1 | 2 | 3 |\n"
        );
        // A lone first row of td cells is not a header.
        assert_eq!(md("<table><tr><td>x</table>"), "|  |\n| --- |\n| x |\n");
        // Block content in a cell is one line.
        assert_eq!(
            md("<table><tr><td><p>a</p><p>b<br>c</p></table>"),
            "|  |\n| --- |\n| a b c |\n"
        );
    }

    #[test]
    fn code_blocks_pick_fences_and_languages() {
        assert_eq!(
            md("<pre><code class='x language-js y'>a</code></pre>"),
            "```js\na\n```\n"
        );
        assert_eq!(md("<pre>a ```` b</pre>"), "`````\na ```` b\n`````\n");
        // Not a language: backtick in the class.
        assert_eq!(
            md("<pre><code class='language-a`b'>a</code></pre>"),
            "```\na\n```\n"
        );
        assert_eq!(md("<p>x <code>a``b</code></p>"), "x ```a``b```\n");
    }

    #[test]
    fn non_html_elements_and_forms_are_left_out() {
        assert_eq!(
            md(
                "<p>a</p><svg><text>no</text></svg><input value=v><select><option>o</select>\
                <textarea>t</textarea><p>b</p>"
            ),
            "a\n\nb\n"
        );
    }

    #[test]
    fn inline_styles_hide_without_a_style_engine() {
        assert_eq!(
            md(
                "<p style='display: none'>a</p><p style='color:red;DISPLAY:NONE !important'>b</p><p>c</p>"
            ),
            "c\n"
        );
        assert_eq!(
            md("<div style='visibility:hidden'>a<span style='visibility:visible'>b</span></div>"),
            "b\n"
        );
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let html = format!("{}x{}", "<div>".repeat(5000), "</div>".repeat(5000));
        let e = run(&html);
        // Past the depth limit the rest is dropped, but nothing crashes.
        assert!(e.markdown.is_empty() || e.markdown == "x\n");
        let html = format!("{}x{}", "<span>".repeat(5000), "</span>".repeat(5000));
        let _ = run(&html);
    }

    #[test]
    fn json_has_a_fixed_key_order_and_valid_escapes() {
        let e = Extracted {
            title: "T \"q\"".into(),
            lang: Some("en".into()),
            description: None,
            canonical: Some("https://e.com/c".into()),
            headings: vec![Heading {
                level: 2,
                text: "H\t1".into(),
            }],
            links: vec![Link {
                url: "https://e.com/a".into(),
                text: "A\u{e9}".into(),
                rel: "nofollow".into(),
            }],
            markdown: "# x\n".into(),
            text: "a\nb".into(),
        };
        let j = json_dump(&e, "http://e.com/", "https://e.com/", 200);
        assert!(j.ends_with("}\n"));
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["title"], "T \"q\"");
        assert_eq!(v["description"], serde_json::Value::Null);
        assert_eq!(v["headings"][0]["text"], "H\t1");
        assert_eq!(v["links"][0]["text"], "A\u{e9}");
        assert_eq!(v["status"], 200);
        let keys = [
            "url",
            "final_url",
            "status",
            "title",
            "lang",
            "description",
            "canonical",
            "headings",
            "links",
            "text",
            "markdown",
        ];
        let at: Vec<usize> = keys
            .iter()
            .map(|k| j.find(&format!("\n  \"{k}\": ")).expect(k))
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "{at:?}");
        let empty = json_dump(&Extracted::default(), "u", "u", 404);
        assert!(empty.contains("  \"headings\": [],\n  \"links\": [],\n"));
    }

    #[test]
    fn metadata_comes_from_the_head() {
        let e = run("<html lang=' fr-CA '><head><title> A  B </title>\
             <meta name='Description' content=' x   y '>\
             <meta name=description content=second>\
             <link rel='alternate Canonical' href='c'></head>");
        assert_eq!(e.title, "A B");
        assert_eq!(e.lang.as_deref(), Some("fr-CA"));
        assert_eq!(e.description.as_deref(), Some("x y"));
        assert_eq!(e.canonical.as_deref(), Some("https://example.com/dir/c"));
        let e = run("<p>x");
        assert_eq!((e.lang, e.description, e.canonical), (None, None, None));
    }
}

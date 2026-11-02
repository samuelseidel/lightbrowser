//! The cascade: collects the document's sheets, matches rules lazily per
//! element and caches computed styles until the document or the engine
//! changes.

use std::collections::HashMap;
use std::rc::Rc;

use lb_dom::{Document, NodeId, NodeType, local_name, ns};
use lb_persona::PersonaProfile;

use crate::computed::{ComputeCtx, ComputedStyle, Defaults, compute};
use crate::media::{MediaEnv, MediaQueryList, MediaQueryResult, match_media};
use crate::props::{CustomDeclaration, Decl, DeclarationBlock, LonghandId};
use crate::sheet::{ImportRule, Origin, Stylesheet};
use crate::ua::ua_stylesheet;

/// Handle for a sheet added with [`StyleEngine::add_sheet`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SheetId(u32);

/// Counters for tests and tracing.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct StyleStats {
    /// Elements whose style was computed (cache misses).
    pub computed: u64,
    /// Times the active rule set was rebuilt.
    pub rebuilds: u64,
    /// Stylesheets parsed (cache misses on sheet text).
    pub sheets_parsed: u64,
}

/// Cascade rank of a declaration source, normal declarations.
const RANK_UA: u8 = 0;
const RANK_HINT: u8 = 1;
const RANK_AUTHOR: u8 = 2;
const RANK_INLINE: u8 = 3;

struct ActiveRule {
    sheet: Rc<Stylesheet>,
    index: u32,
    rank: u8,
}

struct DocSheet {
    node: NodeId,
    text: String,
    sheet: Rc<Stylesheet>,
}

/// Lazy style resolution for one document.
///
/// Holds the UA sheet, the document's `<style>` / `<link>` sheets and any
/// extra sheets, and computes [`ComputedStyle`] on demand. The cache is
/// keyed on [`Document::render_generation`] plus an engine epoch that
/// bumps on sheet or media changes, so any DOM change invalidates it.
pub struct StyleEngine {
    env: MediaEnv,
    defaults: Defaults,
    initial: ComputedStyle,
    ua: Rc<Stylesheet>,
    extra: Vec<(SheetId, Rc<Stylesheet>)>,
    next_sheet: u32,
    external: HashMap<NodeId, Rc<Stylesheet>>,
    doc_sheets: Vec<DocSheet>,
    active: Vec<ActiveRule>,
    cache: Vec<Option<Rc<ComputedStyle>>>,
    key: Option<(u64, u64)>,
    epoch: u64,
    stats: StyleStats,
}

impl std::fmt::Debug for StyleEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StyleEngine")
            .field("env", &self.env)
            .field("rules", &self.active.len())
            .field("stats", &self.stats)
            .finish()
    }
}

impl StyleEngine {
    /// An engine for `env` (viewport, DPR, OS).
    pub fn new(env: MediaEnv) -> StyleEngine {
        let defaults = Defaults::for_os(env.os);
        let initial = ComputedStyle::initial(&defaults);
        let ua = Rc::new(Stylesheet::parse(&ua_stylesheet(env.os), Origin::UserAgent));
        StyleEngine {
            env,
            defaults,
            initial,
            ua,
            extra: Vec::new(),
            next_sheet: 0,
            external: HashMap::new(),
            doc_sheets: Vec::new(),
            active: Vec::new(),
            cache: Vec::new(),
            key: None,
            epoch: 0,
            stats: StyleStats::default(),
        }
    }

    /// An engine whose media environment comes from the persona's screen.
    pub fn from_persona(persona: &PersonaProfile) -> StyleEngine {
        Self::new(MediaEnv::from_persona(persona))
    }

    pub fn env(&self) -> &MediaEnv {
        &self.env
    }

    pub fn defaults(&self) -> &Defaults {
        &self.defaults
    }

    /// Replaces the media environment (e.g. CDP device metrics).
    pub fn set_env(&mut self, env: MediaEnv) {
        if env != self.env {
            if env.os != self.env.os {
                self.defaults = Defaults::for_os(env.os);
                self.initial = ComputedStyle::initial(&self.defaults);
                self.ua = Rc::new(Stylesheet::parse(&ua_stylesheet(env.os), Origin::UserAgent));
            }
            self.env = env;
            self.invalidate();
        }
    }

    /// Changes the viewport size (window resize).
    pub fn set_viewport(&mut self, width: f32, height: f32) {
        let mut env = self.env.clone();
        env.viewport_width = width;
        env.viewport_height = height;
        self.set_env(env);
    }

    /// Adds an author sheet that applies after the document's sheets
    /// (constructed sheets, CDP-injected styles).
    pub fn add_sheet(&mut self, css: &str) -> SheetId {
        let id = SheetId(self.next_sheet);
        self.next_sheet += 1;
        self.stats.sheets_parsed += 1;
        self.extra
            .push((id, Rc::new(Stylesheet::parse(css, Origin::Author))));
        self.invalidate();
        id
    }

    /// Removes a sheet added with [`StyleEngine::add_sheet`].
    pub fn remove_sheet(&mut self, id: SheetId) {
        self.extra.retain(|(i, _)| *i != id);
        self.invalidate();
    }

    /// Sets (or clears) the fetched text of a `<link rel=stylesheet>`.
    pub fn set_external_sheet(&mut self, link: NodeId, css: Option<&str>) {
        match css {
            Some(css) => {
                self.stats.sheets_parsed += 1;
                self.external
                    .insert(link, Rc::new(Stylesheet::parse(css, Origin::Author)));
            }
            None => {
                self.external.remove(&link);
            }
        }
        self.invalidate();
    }

    /// Drops every cached style. Call after changing something the engine
    /// cannot see (it already sees DOM changes through the document's
    /// render generation).
    pub fn invalidate(&mut self) {
        self.epoch += 1;
    }

    /// Cache key: changes whenever any computed style may change. Layout
    /// uses it to know when to rebuild.
    pub fn style_key(&self, doc: &Document) -> (u64, u64) {
        (doc.render_generation(), self.epoch)
    }

    pub fn stats(&self) -> StyleStats {
        self.stats
    }

    /// `window.matchMedia(query)`.
    pub fn match_media(&self, query: &str) -> MediaQueryResult {
        match_media(query, &self.env)
    }

    /// `@import` rules of the document's sheets, in order, for the loader.
    pub fn imports(&mut self, doc: &Document) -> Vec<ImportRule> {
        self.refresh(doc);
        self.doc_sheets
            .iter()
            .flat_map(|s| s.sheet.imports.iter().cloned())
            .collect()
    }

    /// `<link rel=stylesheet>` elements whose text has not been provided.
    pub fn pending_links(&self, doc: &Document) -> Vec<NodeId> {
        doc.descendants(Document::DOCUMENT)
            .filter(|&n| is_stylesheet_link(doc, n) && !self.external.contains_key(&n))
            .collect()
    }

    fn refresh(&mut self, doc: &Document) {
        let key = self.style_key(doc);
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.stats.rebuilds += 1;
        self.cache.clear();
        self.cache.resize(doc.node_count(), None);

        // Document sheets in tree order; reuse parses whose text is unchanged.
        let mut old: HashMap<NodeId, DocSheet> =
            self.doc_sheets.drain(..).map(|s| (s.node, s)).collect();
        let mut author: Vec<(Rc<Stylesheet>, Option<String>)> = Vec::new();
        for n in doc.descendants(Document::DOCUMENT) {
            if doc.is_html_element_named(n, &local_name!("style")) {
                let ty = doc.get_attribute(n, "type").unwrap_or("");
                if !(ty.is_empty() || ty.eq_ignore_ascii_case("text/css")) {
                    continue;
                }
                let text = doc.child_text_content(n);
                let sheet = match old.remove(&n) {
                    Some(s) if s.text == text => s.sheet,
                    _ => {
                        self.stats.sheets_parsed += 1;
                        Rc::new(Stylesheet::parse(&text, Origin::Author))
                    }
                };
                self.doc_sheets.push(DocSheet {
                    node: n,
                    text,
                    sheet: sheet.clone(),
                });
                author.push((sheet, doc.get_attribute(n, "media").map(str::to_owned)));
            } else if is_stylesheet_link(doc, n)
                && let Some(sheet) = self.external.get(&n)
            {
                if doc.has_attribute(n, "disabled") {
                    continue;
                }
                author.push((sheet.clone(), doc.get_attribute(n, "media").map(str::to_owned)));
            }
        }
        for (_, s) in &self.extra {
            author.push((s.clone(), None));
        }

        self.active.clear();
        let env = &self.env;
        let push_sheet = |active: &mut Vec<ActiveRule>, sheet: &Rc<Stylesheet>, rank: u8| {
            for (i, rule) in sheet.rules.iter().enumerate() {
                if rule.media_matches(env) {
                    active.push(ActiveRule {
                        sheet: sheet.clone(),
                        index: i as u32,
                        rank,
                    });
                }
            }
        };
        push_sheet(&mut self.active, &self.ua, RANK_UA);
        for (sheet, media) in &author {
            if let Some(m) = media
                && !MediaQueryList::parse(m).matches(env)
            {
                continue;
            }
            push_sheet(&mut self.active, sheet, RANK_AUTHOR);
        }
    }

    /// Computed style of element `id`, or `None` for non-elements and
    /// elements not in the document (Chrome returns empty values there).
    pub fn computed_style(&mut self, doc: &Document, id: NodeId) -> Option<Rc<ComputedStyle>> {
        if !doc.is_element(id) || !doc.is_connected(id) {
            return None;
        }
        self.refresh(doc);
        if self.cache.len() < doc.node_count() {
            self.cache.resize(doc.node_count(), None);
        }
        if let Some(s) = &self.cache[id.index()] {
            return Some(s.clone());
        }
        // Compute uncached ancestors top-down.
        let mut chain = vec![id];
        let mut cur = doc.parent_element(id);
        while let Some(p) = cur {
            if self.cache[p.index()].is_some() {
                break;
            }
            chain.push(p);
            cur = doc.parent_element(p);
        }
        let root = doc.document_element();
        let mut root_font = root.and_then(|r| self.cache[r.index()].as_ref().map(|s| s.font_size));
        for &n in chain.iter().rev() {
            let parent = doc
                .parent_element(n)
                .and_then(|p| self.cache[p.index()].clone());
            let parent_display = doc
                .ancestors(n)
                .filter(|&a| doc.is_element(a))
                .filter_map(|a| self.cache[a.index()].as_ref())
                .map(|s| s.display)
                .find(|d| *d != crate::values::Display::Contents);
            let is_root = Some(n) == root;
            let style = self.compute_one(doc, n, parent.as_deref(), parent_display, is_root, root_font);
            if is_root {
                root_font = Some(style.font_size);
            }
            self.stats.computed += 1;
            self.cache[n.index()] = Some(Rc::new(style));
        }
        self.cache[id.index()].clone()
    }

    fn compute_one(
        &self,
        doc: &Document,
        id: NodeId,
        parent: Option<&ComputedStyle>,
        parent_display: Option<crate::values::Display>,
        is_root: bool,
        root_font: Option<f32>,
    ) -> ComputedStyle {
        // Matching blocks: (rank, specificity, order, block).
        let mut matched: Vec<(u8, u32, u32, Rc<DeclarationBlock>)> = Vec::new();
        for (order, rule) in self.active.iter().enumerate() {
            let r = &rule.sheet.rules[rule.index as usize];
            if let Some(spec) = doc.match_specificity(id, &r.selectors) {
                matched.push((rule.rank, spec, order as u32, r.block.clone()));
            }
        }
        if let Some(h) = presentational_hints(doc, id) {
            matched.push((RANK_HINT, 0, 0, Rc::new(h)));
        }
        if let Some(style) = doc.get_attribute(id, "style")
            && !style.is_empty()
        {
            matched.push((RANK_INLINE, 0, 0, Rc::new(DeclarationBlock::parse(style))));
        }
        matched.sort_by_key(|m| (m.0, m.1, m.2));

        let mut cascaded: Vec<Option<&Decl>> = vec![None; LonghandId::COUNT];
        let mut customs: Vec<&CustomDeclaration> = Vec::new();
        for (_, _, _, block) in &matched {
            for d in block.decls.iter().filter(|d| !d.important) {
                cascaded[d.id as usize] = Some(&d.value);
            }
            customs.extend(block.customs.iter().filter(|c| !c.important));
        }
        // Important: author < inline < UA.
        let imp_rank = |rank: u8| match rank {
            RANK_UA => 2,
            RANK_INLINE => 1,
            _ => 0,
        };
        let mut imp: Vec<&(u8, u32, u32, Rc<DeclarationBlock>)> = matched.iter().collect();
        imp.sort_by_key(|m| (imp_rank(m.0), m.1, m.2));
        for (_, _, _, block) in imp {
            for d in block.decls.iter().filter(|d| d.important) {
                cascaded[d.id as usize] = Some(&d.value);
            }
            customs.extend(block.customs.iter().filter(|c| c.important));
        }
        let ctx = ComputeCtx {
            env: &self.env,
            defaults: &self.defaults,
            initial: &self.initial,
            root_font_size: if is_root { None } else { root_font },
            is_root,
        };
        compute(&cascaded, &customs, parent, parent_display, &ctx)
    }

    /// A style resolved for an element, serialized like
    /// `getComputedStyle(el).getPropertyValue(name)`. Layout-dependent
    /// values (`width` in px) need `lb-layout`; this returns the computed
    /// value. Empty string for unknown properties or detached elements.
    pub fn property_value(&mut self, doc: &Document, id: NodeId, name: &str) -> String {
        self.computed_style(doc, id)
            .and_then(|s| crate::serialize::computed_value(&s, name))
            .unwrap_or_default()
    }
}

fn is_stylesheet_link(doc: &Document, n: NodeId) -> bool {
    doc.is_html_element_named(n, &local_name!("link"))
        && doc.get_attribute(n, "rel").is_some_and(|r| {
            r.split_ascii_whitespace()
                .any(|t| t.eq_ignore_ascii_case("stylesheet"))
                && !r
                    .split_ascii_whitespace()
                    .any(|t| t.eq_ignore_ascii_case("alternate"))
        })
}

/// HTML presentational attributes mapped to CSS (author level,
/// specificity 0): `width`/`height` on replaced elements and tables,
/// `align`, `bgcolor`, `border` on tables and images, `hidden`.
fn presentational_hints(doc: &Document, id: NodeId) -> Option<DeclarationBlock> {
    let el = doc.element(id)?;
    if el.name.ns != ns!(html) {
        return None;
    }
    let name = &*el.name.local;
    let mut b = DeclarationBlock::default();
    let dim = |v: &str| -> Option<String> {
        let v = v.trim();
        let num: String = v
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if num.is_empty() {
            return None;
        }
        let n: f32 = num.parse().ok()?;
        Some(if v[num.len()..].starts_with('%') {
            format!("{n}%")
        } else {
            format!("{n}px")
        })
    };
    if matches!(
        name,
        "img"
            | "video"
            | "canvas"
            | "iframe"
            | "embed"
            | "object"
            | "table"
            | "td"
            | "th"
            | "col"
            | "hr"
            | "svg"
    ) || (name == "input" && el.attr("type").is_some_and(|t| t.eq_ignore_ascii_case("image")))
    {
        if let Some(w) = el.attr("width").and_then(dim) {
            b.push_text("width", &w);
        }
        if name != "hr"
            && let Some(h) = el.attr("height").and_then(dim)
        {
            b.push_text("height", &h);
        }
    }
    if let Some(c) = el.attr("bgcolor") {
        b.push_text("background-color", c);
    }
    if name == "font"
        && let Some(c) = el.attr("color")
    {
        b.push_text("color", c);
    }
    if matches!(name, "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "td" | "th")
        && let Some(a) = el.attr("align")
    {
        let v = match a.to_ascii_lowercase().as_str() {
            "center" | "middle" => "-webkit-center",
            "left" => "-webkit-left",
            "right" => "-webkit-right",
            "justify" => "justify",
            _ => "",
        };
        if !v.is_empty() {
            b.push_text("text-align", v);
        }
    }
    if matches!(name, "table" | "img")
        && let Some(bw) = el.attr("border")
    {
        let w = bw.trim().parse::<f32>().unwrap_or(1.0);
        b.push_text("border-width", &format!("{w}px"));
        b.push_text(
            "border-style",
            if name == "table" { "outset" } else { "solid" },
        );
    }
    if b.decls.is_empty() { None } else { Some(b) }
}

/// True if any ancestor-or-self of `id` has `display: none`. Helper for
/// visibility checks.
pub fn in_display_none(engine: &mut StyleEngine, doc: &Document, id: NodeId) -> bool {
    std::iter::once(id)
        .chain(doc.ancestors(id))
        .filter(|&n| doc.node_type(n) == NodeType::Element)
        .any(|n| {
            engine
                .computed_style(doc, n)
                .is_none_or(|s| s.is_display_none())
        })
}

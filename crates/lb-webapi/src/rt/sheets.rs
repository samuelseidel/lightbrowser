//! Style sheets of a window's document, as Chrome handles them (checked
//! against Chrome 153 in `lb-page`'s `stylesheets` tests):
//!
//! - **Loading.** A `<link rel=stylesheet href>` starts loading when it is
//!   inserted (by the parser or by script) or its `href` / `rel` changes;
//!   `@import`s of link sheets and `<style>` elements load in parallel,
//!   nested imports too. A sheet with an HTTP error or a network error
//!   fires `error`; so does a link or style whose imports failed. A
//!   response that is not `text/css` (same-origin quirks documents
//!   excepted) gives an empty sheet and still fires `load`. Every `<style>`
//!   fires `load` (or `error`) too. These loads delay the window's `load`.
//! - **Blocking.** A sheet is *script-blocking* while it loads when its
//!   element was made by the parser, is not an alternate sheet and its
//!   `media` matches (a `<style>` blocks while its imports load, whatever
//!   their media). Parser-inserted classic scripts, `defer` scripts and
//!   module scripts wait until no such sheet loads; when the last one
//!   finishes the waiting script runs before the sheet's `load` event,
//!   and parsing resumes after it (the page layer drives this through
//!   [`Window::script_blocking_sheets`] and the gate below). A blocking
//!   sheet inserted after `<body>` exists is also *parser-blocking*: the
//!   parser stops after the element until it loads and its `load` event
//!   has fired.
//! - **CSSOM.** `document.styleSheets` lists `<style>` sheets and loaded
//!   (or failed) link sheets in tree order, without links whose
//!   `disabled` attribute is set. `CSSStyleSheet` (`href`, `ownerNode`,
//!   `media`, `title`, `disabled`, `cssRules`, `insertRule`, `deleteRule`,
//!   `ownerRule`, `parentStyleSheet`), `MediaList`, `CSSRuleList` and the
//!   rule classes (`CSSStyleRule` with a live `style`, `CSSImportRule`
//!   with its `styleSheet`, grouping rules with their `cssRules`). Changes
//!   made through the CSSOM reach the cascade. Declarations keep their
//!   source text (Chrome normalizes values).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use lb_css::cssom::{RawRule, RuleKind, parse_one_rule, rule_text, split_rules};
use lb_css::{Origin, SheetTree, Stylesheet};
use lb_dom::{Document, NodeFlags, NodeId, local_name};
use lb_js::{Arg, CallCtx, HostError};
use url::Url;

use super::{
    K_OBJ, Obj, Runtime, TaskSource, Window, fire_event, getter, illegal, method, need_args,
    setter, this_node, this_obj,
};
use crate::host::{Destination, NetRequest, NetResponse};
use crate::registry::Registry;

/// Nested `@import` depth Chrome-like loaders accept before giving up.
const MAX_DEPTH: usize = 16;

/// Per-window style sheet state.
#[derive(Default)]
pub(crate) struct CssState {
    owners: HashMap<NodeId, Owner>,
    sheets: Vec<SheetEntry>,
    /// Changes for the style engine, applied when layout is next used.
    pub(crate) updates: Vec<Update>,
    next_uid: u32,
    /// Parser-inserted sheets loading that block scripts / the parser.
    script_blocking: u32,
    parser_blocking: u32,
    /// `load` / `error` events queued and not yet fired.
    events: u32,
    /// A parser script is waiting for script-blocking sheets: sheet events
    /// wait until it ran once no such sheet loads (Chrome runs it first).
    gate: bool,
}

/// A change for the style engine.
pub(crate) enum Update {
    /// A link's sheet (entry key), parsed when layout applies it.
    Link(NodeId, Option<u32>),
    StyleImports(NodeId, Vec<Option<Rc<SheetTree>>>),
    StyleOverride(NodeId, Option<Rc<Stylesheet>>),
    Disabled(NodeId, bool),
}

impl CssState {
    pub(crate) fn has_owner(&self, n: NodeId) -> bool {
        self.owners.contains_key(&n)
    }
}

impl Update {
    pub(crate) fn apply(self, styles: &mut lb_css::StyleEngine, st: &mut CssState) {
        match self {
            Update::Link(n, k) => {
                let t = k.map(|k| build_tree(st, k));
                styles.set_external_tree(n, t)
            }
            Update::StyleImports(n, i) => styles.set_style_imports(n, i),
            Update::StyleOverride(n, s) => styles.set_style_override(n, s),
            Update::Disabled(n, d) => styles.set_sheet_disabled(n, d),
        }
    }
}

/// A `<link>` or `<style>` element with a sheet (or one loading).
struct Owner {
    link: bool,
    /// Bumped on every (re)load; stale completions are dropped.
    seq: u32,
    /// The link URL or style text the current sheet is for.
    key: String,
    loading: bool,
    script_blocking: bool,
    parser_blocking: bool,
    sheet: Option<u32>,
}

/// One sheet of the CSSOM: an owner's top-level sheet or an imported one.
struct SheetEntry {
    owner: Option<NodeId>,
    /// `(sheet, rule uid)` of the `@import` rule that loaded it.
    parent: Option<(u32, u32)>,
    href: Option<String>,
    origin_clean: bool,
    text: String,
    /// Parsed on first use (layout or the CSSOM).
    parsed: Option<Rc<Stylesheet>>,
    /// Sheet keys of the loaded imports, in import order.
    imports: Vec<Option<u32>>,
    /// The rule list, made on first CSSOM use.
    rules: Option<Vec<RuleEntry>>,
    disabled: bool,
}

struct RuleEntry {
    uid: u32,
    raw: RawRule,
    /// Declarations changed through `rule.style` (source text otherwise).
    decls: Option<String>,
    children: Option<Vec<RuleEntry>>,
}

/// A fetched sheet and its imports.
struct Loaded {
    url: Url,
    ok: bool,
    origin_clean: bool,
    text: String,
    /// Only when the text has `@import`s (to load them).
    parsed: Option<Rc<Stylesheet>>,
    imports: Vec<Option<Loaded>>,
}

impl Loaded {
    /// The sheet and every import loaded.
    fn all_ok(&self) -> bool {
        self.ok
            && self
                .imports
                .iter()
                .all(|i| i.as_ref().is_some_and(Loaded::all_ok))
    }
}

/// CSSOM host objects (one `Obj` variant).
pub(crate) enum CssObj {
    /// `document.styleSheets` / `shadowRoot.styleSheets` (the tree root).
    List(NodeId),
    Sheet(u32),
    /// `MediaList` of a sheet (`rule: None`) or of a rule.
    Media {
        sheet: u32,
        rule: Option<u32>,
    },
    /// `CSSRuleList` of a sheet (`rule: None`) or of a grouping rule.
    Rules {
        sheet: u32,
        rule: Option<u32>,
    },
    Rule {
        sheet: u32,
        uid: u32,
        kind: RuleKind,
    },
}

impl CssObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            CssObj::List(_) => "StyleSheetList",
            CssObj::Sheet(_) => "CSSStyleSheet",
            CssObj::Media { .. } => "MediaList",
            CssObj::Rules { .. } => "CSSRuleList",
            CssObj::Rule { kind, .. } => kind.iface(),
        }
    }
}

// ---------------------------------------------------------------------------
// Window API for the page layer
// ---------------------------------------------------------------------------

impl Window {
    /// Parser-inserted sheets still loading that block scripts.
    pub fn script_blocking_sheets(&self) -> u32 {
        self.css.borrow().script_blocking
    }

    /// Sheets inserted in `<body>` by the parser still loading: the parser
    /// waits for them.
    pub fn parser_blocking_sheets(&self) -> u32 {
        self.css.borrow().parser_blocking
    }

    /// Sheet `load` / `error` events queued and not yet fired.
    pub fn sheet_events_pending(&self) -> u32 {
        self.css.borrow().events
    }

    /// Set while a parser script waits for script-blocking sheets, and
    /// until it has run; sheet events wait meanwhile once no sheet blocks.
    pub fn set_script_waiting_for_sheets(&self, waiting: bool) {
        let changed = {
            let mut st = self.css.borrow_mut();
            std::mem::replace(&mut st.gate, waiting) != waiting
        };
        if changed && !waiting {
            self.wake();
        }
    }

    /// Does `query` match the window's media environment?
    pub(crate) fn media_matches(&self, query: &str) -> bool {
        if let Ok(mut l) = self.layout.try_borrow_mut()
            && let Some(l) = l.as_mut()
        {
            return l.styles().match_media(query).matches;
        }
        let env = match &self.profile {
            Some(p) => lb_css::MediaEnv::from_persona(p),
            None => lb_css::MediaEnv::default(),
        };
        lb_css::match_media(query, &env).matches
    }
}

// ---------------------------------------------------------------------------
// Owners: links and style elements
// ---------------------------------------------------------------------------

fn rel_tokens(d: &Document, n: NodeId) -> (bool, bool) {
    let rel = d.get_attribute(n, "rel").unwrap_or("");
    let has = |t: &str| {
        rel.split_ascii_whitespace()
            .any(|x| x.eq_ignore_ascii_case(t))
    };
    (has("stylesheet"), has("alternate"))
}

fn css_type(d: &Document, n: NodeId) -> bool {
    let ty = d.get_attribute(n, "type").unwrap_or("");
    ty.is_empty() || ty.eq_ignore_ascii_case("text/css")
}

/// A parser-inserted element whose sheet blocks scripts (and, in the
/// body, the parser): `(script_blocking, parser_blocking)`.
fn blocking(w: &Window, n: NodeId, alternate: bool) -> (bool, bool) {
    if alternate || w.parser.borrow().is_none() {
        return (false, false);
    }
    let (parser_made, media, body) = w.with_doc(|d| {
        (
            d.flags(n).contains(NodeFlags::PARSER_INSERTED),
            d.get_attribute(n, "media").map(str::to_owned),
            d.body().is_some(),
        )
    });
    if !parser_made || media.is_some_and(|m| !m.trim().is_empty() && !w.media_matches(&m)) {
        return (false, false);
    }
    (true, body)
}

/// Mutation hook: a link was inserted, removed or changed.
pub(crate) fn link_changed(w: &Rc<Window>, n: NodeId) {
    let info = w.with_doc(|d| {
        if !d.is_connected(n) || !d.is_html_element_named(n, &local_name!("link")) {
            return None;
        }
        let (sheet, alt) = rel_tokens(d, n);
        let href = d.get_attribute(n, "href").map(str::to_owned)?;
        (sheet && !href.trim().is_empty()).then_some((href, alt))
    });
    let Some((href, alternate)) = info else {
        remove_owner(w, n);
        return;
    };
    let Some(url) = w.resolve_url(&href) else {
        remove_owner(w, n);
        return;
    };
    let key = url.to_string();
    if w.css.borrow().owners.get(&n).is_some_and(|o| o.key == key) {
        return;
    }
    let (sb, pb) = blocking(w, n, alternate);
    let seq = begin(w, n, true, key, sb, pb);
    let referrer = w.url();
    let quirks = w.with_doc(|d| d.quirks_mode() == lb_dom::QuirksMode::Quirks);
    load_sheet(
        w,
        url,
        referrer,
        quirks,
        Vec::new(),
        Box::new(move |w, loaded| {
            w.queue_task(
                TaskSource::Networking,
                Box::new(move |_js, w| finish_link(w, n, seq, loaded)),
            );
        }),
    );
}

/// Mutation hook (and `</style>`): a style element was inserted, removed
/// or its text changed.
pub(crate) fn style_changed(w: &Rc<Window>, n: NodeId) {
    let text = w.with_doc(|d| {
        (d.is_connected(n) && d.is_html_element_named(n, &local_name!("style")) && css_type(d, n))
            .then(|| d.child_text_content(n))
    });
    let Some(text) = text else {
        remove_owner(w, n);
        return;
    };
    if w.css.borrow().owners.get(&n).is_some_and(|o| o.key == text) {
        return;
    }
    // New text: a new sheet (the CSSOM entry follows the text).
    let lower = text.to_ascii_lowercase();
    if lower.contains("url(") {
        w.bg.borrow_mut().hint = true;
    }
    let has_imports = lower.contains("@import");
    let parsed = has_imports.then(|| Rc::new(Stylesheet::parse(&text, Origin::Author)));
    let urls: Vec<Option<Url>> = parsed
        .as_ref()
        .map(|p| p.imports.iter().map(|i| w.resolve_url(&i.url)).collect())
        .unwrap_or_default();
    let (sb, pb) = if urls.is_empty() {
        (false, false)
    } else {
        blocking(w, n, false)
    };
    let seq = begin(w, n, false, text.clone(), sb, pb);
    {
        let mut st = w.css.borrow_mut();
        if let Some(k) = st.owners.get(&n).and_then(|o| o.sheet) {
            let e = &mut st.sheets[k as usize];
            e.text = text;
            e.parsed = parsed.clone();
            e.imports = Vec::new();
            e.rules = None;
        }
        st.updates.push(Update::StyleOverride(n, None));
        st.updates.push(Update::StyleImports(n, Vec::new()));
    }
    if urls.is_empty() {
        finish_style(w, n, seq, Vec::new());
        return;
    }
    let results: Rc<RefCell<Vec<Option<Loaded>>>> =
        Rc::new(RefCell::new((0..urls.len()).map(|_| None).collect()));
    let left = Rc::new(Cell::new(urls.len()));
    let referrer = w.url();
    let quirks = w.with_doc(|d| d.quirks_mode() == lb_dom::QuirksMode::Quirks);
    for (i, url) in urls.into_iter().enumerate() {
        let (results, left) = (results.clone(), left.clone());
        let done: Box<dyn FnOnce(&Rc<Window>, Loaded)> = Box::new(move |w, loaded| {
            results.borrow_mut()[i] = Some(loaded);
            left.set(left.get() - 1);
            if left.get() == 0 {
                let list = std::mem::take(&mut *results.borrow_mut());
                w.queue_task(
                    TaskSource::Networking,
                    Box::new(move |_js, w| finish_style(w, n, seq, list)),
                );
            }
        });
        match url {
            Some(u) => load_sheet(w, u, referrer.clone(), quirks, Vec::new(), done),
            None => {
                let bad = failed(referrer.clone());
                done(w, bad)
            }
        }
    }
}

/// An owner starts a new load: counters, `load` delay, sequence number.
fn begin(w: &Rc<Window>, n: NodeId, link: bool, key: String, sb: bool, pb: bool) -> u32 {
    cancel(w, n);
    let mut st = w.css.borrow_mut();
    if sb {
        st.script_blocking += 1;
    }
    if pb {
        st.parser_blocking += 1;
    }
    let o = st.owners.entry(n).or_insert(Owner {
        link,
        seq: 0,
        key: String::new(),
        loading: false,
        script_blocking: false,
        parser_blocking: false,
        sheet: None,
    });
    o.seq = o.seq.wrapping_add(1);
    o.key = key;
    o.loading = true;
    o.script_blocking = sb;
    o.parser_blocking = pb;
    let seq = o.seq;
    drop(st);
    w.pending_loads.set(w.pending_loads.get() + 1);
    seq
}

/// Stop an owner's load in progress (its completion is dropped).
fn cancel(w: &Rc<Window>, n: NodeId) {
    let was_loading = {
        let mut st = w.css.borrow_mut();
        let Some(o) = st.owners.get_mut(&n) else {
            return;
        };
        let loading = std::mem::replace(&mut o.loading, false);
        o.seq = o.seq.wrapping_add(1);
        let (sb, pb) = (
            std::mem::replace(&mut o.script_blocking, false),
            std::mem::replace(&mut o.parser_blocking, false),
        );
        if sb {
            st.script_blocking -= 1;
        }
        if pb {
            st.parser_blocking -= 1;
        }
        loading
    };
    if was_loading {
        w.pending_loads.set(w.pending_loads.get().saturating_sub(1));
        w.wake();
    }
}

fn remove_owner(w: &Rc<Window>, n: NodeId) {
    if !w.css.borrow().owners.contains_key(&n) {
        return;
    }
    cancel(w, n);
    let mut st = w.css.borrow_mut();
    let Some(o) = st.owners.remove(&n) else {
        return;
    };
    if let Some(k) = o.sheet {
        st.sheets[k as usize].owner = None;
    }
    if o.link {
        st.updates.push(Update::Link(n, None));
    } else {
        st.updates.push(Update::StyleImports(n, Vec::new()));
        st.updates.push(Update::StyleOverride(n, None));
    }
}

/// Mutation hook: nodes were removed; drop owners no longer connected.
pub(crate) fn nodes_removed(w: &Rc<Window>) {
    let gone: Vec<NodeId> = {
        let st = w.css.borrow();
        if st.owners.is_empty() {
            return;
        }
        w.with_doc(|d| {
            st.owners
                .keys()
                .copied()
                .filter(|&n| !d.is_connected(n))
                .collect()
        })
    };
    for n in gone {
        remove_owner(w, n);
    }
}

fn release(w: &Rc<Window>, o: &mut Owner) {
    let mut st = w.css.borrow_mut();
    if std::mem::replace(&mut o.script_blocking, false) {
        st.script_blocking -= 1;
    }
    if std::mem::replace(&mut o.parser_blocking, false) {
        st.parser_blocking -= 1;
    }
}

/// Take an owner out of the table while it is updated.
fn take_owner(w: &Window, n: NodeId, seq: u32) -> Option<Owner> {
    let mut st = w.css.borrow_mut();
    if st.owners.get(&n).is_none_or(|o| o.seq != seq) {
        return None;
    }
    st.owners.remove(&n)
}

fn finish_link(w: &Rc<Window>, n: NodeId, seq: u32, loaded: Loaded) {
    let Some(mut o) = take_owner(w, n, seq) else {
        return;
    };
    o.loading = false;
    release(w, &mut o);
    let ok = loaded.all_ok();
    let href = loaded.url.to_string();
    let key = {
        let mut st = w.css.borrow_mut();
        if let Some(old) = o.sheet {
            st.sheets[old as usize].owner = None;
        }
        let key = add_loaded(&mut st, loaded, Some(n), None, href);
        st.updates.push(Update::Link(n, Some(key)));
        st.owners.insert(n, o);
        key
    };
    if let Some(o) = w.css.borrow_mut().owners.get_mut(&n) {
        o.sheet = Some(key);
    }
    queue_event(w, n, ok);
}

fn finish_style(w: &Rc<Window>, n: NodeId, seq: u32, imports: Vec<Option<Loaded>>) {
    let Some(mut o) = take_owner(w, n, seq) else {
        return;
    };
    o.loading = false;
    release(w, &mut o);
    let ok = imports
        .iter()
        .all(|i| i.as_ref().is_some_and(Loaded::all_ok));
    {
        let mut st = w.css.borrow_mut();
        let has = !imports.is_empty();
        let keys: Vec<Option<u32>> = imports
            .into_iter()
            .map(|l| {
                l.map(|l| {
                    let href = l.url.to_string();
                    add_loaded(&mut st, l, None, None, href)
                })
            })
            .collect();
        if has {
            let sheet = match o.sheet {
                Some(k) => k,
                None => {
                    let k = new_style_entry(&mut st, n, &o.key);
                    o.sheet = Some(k);
                    k
                }
            };
            for (i, k) in keys.iter().enumerate() {
                if let Some(k) = k {
                    st.sheets[*k as usize].parent = Some((sheet, import_uid(&mut st, sheet, i)));
                }
            }
            let trees: Vec<Option<Rc<SheetTree>>> = keys
                .iter()
                .map(|k| k.map(|k| build_tree(&mut st, k)))
                .collect();
            st.sheets[sheet as usize].imports = keys;
            st.updates.push(Update::StyleImports(n, trees));
        }
        st.owners.insert(n, o);
    }
    queue_event(w, n, ok);
}

/// Queue the owner's `load` / `error` event (see the module docs).
fn queue_event(w: &Rc<Window>, n: NodeId, ok: bool) {
    w.css.borrow_mut().events += 1;
    w.queue_task(TaskSource::Networking, sheet_event(n, ok));
    w.wake();
}

fn sheet_event(n: NodeId, ok: bool) -> super::TaskFn {
    Box::new(move |js, w| {
        let wait = {
            let st = w.css.borrow();
            st.gate && st.script_blocking == 0
        };
        if wait {
            w.queue_task(TaskSource::Networking, sheet_event(n, ok));
            return;
        }
        fire_event(
            js,
            w,
            w.node_ref(0, n),
            if ok { "load" } else { "error" },
            false,
            false,
        );
        w.css.borrow_mut().events -= 1;
        // New rules may show background images (Chrome requests them at
        // the next style update).
        w.load_background_images();
        w.pending_loads.set(w.pending_loads.get().saturating_sub(1));
        w.wake();
    })
}

// ---------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------

fn failed(url: Url) -> Loaded {
    Loaded {
        url,
        ok: false,
        origin_clean: true,
        text: String::new(),
        parsed: None,
        imports: Vec::new(),
    }
}

/// Fetch `url` and its imports (in parallel), then call `done` (on the
/// page's worker, outside any task).
fn load_sheet(
    w: &Rc<Window>,
    url: Url,
    referrer: Url,
    quirks: bool,
    chain: Vec<String>,
    done: Box<dyn FnOnce(&Rc<Window>, Loaded)>,
) {
    let doc_origin = w.url().origin();
    if url.scheme() == "data" {
        let r = super::image::data_url(&url).map(|(body, mime)| NetResponse {
            status: 200,
            status_text: "OK".into(),
            headers: vec![("content-type".into(), mime)],
            body: body.into(),
            url: url.clone(),
            redirected: false,
        });
        let w = w.clone();
        let w2 = w.clone();
        w.net.spawn(Box::pin(async move {
            got_sheet(
                &w2,
                url,
                r.ok_or_else(String::new),
                doc_origin,
                quirks,
                chain,
                done,
            );
        }));
        return;
    }
    if !matches!(url.scheme(), "http" | "https") {
        let w2 = w.clone();
        w.net.spawn(Box::pin(async move {
            done(&w2, failed(url));
        }));
        return;
    }
    let req = NetRequest::get(url.clone(), Destination::Style, Some(referrer));
    w.in_flight.set(w.in_flight.get() + 1);
    let fut = w.net_fetch(req);
    let weak = Rc::downgrade(w);
    w.net.spawn(Box::pin(async move {
        let r = fut.await;
        let Some(w) = weak.upgrade() else { return };
        w.in_flight.set(w.in_flight.get().saturating_sub(1));
        if w.is_closed() {
            return;
        }
        got_sheet(&w, url, r, doc_origin, quirks, chain, done);
    }));
}

fn got_sheet(
    w: &Rc<Window>,
    url: Url,
    r: Result<NetResponse, String>,
    doc_origin: url::Origin,
    quirks: bool,
    mut chain: Vec<String>,
    done: Box<dyn FnOnce(&Rc<Window>, Loaded)>,
) {
    let resp = match r {
        Ok(r) if (200..300).contains(&r.status) => r,
        _ => {
            done(w, failed(url));
            return;
        }
    };
    let same = resp.url.origin() == doc_origin || resp.url.scheme() == "data";
    let ct = resp.header("content-type");
    let essence = ct
        .map(|c| {
            c.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    let css_ok = ct.is_none() || essence == "text/css" || (quirks && same);
    let text = if css_ok {
        crate::host::decode_text(&resp.body, ct.or(Some("text/css")), false).0
    } else {
        String::new()
    };
    if text.to_ascii_lowercase().contains("url(") {
        w.bg.borrow_mut().hint = true;
    }
    let base = resp.url.clone();
    // Parse now only to find `@import`s; otherwise when layout needs it.
    let parsed = text.to_ascii_lowercase().contains("@import").then(|| {
        let mut sheet = Stylesheet::parse(&text, Origin::Author);
        sheet.resolve_urls(&base);
        Rc::new(sheet)
    });
    let mut loaded = Loaded {
        url,
        ok: true,
        origin_clean: same,
        text,
        parsed: parsed.clone(),
        imports: Vec::new(),
    };
    let Some(parsed) = parsed.filter(|p| !p.imports.is_empty()) else {
        done(w, loaded);
        return;
    };
    chain.push(loaded.url.to_string());
    let n = parsed.imports.len();
    let results: Rc<RefCell<Vec<Option<Loaded>>>> =
        Rc::new(RefCell::new((0..n).map(|_| None).collect()));
    let left = Rc::new(Cell::new(n));
    let parent: Rc<RefCell<Option<(Loaded, Box<dyn FnOnce(&Rc<Window>, Loaded)>)>>> =
        Rc::new(RefCell::new(None));
    loaded.imports = Vec::new();
    *parent.borrow_mut() = Some((loaded, done));
    for (i, imp) in parsed.imports.iter().enumerate() {
        let (results, left, parent) = (results.clone(), left.clone(), parent.clone());
        let child_done: Box<dyn FnOnce(&Rc<Window>, Loaded)> = Box::new(move |w, l| {
            results.borrow_mut()[i] = Some(l);
            left.set(left.get() - 1);
            if left.get() == 0
                && let Some((mut me, done)) = parent.borrow_mut().take()
            {
                me.imports = std::mem::take(&mut *results.borrow_mut());
                done(w, me);
            }
        });
        let target = base.join(imp.url.trim()).ok();
        match target {
            Some(t) if chain.len() < MAX_DEPTH && !chain.contains(&t.to_string()) => {
                load_sheet(w, t, base.clone(), quirks, chain.clone(), child_done)
            }
            Some(t) => {
                // A cycle or too deep: Chrome ignores the import.
                let mut l = failed(t);
                l.ok = true;
                child_done(w, l)
            }
            None => child_done(w, failed(base.clone())),
        }
    }
}

// ---------------------------------------------------------------------------
// Sheet entries
// ---------------------------------------------------------------------------

fn add_loaded(
    st: &mut CssState,
    l: Loaded,
    owner: Option<NodeId>,
    parent: Option<(u32, u32)>,
    href: String,
) -> u32 {
    let key = st.sheets.len() as u32;
    st.sheets.push(SheetEntry {
        owner,
        parent,
        href: Some(href),
        origin_clean: l.origin_clean,
        text: l.text,
        parsed: l.parsed,
        imports: Vec::new(),
        rules: None,
        disabled: false,
    });
    let mut kids = Vec::new();
    for (i, c) in l.imports.into_iter().enumerate() {
        kids.push(c.map(|c| {
            let href = c.url.to_string();
            let uid = import_uid(st, key, i);
            add_loaded(st, c, None, Some((key, uid)), href)
        }));
    }
    st.sheets[key as usize].imports = kids;
    key
}

fn new_style_entry(st: &mut CssState, n: NodeId, text: &str) -> u32 {
    let key = st.sheets.len() as u32;
    st.sheets.push(SheetEntry {
        owner: Some(n),
        parent: None,
        href: None,
        origin_clean: true,
        text: text.to_owned(),
        parsed: None,
        imports: Vec::new(),
        rules: None,
        disabled: false,
    });
    key
}

/// Uid of the `i`-th `@import` rule of `sheet` (makes the rule list).
fn import_uid(st: &mut CssState, sheet: u32, i: usize) -> u32 {
    ensure_rules(st, sheet);
    st.sheets[sheet as usize]
        .rules
        .as_ref()
        .and_then(|r| {
            r.iter()
                .filter(|e| e.raw.kind == RuleKind::Import)
                .nth(i)
                .map(|e| e.uid)
        })
        .unwrap_or(u32::MAX)
}

/// The parsed sheet of an entry (parsed now if needed; `url()`s resolve
/// against the sheet's URL).
fn sheet_of(st: &mut CssState, key: u32) -> Rc<Stylesheet> {
    let e = &mut st.sheets[key as usize];
    if let Some(p) = &e.parsed {
        return p.clone();
    }
    let mut sheet = Stylesheet::parse(&e.text, Origin::Author);
    if let Some(base) = e.href.as_deref().and_then(|h| Url::parse(h).ok()) {
        sheet.resolve_urls(&base);
    }
    let p = Rc::new(sheet);
    e.parsed = Some(p.clone());
    p
}

pub(crate) fn build_tree(st: &mut CssState, key: u32) -> Rc<SheetTree> {
    let sheet = sheet_of(st, key);
    let imports: Vec<Option<u32>> = st.sheets[key as usize].imports.clone();
    Rc::new(SheetTree {
        sheet,
        imports: imports
            .into_iter()
            .map(|k| k.map(|k| build_tree(st, k)))
            .collect(),
    })
}

fn entries(st: &mut CssState, raws: Vec<RawRule>) -> Vec<RuleEntry> {
    raws.into_iter()
        .map(|raw| {
            st.next_uid += 1;
            RuleEntry {
                uid: st.next_uid,
                raw,
                decls: None,
                children: None,
            }
        })
        .collect()
}

fn ensure_rules(st: &mut CssState, key: u32) {
    if st.sheets[key as usize].rules.is_none() {
        let raws = split_rules(&st.sheets[key as usize].text);
        let r = entries(st, raws);
        st.sheets[key as usize].rules = Some(r);
    }
}

/// Find a rule by uid (making nested lists on the way as needed).
fn find_rule(list: &mut [RuleEntry], uid: u32) -> Option<&mut RuleEntry> {
    for e in list.iter_mut() {
        if e.uid == uid {
            return Some(e);
        }
        if let Some(c) = e.children.as_mut()
            && let Some(r) = find_rule(c, uid)
        {
            return Some(r);
        }
    }
    None
}

fn with_rule<R>(
    w: &Window,
    sheet: u32,
    uid: u32,
    f: impl FnOnce(&mut RuleEntry) -> R,
) -> Option<R> {
    let mut st = w.css.borrow_mut();
    ensure_rules(&mut st, sheet);
    let rules = st.sheets.get_mut(sheet as usize)?.rules.as_mut()?;
    find_rule(rules, uid).map(f)
}

/// The rules of a sheet (`rule: None`) or grouping rule: `(uid, kind)`.
fn rule_list(w: &Window, sheet: u32, rule: Option<u32>) -> Vec<(u32, RuleKind)> {
    let mut st = w.css.borrow_mut();
    ensure_rules(&mut st, sheet);
    let st = &mut *st;
    let Some(rules) = st
        .sheets
        .get_mut(sheet as usize)
        .and_then(|s| s.rules.as_mut())
    else {
        return Vec::new();
    };
    let list: &mut Vec<RuleEntry> = match rule {
        None => rules,
        Some(uid) => {
            let Some(e) = find_rule(rules, uid) else {
                return Vec::new();
            };
            if e.children.is_none() {
                let raws = if e.raw.kind.is_grouping() {
                    split_rules(e.raw.body.as_deref().unwrap_or(""))
                } else {
                    Vec::new()
                };
                let mut next = st.next_uid;
                let kids = raws
                    .into_iter()
                    .map(|raw| {
                        next += 1;
                        RuleEntry {
                            uid: next,
                            raw,
                            decls: None,
                            children: None,
                        }
                    })
                    .collect();
                st.next_uid = next;
                e.children = Some(kids);
            }
            e.children.as_mut().expect("children")
        }
    };
    list.iter().map(|e| (e.uid, e.raw.kind)).collect()
}

/// Declarations as the CSSOM serializes them: valid ones only, each
/// ending in `;`.
/// Descriptor rules (`@font-face`, `@page`, ...) keep all of theirs.
fn clean_decls(text: &str, kind: RuleKind) -> Vec<(String, String, bool)> {
    super::style::parse_decls(text)
        .into_iter()
        .filter(|(n, v, _)| {
            kind != RuleKind::Style || n.starts_with("--") || lb_css::is_valid_declaration(n, v)
        })
        .collect()
}

fn entry_text(e: &RuleEntry) -> String {
    match &e.children {
        Some(kids) if e.raw.kind.is_grouping() => {
            let mut raw = e.raw.clone();
            raw.body = Some(kids.iter().map(entry_text).collect::<Vec<_>>().join("\n"));
            rule_text(&raw, None)
        }
        _ if e.raw.kind.has_declarations() => {
            let text = e
                .decls
                .clone()
                .unwrap_or_else(|| e.raw.body.clone().unwrap_or_default());
            let d = super::style::serialize_decls(&clean_decls(&text, e.raw.kind));
            rule_text(&e.raw, Some(&d))
        }
        _ => rule_text(&e.raw, None),
    }
}

/// After a CSSOM change: new text, new parse, the engine gets the sheet.
fn sheet_modified(w: &Rc<Window>, sheet: u32) {
    let mut st = w.css.borrow_mut();
    let text = st.sheets[sheet as usize]
        .rules
        .as_ref()
        .map(|r| r.iter().map(entry_text).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default();
    let mut parsed = Stylesheet::parse(&text, Origin::Author);
    if let Some(base) = st.sheets[sheet as usize]
        .href
        .as_deref()
        .and_then(|h| Url::parse(h).ok())
    {
        parsed.resolve_urls(&base);
    }
    let parsed = Rc::new(parsed);
    {
        let e = &mut st.sheets[sheet as usize];
        // Imports keep their loaded sheets by position.
        e.imports.resize(parsed.imports.len(), None);
        e.text = text;
        e.parsed = Some(parsed.clone());
    }
    // The top-level sheet and its owner.
    let mut top = sheet;
    while let Some((p, _)) = st.sheets[top as usize].parent {
        top = p;
    }
    let Some(owner) = st.sheets[top as usize].owner else {
        return;
    };
    let link = st.owners.get(&owner).is_some_and(|o| o.link);
    if link {
        st.updates.push(Update::Link(owner, Some(top)));
    } else {
        let tree = build_tree(&mut st, top);
        st.updates
            .push(Update::StyleOverride(owner, Some(tree.sheet.clone())));
        st.updates
            .push(Update::StyleImports(owner, tree.imports.clone()));
    }
    drop(st);
    w.wake();
}

// ---------------------------------------------------------------------------
// The document's sheets
// ---------------------------------------------------------------------------

/// The sheet key of a style element (making its entry).
fn style_sheet(w: &Rc<Window>, n: NodeId) -> Option<u32> {
    let text = w.with_doc(|d| {
        (d.is_connected(n) && d.is_html_element_named(n, &local_name!("style")) && css_type(d, n))
            .then(|| d.child_text_content(n))
    })?;
    if !w.css.borrow().owners.contains_key(&n) {
        style_changed(w, n);
    }
    let mut st = w.css.borrow_mut();
    let o = st.owners.get(&n)?;
    if let Some(k) = o.sheet {
        return Some(k);
    }
    let k = new_style_entry(&mut st, n, &text);
    if let Some(o) = st.owners.get_mut(&n) {
        o.sheet = Some(k);
    }
    Some(k)
}

/// The sheet of `n` (`link.sheet`, `style.sheet`).
fn owner_sheet(w: &Rc<Window>, n: NodeId) -> Option<u32> {
    let is_style = w.with_doc(|d| d.is_html_element_named(n, &local_name!("style")));
    if is_style {
        return style_sheet(w, n);
    }
    let st = w.css.borrow();
    let o = st.owners.get(&n)?;
    if !o.link {
        return None;
    }
    o.sheet
}

/// `document.styleSheets`: sheet keys in tree order.
fn document_sheets(w: &Rc<Window>, root: NodeId) -> Vec<u32> {
    let nodes: Vec<(NodeId, bool)> = w.with_doc(|d| {
        d.descendants(root)
            .filter_map(|n| {
                if d.is_html_element_named(n, &local_name!("style")) {
                    css_type(d, n).then_some((n, true))
                } else if d.is_html_element_named(n, &local_name!("link")) {
                    (!d.has_attribute(n, "disabled")).then_some((n, false))
                } else {
                    None
                }
            })
            .collect()
    });
    nodes
        .into_iter()
        .filter_map(|(n, style)| {
            if style {
                style_sheet(w, n)
            } else {
                owner_sheet(w, n)
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

fn with_css<R>(w: &Window, id: u32, f: impl FnOnce(&CssObj) -> Option<R>) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Css(c) => f(c),
        _ => None,
    })
    .flatten()
}

fn this_sheet(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    with_css(w, id, |c| match c {
        CssObj::Sheet(k) => Some(*k),
        _ => None,
    })
    .ok_or_else(illegal)
}

fn this_rule(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, u32, RuleKind), HostError> {
    let id = this_obj(cx)?;
    with_css(w, id, |c| match c {
        CssObj::Rule { sheet, uid, kind } => Some((*sheet, *uid, *kind)),
        _ => None,
    })
    .ok_or_else(illegal)
}

fn sheet_arg(w: &Window, key: u32) -> Arg {
    let id = w.cached_obj((0, key, "cssSheet", String::new()), || {
        Obj::Css(Box::new(CssObj::Sheet(key)))
    });
    w.obj_arg(id)
}

fn rule_arg(w: &Window, sheet: u32, uid: u32, kind: RuleKind) -> Arg {
    let id = w.cached_obj((sheet, uid, "cssRule", String::new()), || {
        Obj::Css(Box::new(CssObj::Rule { sheet, uid, kind }))
    });
    w.obj_arg(id)
}

fn rules_arg(w: &Window, sheet: u32, rule: Option<u32>) -> Arg {
    let id = w.cached_obj(
        (sheet, rule.unwrap_or(0), "cssRules", String::new()),
        || Obj::Css(Box::new(CssObj::Rules { sheet, rule })),
    );
    w.obj_arg(id)
}

fn media_arg(w: &Window, sheet: u32, rule: Option<u32>) -> Arg {
    let id = w.cached_obj(
        (sheet, rule.unwrap_or(0), "cssMedia", String::new()),
        || Obj::Css(Box::new(CssObj::Media { sheet, rule })),
    );
    w.obj_arg(id)
}

fn sheet_owner(w: &Window, key: u32) -> Option<NodeId> {
    w.css
        .borrow()
        .sheets
        .get(key as usize)
        .and_then(|e| e.owner)
}

/// The media text of a `MediaList`.
fn media_text(w: &Window, sheet: u32, rule: Option<u32>) -> String {
    match rule {
        Some(uid) => with_rule(w, sheet, uid, |e| e.raw.prelude.clone()).unwrap_or_default(),
        None => {
            let parent = w
                .css
                .borrow()
                .sheets
                .get(sheet as usize)
                .and_then(|e| e.parent);
            if let Some((p, uid)) = parent {
                return with_rule(w, p, uid, |e| e.raw.prelude.clone()).unwrap_or_default();
            }
            sheet_owner(w, sheet)
                .and_then(|n| w.with_doc(|d| d.get_attribute(n, "media").map(str::to_owned)))
                .unwrap_or_default()
        }
    }
}

fn split_media(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in text.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth <= 0 => {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.split_ascii_whitespace().collect::<Vec<_>>().join(" "));
                }
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.split_ascii_whitespace().collect::<Vec<_>>().join(" "));
    }
    out
}

fn this_media(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, Option<u32>), HostError> {
    let id = this_obj(cx)?;
    with_css(w, id, |c| match c {
        CssObj::Media { sheet, rule } => Some((*sheet, *rule)),
        _ => None,
    })
    .ok_or_else(illegal)
}

fn this_rules(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, Option<u32>), HostError> {
    let id = this_obj(cx)?;
    with_css(w, id, |c| match c {
        CssObj::Rules { sheet, rule } => Some((*sheet, *rule)),
        _ => None,
    })
    .ok_or_else(illegal)
}

/// Declarations of a rule (`rule.style`), as `(name, value, important)`.
pub(crate) fn rule_decls(w: &Window, sheet: u32, uid: u32) -> Vec<(String, String, bool)> {
    with_rule(w, sheet, uid, |e| {
        let text = e
            .decls
            .clone()
            .unwrap_or_else(|| e.raw.body.clone().unwrap_or_default());
        clean_decls(&text, e.raw.kind)
    })
    .unwrap_or_default()
}

/// Property names of a rule's declarations with shorthands expanded
/// (`rule.style.length` / `item()`).
pub(crate) fn rule_longhands(w: &Window, sheet: u32, uid: u32) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (n, v, _) in rule_decls(w, sheet, uid) {
        let longs = if n.starts_with("--") {
            Vec::new()
        } else {
            lb_css::declaration_longhands(&n, &v)
        };
        if longs.is_empty() {
            if !out.contains(&n) {
                out.push(n);
            }
        } else {
            for l in longs {
                if !out.iter().any(|x| x == l) {
                    out.push(l.to_owned());
                }
            }
        }
    }
    out
}

/// Set a rule's declarations (`rule.style` changes).
pub(crate) fn set_rule_decls(w: &Rc<Window>, sheet: u32, uid: u32, text: String) {
    let ok = with_rule(w, sheet, uid, |e| e.decls = Some(text)).is_some();
    if ok {
        sheet_modified(w, sheet);
    }
}

fn origin_clean(w: &Window, sheet: u32) -> bool {
    w.css
        .borrow()
        .sheets
        .get(sheet as usize)
        .is_none_or(|e| e.origin_clean)
}

fn rules_access(w: &Window, sheet: u32, prop: &str) -> Result<(), HostError> {
    if origin_clean(w, sheet) {
        Ok(())
    } else {
        Err(w.rt.err(
            "SecurityError",
            format!(
                "Failed to read the '{prop}' property from 'CSSStyleSheet': Cannot access rules"
            ),
        ))
    }
}

fn insert_rule(
    w: &Rc<Window>,
    sheet: u32,
    parent: Option<u32>,
    text: &str,
    index: u32,
    iface: &str,
) -> Result<u32, HostError> {
    let len = rule_list(w, sheet, parent).len() as u32;
    if index > len {
        return Err(w.rt.err(
            "IndexSizeError",
            format!(
                "Failed to execute 'insertRule' on '{iface}': The index provided ({index}) is larger than the maximum index ({len})."
            ),
        ));
    }
    let Some(raw) = parse_one_rule(text) else {
        return Err(w.rt.err(
            "SyntaxError",
            format!(
                "Failed to execute 'insertRule' on '{iface}': Failed to parse the rule '{text}'."
            ),
        ));
    };
    if raw.kind == RuleKind::Import && parent.is_none() {
        let before_ok = rule_list(w, sheet, None)
            .iter()
            .take(index as usize)
            .all(|(_, k)| *k == RuleKind::Import);
        if !before_ok {
            return Err(w.rt.err(
                "HierarchyRequestError",
                format!("Failed to execute 'insertRule' on '{iface}': Failed to insert the rule."),
            ));
        }
    }
    {
        let mut st = w.css.borrow_mut();
        st.next_uid += 1;
        let uid = st.next_uid;
        let rules = st.sheets[sheet as usize].rules.as_mut().expect("rules");
        let list = match parent {
            None => Some(rules),
            Some(p) => find_rule(rules, p).and_then(|e| e.children.as_mut()),
        };
        if let Some(list) = list {
            list.insert(
                index as usize,
                RuleEntry {
                    uid,
                    raw,
                    decls: None,
                    children: None,
                },
            );
        }
    }
    sheet_modified(w, sheet);
    Ok(index)
}

fn delete_rule(
    w: &Rc<Window>,
    sheet: u32,
    parent: Option<u32>,
    index: u32,
    iface: &str,
) -> Result<(), HostError> {
    let len = rule_list(w, sheet, parent).len() as u32;
    if index >= len {
        return Err(w.rt.err(
            "IndexSizeError",
            format!(
                "Failed to execute 'deleteRule' on '{iface}': The index provided ({index}) is larger than the maximum index ({}).",
                len.saturating_sub(1)
            ),
        ));
    }
    {
        let mut st = w.css.borrow_mut();
        let rules = st.sheets[sheet as usize].rules.as_mut().expect("rules");
        let list = match parent {
            None => Some(rules),
            Some(p) => find_rule(rules, p).and_then(|e| e.children.as_mut()),
        };
        if let Some(list) = list {
            let removed = list.remove(index as usize);
            if removed.raw.kind == RuleKind::Import && parent.is_none() {
                let k = list
                    .iter()
                    .take(index as usize)
                    .filter(|e| e.raw.kind == RuleKind::Import)
                    .count();
                let imps = &mut st.sheets[sheet as usize].imports;
                if k < imps.len() {
                    imps.remove(k);
                }
            }
        }
    }
    sheet_modified(w, sheet);
    Ok(())
}

fn index_arg(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Result<u32, HostError> {
    if i >= cx.len() || cx.arg_kind(i) == lb_js::ValueKind::Undefined {
        return Ok(0);
    }
    cx.arg_u32(i)
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Document", "styleSheets", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let id = w.cached_obj((slot, 0, "styleSheets", String::new()), || {
            Obj::Css(Box::new(CssObj::List(Document::DOCUMENT)))
        });
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "ShadowRoot", "styleSheets", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let id = w.cached_obj((slot, n.0, "styleSheets", String::new()), || {
            Obj::Css(Box::new(CssObj::List(n)))
        });
        cx.ret(w.obj_arg(id))
    });
    fn list_root(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<NodeId, HostError> {
        let id = super::this_obj(cx)?;
        with_css(w, id, |c| match c {
            CssObj::List(r) => Some(*r),
            _ => None,
        })
        .ok_or_else(super::illegal)
    }
    for iface in ["HTMLLinkElement", "HTMLStyleElement"] {
        getter(reg, rt, iface, "sheet", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            if slot != 0 {
                return cx.ret(Arg::Null);
            }
            match owner_sheet(w, n) {
                Some(k) => cx.ret(sheet_arg(w, k)),
                None => cx.ret(Arg::Null),
            }
        });
    }

    // StyleSheetList
    getter(reg, rt, "StyleSheetList", "length", |cx, w| {
        let root = list_root(cx, w)?;
        cx.ret(document_sheets(w, root).len() as u32)
    });
    method(reg, rt, "StyleSheetList", "item", |cx, w| {
        need_args(cx, 1, "item", "StyleSheetList")?;
        let i = cx.arg_u32(0)? as usize;
        let root = list_root(cx, w)?;
        match document_sheets(w, root).get(i) {
            Some(&k) => cx.ret(sheet_arg(w, k)),
            None => cx.ret(Arg::Null),
        }
    });
    let rt1 = rt.clone();
    reg.indexed_getter(
        "StyleSheetList",
        Rc::new(move |cx, i| {
            let Some(w) = rt1.window(cx.holder_realm()) else {
                return Ok(None);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(None);
            };
            let Some(root) = with_css(&w, h.id, |c| match c {
                CssObj::List(r) => Some(*r),
                _ => None,
            }) else {
                return Ok(None);
            };
            Ok(document_sheets(&w, root)
                .get(i as usize)
                .map(|&k| sheet_arg(&w, k)))
        }),
    );

    // StyleSheet / CSSStyleSheet
    getter(reg, rt, "StyleSheet", "type", |cx, w| {
        this_sheet(cx, w)?;
        cx.ret("text/css")
    });
    getter(reg, rt, "StyleSheet", "href", |cx, w| {
        let k = this_sheet(cx, w)?;
        let href = w.css.borrow().sheets[k as usize].href.clone();
        cx.ret(href.map_or(Arg::Null, Arg::String))
    });
    getter(reg, rt, "StyleSheet", "ownerNode", |cx, w| {
        let k = this_sheet(cx, w)?;
        let n = sheet_owner(w, k);
        cx.ret(w.opt_node_arg(0, n))
    });
    getter(reg, rt, "StyleSheet", "parentStyleSheet", |cx, w| {
        let k = this_sheet(cx, w)?;
        let p = w.css.borrow().sheets[k as usize].parent;
        cx.ret(p.map_or(Arg::Null, |(p, _)| sheet_arg(w, p)))
    });
    getter(reg, rt, "StyleSheet", "title", |cx, w| {
        let k = this_sheet(cx, w)?;
        let t = sheet_owner(w, k)
            .and_then(|n| w.with_doc(|d| d.get_attribute(n, "title").map(str::to_owned)))
            .filter(|t| !t.is_empty());
        cx.ret(t.map_or(Arg::Null, Arg::String))
    });
    getter(reg, rt, "StyleSheet", "media", |cx, w| {
        let k = this_sheet(cx, w)?;
        cx.ret(media_arg(w, k, None))
    });
    getter(reg, rt, "StyleSheet", "disabled", |cx, w| {
        let k = this_sheet(cx, w)?;
        let (own, owner) = {
            let st = w.css.borrow();
            let e = &st.sheets[k as usize];
            (e.disabled, e.owner)
        };
        let alt = owner.is_some_and(|n| {
            w.with_doc(|d| d.is_html_element_named(n, &local_name!("link")) && rel_tokens(d, n).1)
        });
        cx.ret(own || alt)
    });
    setter(reg, rt, "StyleSheet", "disabled", |cx, w| {
        let k = this_sheet(cx, w)?;
        let v = cx.arg_bool(0);
        let mut st = w.css.borrow_mut();
        st.sheets[k as usize].disabled = v;
        if let Some(n) = st.sheets[k as usize].owner {
            st.updates.push(Update::Disabled(n, v));
        }
        Ok(())
    });
    getter(reg, rt, "CSSStyleSheet", "ownerRule", |cx, w| {
        let k = this_sheet(cx, w)?;
        let p = w.css.borrow().sheets[k as usize].parent;
        cx.ret(p.map_or(Arg::Null, |(s, uid)| rule_arg(w, s, uid, RuleKind::Import)))
    });
    for name in ["cssRules", "rules"] {
        getter(reg, rt, "CSSStyleSheet", name, move |cx, w| {
            let k = this_sheet(cx, w)?;
            rules_access(w, k, name)?;
            cx.ret(rules_arg(w, k, None))
        });
    }
    method(reg, rt, "CSSStyleSheet", "insertRule", |cx, w| {
        need_args(cx, 1, "insertRule", "CSSStyleSheet")?;
        let k = this_sheet(cx, w)?;
        rules_access(w, k, "cssRules")?;
        let text = cx.arg_string(0)?;
        let i = index_arg(cx, 1)?;
        let r = insert_rule(w, k, None, &text, i, "CSSStyleSheet")?;
        cx.ret(r)
    });
    method(reg, rt, "CSSStyleSheet", "deleteRule", |cx, w| {
        need_args(cx, 1, "deleteRule", "CSSStyleSheet")?;
        let k = this_sheet(cx, w)?;
        rules_access(w, k, "cssRules")?;
        let i = cx.arg_u32(0)?;
        delete_rule(w, k, None, i, "CSSStyleSheet")
    });
    method(reg, rt, "CSSStyleSheet", "addRule", |cx, w| {
        let k = this_sheet(cx, w)?;
        rules_access(w, k, "cssRules")?;
        let sel = super::opt_string(cx, 0)?.unwrap_or_else(|| "undefined".into());
        let style = super::opt_string(cx, 1)?.unwrap_or_else(|| "undefined".into());
        let len = rule_list(w, k, None).len() as u32;
        let i = if cx.len() > 2 && cx.arg_kind(2) != lb_js::ValueKind::Undefined {
            cx.arg_u32(2)?
        } else {
            len
        };
        let text = if style.is_empty() {
            format!("{sel} {{ }}")
        } else {
            format!("{sel} {{ {style} }}")
        };
        insert_rule(w, k, None, &text, i, "CSSStyleSheet")?;
        cx.ret(-1)
    });
    method(reg, rt, "CSSStyleSheet", "removeRule", |cx, w| {
        let k = this_sheet(cx, w)?;
        rules_access(w, k, "cssRules")?;
        let i = index_arg(cx, 0)?;
        delete_rule(w, k, None, i, "CSSStyleSheet")
    });

    // MediaList
    getter(reg, rt, "MediaList", "mediaText", |cx, w| {
        let (s, r) = this_media(cx, w)?;
        cx.ret(split_media(&media_text(w, s, r)).join(", "))
    });
    getter(reg, rt, "MediaList", "length", |cx, w| {
        let (s, r) = this_media(cx, w)?;
        cx.ret(split_media(&media_text(w, s, r)).len() as u32)
    });
    method(reg, rt, "MediaList", "item", |cx, w| {
        need_args(cx, 1, "item", "MediaList")?;
        let (s, r) = this_media(cx, w)?;
        let i = cx.arg_u32(0)? as usize;
        match split_media(&media_text(w, s, r)).get(i) {
            Some(m) => cx.ret(m.clone()),
            None => cx.ret(Arg::Null),
        }
    });
    let rt2 = rt.clone();
    reg.indexed_getter(
        "MediaList",
        Rc::new(move |cx, i| {
            let Some(w) = rt2.window(cx.holder_realm()) else {
                return Ok(None);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(None);
            };
            let Some((s, r)) = with_css(&w, h.id, |c| match c {
                CssObj::Media { sheet, rule } => Some((*sheet, *rule)),
                _ => None,
            }) else {
                return Ok(None);
            };
            Ok(split_media(&media_text(&w, s, r))
                .get(i as usize)
                .map(|m| Arg::String(m.clone())))
        }),
    );

    // CSSRuleList
    getter(reg, rt, "CSSRuleList", "length", |cx, w| {
        let (s, r) = this_rules(cx, w)?;
        cx.ret(rule_list(w, s, r).len() as u32)
    });
    method(reg, rt, "CSSRuleList", "item", |cx, w| {
        need_args(cx, 1, "item", "CSSRuleList")?;
        let (s, r) = this_rules(cx, w)?;
        let i = cx.arg_u32(0)? as usize;
        match rule_list(w, s, r).get(i) {
            Some(&(uid, kind)) => cx.ret(rule_arg(w, s, uid, kind)),
            None => cx.ret(Arg::Null),
        }
    });
    let rt3 = rt.clone();
    reg.indexed_getter(
        "CSSRuleList",
        Rc::new(move |cx, i| {
            let Some(w) = rt3.window(cx.holder_realm()) else {
                return Ok(None);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(None);
            };
            let Some((s, r)) = with_css(&w, h.id, |c| match c {
                CssObj::Rules { sheet, rule } => Some((*sheet, *rule)),
                _ => None,
            }) else {
                return Ok(None);
            };
            Ok(rule_list(&w, s, r)
                .get(i as usize)
                .map(|&(uid, kind)| rule_arg(&w, s, uid, kind)))
        }),
    );

    // CSSRule and subclasses
    getter(reg, rt, "CSSRule", "type", |cx, w| {
        let (_, _, kind) = this_rule(cx, w)?;
        cx.ret(kind.type_num() as u32)
    });
    getter(reg, rt, "CSSRule", "cssText", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        let t = with_rule(w, s, uid, |e| entry_text(e)).unwrap_or_default();
        cx.ret(t)
    });
    getter(reg, rt, "CSSRule", "parentStyleSheet", |cx, w| {
        let (s, _, _) = this_rule(cx, w)?;
        cx.ret(sheet_arg(w, s))
    });
    getter(reg, rt, "CSSRule", "parentRule", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        let parent = {
            let mut st = w.css.borrow_mut();
            ensure_rules(&mut st, s);
            fn parent_of(list: &[RuleEntry], uid: u32) -> Option<(u32, RuleKind)> {
                for e in list {
                    if let Some(kids) = &e.children {
                        if kids.iter().any(|k| k.uid == uid) {
                            return Some((e.uid, e.raw.kind));
                        }
                        if let Some(p) = parent_of(kids, uid) {
                            return Some(p);
                        }
                    }
                }
                None
            }
            st.sheets[s as usize]
                .rules
                .as_deref()
                .and_then(|r| parent_of(r, uid))
        };
        cx.ret(parent.map_or(Arg::Null, |(p, k)| rule_arg(w, s, p, k)))
    });
    getter(reg, rt, "CSSStyleRule", "selectorText", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        cx.ret(with_rule(w, s, uid, |e| e.raw.prelude.clone()).unwrap_or_default())
    });
    setter(reg, rt, "CSSStyleRule", "selectorText", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        let v = cx.arg_string(0)?;
        if lb_dom::SelectorList::parse(v.trim()).is_ok() {
            let v = v.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
            if with_rule(w, s, uid, |e| e.raw.prelude = v).is_some() {
                sheet_modified(w, s);
            }
        }
        Ok(())
    });
    for iface in ["CSSStyleRule", "CSSFontFaceRule", "CSSPageRule"] {
        getter(reg, rt, iface, "style", |cx, w| {
            let (sheet, uid, _) = this_rule(cx, w)?;
            let id = w.cached_obj((sheet, uid, "cssRuleStyle", String::new()), || {
                Obj::RuleStyle { sheet, uid }
            });
            cx.ret(w.obj_arg(id))
        });
    }
    getter(reg, rt, "CSSImportRule", "href", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        cx.ret(with_rule(w, s, uid, |e| e.raw.url.clone().unwrap_or_default()).unwrap_or_default())
    });
    getter(reg, rt, "CSSImportRule", "media", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        cx.ret(media_arg(w, s, Some(uid)))
    });
    getter(reg, rt, "CSSImportRule", "styleSheet", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        let child = {
            let mut st = w.css.borrow_mut();
            ensure_rules(&mut st, s);
            let e = &st.sheets[s as usize];
            let pos = e.rules.as_ref().and_then(|r| {
                r.iter()
                    .filter(|x| x.raw.kind == RuleKind::Import)
                    .position(|x| x.uid == uid)
            });
            pos.and_then(|p| e.imports.get(p).copied().flatten())
        };
        cx.ret(child.map_or(Arg::Null, |k| sheet_arg(w, k)))
    });
    getter(reg, rt, "CSSImportRule", "layerName", |cx, _w| {
        cx.ret(Arg::Null)
    });
    getter(reg, rt, "CSSImportRule", "supportsText", |cx, _w| {
        cx.ret(Arg::Null)
    });
    getter(reg, rt, "CSSGroupingRule", "cssRules", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        cx.ret(rules_arg(w, s, Some(uid)))
    });
    method(reg, rt, "CSSGroupingRule", "insertRule", |cx, w| {
        need_args(cx, 1, "insertRule", "CSSGroupingRule")?;
        let (s, uid, _) = this_rule(cx, w)?;
        let text = cx.arg_string(0)?;
        let i = index_arg(cx, 1)?;
        let r = insert_rule(w, s, Some(uid), &text, i, "CSSGroupingRule")?;
        cx.ret(r)
    });
    method(reg, rt, "CSSGroupingRule", "deleteRule", |cx, w| {
        need_args(cx, 1, "deleteRule", "CSSGroupingRule")?;
        let (s, uid, _) = this_rule(cx, w)?;
        let i = cx.arg_u32(0)?;
        delete_rule(w, s, Some(uid), i, "CSSGroupingRule")
    });
    getter(reg, rt, "CSSConditionRule", "conditionText", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        cx.ret(with_rule(w, s, uid, |e| e.raw.prelude.clone()).unwrap_or_default())
    });
    getter(reg, rt, "CSSMediaRule", "media", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        cx.ret(media_arg(w, s, Some(uid)))
    });
    getter(reg, rt, "CSSKeyframesRule", "name", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        cx.ret(with_rule(w, s, uid, |e| e.raw.prelude.clone()).unwrap_or_default())
    });
    getter(reg, rt, "CSSLayerBlockRule", "name", |cx, w| {
        let (s, uid, _) = this_rule(cx, w)?;
        cx.ret(with_rule(w, s, uid, |e| e.raw.prelude.clone()).unwrap_or_default())
    });
}

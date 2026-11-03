//! [`LayoutEngine`]: lazy layout, geometry queries, hit testing and
//! scrolling for one document.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use lb_css::{
    ComputedStyle, Display, MediaEnv, Overflow, PointerEvents, Position, StyleEngine, Visibility,
    computed_value,
};
use lb_dom::{Document, NodeId, NodeType, local_name};
use lb_persona::PersonaProfile;

use crate::build::{Builder, Built};
use crate::fonts::{FontAvailability, FontDb, TextFont, resolve_font};
use crate::tree::BoxKind;

/// An axis-aligned rectangle in CSS px.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const ZERO: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// Half-open containment (`x <= px < right`).
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect::new(x, y, (r - x).max(0.0), (b - y).max(0.0))
    }

    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    fn inset(&self, top: f32, right: f32, bottom: f32, left: f32) -> Rect {
        Rect::new(
            self.x + left,
            self.y + top,
            (self.width - left - right).max(0.0),
            (self.height - top - bottom).max(0.0),
        )
    }
}

/// CSS box model of an element, in viewport (client) coordinates.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BoxModel {
    pub content: Rect,
    pub padding: Rect,
    pub border: Rect,
    pub margin: Rect,
}

/// How `scroll_into_view` aligns the element.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ScrollAlign {
    Start,
    Center,
    End,
    /// Scroll as little as possible.
    Nearest,
}

/// Counters for tests and tracing.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LayoutStats {
    /// Full layouts run (box tree build + taffy).
    pub layouts: u64,
    /// Geometry passes (after layout or scrolling).
    pub placements: u64,
}

#[derive(Copy, Clone, Debug, Default)]
struct Geo {
    /// Border box in client coordinates (scroll applied).
    rect: Rect,
    /// Border box in document coordinates (no scroll applied).
    doc: Rect,
    /// Clip from overflow ancestors, client coordinates.
    clip: Option<Rect>,
}

struct LayoutState {
    key: (u64, u64),
    viewport: (f32, f32),
    built: Built,
    geo: Vec<Geo>,
    geo_valid: bool,
    paint_order: Vec<usize>,
    /// Scrollable content size per scroll container box (incl. viewport).
    extents: HashMap<usize, (f32, f32)>,
}

/// Lazy layout for one document.
///
/// Owns the document's [`StyleEngine`]. Nothing is laid out until a
/// geometry query runs; results are reused until the document's render
/// generation, the style engine or the viewport changes. Scrolling only
/// re-runs the cheap placement pass.
pub struct LayoutEngine {
    styles: StyleEngine,
    fonts: FontDb,
    available: FontAvailability,
    viewport: (f32, f32),
    state: Option<LayoutState>,
    scroll: HashMap<NodeId, (f32, f32)>,
    viewport_scroll: (f32, f32),
    stats: LayoutStats,
    persona: Option<Arc<PersonaProfile>>,
}

impl std::fmt::Debug for LayoutEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayoutEngine")
            .field("viewport", &self.viewport)
            .field("stats", &self.stats)
            .finish()
    }
}

impl LayoutEngine {
    /// Layout for a persona: viewport, media, OS fonts and the installed
    /// font list all come from it.
    pub fn new(persona: Arc<PersonaProfile>) -> LayoutEngine {
        let env = MediaEnv::from_persona(&persona);
        let available =
            FontAvailability::from_list(persona.fonts.available.iter().map(String::as_str));
        let mut e = Self::with_env(env);
        e.available = available;
        e.persona = Some(persona);
        e
    }

    /// Layout for a bare media environment; every bundled font counts as
    /// installed. For tests and tools.
    pub fn with_env(env: MediaEnv) -> LayoutEngine {
        let fonts = FontDb::for_os(env.os);
        let viewport = (env.viewport_width, env.viewport_height);
        LayoutEngine {
            styles: StyleEngine::new(env),
            fonts,
            available: FontAvailability::everything(),
            viewport,
            state: None,
            scroll: HashMap::new(),
            viewport_scroll: (0.0, 0.0),
            stats: LayoutStats::default(),
            persona: None,
        }
    }

    pub fn persona(&self) -> Option<&Arc<PersonaProfile>> {
        self.persona.as_ref()
    }

    /// The style engine (add sheets, match media, computed styles).
    pub fn styles(&mut self) -> &mut StyleEngine {
        &mut self.styles
    }

    pub fn fonts(&self) -> &FontDb {
        &self.fonts
    }

    pub fn stats(&self) -> LayoutStats {
        self.stats
    }

    /// Resizes the viewport (and media environment).
    pub fn set_viewport(&mut self, width: f32, height: f32) {
        self.viewport = (width, height);
        self.styles.set_viewport(width, height);
    }

    pub fn viewport(&self) -> (f32, f32) {
        self.viewport
    }

    /// Frees layout data (memory pressure). The next query lays out again.
    pub fn drop_layout(&mut self) {
        self.state = None;
    }

    /// Computed style shortcut.
    pub fn computed_style(&mut self, doc: &Document, node: NodeId) -> Option<Rc<ComputedStyle>> {
        self.styles.computed_style(doc, node)
    }

    /// The font a style resolves to (for canvas text and font probes).
    pub fn font_for(&self, style: &ComputedStyle) -> TextFont {
        resolve_font(&self.fonts, &self.available, style)
    }

    // ----- layout -----

    fn ensure(&mut self, doc: &Document) {
        let key = self.styles.style_key(doc);
        let fresh = self
            .state
            .as_ref()
            .is_some_and(|s| s.key == key && s.viewport == self.viewport);
        if !fresh {
            // Styles may refresh (and bump nothing): re-read the key after.
            let built = Builder::new(doc, &mut self.styles, &self.fonts, &self.available)
                .build(self.viewport.0, self.viewport.1);
            let mut built = built;
            built
                .tree
                .compute(built.root, self.viewport.0, self.viewport.1);
            self.stats.layouts += 1;
            let n = built.tree.boxes.len();
            let paint_order = paint_order(&built);
            self.state = Some(LayoutState {
                key: self.styles.style_key(doc),
                viewport: self.viewport,
                built,
                geo: vec![Geo::default(); n],
                geo_valid: false,
                paint_order,
                extents: HashMap::new(),
            });
        }
        let state = self.state.as_mut().expect("layout state");
        if !state.geo_valid {
            place_all(state, &mut self.scroll, &mut self.viewport_scroll);
            state.geo_valid = true;
            self.stats.placements += 1;
        }
    }

    fn state(&mut self, doc: &Document) -> &LayoutState {
        self.ensure(doc);
        self.state.as_ref().expect("layout state")
    }

    fn box_of(&mut self, doc: &Document, node: NodeId) -> Option<usize> {
        self.state(doc).built.node_box.get(&node).copied()
    }

    // ----- geometry -----

    /// `getBoundingClientRect()`: border box in viewport coordinates.
    /// All zeros for nodes without a box (`display: none`, detached).
    pub fn bounding_client_rect(&mut self, doc: &Document, node: NodeId) -> Rect {
        if let Some(b) = self.box_of(doc, node) {
            return self.state(doc).geo[b].rect;
        }
        // display: contents: union of the children's boxes.
        let rects: Vec<Rect> = doc
            .children(node)
            .map(|c| self.bounding_client_rect(doc, c))
            .filter(|r| !r.is_empty())
            .collect();
        union(&rects)
    }

    /// `getClientRects()`: one rect per box (we have one box per node).
    pub fn client_rects(&mut self, doc: &Document, node: NodeId) -> Vec<Rect> {
        match self.box_of(doc, node) {
            Some(b) => vec![self.state(doc).geo[b].rect],
            None => Vec::new(),
        }
    }

    /// Content, padding, border and margin boxes (CDP `DOM.getBoxModel`).
    pub fn box_model(&mut self, doc: &Document, node: NodeId) -> Option<BoxModel> {
        let b = self.box_of(doc, node)?;
        let st = self.state(doc);
        let l = &st.built.tree.boxes[b].layout;
        let border = st.geo[b].rect;
        let padding = border.inset(l.border.top, l.border.right, l.border.bottom, l.border.left);
        let content = padding.inset(
            l.padding.top,
            l.padding.right,
            l.padding.bottom,
            l.padding.left,
        );
        let margin = Rect::new(
            border.x - l.margin.left,
            border.y - l.margin.top,
            border.width + l.margin.left + l.margin.right,
            border.height + l.margin.top + l.margin.bottom,
        );
        Some(BoxModel {
            content,
            padding,
            border,
            margin,
        })
    }

    /// `offsetParent`.
    pub fn offset_parent(&mut self, doc: &Document, node: NodeId) -> Option<NodeId> {
        let style = self.styles.computed_style(doc, node)?;
        self.box_of(doc, node)?;
        if style.position == Position::Fixed
            || Some(node) == doc.body()
            || Some(node) == doc.document_element()
        {
            return None;
        }
        let static_pos = style.position == Position::Static;
        let mut cur = doc.parent_element(node);
        while let Some(a) = cur {
            if let Some(s) = self.styles.computed_style(doc, a) {
                if s.position != Position::Static || Some(a) == doc.body() {
                    return Some(a);
                }
                if static_pos
                    && (doc.is_html_element_named(a, &local_name!("td"))
                        || doc.is_html_element_named(a, &local_name!("th"))
                        || doc.is_html_element_named(a, &local_name!("table")))
                {
                    return Some(a);
                }
            }
            cur = doc.parent_element(a);
        }
        None
    }

    /// `offsetLeft` / `offsetTop`.
    pub fn offset_position(&mut self, doc: &Document, node: NodeId) -> (f32, f32) {
        let Some(b) = self.box_of(doc, node) else {
            return (0.0, 0.0);
        };
        let mine = self.state(doc).geo[b].doc;
        let parent = self.offset_parent(doc, node);
        let origin = match parent {
            Some(p) if Some(p) != doc.body() => match self.box_of(doc, p) {
                Some(pb) => {
                    let st = self.state(doc);
                    let l = &st.built.tree.boxes[pb].layout;
                    let r = st.geo[pb].doc;
                    (r.x + l.border.left, r.y + l.border.top)
                }
                None => (0.0, 0.0),
            },
            _ => (0.0, 0.0),
        };
        ((mine.x - origin.0).round(), (mine.y - origin.1).round())
    }

    /// `offsetWidth` / `offsetHeight`.
    pub fn offset_size(&mut self, doc: &Document, node: NodeId) -> (f32, f32) {
        let r = self.bounding_client_rect(doc, node);
        (r.width.round(), r.height.round())
    }

    /// `clientWidth` / `clientHeight`: padding box (no scrollbars), 0 for
    /// inline boxes; the viewport for the root element.
    pub fn client_size(&mut self, doc: &Document, node: NodeId) -> (f32, f32) {
        if Some(node) == doc.document_element() {
            return (self.viewport.0, self.viewport.1);
        }
        let Some(style) = self.styles.computed_style(doc, node) else {
            return (0.0, 0.0);
        };
        if style.display == Display::Inline {
            return (0.0, 0.0);
        }
        match self.box_model(doc, node) {
            Some(m) => (m.padding.width.round(), m.padding.height.round()),
            None => (0.0, 0.0),
        }
    }

    /// `clientLeft` / `clientTop`: left and top border widths.
    pub fn client_offset(&mut self, doc: &Document, node: NodeId) -> (f32, f32) {
        match self.box_of(doc, node) {
            Some(b) => {
                let l = self.state(doc).built.tree.boxes[b].layout;
                (l.border.left.round(), l.border.top.round())
            }
            None => (0.0, 0.0),
        }
    }

    /// `scrollWidth` / `scrollHeight`.
    pub fn scroll_size(&mut self, doc: &Document, node: NodeId) -> (f32, f32) {
        if Some(node) == doc.document_element() {
            return self.document_size(doc);
        }
        let Some(b) = self.box_of(doc, node) else {
            return (0.0, 0.0);
        };
        let (cw, ch) = self.client_size(doc, node);
        let st = self.state(doc);
        let (ew, eh) = st.extents.get(&b).copied().unwrap_or((0.0, 0.0));
        (cw.max(ew.round()), ch.max(eh.round()))
    }

    /// Scrollable size of the whole document.
    pub fn document_size(&mut self, doc: &Document) -> (f32, f32) {
        let vp = self.viewport;
        let st = self.state(doc);
        let root = st.built.root;
        let (w, h) = st.extents.get(&root).copied().unwrap_or((0.0, 0.0));
        (w.max(vp.0).round(), h.max(vp.1).round())
    }

    /// `scrollLeft` / `scrollTop`. The root element (and, in quirks mode,
    /// body) reports the viewport scroll.
    pub fn scroll_position(&mut self, doc: &Document, node: NodeId) -> (f32, f32) {
        if self.is_viewport_scroller(doc, node) {
            self.ensure(doc);
            return self.viewport_scroll;
        }
        self.ensure(doc);
        self.scroll.get(&node).copied().unwrap_or((0.0, 0.0))
    }

    fn is_viewport_scroller(&self, doc: &Document, node: NodeId) -> bool {
        let quirks = doc.quirks_mode() == lb_dom::QuirksMode::Quirks;
        if quirks {
            Some(node) == doc.body()
        } else {
            Some(node) == doc.document_element()
        }
    }

    /// Sets an element's scroll offset (clamped). Non-scrollable elements
    /// ignore it.
    pub fn set_scroll_position(&mut self, doc: &Document, node: NodeId, x: f32, y: f32) {
        if self.is_viewport_scroller(doc, node) {
            self.scroll_window_to(doc, x, y);
            return;
        }
        let Some(b) = self.box_of(doc, node) else {
            return;
        };
        let scrollable = {
            let st = self.state.as_ref().expect("state");
            st.built.tree.boxes[b]
                .css
                .as_ref()
                .is_some_and(|c| c.is_scroll_container())
        };
        if !scrollable {
            return;
        }
        let (sw, sh) = self.scroll_size(doc, node);
        let (cw, ch) = self.client_size(doc, node);
        let x = x.clamp(0.0, (sw - cw).max(0.0));
        let y = y.clamp(0.0, (sh - ch).max(0.0));
        self.scroll.insert(node, (x, y));
        if let Some(st) = self.state.as_mut() {
            st.geo_valid = false;
        }
    }

    /// `window.scrollX/Y`.
    pub fn window_scroll(&mut self, doc: &Document) -> (f32, f32) {
        self.ensure(doc);
        self.viewport_scroll
    }

    /// `window.scrollTo(x, y)`, clamped to the document size.
    pub fn scroll_window_to(&mut self, doc: &Document, x: f32, y: f32) {
        let (dw, dh) = self.document_size(doc);
        let x = x.clamp(0.0, (dw - self.viewport.0).max(0.0));
        let y = y.clamp(0.0, (dh - self.viewport.1).max(0.0));
        if (x, y) != self.viewport_scroll {
            self.viewport_scroll = (x, y);
            if let Some(st) = self.state.as_mut() {
                st.geo_valid = false;
            }
        }
    }

    /// Scrolls every scrolling ancestor (and the viewport) so the element is
    /// visible. With `if_needed`, does nothing when it already is.
    pub fn scroll_into_view(
        &mut self,
        doc: &Document,
        node: NodeId,
        align: ScrollAlign,
        if_needed: bool,
    ) {
        let Some(b) = self.box_of(doc, node) else {
            return;
        };
        // Scroll containers from the inside out.
        let mut containers = Vec::new();
        {
            let st = self.state.as_ref().expect("state");
            let mut cur = st.built.tree.boxes[b].parent;
            while let Some(p) = cur {
                let pb = &st.built.tree.boxes[p];
                if matches!(pb.kind, BoxKind::Viewport) {
                    containers.push((p, None));
                } else if pb.css.as_ref().is_some_and(|c| c.is_scroll_container())
                    && let Some(dom) = pb.dom
                {
                    containers.push((p, Some(dom)));
                }
                cur = pb.parent;
            }
        }
        for (cb, dom) in containers {
            let target = self.state(doc).geo[b].rect;
            let view = match dom {
                None => Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1),
                Some(_) => {
                    let st = self.state(doc);
                    let l = &st.built.tree.boxes[cb].layout;
                    st.geo[cb].rect.inset(
                        l.border.top,
                        l.border.right,
                        l.border.bottom,
                        l.border.left,
                    )
                }
            };
            let delta = |t0: f32, t1: f32, v0: f32, v1: f32| -> f32 {
                let fully = t0 >= v0 && t1 <= v1;
                if if_needed && fully {
                    return 0.0;
                }
                match align {
                    ScrollAlign::Start => t0 - v0,
                    ScrollAlign::End => t1 - v1,
                    ScrollAlign::Center => (t0 + t1) / 2.0 - (v0 + v1) / 2.0,
                    ScrollAlign::Nearest => {
                        if fully {
                            0.0
                        } else if t0 < v0 || (t1 - t0) > (v1 - v0) {
                            t0 - v0
                        } else {
                            t1 - v1
                        }
                    }
                }
            };
            let dx = delta(target.x, target.right(), view.x, view.right());
            let dy = delta(target.y, target.bottom(), view.y, view.bottom());
            if dx == 0.0 && dy == 0.0 {
                continue;
            }
            match dom {
                None => {
                    let (sx, sy) = self.viewport_scroll;
                    self.scroll_window_to(doc, sx + dx, sy + dy);
                }
                Some(el) => {
                    let (sx, sy) = self.scroll.get(&el).copied().unwrap_or((0.0, 0.0));
                    self.set_scroll_position(doc, el, sx + dx, sy + dy);
                }
            }
        }
    }

    // ----- hit testing and visibility -----

    /// `document.elementFromPoint(x, y)` in client coordinates: the topmost
    /// element whose box contains the point, honoring z-order, overflow
    /// clips, `visibility` and `pointer-events`.
    pub fn element_from_point(&mut self, doc: &Document, x: f32, y: f32) -> Option<NodeId> {
        self.elements_from_point(doc, x, y).into_iter().next()
    }

    /// `document.elementsFromPoint(x, y)`, topmost first.
    pub fn elements_from_point(&mut self, doc: &Document, x: f32, y: f32) -> Vec<NodeId> {
        let vp = self.viewport;
        if x < 0.0 || y < 0.0 || x >= vp.0 || y >= vp.1 {
            return Vec::new();
        }
        let st = self.state(doc);
        let mut out: Vec<NodeId> = Vec::new();
        for &i in st.paint_order.iter().rev() {
            let b = &st.built.tree.boxes[i];
            let g = &st.geo[i];
            if !g.rect.contains(x, y) || g.clip.is_some_and(|c| !c.contains(x, y)) {
                continue;
            }
            let el = match (&b.kind, b.dom) {
                (BoxKind::Element | BoxKind::Replaced { .. }, Some(d)) => d,
                (BoxKind::Text(_), Some(d)) => match doc.parent_element(d) {
                    Some(p) => p,
                    None => continue,
                },
                _ => continue,
            };
            if let Some(css) = &b.css
                && (css.visibility != Visibility::Visible
                    || css.pointer_events == PointerEvents::None)
            {
                continue;
            }
            if !out.contains(&el) {
                out.push(el);
            }
        }
        // Ancestors of hit elements are hit too (their boxes contain the
        // point by containment in normal flow; add any that were skipped).
        if let Some(root) = doc.document_element()
            && !out.contains(&root)
        {
            out.push(root);
        }
        out
    }

    /// Visible in Playwright's sense: has a non-empty box and computed
    /// `visibility: visible` (so not inside `display: none`).
    pub fn is_visible(&mut self, doc: &Document, node: NodeId) -> bool {
        let el = if doc.node_type(node) == NodeType::Text {
            match doc.parent_element(node) {
                Some(p) => p,
                None => return false,
            }
        } else {
            node
        };
        let Some(style) = self.styles.computed_style(doc, el) else {
            return false;
        };
        if style.visibility != Visibility::Visible {
            return false;
        }
        !self.bounding_client_rect(doc, node).is_empty()
    }

    /// `getComputedStyle(el).getPropertyValue(name)` with Chrome's resolved
    /// values: used px for box sizes, margins and paddings of rendered
    /// elements.
    pub fn resolved_style_value(&mut self, doc: &Document, node: NodeId, name: &str) -> String {
        let Some(style) = self.styles.computed_style(doc, node) else {
            return String::new();
        };
        let base = computed_value(&style, name).unwrap_or_default();
        let lname = name.to_ascii_lowercase();
        let box_props = [
            "width",
            "height",
            "margin-top",
            "margin-right",
            "margin-bottom",
            "margin-left",
            "padding-top",
            "padding-right",
            "padding-bottom",
            "padding-left",
        ];
        if !box_props.contains(&lname.as_str()) {
            return base;
        }
        if style.display == Display::Inline && (lname == "width" || lname == "height") {
            return base;
        }
        let Some(m) = self.box_model(doc, node) else {
            return base;
        };
        let border_box = style.box_sizing == lb_css::BoxSizing::BorderBox;
        let v = match lname.as_str() {
            "width" if border_box => m.border.width,
            "height" if border_box => m.border.height,
            "width" => m.content.width,
            "height" => m.content.height,
            "margin-top" => m.border.y - m.margin.y,
            "margin-left" => m.border.x - m.margin.x,
            "margin-right" => m.margin.right() - m.border.right(),
            "margin-bottom" => m.margin.bottom() - m.border.bottom(),
            "padding-top" => m.content.y - m.padding.y,
            "padding-left" => m.content.x - m.padding.x,
            "padding-right" => m.padding.right() - m.content.right(),
            "padding-bottom" => m.padding.bottom() - m.content.bottom(),
            _ => return base,
        };
        lb_css::fmt_px(v)
    }

    /// The accessibility tree of the document.
    pub fn accessibility_tree(&mut self, doc: &Document) -> crate::ax::AxTree {
        crate::ax::AxTree::build(doc, &mut self.styles)
    }

    /// Number of boxes in the current layout (0 if none computed).
    pub fn box_count(&self) -> usize {
        self.state.as_ref().map_or(0, |s| s.built.tree.boxes.len())
    }
}

fn union(rects: &[Rect]) -> Rect {
    let Some(first) = rects.first() else {
        return Rect::ZERO;
    };
    let mut r = *first;
    for o in &rects[1..] {
        let x = r.x.min(o.x);
        let y = r.y.min(o.y);
        let right = r.right().max(o.right());
        let bottom = r.bottom().max(o.bottom());
        r = Rect::new(x, y, right - x, bottom - y);
    }
    r
}

/// Computes client and document rects, clips and scroll extents.
fn place_all(
    st: &mut LayoutState,
    scroll: &mut HashMap<NodeId, (f32, f32)>,
    viewport_scroll: &mut (f32, f32),
) {
    st.extents.clear();
    let root = st.built.root;
    let mut stack: Vec<Frame> = vec![Frame {
        idx: root,
        doc_origin: (0.0, 0.0),
        client_origin: (0.0, 0.0),
        clip: None,
        scroller: None,
        fixed: false,
    }];
    let vs = *viewport_scroll;
    while let Some(f) = stack.pop() {
        let b = &st.built.tree.boxes[f.idx];
        let loc = b.layout.location;
        let size = b.layout.size;
        let doc = Rect::new(
            f.doc_origin.0 + loc.x,
            f.doc_origin.1 + loc.y,
            size.width,
            size.height,
        );
        let rect = Rect::new(
            f.client_origin.0 + loc.x,
            f.client_origin.1 + loc.y,
            size.width,
            size.height,
        );
        st.geo[f.idx] = Geo {
            rect,
            doc,
            clip: f.clip,
        };
        // Scroll extent of the nearest scroller.
        if let Some((sc, sc_doc)) = f.scroller {
            let e = st.extents.entry(sc).or_insert((0.0, 0.0));
            e.0 = e.0.max(doc.right() - sc_doc.0);
            e.1 = e.1.max(doc.bottom() - sc_doc.1);
        }
        let is_viewport = matches!(b.kind, BoxKind::Viewport);
        let css = b.css.clone();
        let is_scroller = is_viewport || css.as_ref().is_some_and(|c| c.is_scroll_container());
        let clips = !is_viewport
            && css.as_ref().is_some_and(|c| {
                c.overflow_x != Overflow::Visible || c.overflow_y != Overflow::Visible
            });
        let l = b.layout;
        let padding_client =
            rect.inset(l.border.top, l.border.right, l.border.bottom, l.border.left);
        let child_clip = if clips {
            Some(match f.clip {
                Some(c) => c.intersect(&padding_client),
                None => padding_client,
            })
        } else {
            f.clip
        };
        let (sx, sy) = if is_viewport {
            vs
        } else if is_scroller {
            b.dom
                .and_then(|d| scroll.get(&d).copied())
                .unwrap_or((0.0, 0.0))
        } else {
            (0.0, 0.0)
        };
        let scroller = if is_scroller {
            Some((f.idx, (doc.x + l.border.left, doc.y + l.border.top)))
        } else {
            f.scroller
        };
        let children: Vec<_> = b.children.iter().map(|c| usize::from(*c)).collect();
        let hoisted: Vec<_> = st
            .built
            .tree
            .hoisted(f.idx)
            .iter()
            .map(|c| usize::from(*c))
            .collect();
        for c in children.into_iter().rev() {
            if st.built.tree.boxes[c].is_out_of_flow() {
                continue;
            }
            stack.push(Frame {
                idx: c,
                doc_origin: (doc.x, doc.y),
                client_origin: (rect.x - sx, rect.y - sy),
                clip: child_clip,
                scroller,
                fixed: f.fixed,
            });
        }
        for c in hoisted.into_iter().rev() {
            let fixed = st.built.tree.boxes[c].style.position == taffy::Position::Fixed;
            let (cx, cy) = if fixed && is_viewport {
                (rect.x, rect.y)
            } else {
                (rect.x - sx, rect.y - sy)
            };
            stack.push(Frame {
                idx: c,
                doc_origin: (doc.x, doc.y),
                client_origin: (cx, cy),
                clip: if fixed { None } else { child_clip },
                scroller: if fixed { None } else { scroller },
                fixed: fixed || f.fixed,
            });
        }
    }
    // Keep scroll offsets inside the (possibly smaller) new extents.
    let _ = scroll;
}

struct Frame {
    idx: usize,
    doc_origin: (f32, f32),
    client_origin: (f32, f32),
    clip: Option<Rect>,
    /// Nearest scroll container box and its padding-box origin (doc).
    scroller: Option<(usize, (f32, f32))>,
    fixed: bool,
}

fn creates_stacking_context(css: &ComputedStyle, parent_flex_or_grid: bool) -> bool {
    (css.position != Position::Static && css.z_index.is_some())
        || matches!(css.position, Position::Fixed | Position::Sticky)
        || css.opacity < 1.0
        || css.transform.is_some()
        || (parent_flex_or_grid && css.z_index.is_some())
}

/// Paint order (CSS 2.1 Appendix E, simplified): per stacking context,
/// negative z-index contexts, in-flow boxes in tree order, positioned
/// boxes with `z-index: auto/0`, then positive z-index contexts.
fn paint_order(built: &Built) -> Vec<usize> {
    let tree = &built.tree;
    let mut out = Vec::with_capacity(tree.boxes.len());
    fn is_fg(tree: &crate::tree::BoxTree, i: usize) -> bool {
        tree.boxes[i]
            .css
            .as_ref()
            .is_some_and(|c| c.display.is_flex() || c.display.is_grid())
    }
    fn paint(tree: &crate::tree::BoxTree, sc: usize, out: &mut Vec<usize>) {
        out.push(sc);
        let mut neg: Vec<(i32, usize)> = Vec::new();
        let mut normal: Vec<usize> = Vec::new();
        let mut zero: Vec<usize> = Vec::new();
        let mut pos: Vec<(i32, usize)> = Vec::new();
        let mut stack: Vec<usize> = tree.boxes[sc]
            .children
            .iter()
            .rev()
            .map(|c| usize::from(*c))
            .collect();
        while let Some(i) = stack.pop() {
            let b = &tree.boxes[i];
            let parent_fg = b.parent.is_some_and(|p| is_fg(tree, p));
            if let Some(css) = &b.css {
                if creates_stacking_context(css, parent_fg) {
                    let z = css.z_index.unwrap_or(0);
                    if z < 0 {
                        neg.push((z, i));
                    } else if z == 0 {
                        zero.push(i);
                    } else {
                        pos.push((z, i));
                    }
                    continue;
                }
                if css.position != Position::Static {
                    zero.push(i);
                    continue;
                }
            }
            normal.push(i);
            for c in b.children.iter().rev() {
                stack.push(usize::from(*c));
            }
        }
        neg.sort_by_key(|(z, _)| *z);
        pos.sort_by_key(|(z, _)| *z);
        for (_, i) in neg {
            paint(tree, i, out);
        }
        out.extend(normal);
        for i in zero {
            paint(tree, i, out);
        }
        for (_, i) in pos {
            paint(tree, i, out);
        }
    }
    paint(tree, built.root, &mut out);
    out
}

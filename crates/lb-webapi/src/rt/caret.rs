//! Caret positions and text units for `Selection.modify`, after Blink's
//! editing code (`VisiblePosition`, `TextOffsetMapping`,
//! `SelectionModifier`), recorded against Chrome 153.
//!
//! The rendered text of each inline formatting context (IFC) is built as
//! Blink's `TextOffsetMapping` sees it: white space collapsed per
//! `white-space`, `\n` for `<br>`, `,` for replaced elements, hidden
//! content left out. A caret position ([`Pos`]) is an IFC and a slot in
//! its text; every DOM position maps to one ([`Model::canonical`], Blink's
//! `CanonicalPositionOf`: the end of the character before it, or the start
//! of the character after it at a line start) and back ([`Model::dom`]).
//! Words and sentences come from ICU's break iterators with Blink's rules
//! on top (`position_units_word.cc`, `position_units_sentence.cc`), lines
//! from lb-layout's line boxes (`LayoutEngine::caret_lines`).
//!
//! Approximations: inline-blocks are flattened into their IFC, text is
//! left to right, floats and positioned boxes split IFCs like blocks,
//! editable content is treated as not editable.

use std::collections::HashMap;

use lb_js::text_break::{self as tb, BreakKind, Breaker};
use lb_layout::{CaretLine, LayoutEngine};

use super::*;

/// Tree-order key of a boundary point: (flat-tree preorder number, 0
/// before the node / 1 + offset inside it).
type Key = (u32, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Src {
    /// A UTF-16 unit of a text node at this offset.
    Text(NodeId, u32),
    Br(NodeId),
    /// A replaced element (`,` in the text).
    Atomic(NodeId),
}

impl Src {
    fn node(self) -> NodeId {
        match self {
            Src::Text(n, _) | Src::Br(n) | Src::Atomic(n) => n,
        }
    }
}

struct Ifc {
    text: Vec<u16>,
    src: Vec<Src>,
    /// The block whose content this is.
    block: NodeId,
}

impl Ifc {
    /// The last slot: none follows a final line break.
    fn last(&self) -> u32 {
        let n = self.text.len() as u32;
        if self.text.last() == Some(&NL) {
            n - 1
        } else {
            n
        }
    }
}

const NL: u16 = b'\n' as u16;
const SP: u16 = b' ' as u16;

/// A caret position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Pos {
    ifc: usize,
    at: u32,
}

/// Blink's `EditingBehavior` differences that `modify` sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Platform {
    Mac,
    Windows,
    Unix,
}

/// The document's rendered text, positions and lines.
pub(super) struct Model<'a> {
    d: &'a Document,
    l: &'a mut LayoutEngine,
    ifcs: Vec<Ifc>,
    /// Flat-tree preorder number and the last number in the subtree.
    order: HashMap<NodeId, (u32, u32)>,
    /// Every unit of every IFC: (start key, end key, IFC, index).
    units: Vec<(Key, Key, usize, u32)>,
    atomic: HashMap<NodeId, ()>,
    /// Block-level boxes.
    blockish: HashMap<NodeId, ()>,
    locale: String,
    geo: HashMap<usize, Geo>,
    caret_lines: HashMap<NodeId, Vec<CaretLine>>,
    /// The cache key this model was built for.
    key: CacheKey,
    /// The window scroll position `geo` was measured at (checked once per
    /// call).
    scroll: Option<(f32, f32)>,
    scroll_checked: bool,
    /// A text control whose value is the model's only text
    /// ([`Model::for_value`]); its positions are (control, offset).
    value_node: Option<NodeId>,
}

/// Style key, viewport and locale.
type CacheKey = ((u64, u64), (f32, f32), String);

/// A [`Model`] kept between calls while the document's style and layout
/// inputs are unchanged.
pub(super) struct Cache {
    key: CacheKey,
    ifcs: Vec<Ifc>,
    order: HashMap<NodeId, (u32, u32)>,
    units: Vec<(Key, Key, usize, u32)>,
    atomic: HashMap<NodeId, ()>,
    blockish: HashMap<NodeId, ()>,
    geo: HashMap<usize, Geo>,
    caret_lines: HashMap<NodeId, Vec<CaretLine>>,
    scroll: Option<(f32, f32)>,
}

/// Replaced elements: one position before and one after, no content.
fn is_atomic(d: &Document, n: NodeId) -> bool {
    d.local_name(n).is_some_and(|l| {
        d.is_html_element_named(n, l)
            && matches!(
                &**l,
                "img"
                    | "input"
                    | "textarea"
                    | "select"
                    | "video"
                    | "audio"
                    | "canvas"
                    | "iframe"
                    | "embed"
                    | "object"
                    | "meter"
                    | "progress"
            )
    }) || d
        .local_name(n)
        .is_some_and(|l| &**l == "svg" && !d.is_html_element_named(n, l))
}

fn is_ws_unit(c: u16) -> bool {
    matches!(c, 0x20 | 0x09 | 0x0A | 0x0D | 0x0C)
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

/// Builds IFCs while walking the flat tree.
struct Builder {
    ifcs: Vec<Ifc>,
    cur: Ifc,
    /// The last unit is a collapsible space.
    space: bool,
    /// At the start of a line (collapsible spaces are dropped).
    line_start: bool,
}

impl Builder {
    fn push(&mut self, c: u16, s: Src) {
        self.cur.text.push(c);
        self.cur.src.push(s);
    }

    fn trim(&mut self) {
        if self.space {
            self.cur.text.pop();
            self.cur.src.pop();
            self.space = false;
        }
    }

    fn newline(&mut self, s: Src) {
        self.trim();
        self.push(NL, s);
        self.line_start = true;
    }

    fn finish(&mut self, next_block: NodeId) {
        self.trim();
        let cur = std::mem::replace(
            &mut self.cur,
            Ifc {
                text: Vec::new(),
                src: Vec::new(),
                block: next_block,
            },
        );
        if !cur.text.is_empty() {
            self.ifcs.push(cur);
        }
        self.line_start = true;
        self.space = false;
    }

    fn text(&mut self, n: NodeId, raw: &str, ws: lb_css::WhiteSpace) {
        let mut o = 0u32;
        for c in raw.chars() {
            let u = c.len_utf16() as u32;
            if ws.collapses_spaces() {
                if c == '\n' && ws.preserves_newlines() {
                    self.newline(Src::Text(n, o));
                } else if is_ws(c) {
                    if !self.space && !self.line_start {
                        self.push(SP, Src::Text(n, o));
                        self.space = true;
                    }
                } else {
                    self.chr(n, o, c);
                }
            } else if c == '\n' {
                self.newline(Src::Text(n, o));
            } else if c != '\r' {
                self.chr(n, o, c);
            }
            o += u;
        }
    }

    fn chr(&mut self, n: NodeId, o: u32, c: char) {
        let mut buf = [0u16; 2];
        for (i, u) in c.encode_utf16(&mut buf).iter().enumerate() {
            self.push(*u, Src::Text(n, o + i as u32));
        }
        self.space = false;
        self.line_start = false;
    }
}

impl<'a> Model<'a> {
    /// The model of `d`, from `cache` when it is still valid.
    pub(super) fn new(
        d: &'a Document,
        l: &'a mut LayoutEngine,
        locale: &str,
        cache: Option<Box<Cache>>,
    ) -> Model<'a> {
        let key: CacheKey = (l.styles().style_key(d), l.viewport(), locale.to_string());
        if let Some(c) = cache.filter(|c| c.key == key) {
            let c = *c;
            return Model {
                d,
                l,
                ifcs: c.ifcs,
                order: c.order,
                units: c.units,
                atomic: c.atomic,
                blockish: c.blockish,
                locale: locale.to_string(),
                geo: c.geo,
                caret_lines: c.caret_lines,
                key,
                scroll: c.scroll,
                scroll_checked: false,
                value_node: None,
            };
        }
        let mut m = Model {
            d,
            l,
            ifcs: Vec::new(),
            order: HashMap::new(),
            units: Vec::new(),
            atomic: HashMap::new(),
            blockish: HashMap::new(),
            locale: locale.to_string(),
            geo: HashMap::new(),
            caret_lines: HashMap::new(),
            key,
            scroll: None,
            scroll_checked: false,
            value_node: None,
        };
        m.number();
        let root = d.document_element();
        let mut b = Builder {
            ifcs: Vec::new(),
            cur: Ifc {
                text: Vec::new(),
                src: Vec::new(),
                block: root.unwrap_or(Document::DOCUMENT),
            },
            space: false,
            line_start: true,
        };
        if let Some(r) = root {
            let mut blocks = vec![r];
            m.visit(&mut b, r, &mut blocks);
        }
        b.finish(Document::DOCUMENT);
        m.ifcs = b.ifcs;
        for (i, ifc) in m.ifcs.iter().enumerate() {
            for (k, s) in ifc.src.iter().enumerate() {
                let (start, end) = match *s {
                    Src::Text(n, o) => {
                        let p = m.order[&n].0;
                        ((p, 1 + o), (p, 2 + o))
                    }
                    Src::Br(n) | Src::Atomic(n) => {
                        let (p, last) = m.order[&n];
                        ((p, 0), (last + 1, 0))
                    }
                };
                m.units.push((start, end, i, k as u32));
            }
        }
        m
    }

    /// A model of a text control's value (`modify` while the selection is
    /// in an `<input>` or `<textarea>`): one IFC, lines at line breaks.
    pub(super) fn for_value(
        d: &'a Document,
        l: &'a mut LayoutEngine,
        locale: &str,
        node: NodeId,
        value: &str,
    ) -> Model<'a> {
        let text: Vec<u16> = value
            .encode_utf16()
            .filter(|&c| c != u16::from(b'\r'))
            .collect();
        let src = (0..text.len() as u32).map(|o| Src::Text(node, o)).collect();
        let key = (l.styles().style_key(d), l.viewport(), locale.to_string());
        let mut m = Model {
            d,
            l,
            ifcs: vec![Ifc {
                text,
                src,
                block: node,
            }],
            order: HashMap::from([(node, (0, 0))]),
            units: Vec::new(),
            atomic: HashMap::new(),
            blockish: HashMap::new(),
            locale: locale.to_string(),
            geo: HashMap::new(),
            caret_lines: HashMap::new(),
            key,
            scroll: None,
            scroll_checked: true,
            value_node: Some(node),
        };
        m.units = (0..m.ifcs[0].text.len() as u32)
            .map(|o| ((0, 1 + o), (0, 2 + o), 0, o))
            .collect();
        m
    }

    /// The model's data, for the next call.
    pub(super) fn into_cache(self) -> Box<Cache> {
        Box::new(Cache {
            key: self.key,
            ifcs: self.ifcs,
            order: self.order,
            units: self.units,
            atomic: self.atomic,
            blockish: self.blockish,
            geo: self.geo,
            caret_lines: self.caret_lines,
            scroll: self.scroll,
        })
    }

    /// Flat-tree preorder numbers for every node.
    fn number(&mut self) {
        let d = self.d;
        let mut next = 0u32;
        let mut stack = vec![(Document::DOCUMENT, false)];
        let mut open: Vec<NodeId> = Vec::new();
        while let Some((n, done)) = stack.pop() {
            if done {
                let p = self.order[&n].0;
                self.order.insert(n, (p, next.saturating_sub(1).max(p)));
                open.pop();
                continue;
            }
            self.order.insert(n, (next, next));
            next += 1;
            open.push(n);
            stack.push((n, true));
            for c in d.flat_children(n).into_iter().rev() {
                stack.push((c, false));
            }
        }
    }

    fn visit(&mut self, b: &mut Builder, n: NodeId, blocks: &mut Vec<NodeId>) {
        let d = self.d;
        let Some(css) = self.l.computed_style(d, n) else {
            return;
        };
        if css.is_display_none() {
            return;
        }
        let local = d.local_name(n).map(|l| l.to_string()).unwrap_or_default();
        let html = d
            .local_name(n)
            .is_some_and(|l| d.is_html_element_named(n, l));
        if html && local == "br" {
            if css.visibility == lb_css::Visibility::Visible {
                b.newline(Src::Br(n));
            }
            return;
        }
        if is_atomic(d, n) {
            self.atomic.insert(n, ());
            let block = !css.display.is_inline_level();
            if block {
                b.finish(n);
            }
            b.push(u16::from(b','), Src::Atomic(n));
            b.space = false;
            b.line_start = false;
            if block {
                b.finish(*blocks.last().unwrap_or(&n));
            }
            return;
        }
        let out_of_flow = matches!(
            css.position,
            lb_css::Position::Absolute | lb_css::Position::Fixed
        ) || css.float != lb_css::Float::None;
        let block = n == *blocks.first().unwrap_or(&n)
            || out_of_flow
            || (!css.display.is_inline_level() && css.display != lb_css::Display::Contents);
        if block {
            self.blockish.insert(n, ());
            b.finish(n);
            blocks.push(n);
        }
        for c in d.flat_children(n) {
            match d.node_type(c) {
                NodeType::Text => {
                    if css.visibility == lb_css::Visibility::Visible {
                        let raw = d.character_data(c).unwrap_or("").to_string();
                        b.text(c, &raw, css.white_space);
                    }
                }
                NodeType::Element => self.visit(b, c, blocks),
                _ => {}
            }
        }
        if block {
            blocks.pop();
            b.finish(*blocks.last().unwrap_or(&n));
        }
    }

    fn key(&self, (c, off): Bp) -> Option<Key> {
        let d = self.d;
        if self.value_node == Some(c) {
            return Some((0, 1 + off));
        }
        let &(pre, last) = self.order.get(&c)?;
        if is_chardata(d, c) {
            return Some((pre, 1 + off));
        }
        if self.atomic.contains_key(&c) || d.is_html_element_named(c, &LocalName::from("br")) {
            return Some((pre, 1));
        }
        match d.children(c).nth(off as usize) {
            Some(ch) => self.order.get(&ch).map(|o| (o.0, 0)),
            None => Some((last + 1, 0)),
        }
    }

    fn inside(&self, c: NodeId, block: NodeId) -> bool {
        match (self.order.get(&c), self.order.get(&block)) {
            (Some(a), Some(b)) => b.0 <= a.0 && a.0 <= b.1,
            _ => false,
        }
    }

    fn clamp(&self, p: Pos) -> Pos {
        let ifc = &self.ifcs[p.ifc];
        let mut at = p.at.min(ifc.last());
        // Not inside a surrogate pair.
        while at > 0
            && (at as usize) < ifc.text.len()
            && (0xDC00..0xE000).contains(&ifc.text[at as usize])
        {
            at -= 1;
        }
        Pos { ifc: p.ifc, at }
    }

    /// The caret position of a DOM position (`CanonicalPositionOf`), `None`
    /// when the document renders no text.
    pub(super) fn canonical(&self, bp: Bp) -> Option<Pos> {
        let k = self.key(bp)?;
        let i = self.units.partition_point(|u| u.0 < k);
        if i > 0 {
            let p = self.units[i - 1];
            if p.1 > k {
                // Inside a <br> or replaced element: before it.
                return Some(self.clamp(Pos { ifc: p.2, at: p.3 }));
            }
        }
        let prev = i.checked_sub(1).map(|i| self.units[i]);
        let next = self.units.get(i).copied();
        Some(match (prev, next) {
            (None, None) => return None,
            (None, Some(n)) => self.clamp(Pos { ifc: n.2, at: n.3 }),
            (Some(p), None) => self.clamp(Pos {
                ifc: p.2,
                at: p.3 + 1,
            }),
            (Some(p), Some(n)) if p.2 == n.2 => self.clamp(Pos { ifc: n.2, at: n.3 }),
            (Some(p), Some(n)) => {
                // Between two IFCs (`CanonicalPosition`): the end of the
                // previous text when no block boundary lies between
                // (`MostBackwardCaretPosition` stays in the enclosing
                // block), else the start of the next; failing both, the
                // one inside the position's node, else the next.
                let prev = self.ifcs[p.2].src[p.3 as usize].node();
                let next = self.ifcs[n.2].src[n.3 as usize].node();
                let eb = self.enclosing_block(bp.0);
                let before = self.clamp(Pos {
                    ifc: p.2,
                    at: p.3 + 1,
                });
                let after = Pos { ifc: n.2, at: n.3 };
                if self.enclosing_block(prev) == eb {
                    before
                } else if self.enclosing_block(next) == eb {
                    after
                } else if self.descendant(prev, bp.0) && !self.descendant(next, bp.0) {
                    before
                } else {
                    after
                }
            }
        })
    }

    /// The nearest block-level inclusive ancestor (Blink's
    /// `EnclosingBlockFlowElement`, roughly).
    fn enclosing_block(&self, n: NodeId) -> Option<NodeId> {
        let d = self.d;
        std::iter::once(n)
            .chain(std::iter::successors(
                d.flat_parent(n).or(d.parent(n)),
                |&x| d.flat_parent(x).or(d.parent(x)),
            ))
            .find(|x| self.blockish.contains_key(x))
    }

    /// `n` is a strict descendant of `of`.
    fn descendant(&self, n: NodeId, of: NodeId) -> bool {
        n != of && self.inside(n, of)
    }

    /// The DOM position of a caret position.
    pub(super) fn dom(&self, p: Pos) -> Bp {
        let d = self.d;
        let ifc = &self.ifcs[p.ifc];
        let k = p.at as usize;
        let around = |n: NodeId, after: bool| match d.parent(n) {
            Some(par) => (par, index(d, n) + u32::from(after)),
            None => (n, 0),
        };
        if k > 0 {
            match ifc.src[k - 1] {
                Src::Text(n, o) => return (n, o + 1),
                Src::Atomic(n) => return around(n, true),
                Src::Br(_) => {}
            }
        }
        match ifc.src.get(k) {
            Some(Src::Text(n, o)) => (*n, *o),
            Some(Src::Br(n) | Src::Atomic(n)) => around(*n, false),
            None => (ifc.block, 0),
        }
    }

    /// The DOM position of `p` as a selection's start: the most forward
    /// equivalent one (Blink's `MostForwardCaretPosition` in
    /// `SelectionTypeAdjuster`), at the start of the next node's text.
    pub(super) fn forward_dom(&self, p: Pos) -> Bp {
        let d = self.d;
        if self.value_node.is_some() {
            return self.dom(p);
        }
        let ifc = &self.ifcs[p.ifc];
        let k = p.at as usize;
        match ifc.src.get(k) {
            Some(Src::Text(n, o)) => (*n, *o),
            Some(Src::Br(n) | Src::Atomic(n)) => match d.parent(*n) {
                Some(par) => (par, index(d, *n)),
                None => self.dom(p),
            },
            // Past collapsed white space to the end of the last text.
            None => match ifc.src.last() {
                Some(Src::Text(n, _)) => (*n, node_len(d, *n)),
                _ => self.dom(p),
            },
        }
    }

    /// The DOM position of `p` as a selection's end: the most backward
    /// equivalent one (`MostBackwardCaretPosition`): after a `<br>`, and at
    /// the start of an IFC the outermost element it starts (its block when
    /// nothing rendered comes first).
    pub(super) fn backward_dom(&self, p: Pos) -> Bp {
        let d = self.d;
        if self.value_node.is_some() {
            return self.dom(p);
        }
        let ifc = &self.ifcs[p.ifc];
        let k = p.at as usize;
        if k > 0 {
            if let Src::Br(n) = ifc.src[k - 1]
                && let Some(par) = d.parent(n)
            {
                return (par, index(d, n) + 1);
            }
            return self.dom(p);
        }
        let start = self.dom(p);
        let Some(first) = ifc.src.first() else {
            return start;
        };
        let mut n = first.node();
        if !matches!(first, Src::Text(_, 0)) && matches!(first, Src::Text(..)) {
            // Collapsed white space before the first character.
            if let Src::Text(t, o) = *first
                && d.character_data(t)
                    .is_some_and(|s| s.encode_utf16().take(o as usize).any(|c| !is_ws_unit(c)))
            {
                return start;
            }
        }
        let mut last = start;
        while let Some(par) = d.parent(n) {
            let rendered_before = std::iter::successors(d.prev_sibling(n), |&s| d.prev_sibling(s))
                .any(|s| self.renders(s));
            if rendered_before {
                return last;
            }
            last = (par, 0);
            if par == ifc.block {
                return last;
            }
            n = par;
        }
        last
    }

    /// `n` renders something a caret walk stops at: text of an IFC, a
    /// replaced element, or a block-level box.
    fn renders(&self, n: NodeId) -> bool {
        let Some(&(pre, last)) = self.order.get(&n) else {
            return false;
        };
        let i = self.units.partition_point(|u| u.0 < (pre, 0));
        if self.units.get(i).is_some_and(|u| u.0 < (last + 1, 0)) {
            return true;
        }
        self.blockish.contains_key(&n)
    }

    fn first(&self) -> Option<Pos> {
        (!self.ifcs.is_empty()).then_some(Pos { ifc: 0, at: 0 })
    }

    fn last(&self) -> Option<Pos> {
        let i = self.ifcs.len().checked_sub(1)?;
        Some(Pos {
            ifc: i,
            at: self.ifcs[i].last(),
        })
    }

    fn breaker(&self, kind: BreakKind, text: &[u16]) -> Option<Breaker> {
        Breaker::new(kind, &self.locale, text)
    }

    // ----- characters -----

    /// `NextPositionOf`: the next grapheme boundary, then the next IFC.
    pub(super) fn next_char(&self, p: Pos) -> Option<Pos> {
        let ifc = &self.ifcs[p.ifc];
        if p.at < ifc.last() {
            let at = self
                .breaker(BreakKind::Character, &ifc.text)
                .and_then(|mut b| b.following(p.at as usize))
                .map_or(p.at + 1, |o| o as u32);
            return Some(self.clamp(Pos { ifc: p.ifc, at }));
        }
        (p.ifc + 1 < self.ifcs.len()).then_some(Pos {
            ifc: p.ifc + 1,
            at: 0,
        })
    }

    /// `PreviousPositionOf`.
    pub(super) fn prev_char(&self, p: Pos) -> Option<Pos> {
        let ifc = &self.ifcs[p.ifc];
        if p.at > 0 {
            let at = self
                .breaker(BreakKind::Character, &ifc.text)
                .and_then(|mut b| b.preceding(p.at as usize))
                .map_or(p.at - 1, |o| o as u32);
            return Some(self.clamp(Pos { ifc: p.ifc, at }));
        }
        let i = p.ifc.checked_sub(1)?;
        Some(Pos {
            ifc: i,
            at: self.ifcs[i].last(),
        })
    }

    // ----- text segments (`TextSegments::FindBoundary*`) -----

    fn find_forward(&self, p: Pos, mut f: impl FnMut(&[u16], usize) -> Found) -> Option<Pos> {
        for i in p.ifc..self.ifcs.len() {
            let text = &self.ifcs[i].text;
            let off = if i == p.ifc { p.at as usize } else { 0 };
            match f(text, off) {
                Found::Before(o) => {
                    return Some(self.clamp(Pos {
                        ifc: i,
                        at: o as u32,
                    }));
                }
                Found::After(o) => {
                    return Some(self.clamp(Pos {
                        ifc: i,
                        at: o as u32 + 1,
                    }));
                }
                Found::None => {}
            }
        }
        self.last()
    }

    fn find_backward(&self, p: Pos, mut f: impl FnMut(&[u16], usize) -> Found) -> Option<Pos> {
        for i in (0..=p.ifc).rev() {
            let text = &self.ifcs[i].text;
            let off = if i == p.ifc {
                p.at as usize
            } else {
                text.len()
            };
            match f(text, off) {
                Found::Before(o) => {
                    return Some(self.clamp(Pos {
                        ifc: i,
                        at: o as u32,
                    }));
                }
                Found::After(o) => {
                    return Some(self.clamp(Pos {
                        ifc: i,
                        at: o as u32 + 1,
                    }));
                }
                Found::None => {}
            }
        }
        self.first()
    }

    // ----- words (`position_units_word.cc`) -----

    /// `NextWordPosition`; `skip_spaces` is Windows' behavior.
    pub(super) fn next_word(&self, p: Pos, skip_spaces: bool) -> Option<Pos> {
        let mut first = true;
        self.find_forward(p, |text, offset| {
            let len = text.len();
            let skip = |offset: usize| skip_ws(text, offset, skip_spaces);
            if !first && offset < len && is_word_break(text[offset]) {
                return skip(offset);
            }
            first = false;
            if offset == len || len == 0 {
                return Found::None;
            }
            let Some(mut it) = self.breaker(BreakKind::Word, text) else {
                return Found::None;
            };
            let mut punct: Option<usize> = None;
            let mut runner = it.following(offset);
            while let Some(r) = runner {
                let pending = skip_spaces && punct.is_some();
                if is_line_break(at(text, r)) {
                    if let (true, Some(pr)) = (pending, punct) {
                        return skip(pr);
                    }
                    return skip(r);
                }
                if r < len && is_word_boundary(text[r]) {
                    if r > 0 && tb::is_alnum(u32::from(text[r - 1])) {
                        return skip(r);
                    }
                    if skip_spaces && punct.is_none() && r > 0 && is_whitespace(text[r - 1]) {
                        punct = Some(r);
                    }
                    runner = it.following(r);
                    continue;
                }
                if let (true, Some(pr)) = (pending, punct) {
                    return skip(pr);
                }
                // After a word character, or (Windows) at one after a space.
                let stop = r < len
                    && r > 0
                    && (is_word_break(text[r - 1])
                        || (skip_spaces && is_whitespace(text[r - 1]) && is_word_break(text[r])));
                if stop {
                    return skip(r);
                }
                runner = it.following(r);
            }
            if text[len - 1] != NL {
                return Found::After(len - 1);
            }
            Found::None
        })
    }

    /// `PreviousWordPosition`.
    pub(super) fn prev_word(&self, p: Pos) -> Option<Pos> {
        let mut first = true;
        self.find_backward(p, |text, offset| {
            let len = text.len();
            if !first && len > 0 && offset <= len && offset > 0 && is_word_break(text[offset - 1]) {
                return Found::Before(offset);
            }
            first = false;
            if offset == 0 || len == 0 {
                return Found::None;
            }
            let Some(mut it) = self.breaker(BreakKind::Word, text) else {
                return Found::None;
            };
            let mut punct: Option<usize> = None;
            let mut runner = it.preceding(offset);
            while let Some(r) = runner {
                if r < len && is_word_boundary(text[r]) {
                    if r > 0 && tb::is_alnum(u32::from(text[r - 1])) {
                        return Found::Before(r);
                    }
                    punct = Some(r);
                    runner = it.preceding(r);
                    continue;
                }
                if let Some(pr) = punct {
                    return Found::Before(pr);
                }
                if r < len && is_word_break(text[r]) {
                    return Found::Before(r);
                }
                runner = it.preceding(r);
            }
            Found::Before(0)
        })
    }

    // ----- sentences (`position_units_sentence.cc`) -----

    /// `NextSentencePosition`.
    pub(super) fn next_sentence(&self, p: Pos) -> Option<Pos> {
        let mut stop = false;
        self.find_forward(p, |text, offset| {
            if stop {
                return Found::Before(0);
            }
            let len = text.len();
            if offset == len || (offset + 1 == len && text[offset] == NL) {
                stop = true;
                return Found::None;
            }
            match self
                .breaker(BreakKind::Sentence, text)
                .and_then(|mut it| it.following(offset))
            {
                None => Found::None,
                Some(0) => Found::Before(0),
                Some(r) => Found::After(r - 1),
            }
        })
    }

    /// `PreviousSentencePosition`.
    pub(super) fn prev_sentence(&self, p: Pos) -> Option<Pos> {
        self.find_backward(p, |text, passed| {
            let offset = last_non_space(text, passed);
            match self
                .breaker(BreakKind::Sentence, text)
                .and_then(|mut it| it.preceding(offset))
            {
                None => Found::None,
                Some(r) => Found::Before(r),
            }
        })
    }

    /// `StartOfSentencePosition`.
    pub(super) fn sentence_start(&self, p: Pos) -> Option<Pos> {
        self.find_backward(p, |text, passed| {
            let offset = last_non_space(text, passed);
            match self
                .breaker(BreakKind::Sentence, text)
                .and_then(|mut it| it.preceding(offset))
            {
                None if !text.is_empty() => Found::Before(0),
                None => Found::None,
                Some(r) => Found::Before(r),
            }
        })
    }

    /// `EndOfSentence` (trailing space included).
    pub(super) fn sentence_end(&self, p: Pos) -> Option<Pos> {
        self.find_forward(p, |text, passed| {
            let len = text.len();
            let mut offset = passed;
            while offset < len && text[offset] == SP {
                offset += 1;
            }
            match self
                .breaker(BreakKind::Sentence, text)
                .and_then(|mut it| it.following(offset))
            {
                None if len > 0 => Found::Before(len),
                None => Found::None,
                Some(0) => Found::Before(0),
                Some(r) => Found::After(r - 1),
            }
        })
    }

    // ----- paragraphs -----

    pub(super) fn para_start(&self, p: Pos) -> Pos {
        let t = &self.ifcs[p.ifc].text;
        let at = t[..p.at as usize]
            .iter()
            .rposition(|&c| c == NL)
            .map_or(0, |i| i as u32 + 1);
        Pos { ifc: p.ifc, at }
    }

    pub(super) fn para_end(&self, p: Pos) -> Pos {
        let ifc = &self.ifcs[p.ifc];
        let at = ifc.text[p.at as usize..]
            .iter()
            .position(|&c| c == NL)
            .map_or(ifc.last(), |i| p.at + i as u32);
        self.clamp(Pos { ifc: p.ifc, at })
    }

    pub(super) fn doc_start(&self) -> Option<Pos> {
        self.first()
    }

    pub(super) fn doc_end(&self) -> Option<Pos> {
        self.last()
    }

    // ----- lines -----

    fn caret_lines_of(&mut self, n: NodeId) -> &Vec<CaretLine> {
        if !self.caret_lines.contains_key(&n) {
            let v = self.l.caret_lines(self.d, n);
            self.caret_lines.insert(n, v);
        }
        &self.caret_lines[&n]
    }

    /// The line geometry of IFC `i`, computed on first use.
    fn geo(&mut self, i: usize) -> &Geo {
        if !self.scroll_checked {
            // Geometry is in viewport coordinates: drop it after a scroll.
            self.scroll_checked = true;
            let now = Some(self.l.window_scroll(self.d));
            if self.scroll != now {
                self.geo.clear();
                self.caret_lines.clear();
                self.scroll = now;
            }
        }
        if !self.geo.contains_key(&i) {
            let g = self.compute_geo(i);
            self.geo.insert(i, g);
        }
        &self.geo[&i]
    }

    fn lines_in(&mut self, i: usize) -> usize {
        self.geo(i).lines.len()
    }

    fn compute_geo(&mut self, i: usize) -> Geo {
        let srcs = self.ifcs[i].src.clone();
        let text = self.ifcs[i].text.clone();
        let block = self.ifcs[i].block;
        let block_x = self.l.bounding_client_rect(self.d, block).x;
        let mut g = Geo {
            line_of: Vec::with_capacity(srcs.len()),
            left: Vec::with_capacity(srcs.len()),
            right: Vec::with_capacity(srcs.len()),
            lines: Vec::new(),
        };
        let mut text_top: Option<f32> = None;
        let mut bottom = 0.0f32;
        for (k, s) in srcs.iter().enumerate() {
            let after_nl = k == 0 || text[k - 1] == NL;
            let (left, right, top, height, is_text) = match *s {
                Src::Text(n, o) => {
                    let lines = self.caret_lines_of(n);
                    match lines
                        .iter()
                        .find(|l| l.start <= o && o < l.end)
                        .or(lines.last())
                    {
                        Some(l) => {
                            let at = |x: u32| l.xs[(x.clamp(l.start, l.end) - l.start) as usize];
                            (at(o), at(o + 1), l.rect.y, l.rect.height, true)
                        }
                        None => (0.0, 0.0, bottom, 0.0, true),
                    }
                }
                Src::Br(_) => {
                    if after_nl {
                        (block_x, block_x, bottom, 0.0, false)
                    } else {
                        let x = *g.right.last().unwrap_or(&block_x);
                        (x, x, bottom, 0.0, false)
                    }
                }
                Src::Atomic(n) => {
                    let r = self.l.bounding_client_rect(self.d, n);
                    (r.x, r.right(), r.y, r.height, false)
                }
            };
            let new_line = after_nl
                || (is_text && text_top.is_some_and(|t| top > t + 0.5))
                || g.lines.is_empty();
            if new_line {
                g.lines.push(Line {
                    first: k as u32,
                    end: k as u32,
                });
                text_top = None;
            }
            if is_text && text_top.is_none() {
                text_top = Some(top);
            }
            bottom = bottom.max(top + height);
            let li = g.lines.len() - 1;
            g.lines[li].end = k as u32 + 1;
            g.line_of.push(li);
            g.left.push(left);
            g.right.push(right);
        }
        g
    }

    fn line_of(&mut self, p: Pos) -> usize {
        let len = self.ifcs[p.ifc].text.len();
        let k = (p.at as usize).min(len.saturating_sub(1));
        self.geo(p.ifc).line_of[k]
    }

    /// The caret's x (`LineDirectionPointForBlockDirectionNavigation`).
    pub(super) fn caret_x(&mut self, p: Pos) -> f32 {
        self.geo(p.ifc);
        let text = &self.ifcs[p.ifc].text;
        let k = p.at as usize;
        let g = &self.geo[&p.ifc];
        if k < text.len() && (text[k] != NL || k == 0 || text[k - 1] == NL) {
            g.left[k]
        } else if k > 0 {
            g.right[k - 1]
        } else {
            0.0
        }
    }

    /// The position on line `li` of IFC `i` closest to `x`
    /// (`AbstractLineBox::PositionForPoint`).
    fn hit(&mut self, i: usize, li: usize, x: f32) -> Pos {
        self.geo(i);
        let text = &self.ifcs[i].text;
        let srcs = &self.ifcs[i].src;
        let g = &self.geo[&i];
        let line = g.lines[li];
        let (first, mut end) = (line.first as usize, line.end as usize);
        let soft = li + 1 < g.lines.len() && text[end - 1] != NL;
        if soft {
            // Spaces hanging at a soft wrap are not hit.
            while end > first + 1 && text[end - 1] == SP {
                end -= 1;
            }
        }
        // Leaves: runs of one text node, or one <br> / replaced element.
        let mut leaves: Vec<(usize, usize)> = Vec::new();
        for k in first..end {
            let same = leaves.last().is_some_and(|&(_, e)| {
                matches!((srcs[e - 1], srcs[k]), (Src::Text(a, _), Src::Text(b, _)) if a == b)
            });
            if same {
                leaves.last_mut().unwrap().1 = k + 1;
            } else {
                leaves.push((k, k + 1));
            }
        }
        let mut best: Option<((usize, usize), f32)> = None;
        for &(s, e) in &leaves {
            let (min, max) = (g.left[s], g.right[e - 1]);
            if x >= min && x < max {
                best = Some(((s, e), 0.0));
                break;
            }
            let dist = if x < min { min - x } else { x - max + 1.0 };
            if best.is_none_or(|(_, d)| dist < d) {
                best = Some(((s, e), dist));
            }
        }
        let Some(((s, e), _)) = best else {
            return Pos {
                ifc: i,
                at: line.first,
            };
        };
        if !matches!(srcs[s], Src::Text(..)) {
            return Pos {
                ifc: i,
                at: s as u32,
            };
        }
        // The closest boundary in the run.
        let bx = |b: usize| if b < e { g.left[b] } else { g.right[e - 1] };
        let mut at = s;
        for (b, &c) in text.iter().enumerate().take(e).skip(s).chain([(e, &0)]) {
            if b < e && b > s && (0xDC00..0xE000).contains(&c) {
                continue;
            }
            if (bx(b) - x).abs() < (bx(at) - x).abs() {
                at = b;
            }
        }
        self.clamp(Pos {
            ifc: i,
            at: at as u32,
        })
    }

    /// `NextLinePosition`.
    pub(super) fn next_line(&mut self, p: Pos, x: f32) -> Option<Pos> {
        let li = self.line_of(p);
        if li + 1 < self.lines_in(p.ifc) {
            return Some(self.hit(p.ifc, li + 1, x));
        }
        if p.ifc + 1 < self.ifcs.len() {
            return Some(self.hit(p.ifc + 1, 0, x));
        }
        self.last()
    }

    /// `PreviousLinePosition`.
    pub(super) fn prev_line(&mut self, p: Pos, x: f32) -> Option<Pos> {
        let li = self.line_of(p);
        if li > 0 {
            return Some(self.hit(p.ifc, li - 1, x));
        }
        // The last leaf before this line with a caret position at its end
        // (`PreviousRootInlineBoxCandidatePosition`): a text node ending in
        // a line break at the end of its IFC has none there and is skipped.
        for i in (0..p.ifc).rev() {
            let ifc = &self.ifcs[i];
            let mut k = ifc.text.len() - 1;
            if ifc.text[k] == NL
                && let Src::Text(n, _) = ifc.src[k]
            {
                match ifc.src.iter().rposition(|s| s.node() != n) {
                    Some(j) => k = j,
                    None => continue,
                }
            }
            let li = self.geo(i).line_of[k];
            return Some(self.hit(i, li, x));
        }
        self.first()
    }

    /// `LogicalStartOfLine`.
    pub(super) fn line_start(&mut self, p: Pos) -> Pos {
        let li = self.line_of(p);
        let at = self.geo(p.ifc).lines[li].first;
        Pos { ifc: p.ifc, at }
    }

    /// `LogicalEndOfLine`: before a line break, before spaces hanging at a
    /// soft wrap.
    pub(super) fn line_end(&mut self, p: Pos) -> Pos {
        let li = self.line_of(p);
        self.geo(p.ifc);
        let text = &self.ifcs[p.ifc].text;
        let g = &self.geo[&p.ifc];
        let line = g.lines[li];
        let mut end = line.end as usize;
        if text[end - 1] == NL {
            end -= 1;
        } else if li + 1 < g.lines.len() {
            while end > line.first as usize + 1 && text[end - 1] == SP {
                end -= 1;
            }
        }
        self.clamp(Pos {
            ifc: p.ifc,
            at: end as u32,
        })
    }

    /// Same paragraph (`InSameParagraph`).
    pub(super) fn same_para(&self, a: Pos, b: Pos) -> bool {
        a.ifc == b.ifc && self.para_start(a) == self.para_start(b)
    }
}

#[derive(Clone, Copy)]
struct Line {
    first: u32,
    end: u32,
}

/// Per-unit line geometry of an IFC.
struct Geo {
    line_of: Vec<usize>,
    left: Vec<f32>,
    right: Vec<f32>,
    lines: Vec<Line>,
}

enum Found {
    None,
    Before(usize),
    After(usize),
}

fn at(text: &[u16], i: usize) -> u16 {
    text.get(i).copied().unwrap_or(0)
}

fn is_line_break(c: u16) -> bool {
    c == NL || c == u16::from(b'\r')
}

/// Blink's `IsWhitespace` (editing).
fn is_whitespace(c: u16) -> bool {
    matches!(c, 0xA0 | 0x20 | 0x0A | 0x09)
}

fn is_surrogate(c: u16) -> bool {
    (0xD800..0xE000).contains(&c)
}

/// `IsWordBreak`: printable and not white space, a surrogate, a line
/// break or `_`.
fn is_word_break(c: u16) -> bool {
    (tb::is_print(u32::from(c)) && !is_whitespace(c))
        || is_surrogate(c)
        || is_line_break(c)
        || c == u16::from(b'_')
}

/// `IsWordBoundary`: punctuation, a symbol or a surrogate.
fn is_word_boundary(c: u16) -> bool {
    let c32 = u32::from(c);
    tb::is_punct(c32) || (25..=28).contains(&tb::char_type(c32)) || is_surrogate(c)
}

/// `SkipWhitespaceIfNeeded`.
fn skip_ws(text: &[u16], offset: usize, skip_spaces: bool) -> Found {
    if skip_spaces {
        for (r, &c) in text.iter().enumerate().skip(offset) {
            if !(is_whitespace(c) || tb::is_white_space_neutral(u32::from(c))) || is_line_break(c) {
                return Found::Before(r);
            }
        }
    }
    Found::Before(offset)
}

fn last_non_space(text: &[u16], passed: usize) -> usize {
    let mut o = passed;
    while o > 0 && text[o - 1] == SP {
        o -= 1;
    }
    o
}

// ---------------------------------------------------------------------------
// SelectionModifier
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Alter {
    Move,
    Extend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Forward,
    Backward,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Granularity {
    Character,
    Word,
    Sentence,
    Line,
    Paragraph,
    LineBoundary,
    SentenceBoundary,
    ParagraphBoundary,
    DocumentBoundary,
}

impl Granularity {
    pub(super) fn parse(s: &str) -> Option<Granularity> {
        Some(match s.to_ascii_lowercase().as_str() {
            "character" => Granularity::Character,
            "word" => Granularity::Word,
            "sentence" => Granularity::Sentence,
            "line" => Granularity::Line,
            "paragraph" => Granularity::Paragraph,
            "lineboundary" => Granularity::LineBoundary,
            "sentenceboundary" => Granularity::SentenceBoundary,
            "paragraphboundary" => Granularity::ParagraphBoundary,
            "documentboundary" => Granularity::DocumentBoundary,
            _ => return None,
        })
    }

    fn is_boundary(self) -> bool {
        matches!(
            self,
            Granularity::LineBoundary
                | Granularity::ParagraphBoundary
                | Granularity::DocumentBoundary
        )
    }

    fn vertical(self) -> bool {
        matches!(self, Granularity::Line | Granularity::Paragraph)
    }
}

/// The selection `modify` leaves.
pub(super) struct Modified {
    pub(super) anchor: Bp,
    pub(super) focus: Bp,
    /// The x kept for the next line or paragraph move.
    pub(super) xpos: Option<f32>,
}

/// `SelectionModifier::Modify` for the selection `anchor`..`focus`
/// (`directional`: Blink's `FrameSelection::IsDirectional`). `None` when
/// nothing changes.
#[allow(clippy::too_many_arguments)]
pub(super) fn modify(
    m: &mut Model<'_>,
    anchor: Bp,
    focus: Bp,
    directional: bool,
    alter: Alter,
    dir: Direction,
    gran: Granularity,
    xpos: Option<f32>,
    platform: Platform,
) -> Option<Modified> {
    let mac = platform == Platform::Mac;
    let (a0, f0) = (m.canonical(anchor)?, m.canonical(focus)?);
    // PrepareToModifySelection.
    let (mut a, mut f) = (a0, f0);
    if alter == Alter::Extend && a0 != f0 {
        let (s, e) = (a0.min(f0), a0.max(f0));
        let anchor_is_start = if directional {
            a0 < f0
        } else {
            matches!(dir, Direction::Forward | Direction::Right)
        };
        (a, f) = if anchor_is_start { (s, e) } else { (e, s) };
    }
    let range = a != f;
    let anchor_first = a <= f;
    let (start, end) = (a.min(f), a.max(f));
    // PositionForPlatform: Mac uses the range's ends, others the focus.
    let (sfp, efp) = if mac { (start, end) } else { (f, f) };
    let x = if gran.vertical() {
        match xpos {
            Some(x) => x,
            None => m.caret_x(f),
        }
    } else {
        0.0
    };
    let skip = platform == Platform::Windows;
    use Direction::*;
    use Granularity::*;
    let forward = matches!(dir, Forward | Right);
    let pos = match (alter, gran) {
        (Alter::Move, Character) if range => Some(if forward { end } else { start }),
        (_, Character) => {
            if forward {
                m.next_char(f)
            } else {
                m.prev_char(f)
            }
        }
        (_, Word) => {
            if forward {
                m.next_word(f, skip)
            } else {
                m.prev_word(f)
            }
        }
        (_, Sentence) => {
            if forward {
                m.next_sentence(f)
            } else {
                m.prev_sentence(f)
            }
        }
        (_, Line) => {
            let from = if alter == Alter::Move {
                if forward { efp } else { sfp }
            } else {
                f
            };
            if forward {
                m.next_line(from, x)
            } else {
                m.prev_line(from, x)
            }
        }
        (_, Paragraph) => {
            let from = if alter == Alter::Move {
                if forward { efp } else { sfp }
            } else {
                f
            };
            Some(paragraph(m, from, x, forward))
        }
        (_, SentenceBoundary) => {
            if forward {
                m.sentence_end(efp)
            } else {
                m.sentence_start(sfp)
            }
        }
        (Alter::Move, LineBoundary) if matches!(dir, Left | Right) => Some(if dir == Right {
            m.line_end(sfp)
        } else {
            m.line_start(sfp)
        }),
        (_, LineBoundary) => Some(if forward {
            m.line_end(efp)
        } else {
            m.line_start(sfp)
        }),
        (_, ParagraphBoundary) => Some(if forward {
            m.para_end(efp)
        } else {
            m.para_start(sfp)
        }),
        (_, DocumentBoundary) => {
            if forward {
                m.doc_end()
            } else {
                m.doc_start()
            }
        }
    };
    let mut pos = pos?;
    let (na, nf) = match alter {
        Alter::Move => (pos, pos),
        Alter::Extend => {
            if range && matches!(gran, Word | Paragraph | Line) && mac && (a <= pos) != anchor_first
            {
                // Mac: the selection does not cross its anchor at once.
                pos = a;
            }
            if !mac || !range || !gran.is_boundary() {
                (a, pos)
            } else if forward {
                // Mac grows the selection to a boundary.
                if anchor_first { (a, pos) } else { (pos, f) }
            } else if anchor_first {
                (pos, f)
            } else {
                (a, pos)
            }
        }
    };
    // The visible selection's DOM ends (`SelectionTypeAdjuster`): a caret
    // at its canonical position, a range from the most forward start to the
    // most backward end.
    let (anchor, focus) = if na == nf {
        let p = m.dom(na);
        (p, p)
    } else {
        let (s, e) = (m.forward_dom(na.min(nf)), m.backward_dom(na.max(nf)));
        if na < nf { (s, e) } else { (e, s) }
    };
    Some(Modified {
        anchor,
        focus,
        xpos: gran.vertical().then_some(x),
    })
}

/// `Next/PreviousParagraphPosition`: line moves until the paragraph
/// changes.
fn paragraph(m: &mut Model<'_>, from: Pos, x: f32, forward: bool) -> Pos {
    let mut p = from;
    loop {
        let next = if forward {
            m.next_line(p, x)
        } else {
            m.prev_line(p, x)
        };
        match next {
            Some(n) if n != p => p = n,
            _ => break,
        }
        if !m.same_para(from, p) {
            break;
        }
    }
    p
}

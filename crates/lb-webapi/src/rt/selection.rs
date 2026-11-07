//! `Selection` (`window.getSelection()`, `document.getSelection()`,
//! `ShadowRoot.getSelection()`): one per window, for its document, on top
//! of a live [`super::RangeObj`]. Behavior and error texts follow Blink's
//! `DOMSelection` as recorded in Chrome 153 on macOS and Linux (Blink's
//! editing behavior differs: selections are directional everywhere but
//! macOS): at most one range, `addRange` keeps the first, nodes outside the
//! document are ignored, every change but `addRange` makes a new `Range`,
//! a `Range` changed by script makes the selection anchored at its start,
//! `containsNode` uses the visible (canonical, normalized) range. A
//! selection across a shadow boundary keeps both ends while its `Range`
//! collapses; a shadow root's selection sees only its own tree. While a
//! focused text control holds the selection, the `Range` is collapsed
//! before the control and type, direction and text come from the control.
//! `selectionchange` fires on the document once per task in which the
//! selection changed (not for no-op changes), and at a text control, bubbling,
//! when the control's selection changes. `modify` runs Blink's
//! `SelectionModifier` over caret positions of the rendered text
//! ([`caret`]), with the persona's platform behavior.

use super::*;
use crate::rt::opt_string;

#[path = "caret.rs"]
mod caret;

const IFACE: &str = "Selection";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Dir {
    #[default]
    None,
    Forward,
    Backward,
}

/// A window's selection.
#[derive(Default)]
pub(crate) struct SelState {
    /// The `Selection` object.
    obj: Option<u32>,
    /// Its range (a live `Range` id).
    range: Option<u32>,
    dir: Dir,
    /// The range when `dir` was set; a different range has no direction.
    snap: Option<(Bp, Bp)>,
    /// Blink still reports "Range" after `deleteFromDocument` until the
    /// selection changes again.
    deleted: bool,
    /// A `selectionchange` task is queued.
    queued: bool,
    /// The x a line or paragraph `modify` keeps for the next one, while
    /// the selection is the one it made.
    xpos: Option<(f32, (Bp, Bp))>,
    /// `modify`'s caret model of the document, while still valid.
    caret: Option<Box<caret::Cache>>,
    /// Blink caches the selection's `Range` when a DOM method sets it or
    /// script reads it; `modify` leaves none (matters for
    /// `deleteFromDocument`).
    cached: bool,
    /// `ShadowRoot.getSelection()` objects, by shadow root.
    scoped: Vec<(NodeId, u32)>,
    /// Text controls with a `selectionchange` queued.
    controls: Vec<(u32, NodeId)>,
    /// Set by `setBaseAndExtent` across trees: the selection's own anchor
    /// and focus (its `Range` is collapsed at one of them).
    cross: Option<(Bp, Bp)>,
    /// The selection is inside this text control (it has focus and its
    /// selection is the document's, as Blink's frame selection in the
    /// control's inner editor): the `Range` is collapsed before it, the
    /// type, direction and text come from the control's selection.
    control: Option<NodeId>,
}

/// The selection's range now, `None` without one.
fn sel_range(w: &Window) -> Option<(u32, RangeObj)> {
    let id = w.ranges.borrow().sel.range?;
    current(w, id).ok().map(|r| (id, r))
}

/// Blink's `ShouldConsiderSelectionAsDirectional`: every selection is
/// directional except on macOS.
fn mac(w: &Window) -> bool {
    w.profile
        .as_deref()
        .is_some_and(|p| p.os_kind() == lb_persona::OsKind::MacOs)
}

fn dir_now(w: &Window, r: &RangeObj) -> Dir {
    if r.start == r.end && !w.ranges.borrow().sel.deleted {
        return Dir::None;
    }
    let snap = w.ranges.borrow().sel.snap;
    if snap != Some((r.start, r.end)) {
        // Script changed the range: anchor at its start, directional
        // except on macOS.
        return if mac(w) { Dir::None } else { Dir::Forward };
    }
    w.ranges.borrow().sel.dir
}

/// Whether the selection is directional (Blink's `IsDirectional`).
fn directional_now(w: &Window, r: &RangeObj) -> bool {
    let st = w.ranges.borrow();
    if st.sel.snap != Some((r.start, r.end)) {
        return !mac(w);
    }
    st.sel.dir != Dir::None
}

/// (anchor, focus).
fn ends(w: &Window) -> Option<(Bp, Bp)> {
    let (_, r) = sel_range(w)?;
    Some(if dir_now(w, &r) == Dir::Backward {
        (r.end, r.start)
    } else {
        (r.start, r.end)
    })
}

/// Queue `selectionchange` for this task (one per task, as in Chrome:
/// after the task's microtasks, before its timers).
fn changed(w: &Window) {
    let queue = {
        let mut st = w.ranges.borrow_mut();
        st.sel.deleted = false;
        !std::mem::replace(&mut st.sel.queued, true)
    };
    if queue {
        let doc = w.node_ref(0, Document::DOCUMENT);
        w.queue_task(
            super::super::TaskSource::UserInteraction,
            Box::new(move |js, w| {
                w.ranges.borrow_mut().sel.queued = false;
                super::super::events::fire_event(js, w, doc, "selectionchange", false, false);
            }),
        );
    }
}

fn clear(w: &Window) {
    let had = {
        let mut st = w.ranges.borrow_mut();
        st.sel.control = None;
        st.sel.range.take().is_some()
    };
    if had {
        changed(w);
    }
}

/// A text control got focus: the selection moves into it (Blink restores
/// the control's selection into the frame selection). Its `selectionchange`
/// goes to the control, not the document.
pub(crate) fn control_focused(w: &Window, n: NodeId) {
    let Some(at) = w.doc_slot(0, |d| d.parent(n).map(|p| (p, index(d, n)))) else {
        return;
    };
    let id = new_range(w, 0, at);
    let mut st = w.ranges.borrow_mut();
    st.sel.range = Some(id);
    st.sel.dir = Dir::None;
    st.sel.snap = Some((at, at));
    st.sel.deleted = false;
    st.sel.xpos = None;
    st.sel.cached = true;
    st.sel.control = Some(n);
}

/// The selection is in text control `n` (for its `selectionStart` while
/// focused: 0 when the selection is elsewhere).
pub(crate) fn in_control(w: &Window, n: NodeId) -> bool {
    w.ranges.borrow().sel.control == Some(n) && sel_range(w).is_some()
}

/// The selection's text control, while the selection is in one.
fn control_of(w: &Window) -> Option<NodeId> {
    let n = w.ranges.borrow().sel.control?;
    let (_, r) = sel_range(w)?;
    let at = w.doc_slot(0, |d| d.parent(n).map(|p| (p, index(d, n))))?;
    (r.start == at && r.end == at).then_some(n)
}

/// A text control's selection changed: `selectionchange` at the control,
/// bubbling, once per control and task (Blink's
/// `TextControlElement::ScheduleSelectionchangeEvent`).
pub(crate) fn control_changed(w: &Window, slot: u32, n: NodeId) {
    {
        let mut st = w.ranges.borrow_mut();
        if st.sel.controls.contains(&(slot, n)) {
            return;
        }
        st.sel.controls.push((slot, n));
    }
    let target = w.node_ref(slot, n);
    w.queue_task(
        super::super::TaskSource::UserInteraction,
        Box::new(move |js, w| {
            w.ranges
                .borrow_mut()
                .sel
                .controls
                .retain(|&c| c != (slot, n));
            super::super::events::fire_event(js, w, target, "selectionchange", true, false);
        }),
    );
}

/// Script changed the selection's `Range` (Blink's
/// `Range::UpdateSelectionIfAddedToSelection`).
pub(super) fn range_changed(w: &Window, id: u32, old: &RangeObj, new: &RangeObj) {
    let ours = w.ranges.borrow().sel.range == Some(id);
    if ours && (old.start, old.end) != (new.start, new.end) {
        w.ranges.borrow_mut().sel.xpos = None;
        changed(w);
    }
}

/// Select `anchor` to `focus` with a new range (both checked).
fn select(w: &Window, anchor: Bp, focus: Bp, directional: bool) {
    let directional = directional || !mac(w);
    let before = sel_range(w).map(|(_, r)| {
        let d = dir_now(w, &r);
        let ends = if d == Dir::Backward {
            (r.end, r.start)
        } else {
            (r.start, r.end)
        };
        (ends, directional_now(w, &r))
    });
    let back = w.doc_slot(0, |d| bp_cmp(d, focus, anchor) == Ordering::Less);
    let (start, end) = if back {
        (focus, anchor)
    } else {
        (anchor, focus)
    };
    let id = new_range(w, 0, start);
    put(
        w,
        id,
        RangeObj {
            slot: 0,
            start,
            end,
            kind: Kind::Live,
        },
    );
    {
        let mut st = w.ranges.borrow_mut();
        st.sel.range = Some(id);
        st.sel.dir = match (directional, back) {
            (false, _) => Dir::None,
            (true, true) => Dir::Backward,
            (true, false) => Dir::Forward,
        };
        st.sel.snap = Some((start, end));
        st.sel.xpos = None;
        st.sel.cached = true;
        st.sel.control = None;
        st.sel.cross = None;
    }
    if before != Some(((anchor, focus), directional)) {
        changed(w);
    }
}

/// `setBaseAndExtent` with ends in different trees (one in a shadow
/// tree): Blink's `Range` collapses at the later end (`setEnd` into another
/// tree), while the selection keeps both ends: a "Range" with a collapsed
/// `Range`.
fn select_across(w: &Window, base: Bp, ext: Bp) {
    let back = w.doc_slot(0, |d| {
        match (
            adjust(d, Document::DOCUMENT, base),
            adjust(d, Document::DOCUMENT, ext),
        ) {
            (Some(b), Some(e)) => bp_cmp(d, e, b) == Ordering::Less,
            _ => false,
        }
    });
    let at = if back { base } else { ext };
    select(w, at, at, true);
    let mut st = w.ranges.borrow_mut();
    st.sel.deleted = true;
    st.sel.dir = if back { Dir::Backward } else { Dir::Forward };
    st.sel.cross = Some((base, ext));
}

/// The selection's own (anchor, focus) when they are in different trees.
fn cross(w: &Window) -> Option<(Bp, Bp)> {
    let (_, r) = sel_range(w)?;
    let st = w.ranges.borrow();
    let c = st.sel.cross?;
    (st.sel.snap == Some((r.start, r.end))).then_some(c)
}

/// Blink's `IsValidForPosition`: a node of this document, connected.
fn valid(w: &Window, slot: u32, n: NodeId) -> bool {
    slot == 0 && w.doc_slot(0, |d| d.shadow_including_root(n) == Document::DOCUMENT)
}

/// `this` as a `Selection`: its tree scope (the document, or the shadow
/// root of a `ShadowRoot.getSelection()` object).
fn this_sel(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<NodeId, HostError> {
    let id = this_obj(cx)?;
    w.with_obj(id, |o| match o {
        Obj::Range(r) if r.kind == Kind::Selection => Some(r.start.0),
        _ => None,
    })
    .flatten()
    .ok_or_else(illegal)
}

/// Blink's `ShadowAdjustedNode`/`ShadowAdjustedOffset`: `bp` seen from
/// `scope` (the position before the shadow host in that scope when `bp`
/// is in a shadow tree inside it), `None` if no ancestor is in `scope`.
fn adjust(d: &Document, scope: NodeId, bp: Bp) -> Option<Bp> {
    let mut n = bp.0;
    if d.shadow_including_root(n) != Document::DOCUMENT && scope == Document::DOCUMENT {
        return None;
    }
    if d.root(n) == scope {
        return Some(bp);
    }
    loop {
        let host = d.shadow_host(d.root(n))?;
        if d.root(host) == scope {
            let p = d.parent(host)?;
            return Some((p, index(d, host)));
        }
        n = host;
    }
}

/// Blink's `DomSelection::Rescope`: `bp` moved out of every shadow tree
/// not in (or inside) `roots`, to before the host (after it for an end).
fn rescope(d: &Document, bp: Bp, roots: &[NodeId], end: bool) -> Bp {
    let (mut n, mut off) = bp;
    loop {
        let root = d.root(n);
        let Some(host) = d.shadow_host(root) else {
            return (n, off);
        };
        let kept = roots.iter().any(|&r| {
            r == root
                || std::iter::successors(Some(r), |&x| d.shadow_host(d.root(x)))
                    .any(|x| x == root || d.root(x) == root)
        });
        if kept {
            return (n, off);
        }
        let Some(p) = d.parent(host) else {
            return (n, off);
        };
        off = index(d, host) + u32::from(end);
        n = p;
    }
}

/// The window lost its frame (a removed iframe's): Blink's
/// `DOMSelection::IsAvailable` is false and the getters report nothing.
fn gone(w: &Window) -> bool {
    crate::rt::frames::is_detached(w)
}

/// (anchor, focus) as `scope`'s selection reports them, `None` when it has
/// no range.
fn scoped_ends(w: &Window, scope: NodeId) -> Option<(Bp, Bp)> {
    if gone(w) {
        return None;
    }
    if scope == Document::DOCUMENT {
        w.ranges.borrow_mut().sel.cached = true;
        return ends(w);
    }
    let (a, f) = cross(w).or_else(|| ends(w))?;
    w.doc_slot(0, |d| Some((adjust(d, scope, a)?, adjust(d, scope, f)?)))
}

/// `scope`'s selection as a range (start, end).
fn scoped_range(w: &Window, scope: NodeId) -> Option<(Bp, Bp)> {
    if scope == Document::DOCUMENT {
        return sel_range(w).map(|(_, r)| (r.start, r.end));
    }
    let (a, f) = scoped_ends(w, scope)?;
    Some(if w.doc_slot(0, |d| bp_cmp(d, f, a) == Ordering::Less) {
        (f, a)
    } else {
        (a, f)
    })
}

fn sel_err(w: &Window, name: &str, op: &str, msg: impl std::fmt::Display) -> HostError {
    op_err_on(w, name, IFACE, op, msg)
}

/// Optional node argument `i` (`Node?`).
fn opt_node(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    op: &str,
) -> Result<Option<(u32, NodeId)>, HostError> {
    if matches!(cx.arg_kind(i), ValueKind::Null | ValueKind::Undefined) {
        return Ok(None);
    }
    arg_node(cx, w, i, op, IFACE).map(Some)
}

fn opt_offset(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Result<u32, HostError> {
    if i >= cx.len() || cx.arg_kind(i) == ValueKind::Undefined {
        Ok(0)
    } else {
        cx.arg_u32(i)
    }
}

fn check(w: &Window, op: &str, (s, n): (u32, NodeId), off: u32) -> R {
    w.doc_slot(s, |d| check_bp_on(w, d, IFACE, op, n, off))
}

/// The window's `Selection` object.
/// A shadow root's `Selection` object (`ShadowRoot.getSelection()`).
fn scoped_obj(w: &Window, root: NodeId) -> u32 {
    if let Some(&(_, id)) = w
        .ranges
        .borrow()
        .sel
        .scoped
        .iter()
        .find(|(r, _)| *r == root)
    {
        return id;
    }
    let id = w.new_obj(Obj::Range(Box::new(RangeObj {
        slot: 0,
        start: (root, 0),
        end: (root, 0),
        kind: Kind::Selection,
    })));
    w.ranges.borrow_mut().sel.scoped.push((root, id));
    id
}

fn selection_obj(w: &Window) -> u32 {
    if let Some(id) = w.ranges.borrow().sel.obj {
        return id;
    }
    let id = w.new_obj(Obj::Range(Box::new(RangeObj {
        slot: 0,
        start: (Document::DOCUMENT, 0),
        end: (Document::DOCUMENT, 0),
        kind: Kind::Selection,
    })));
    w.ranges.borrow_mut().sel.obj = Some(id);
    id
}

/// The range moved forward to its first text and back to its last
/// (Blink's `ToNormalizedEphemeralRange`, roughly).
fn normalized(d: &Document, r: &RangeObj) -> RangeObj {
    let text = |n: NodeId| d.node_type(n) == NodeType::Text && node_len(d, n) > 0;
    let mut out = *r;
    if !is_chardata(d, r.start.0) {
        let mut cur = child_at(d, r.start.0, r.start.1).or_else(|| next_skipping(d, r.start.0));
        while let Some(n) = cur {
            if bp_cmp(d, (n, 0), r.end) != Ordering::Less {
                break;
            }
            if text(n) {
                out.start = (n, 0);
                break;
            }
            cur = next_node(d, n);
        }
    }
    if !is_chardata(d, r.end.0) {
        // Nodes before the end, last first.
        let root = d.root(r.end.0);
        let before: Vec<NodeId> = d
            .descendants(root)
            .filter(|&n| text(n) && bp_cmp(d, (n, node_len(d, n)), r.end) != Ordering::Greater)
            .collect();
        if let Some(&n) = before.last()
            && bp_cmp(d, (n, node_len(d, n)), out.start) == Ordering::Greater
        {
            out.end = (n, node_len(d, n));
        }
    }
    out
}

/// The persona's locale for ICU's break iterators.
fn locale(w: &Window) -> String {
    w.profile
        .as_deref()
        .map_or_else(|| "en-US".to_string(), |p| p.locale.language.clone())
}

/// Runs `f` on the document's caret model (kept between calls while the
/// document's style and layout are unchanged).
fn with_model<T>(w: &Window, f: impl FnOnce(&mut caret::Model<'_>) -> T) -> T {
    let locale = locale(w);
    let cache = w.ranges.borrow_mut().sel.caret.take();
    let (out, cache) = w.with_layout(0, |l, d| {
        let mut m = caret::Model::new(d, l, &locale, cache);
        let out = f(&mut m);
        (out, m.into_cache())
    });
    w.ranges.borrow_mut().sel.caret = Some(cache);
    out
}

/// Blink's `VisibleSelection::ToNormalizedEphemeralRange`: the ends at
/// their caret positions, the start moved forward and the end backward
/// over what renders nothing (a caret: its most backward position).
fn visible_range(w: &Window, r: &RangeObj) -> RangeObj {
    let ends = with_model(w, |m| {
        let (s, e) = (m.canonical(r.start)?, m.canonical(r.end)?);
        Some(if s == e {
            let p = m.dom(s);
            (p, p)
        } else {
            (m.forward_dom(s.min(e)), m.backward_dom(s.max(e)))
        })
    });
    match ends {
        Some((start, end)) => {
            let ok = w.doc_slot(0, |d| d.root(start.0) == d.root(end.0));
            if !ok {
                return w.doc_slot(0, |d| normalized(d, r));
            }
            let end = if w.doc_slot(0, |d| bp_cmp(d, end, start) == Ordering::Less) {
                start
            } else {
                end
            };
            RangeObj { start, end, ..*r }
        }
        None => w.doc_slot(0, |d| normalized(d, r)),
    }
}

/// Blink's `DOMSelection::containsNode` on the visible range `r`.
fn contains_node(d: &Document, r: &RangeObj, n: NodeId, partial: bool) -> bool {
    let Some(p) = d.parent(n) else { return false };
    if d.root(n) != d.root(r.start.0) {
        return false;
    }
    let i = index(d, n);
    let fully = bp_cmp(d, (p, i), r.start) != Ordering::Less
        && bp_cmp(d, (p, i + 1), r.end) != Ordering::Greater;
    if fully {
        return true;
    }
    let outside = bp_cmp(d, (p, i), r.end) == Ordering::Greater
        || bp_cmp(d, (p, i + 1), r.start) == Ordering::Less;
    !outside && (partial || d.node_type(n) == NodeType::Text)
}

enum Piece {
    Text(String),
    /// A block or row boundary (1 newline; 2 around `<p>`), the element.
    Break(u8, NodeId),
    /// Between table cells.
    Tab,
    /// Laid-out text not emitted (`visibility: hidden`): white space does
    /// not collapse across it.
    Barrier,
}

/// The selection's text as Blink's `TextIterator` emits it for
/// `Selection.toString()`: collapsed white space, `\n` for `<br>` and
/// between blocks (`\n\n` around `<p>`), `\t` between table cells,
/// `display: none` and `visibility: hidden` text and form control
/// contents left out.
fn rendered_text(w: &Window, r: &RangeObj) -> String {
    let mut pieces = Vec::new();
    let mut tail = false;
    let mut table_start = false;
    w.with_layout(r.slot, |l, d| {
        let ca = common_ancestor(d, r);
        let hidden = std::iter::once(ca)
            .chain(d.ancestors(ca))
            .any(|n| d.is_element(n) && l.computed_style(d, n).is_none_or(|s| s.is_display_none()));
        if hidden {
            return;
        }
        walk(l, d, r, ca, &mut pieces);
        // A trailing block boundary counts when something (rendered or not)
        // follows the block inside the range (Blink exits the block only
        // then).
        // (Exits of blocks around the range's end are past it.)
        let last = pieces.iter().rev().find(|p| match p {
            Piece::Barrier => false,
            Piece::Break(_, b) => !inclusive_ancestor(d, *b, r.end.0),
            _ => true,
        });
        if let Some(&Piece::Break(_, b)) = last {
            tail = next_skipping(d, b).is_some_and(|n| {
                d.parent(n)
                    .is_some_and(|p| bp_cmp(d, (p, index(d, n)), r.end) == Ordering::Less)
            });
        }
        // A range from the start of a table emits a newline before its
        // first row and after its last (the table is not on the line the
        // range starts on).
        let mut is_table = |n: NodeId| {
            l.computed_style(d, n).is_some_and(|s| {
                matches!(
                    s.display,
                    lb_css::Display::Table
                        | lb_css::Display::InlineTable
                        | lb_css::Display::TableRowGroup
                        | lb_css::Display::TableHeaderGroup
                        | lb_css::Display::TableFooterGroup
                )
            })
        };
        if r.start.1 == 0 && d.is_element(r.start.0) && is_table(r.start.0) {
            table_start = true;
            if r.end.0 == r.start.0 && r.end.1 == node_len(d, r.end.0) {
                tail = true;
            }
        }
    });
    let mut out = String::new();
    let mut breaks: u8 = 0;
    let mut tab = false;
    let mut barrier = false;
    for p in pieces {
        match p {
            Piece::Break(n, _) => breaks = breaks.max(n),
            Piece::Tab => tab = true,
            Piece::Barrier => barrier = true,
            Piece::Text(t) => {
                let mut t = t.as_str();
                if breaks > 0 {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    if !out.is_empty() {
                        out.extend(std::iter::repeat_n('\n', breaks as usize));
                    } else if table_start {
                        out.push('\n');
                    }
                    breaks = 0;
                    tab = false;
                    barrier = false;
                }
                if tab {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    if !out.is_empty() {
                        out.push('\t');
                    }
                    tab = false;
                    barrier = false;
                }
                if !barrier && (out.is_empty() || out.ends_with([' ', '\n', '\t'])) {
                    t = t.trim_start_matches(' ');
                }
                barrier = false;
                out.push_str(t);
            }
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    if tail && breaks > 0 && !out.is_empty() {
        out.push('\n');
    }
    out
}

fn walk(
    l: &mut lb_layout::LayoutEngine,
    d: &Document,
    r: &RangeObj,
    n: NodeId,
    out: &mut Vec<Piece>,
) {
    let len = node_len(d, n);
    let before_end = n == r.end.0 || bp_cmp(d, (n, 0), r.end) == Ordering::Less;
    let after_start = n == r.start.0 || bp_cmp(d, (n, len), r.start) == Ordering::Greater;
    if !(before_end && after_start) {
        return;
    }
    match d.node_type(n) {
        NodeType::Text => {
            let Some(css) = d.parent(n).and_then(|p| l.computed_style(d, p)) else {
                return;
            };
            if css.visibility != lb_css::Visibility::Visible {
                out.push(Piece::Barrier);
                return;
            }
            let a = if n == r.start.0 { r.start.1 } else { 0 };
            let b = if n == r.end.0 { r.end.1 } else { len };
            let s = utf16_sub(d.character_data(n).unwrap_or(""), a, b);
            let ws = css.white_space;
            let mut t = String::new();
            if ws.collapses_spaces() {
                let mut space = false;
                for c in s.chars() {
                    match c {
                        '\n' if ws.preserves_newlines() => {
                            t.push('\n');
                            space = false;
                        }
                        ' ' | '\t' | '\n' | '\r' | '\x0c' => space = true,
                        c => {
                            if space {
                                t.push(' ');
                            }
                            space = false;
                            t.push(c);
                        }
                    }
                }
                if space {
                    t.push(' ');
                }
            } else {
                t = s;
            }
            if !t.is_empty() {
                out.push(Piece::Text(t));
            }
        }
        NodeType::Element => {
            let Some(css) = l.computed_style(d, n) else {
                return;
            };
            if css.is_display_none() {
                return;
            }
            let html = |name: &str| d.is_html_element_named(n, &LocalName::from(name));
            if html("br") {
                out.push(Piece::Text("\n".into()));
                return;
            }
            // Form controls' contents are their own (UA shadow) text.
            if html("textarea") || html("select") {
                return;
            }
            use lb_css::Display as D;
            let cell = css.display == D::TableCell;
            if cell && d.prev_sibling(n).is_some_and(|p| d.is_element(p)) {
                out.push(Piece::Tab);
            }
            let block = !cell
                && !css.display.is_inline_level()
                && css.display != D::Contents
                && !matches!(
                    css.display,
                    D::Table | D::TableRowGroup | D::TableHeaderGroup | D::TableFooterGroup
                );
            let brk = if html("p") { 2 } else { 1 };
            if block {
                out.push(Piece::Break(brk, n));
            }
            for c in d.children(n) {
                walk(l, d, r, c, out);
            }
            if block {
                out.push(Piece::Break(brk, n));
            }
        }
        _ => {
            for c in d.children(n) {
                walk(l, d, r, c, out);
            }
        }
    }
}

fn register_selection(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "Window", "getSelection", |cx, w| {
        // A removed frame's window has no frame and no selection.
        if crate::rt::frames::is_detached(w) {
            return cx.ret(lb_js::Arg::Null);
        }
        let id = selection_obj(w);
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "Document", "getSelection", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        if slot != 0 || n != Document::DOCUMENT || crate::rt::frames::is_detached(w) {
            return cx.ret(lb_js::Arg::Null);
        }
        let id = selection_obj(w);
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "ShadowRoot", "getSelection", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        if slot != 0 || !w.doc_slot(0, |d| d.is_shadow_root(n)) {
            return cx.ret(lb_js::Arg::Null);
        }
        let id = scoped_obj(w, n);
        cx.ret(w.obj_arg(id))
    });
    for (prop, anchor) in [
        ("anchorNode", true),
        ("baseNode", true),
        ("focusNode", false),
        ("extentNode", false),
    ] {
        getter(reg, rt, IFACE, prop, move |cx, w| {
            let scope = this_sel(cx, w)?;
            let n = scoped_ends(w, scope).map(|(a, f)| if anchor { a.0 } else { f.0 });
            cx.ret(w.opt_node_arg(0, n))
        });
    }
    for (prop, anchor) in [
        ("anchorOffset", true),
        ("baseOffset", true),
        ("focusOffset", false),
        ("extentOffset", false),
    ] {
        getter(reg, rt, IFACE, prop, move |cx, w| {
            let scope = this_sel(cx, w)?;
            let o = scoped_ends(w, scope).map_or(0, |(a, f)| if anchor { a.1 } else { f.1 });
            cx.ret(o)
        });
    }
    getter(reg, rt, IFACE, "isCollapsed", |cx, w| {
        let scope = this_sel(cx, w)?;
        cx.ret(scoped_ends(w, scope).is_none_or(|(a, f)| a == f))
    });
    getter(reg, rt, IFACE, "rangeCount", |cx, w| {
        let scope = this_sel(cx, w)?;
        if gone(w) {
            return cx.ret(0u32);
        }
        let n = if scope == Document::DOCUMENT {
            sel_range(w).is_some()
        } else {
            scoped_ends(w, scope).is_some()
        };
        cx.ret(u32::from(n))
    });
    getter(reg, rt, IFACE, "type", |cx, w| {
        let scope = this_sel(cx, w)?;
        if gone(w) {
            return cx.ret("");
        }
        let has = if scope == Document::DOCUMENT {
            sel_range(w).is_some()
        } else {
            scoped_ends(w, scope).is_some()
        };
        if has && let Some(n) = control_of(w) {
            let c = crate::rt::input::selection(w, 0, n);
            return cx.ret(if c.start == c.end { "Caret" } else { "Range" });
        }
        let t = match sel_range(w) {
            Some(_) if !has => "None",
            None => "None",
            Some((_, r)) if r.start == r.end && !w.ranges.borrow().sel.deleted => "Caret",
            Some(_) => "Range",
        };
        cx.ret(t)
    });
    getter(reg, rt, IFACE, "direction", |cx, w| {
        this_sel(cx, w)?;
        if let Some(n) = control_of(w) {
            let c = crate::rt::input::selection(w, 0, n);
            return cx.ret(match c.dir {
                _ if c.start == c.end => "none",
                1 => "forward",
                2 => "backward",
                _ if mac(w) => "none",
                _ => "forward",
            });
        }
        let d = sel_range(w).map_or(Dir::None, |(_, r)| dir_now(w, &r));
        cx.ret(match d {
            Dir::None => "none",
            Dir::Forward => "forward",
            Dir::Backward => "backward",
        })
    });
    method(reg, rt, IFACE, "getRangeAt", |cx, w| {
        let scope = this_sel(cx, w)?;
        need_args(cx, 1, "getRangeAt", IFACE)?;
        let i = cx.arg_u32(0)?;
        let bad = || {
            sel_err(
                w,
                "IndexSizeError",
                "getRangeAt",
                format!("{i} is not a valid index."),
            )
        };
        if scope != Document::DOCUMENT {
            // A new `Range` per call, as for Blink's non-document scopes.
            let Some((start, end)) = scoped_range(w, scope).filter(|_| i == 0) else {
                return Err(bad());
            };
            let id = new_range(w, 0, start);
            put(
                w,
                id,
                RangeObj {
                    slot: 0,
                    start,
                    end,
                    kind: Kind::Live,
                },
            );
            return cx.ret(w.obj_arg(id));
        }
        match sel_range(w) {
            Some((id, _)) if i == 0 => {
                w.ranges.borrow_mut().sel.cached = true;
                cx.ret(w.obj_arg(id))
            }
            _ => Err(bad()),
        }
    });
    method(reg, rt, IFACE, "getComposedRanges", |cx, w| {
        this_sel(cx, w)?;
        // `{shadowRoots: [...]}`, or shadow roots as arguments (the older
        // form Chrome still takes).
        let mut roots: Vec<NodeId> = Vec::new();
        let mut take = |cx: &mut CallCtx<'_, '_, '_>, h: lb_js::Handle| {
            if let Some(hr) = cx.handle_host(h)
                && let Some((0, n)) = w.decode_node(hr)
            {
                roots.push(n);
            }
        };
        if !cx.is_empty() && cx.arg_host(0).is_none() {
            let dict = Dict::arg(cx, 0);
            if let Some(list) = dict.get(cx, "shadowRoots")? {
                let len = cx.get_property(list, "length")?;
                let n = match cx.handle_value(len) {
                    lb_js::JsValue::Number(n) => n as usize,
                    _ => 0,
                };
                cx.release(len);
                for i in 0..n.min(1024) {
                    let item = cx.get_property(list, &i.to_string())?;
                    take(cx, item);
                    cx.release(item);
                }
                cx.release(list);
            }
            dict.release(cx);
        }
        for i in 0..cx.len() {
            if cx.arg_host(i).is_some() {
                let h = cx.arg_handle(i);
                take(cx, h);
            }
        }
        let mut ids = Vec::new();
        if let Some((_, mut r)) = sel_range(w) {
            w.ranges.borrow_mut().sel.cached = true;
            if let Some((a, f)) = cross(w) {
                let back = w.ranges.borrow().sel.dir == Dir::Backward;
                (r.start, r.end) = if back { (f, a) } else { (a, f) };
            }
            let (start, end) = w.doc_slot(0, |d| {
                (
                    rescope(d, r.start, &roots, false),
                    rescope(d, r.end, &roots, true),
                )
            });
            ids.push(w.new_obj(Obj::Range(Box::new(RangeObj {
                kind: Kind::StaticRange,
                start,
                end,
                ..r
            }))));
        }
        cx.ret(lb_js::Arg::Array(
            ids.iter().map(|&id| w.obj_arg(id)).collect(),
        ))
    });
    method(reg, rt, IFACE, "addRange", |cx, w| {
        this_sel(cx, w)?;
        need_args(cx, 1, "addRange", IFACE)?;
        let id = cx
            .arg_host(0)
            .filter(|h| h.kind == K_OBJ)
            .and_then(|h| {
                w.with_obj(h.id, |o| matches!(o, Obj::Range(x) if x.kind == Kind::Live))
                    .filter(|&ok| ok)
                    .map(|_| h.id)
            })
            .ok_or_else(|| {
                HostError::type_error(
                    "Failed to execute 'addRange' on 'Selection': parameter 1 is not of type 'Range'.",
                )
            })?;
        if sel_range(w).is_some() {
            return Ok(());
        }
        let r = current(w, id)?;
        if !valid(w, r.slot, r.start.0) {
            return Ok(());
        }
        {
            let mut st = w.ranges.borrow_mut();
            st.sel.range = Some(id);
            st.sel.dir = Dir::None;
            st.sel.snap = None;
        }
        changed(w);
        Ok(())
    });
    method(reg, rt, IFACE, "removeRange", |cx, w| {
        this_sel(cx, w)?;
        need_args(cx, 1, "removeRange", IFACE)?;
        let h = cx.arg_host(0).filter(|h| h.kind == K_OBJ);
        let is_range = h.is_some_and(|h| {
            w.with_obj(h.id, |o| matches!(o, Obj::Range(x) if x.kind == Kind::Live))
                .unwrap_or(false)
        });
        if !is_range {
            return Err(HostError::type_error(
                "Failed to execute 'removeRange' on 'Selection': parameter 1 is not of type 'Range'.",
            ));
        }
        if h.map(|h| h.id) != w.ranges.borrow().sel.range {
            return Err(sel_err(
                w,
                "NotFoundError",
                "removeRange",
                "Range not found.",
            ));
        }
        clear(w);
        Ok(())
    });
    for op in ["removeAllRanges", "empty"] {
        method(reg, rt, IFACE, op, |cx, w| {
            this_sel(cx, w)?;
            if sel_range(w).is_some() {
                clear(w);
            }
            Ok(())
        });
    }
    for op in ["collapse", "setPosition"] {
        method(reg, rt, IFACE, op, move |cx, w| {
            this_sel(cx, w)?;
            need_args(cx, 1, op, IFACE)?;
            let n = opt_node(cx, w, 0, op)?;
            let off = opt_offset(cx, 1)?;
            let Some(n) = n else {
                clear(w);
                return Ok(());
            };
            check(w, op, n, off)?;
            if valid(w, n.0, n.1) {
                select(w, (n.1, off), (n.1, off), false);
            }
            Ok(())
        });
    }
    for (op, start) in [("collapseToStart", true), ("collapseToEnd", false)] {
        method(reg, rt, IFACE, op, move |cx, w| {
            this_sel(cx, w)?;
            let Some((_, r)) = sel_range(w) else {
                return Err(sel_err(
                    w,
                    "InvalidStateError",
                    op,
                    "there is no selection.",
                ));
            };
            let p = if start { r.start } else { r.end };
            select(w, p, p, false);
            Ok(())
        });
    }
    method(reg, rt, IFACE, "extend", |cx, w| {
        this_sel(cx, w)?;
        need_args(cx, 1, "extend", IFACE)?;
        let n = arg_node(cx, w, 0, "extend", IFACE)?;
        let off = opt_offset(cx, 1)?;
        if !valid(w, n.0, n.1) {
            return Ok(());
        }
        let Some((anchor, _)) = ends(w) else {
            return Err(sel_err(
                w,
                "InvalidStateError",
                "extend",
                "This Selection object doesn't have any Ranges.",
            ));
        };
        check(w, "extend", n, off)?;
        let focus = (n.1, off);
        // Another tree (a shadow tree): Blink's new range collapses there.
        if w.doc_slot(0, |d| d.root(anchor.0) != d.root(focus.0)) {
            select(w, focus, focus, true);
        } else {
            select(w, anchor, focus, true);
        }
        Ok(())
    });
    method(reg, rt, IFACE, "setBaseAndExtent", |cx, w| {
        this_sel(cx, w)?;
        need_args(cx, 4, "setBaseAndExtent", IFACE)?;
        let base = opt_node(cx, w, 0, "setBaseAndExtent")?;
        let bo = cx.arg_u32(1)?;
        let ext = opt_node(cx, w, 2, "setBaseAndExtent")?;
        let eo = cx.arg_u32(3)?;
        let Some(base) = base else {
            clear(w);
            return Ok(());
        };
        check(w, "setBaseAndExtent", base, bo)?;
        let (ext, eo) = match ext {
            Some(e) => {
                check(w, "setBaseAndExtent", e, eo)?;
                (e, eo)
            }
            None => (base, bo),
        };
        if valid(w, base.0, base.1) && valid(w, ext.0, ext.1) {
            let (b, e) = ((base.1, bo), (ext.1, eo));
            if w.doc_slot(0, |d| d.root(b.0) == d.root(e.0)) {
                select(w, b, e, true);
            } else {
                select_across(w, b, e);
            }
        }
        Ok(())
    });
    method(reg, rt, IFACE, "selectAllChildren", |cx, w| {
        this_sel(cx, w)?;
        need_args(cx, 1, "selectAllChildren", IFACE)?;
        let (s, n) = arg_node(cx, w, 0, "selectAllChildren", IFACE)?;
        let len = w.doc_slot(s, |d| {
            if d.node_type(n) == NodeType::DocumentType {
                Err(type_err(w, d, IFACE, "selectAllChildren", n))
            } else {
                Ok(d.child_count(n) as u32)
            }
        })?;
        if valid(w, s, n) {
            select(w, (n, 0), (n, len), true);
        }
        Ok(())
    });
    method(reg, rt, IFACE, "deleteFromDocument", |cx, w| {
        this_sel(cx, w)?;
        let Some((_, r)) = sel_range(w) else {
            return Ok(());
        };
        if r.start == r.end {
            return Ok(());
        }
        // Where the anchor and focus end up (Blink moves the selection's
        // ends as live ranges move): apart, the selection stays a "Range"
        // although its `Range` is collapsed.
        let probes = [new_range(w, 0, r.start), new_range(w, 0, r.end)];
        let bp = delete_contents(w, r);
        w.after_mutation(cx);
        let moved: Vec<Bp> = probes
            .iter()
            .filter_map(|&id| current(w, id).ok().map(|p| p.start))
            .collect();
        w.ranges.borrow_mut().live.retain(|id| !probes.contains(id));
        let (cached, dir) = {
            let st = w.ranges.borrow();
            (st.sel.cached, dir_now(w, &r))
        };
        // Without a cached `Range` the selection's own ends moved with the
        // DOM: the caret is where its start went.
        let bp = match moved.first() {
            Some(&m) if !cached => m,
            _ => bp,
        };
        select(w, bp, bp, false);
        changed(w);
        let mut st = w.ranges.borrow_mut();
        st.sel.deleted = moved.len() == 2 && moved[0] != moved[1];
        // Deleting through the cached `Range` resets the selection to it
        // (non-directional on macOS); otherwise the selection's ends just
        // move with the DOM and keep their direction.
        st.sel.dir = if !cached {
            dir
        } else if mac(w) {
            Dir::None
        } else {
            Dir::Forward
        };
        Ok(())
    });
    method(reg, rt, IFACE, "containsNode", |cx, w| {
        this_sel(cx, w)?;
        need_args(cx, 1, "containsNode", IFACE)?;
        let (s, n) = arg_node(cx, w, 0, "containsNode", IFACE)?;
        let partial = cx.arg_bool(1);
        let v = match sel_range(w) {
            Some((_, r)) if s == r.slot && s == 0 => {
                let r = visible_range(w, &r);
                w.doc_slot(s, |d| contains_node(d, &r, n, partial))
            }
            _ => false,
        };
        cx.ret(v)
    });
    method(reg, rt, IFACE, "toString", |cx, w| {
        this_sel(cx, w)?;
        if gone(w) {
            return cx.ret("");
        }
        if let (Some((a, f)), Some((_, r))) = (cross(w), sel_range(w)) {
            // The selection kept out of the shadow tree it ends in (Blink's
            // `AdjustSelectionToAvoidCrossingShadowBoundaries`): up to the
            // host.
            let back = w.ranges.borrow().sel.dir == Dir::Backward;
            let (s0, e0) = if back { (f, a) } else { (a, f) };
            let range = w.doc_slot(0, |d| {
                let host_in = |bp: Bp, tree: NodeId| {
                    let mut n = bp.0;
                    loop {
                        let h = d.shadow_host(d.root(n))?;
                        if d.root(h) == tree {
                            return Some(h);
                        }
                        n = h;
                    }
                };
                let (rs, re) = (d.root(s0.0), d.root(e0.0));
                if let Some(h) = host_in(e0, rs) {
                    Some((s0, (h, 0)))
                } else {
                    host_in(s0, re).map(|h| ((h, node_len(d, h)), e0))
                }
            });
            let t = range.map_or_else(String::new, |(start, end)| {
                rendered_text(w, &RangeObj { start, end, ..r })
            });
            return cx.ret(t);
        }
        if let Some(n) = control_of(w) {
            let c = crate::rt::input::selection(w, 0, n);
            let v: Vec<u16> = crate::rt::input::control_value(w, 0, n)
                .encode_utf16()
                .collect();
            let t = String::from_utf16_lossy(&v[c.start.min(v.len())..c.end.min(v.len())]);
            return cx.ret(t);
        }
        let s = sel_range(w).map_or_else(String::new, |(_, r)| rendered_text(w, &r));
        cx.ret(s)
    });
    method(reg, rt, IFACE, "modify", |cx, w| {
        this_sel(cx, w)?;
        let alter = opt_string(cx, 0)?.unwrap_or_default().to_ascii_lowercase();
        let dir = opt_string(cx, 1)?.unwrap_or_default().to_ascii_lowercase();
        let gran = opt_string(cx, 2)?.unwrap_or_default();
        let alter = match alter.as_str() {
            "move" => caret::Alter::Move,
            "extend" => caret::Alter::Extend,
            _ => return Ok(()),
        };
        let dir = match dir.as_str() {
            "forward" => caret::Direction::Forward,
            "backward" => caret::Direction::Backward,
            "left" => caret::Direction::Left,
            "right" => caret::Direction::Right,
            _ => return Ok(()),
        };
        let Some(gran) = caret::Granularity::parse(&gran) else {
            return Ok(());
        };
        let Some((_, r)) = sel_range(w) else {
            return Ok(());
        };
        if r.slot != 0 {
            return Ok(());
        }
        let control = control_of(w);
        let (anchor, focus, directional) = match control {
            Some(n) => {
                let c = crate::rt::input::selection(w, 0, n);
                if crate::rt::input::control_value(w, 0, n).is_empty() {
                    return Ok(());
                }
                let (a, f) = if c.dir == 2 {
                    (c.end, c.start)
                } else {
                    (c.start, c.end)
                };
                ((n, a as u32), (n, f as u32), c.dir != 0 || !mac(w))
            }
            None => {
                let Some((anchor, focus)) = ends(w) else {
                    return Ok(());
                };
                (anchor, focus, directional_now(w, &r))
            }
        };
        let xpos = {
            let st = w.ranges.borrow();
            st.sel
                .xpos
                .filter(|(_, at)| *at == (r.start, r.end))
                .map(|(x, _)| x)
        };
        let platform = match w.profile.as_deref().map(|p| p.os_kind()) {
            Some(lb_persona::OsKind::MacOs) => caret::Platform::Mac,
            Some(lb_persona::OsKind::Windows) => caret::Platform::Windows,
            _ => caret::Platform::Unix,
        };
        let locale = w
            .profile
            .as_deref()
            .map_or_else(|| "en-US".to_string(), |p| p.locale.language.clone());
        if let Some(n) = control {
            let value = crate::rt::input::control_value(w, 0, n);
            let out = w.with_layout(0, |l, d| {
                let mut m = caret::Model::for_value(d, l, &locale, n, &value);
                caret::modify(
                    &mut m,
                    anchor,
                    focus,
                    directional,
                    alter,
                    dir,
                    gran,
                    None,
                    platform,
                )
            });
            if let Some(out) = out {
                let (a, f) = (out.anchor.1 as usize, out.focus.1 as usize);
                let d = if a == f {
                    u8::from(platform != caret::Platform::Mac)
                } else if alter == caret::Alter::Move && platform == caret::Platform::Mac {
                    0
                } else if f < a {
                    2
                } else {
                    1
                };
                crate::rt::input::set_selection(w, 0, n, a.min(f), a.max(f), d);
            }
            return Ok(());
        }
        let cache = w.ranges.borrow_mut().sel.caret.take();
        let (out, cache) = w.with_layout(0, |l, d| {
            let mut m = caret::Model::new(d, l, &locale, cache);
            let out = caret::modify(
                &mut m,
                anchor,
                focus,
                directional,
                alter,
                dir,
                gran,
                xpos,
                platform,
            );
            (out, m.into_cache())
        });
        w.ranges.borrow_mut().sel.caret = Some(cache);
        let Some(out) = out else {
            return Ok(());
        };
        // Blink: extending makes the selection directional; moving does
        // too except on macOS (a caret has no direction either way).
        let now_directional = alter == caret::Alter::Extend || platform != caret::Platform::Mac;
        let same_dir = out.anchor == out.focus || now_directional == directional;
        if (out.anchor, out.focus) != (anchor, focus) || !same_dir {
            select(w, out.anchor, out.focus, now_directional);
            w.ranges.borrow_mut().sel.cached = false;
        }
        if let Some(x) = out.xpos {
            let mut st = w.ranges.borrow_mut();
            let snap = st.sel.snap;
            st.sel.xpos = snap.map(|at| (x, at));
        }
        Ok(())
    });
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    register_selection(reg, rt);
}

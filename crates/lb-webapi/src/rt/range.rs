//! DOM ranges: `AbstractRange`, `Range` (live) and `StaticRange`, with
//! `Document.createRange()`. Boundary points, comparisons and the content
//! algorithms (delete, extract, clone, insert, surround) follow the DOM
//! spec; `createContextualFragment` the DOM Parsing spec;
//! `getClientRects`/`getBoundingClientRect` Blink's
//! `Range::GetBorderAndTextQuads`. Error texts are Chrome's.
//!
//! Live ranges are updated from the window's mutation records ([`sync`],
//! run before records are drained and at the start of every range call).
//! Records carry no offsets for character data changes or text splits, so
//! the DOM methods that need them leave a [`Note`] first ([`note_replace`],
//! [`note_split`], [`note_merge`]); a record without a note is treated as
//! a whole-data replacement (`data`, `textContent`, `nodeValue`). Records
//! are applied against the tree as it is after the DOM call, so a call
//! that makes several changes (`innerHTML`) moves boundary points
//! approximately. Ranges on another window's document are not updated
//! (their records go to that window); offsets are clamped when read.

use std::cmp::Ordering;
use std::rc::Rc;

use lb_dom::{Document, LocalName, Mutation, NodeFlags, NodeId, NodeType, QualName, ns};
use lb_js::{CallCtx, HostError, HostRef, ValueKind};

use super::dom::utf16_len;
use super::{
    Dict, K_OBJ, Obj, Runtime, Window, arg_node, ctor, getter, illegal, method, need_args,
    this_node, this_obj,
};
use crate::registry::Registry;

#[path = "selection.rs"]
mod selection;
pub(crate) use selection::{control_changed, control_focused, in_control};

type R = Result<(), HostError>;
type Bp = (NodeId, u32);

/// A range's document slot and boundary points.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RangeObj {
    pub(crate) slot: u32,
    pub(crate) start: Bp,
    pub(crate) end: Bp,
    pub(crate) kind: Kind,
}

/// What a [`RangeObj`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `Range` (live).
    Live,
    StaticRange,
    /// The document's `Selection` (its state is in [`SelState`]).
    Selection,
}

impl RangeObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self.kind {
            Kind::Live => "Range",
            Kind::StaticRange => "StaticRange",
            Kind::Selection => "Selection",
        }
    }
}

/// What a DOM method did that its mutation record does not say.
#[derive(Clone, Copy, Debug)]
enum Note {
    /// "Replace data" of `node`: `count` units at `off` became `len` units.
    Replace {
        slot: u32,
        node: NodeId,
        off: u32,
        count: u32,
        len: u32,
    },
    /// `node.splitText(off)` made `new`.
    Split {
        slot: u32,
        node: NodeId,
        new: NodeId,
        off: u32,
    },
    /// `normalize()` merged `from` into `into`, whose data was `off` long.
    Merge {
        slot: u32,
        from: NodeId,
        into: NodeId,
        off: u32,
    },
}

/// Per-window live ranges and pending notes.
#[derive(Default)]
pub(crate) struct RangeState {
    live: Vec<u32>,
    notes: Vec<Note>,
    /// Records at the front of `Window::mutations` already applied.
    seen: usize,
    /// The window's `Selection`.
    sel: selection::SelState,
}

fn has_live(w: &Window) -> bool {
    !w.ranges.borrow().live.is_empty()
}

/// Before `misc::process_mutations` drains the records: apply them.
pub(crate) fn before_drain(w: &Window) {
    sync(w);
    w.ranges.borrow_mut().seen = 0;
}

/// Note a "replace data" (`insertData`, `deleteData`, `replaceData`,
/// `appendData`) before it changes `node`.
pub(crate) fn note_replace(w: &Window, slot: u32, node: NodeId, off: u32, count: u32, len: u32) {
    if has_live(w) {
        w.ranges.borrow_mut().notes.push(Note::Replace {
            slot,
            node,
            off,
            count,
            len,
        });
    }
}

/// Note `node.splitText(off)` making `new` (the data change needs its own
/// [`note_replace`]).
pub(crate) fn note_split(w: &Window, slot: u32, node: NodeId, new: NodeId, off: u32) {
    if has_live(w) {
        w.ranges.borrow_mut().notes.push(Note::Split {
            slot,
            node,
            new,
            off,
        });
    }
}

/// Note `normalize()` merging Text `from` into `into` (data `off` long
/// before `from`'s data was appended).
pub(crate) fn note_merge(w: &Window, slot: u32, from: NodeId, into: NodeId, off: u32) {
    if has_live(w) {
        w.ranges.borrow_mut().notes.push(Note::Merge {
            slot,
            from,
            into,
            off,
        });
    }
}

/// Apply the mutation records not yet seen to the live ranges.
pub(crate) fn sync(w: &Window) {
    let (ids, notes, seen) = {
        let mut st = w.ranges.borrow_mut();
        let n = w.mutations.borrow().len();
        if st.seen > n {
            st.seen = 0;
        }
        let seen = st.seen;
        st.seen = n;
        if st.live.is_empty() || seen == n {
            st.notes.clear();
            return;
        }
        (st.live.clone(), std::mem::take(&mut st.notes), seen)
    };
    let pending: Vec<(u32, Mutation)> = w.mutations.borrow()[seen..].to_vec();
    let mut rs: Vec<(u32, RangeObj)> = ids
        .iter()
        .filter_map(|&id| {
            w.with_obj(id, |o| match o {
                Obj::Range(r) => Some((id, **r)),
                _ => None,
            })
            .flatten()
        })
        .collect();
    let mut notes = notes;
    for (slot, m) in &pending {
        let slot = *slot;
        if !rs.iter().any(|(_, r)| r.slot == slot) {
            continue;
        }
        w.doc_slot(slot, |d| {
            apply(d, slot, m, &mut notes, &mut rs);
        });
    }
    for (id, r) in rs {
        w.with_obj(id, |o| {
            if let Obj::Range(b) = o {
                **b = r;
            }
        });
    }
}

fn take_note(notes: &mut Vec<Note>, f: impl Fn(&Note) -> bool) -> Option<Note> {
    let i = notes.iter().position(f)?;
    Some(notes.remove(i))
}

/// Both boundary points of the ranges of `slot`.
fn each_bp(slot: u32, rs: &mut [(u32, RangeObj)], mut f: impl FnMut(&mut Bp)) {
    for (_, r) in rs.iter_mut().filter(|(_, r)| r.slot == slot) {
        f(&mut r.start);
        f(&mut r.end);
    }
}

fn apply(d: &Document, slot: u32, m: &Mutation, notes: &mut Vec<Note>, rs: &mut [(u32, RangeObj)]) {
    match *m {
        Mutation::ChildInserted { parent, node, .. } => {
            if d.parent(node) != Some(parent) {
                return;
            }
            let idx = index(d, node);
            each_bp(slot, rs, |bp| {
                if bp.0 == parent && bp.1 > idx {
                    bp.1 += 1;
                }
            });
            let split = take_note(
                notes,
                |n| matches!(*n, Note::Split { slot: s, new, .. } if s == slot && new == node),
            );
            if let Some(Note::Split { node: t, off, .. }) = split {
                each_bp(slot, rs, |bp| {
                    if bp.0 == t && bp.1 > off {
                        *bp = (node, bp.1 - off);
                    } else if bp.0 == parent && bp.1 == idx {
                        bp.1 += 1;
                    }
                });
            }
        }
        Mutation::ChildRemoved {
            parent,
            node,
            prev_sibling,
            next_sibling,
        } => {
            let idx = match (prev_sibling, next_sibling) {
                (Some(p), _) if d.parent(p) == Some(parent) => index(d, p) + 1,
                (_, Some(n)) if d.parent(n) == Some(parent) => index(d, n),
                (None, _) => 0,
                _ => d.child_count(parent) as u32,
            };
            let merge = take_note(
                notes,
                |n| matches!(*n, Note::Merge { slot: s, from, .. } if s == slot && from == node),
            );
            if let Some(Note::Merge { into, off, .. }) = merge {
                each_bp(slot, rs, |bp| {
                    if bp.0 == node {
                        *bp = (into, off + bp.1);
                    } else if bp.0 == parent && bp.1 == idx {
                        *bp = (into, off);
                    }
                });
            }
            each_bp(slot, rs, |bp| {
                if inclusive_ancestor(d, node, bp.0) {
                    *bp = (parent, idx);
                } else if bp.0 == parent && bp.1 > idx {
                    bp.1 -= 1;
                }
            });
        }
        Mutation::CharacterData {
            node,
            ref old_value,
        } => {
            let note = take_note(
                notes,
                |n| matches!(*n, Note::Replace { slot: s, node: x, .. } if s == slot && x == node),
            );
            let (off, count, len) = match note {
                Some(Note::Replace {
                    off, count, len, ..
                }) => (off, count, len),
                _ => (
                    0,
                    utf16_len(old_value) as u32,
                    d.character_data(node).map_or(0, utf16_len) as u32,
                ),
            };
            each_bp(slot, rs, |bp| {
                if bp.0 != node {
                    return;
                }
                if bp.1 > off && bp.1 <= off + count {
                    bp.1 = off;
                } else if bp.1 > off + count {
                    bp.1 = bp.1 + len - count;
                }
            });
        }
        Mutation::Attribute { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Tree helpers
// ---------------------------------------------------------------------------

fn index(d: &Document, n: NodeId) -> u32 {
    std::iter::successors(d.prev_sibling(n), |&p| d.prev_sibling(p)).count() as u32
}

fn is_chardata(d: &Document, n: NodeId) -> bool {
    matches!(
        d.node_type(n),
        NodeType::Text | NodeType::Comment | NodeType::ProcessingInstruction
    )
}

/// The DOM "length" of a node.
fn node_len(d: &Document, n: NodeId) -> u32 {
    match d.node_type(n) {
        NodeType::DocumentType => 0,
        _ if is_chardata(d, n) => d.character_data(n).map_or(0, utf16_len) as u32,
        _ => d.child_count(n) as u32,
    }
}

/// `a` is an inclusive ancestor of `b`.
fn inclusive_ancestor(d: &Document, a: NodeId, b: NodeId) -> bool {
    a == b || d.ancestors(b).any(|x| x == a)
}

/// Root ..= n.
fn chain(d: &Document, n: NodeId) -> Vec<NodeId> {
    let mut v: Vec<NodeId> = std::iter::once(n).chain(d.ancestors(n)).collect();
    v.reverse();
    v
}

/// Compare boundary points of the same tree (DOM "position").
fn bp_cmp(d: &Document, a: Bp, b: Bp) -> Ordering {
    if a.0 == b.0 {
        return a.1.cmp(&b.1);
    }
    let (ca, cb) = (chain(d, a.0), chain(d, b.0));
    let i = ca.iter().zip(&cb).take_while(|(x, y)| x == y).count();
    if i == ca.len() {
        // a.0 is an ancestor of b.0.
        return if index(d, cb[i]) < a.1 {
            Ordering::Greater
        } else {
            Ordering::Less
        };
    }
    if i == cb.len() {
        return if index(d, ca[i]) < b.1 {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    index(d, ca[i]).cmp(&index(d, cb[i]))
}

fn child_at(d: &Document, n: NodeId, off: u32) -> Option<NodeId> {
    d.children(n).nth(off as usize)
}

/// Next node in tree order, skipping `n`'s children.
fn next_skipping(d: &Document, mut n: NodeId) -> Option<NodeId> {
    loop {
        if let Some(s) = d.next_sibling(n) {
            return Some(s);
        }
        n = d.parent(n)?;
    }
}

fn next_node(d: &Document, n: NodeId) -> Option<NodeId> {
    d.first_child(n).or_else(|| next_skipping(d, n))
}

/// `node` is contained in the range.
fn contained(d: &Document, r: &RangeObj, n: NodeId) -> bool {
    d.root(n) == d.root(r.start.0)
        && bp_cmp(d, (n, 0), r.start) == Ordering::Greater
        && bp_cmp(d, (n, node_len(d, n)), r.end) == Ordering::Less
}

fn common_ancestor(d: &Document, r: &RangeObj) -> NodeId {
    let mut c = r.start.0;
    while !inclusive_ancestor(d, c, r.end.0) {
        match d.parent(c) {
            Some(p) => c = p,
            None => break,
        }
    }
    c
}

/// Keep a range valid if records were missed: offsets within lengths and
/// both points in one tree.
fn fixed(d: &Document, mut r: RangeObj) -> RangeObj {
    if (r.start.0.0 as usize) >= d.node_count() || (r.end.0.0 as usize) >= d.node_count() {
        r.start = (Document::DOCUMENT, 0);
        r.end = r.start;
        return r;
    }
    r.start.1 = r.start.1.min(node_len(d, r.start.0));
    r.end.1 = r.end.1.min(node_len(d, r.end.0));
    if d.root(r.start.0) != d.root(r.end.0) || bp_cmp(d, r.start, r.end) == Ordering::Greater {
        r.end = r.start;
    }
    r
}

// ---------------------------------------------------------------------------
// Binding helpers
// ---------------------------------------------------------------------------

fn new_range(w: &Window, slot: u32, at: Bp) -> u32 {
    let id = w.new_obj(Obj::Range(Box::new(RangeObj {
        slot,
        start: at,
        end: at,
        kind: Kind::Live,
    })));
    w.ranges.borrow_mut().live.push(id);
    id
}

/// `this` as a range (`live`: a `Range`, else any `AbstractRange`), with
/// pending records applied.
fn this_range(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    live: bool,
) -> Result<(u32, RangeObj), HostError> {
    let id = this_obj(cx)?;
    let r = w
        .with_obj(id, |o| match o {
            Obj::Range(r) if r.kind == Kind::Live || (!live && r.kind == Kind::StaticRange) => {
                Some(**r)
            }
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)?;
    if r.kind != Kind::Live {
        return Ok((id, r));
    }
    sync(w);
    let r = w
        .with_obj(id, |o| match o {
            Obj::Range(r) => Some(**r),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)?;
    Ok((id, w.doc_slot(r.slot, |d| fixed(d, r))))
}

fn put(w: &Window, id: u32, r: RangeObj) {
    let old = w
        .with_obj(id, |o| match o {
            Obj::Range(b) => {
                let old = **b;
                **b = r;
                Some(old)
            }
            _ => None,
        })
        .flatten();
    if let Some(old) = old {
        selection::range_changed(w, id, &old, &r);
    }
}

fn op_err(w: &Window, name: &str, op: &str, msg: impl std::fmt::Display) -> HostError {
    op_err_on(w, name, "Range", op, msg)
}

fn op_err_on(
    w: &Window,
    name: &str,
    iface: &str,
    op: &str,
    msg: impl std::fmt::Display,
) -> HostError {
    w.rt.err(
        name,
        format!("Failed to execute '{op}' on '{iface}': {msg}"),
    )
}

fn type_err(w: &Window, d: &Document, iface: &str, op: &str, n: NodeId) -> HostError {
    op_err_on(
        w,
        "InvalidNodeTypeError",
        iface,
        op,
        format!("The node provided is of type '{}'.", d.node_name(n)),
    )
}

fn no_parent(w: &Window, op: &str) -> HostError {
    op_err(
        w,
        "InvalidNodeTypeError",
        op,
        "the given Node has no parent.",
    )
}

/// Blink's checks of a boundary point (`Range::CheckNodeWOffset`).
fn check_bp(w: &Window, d: &Document, op: &str, n: NodeId, off: u32) -> R {
    check_bp_on(w, d, "Range", op, n, off)
}

fn check_bp_on(w: &Window, d: &Document, iface: &str, op: &str, n: NodeId, off: u32) -> R {
    if d.node_type(n) == NodeType::DocumentType {
        return Err(type_err(w, d, iface, op, n));
    }
    if is_chardata(d, n) {
        let len = node_len(d, n);
        if off > len {
            return Err(op_err_on(
                w,
                "IndexSizeError",
                iface,
                op,
                format!("The offset {off} is larger than the node's length ({len})."),
            ));
        }
    } else if off > i32::MAX as u32 {
        return Err(op_err_on(
            w,
            "IndexSizeError",
            iface,
            op,
            format!("The offset {off} is invalid."),
        ));
    } else if off > node_len(d, n) {
        return Err(op_err_on(
            w,
            "IndexSizeError",
            iface,
            op,
            format!("There is no child at offset {off}."),
        ));
    }
    Ok(())
}

/// Node argument `i` and offset argument `i + 1`.
fn node_off(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    op: &str,
) -> Result<(u32, NodeId, u32), HostError> {
    need_args(cx, 2, op, "Range")?;
    let (s, n) = arg_node(cx, w, 0, op, "Range")?;
    let off = cx.arg_u32(1)?;
    Ok((s, n, off))
}

/// Set one boundary point (DOM "set the start or end"), after checks.
fn set_bp(w: &Window, mut r: RangeObj, s: u32, bp: Bp, start: bool) -> RangeObj {
    if s != r.slot {
        // A node of another document: the range moves there.
        r.slot = s;
        r.start = bp;
        r.end = bp;
        return r;
    }
    w.doc_slot(s, |d| {
        let other_root = d.root(bp.0) != d.root(r.start.0);
        if start {
            if other_root || bp_cmp(d, bp, r.end) == Ordering::Greater {
                r.end = bp;
            }
            r.start = bp;
        } else {
            if other_root || bp_cmp(d, bp, r.start) == Ordering::Less {
                r.start = bp;
            }
            r.end = bp;
        }
    });
    r
}

fn rect_arr(r: lb_layout::Rect) -> [f64; 4] {
    [
        f64::from(r.x),
        f64::from(r.y),
        f64::from(r.width),
        f64::from(r.height),
    ]
}

/// Blink's `Range::GetBorderAndTextQuads`: the border boxes of the
/// selected elements whose parent is not selected, and the text boxes of
/// the selected text.
fn client_rects(w: &Window, r: &RangeObj) -> Vec<[f64; 4]> {
    let nodes: Vec<NodeId> = w.doc_slot(r.slot, |d| {
        let first = if is_chardata(d, r.start.0) {
            Some(r.start.0)
        } else {
            child_at(d, r.start.0, r.start.1).or_else(|| next_skipping(d, r.start.0))
        };
        let past = if is_chardata(d, r.end.0) {
            next_skipping(d, r.end.0)
        } else {
            child_at(d, r.end.0, r.end.1).or_else(|| next_skipping(d, r.end.0))
        };
        let mut v = Vec::new();
        let mut cur = first;
        while let Some(n) = cur {
            if Some(n) == past {
                break;
            }
            v.push(n);
            cur = next_node(d, n);
        }
        v
    });
    let (sc, ec) = (r.start.0, r.end.0);
    let selected: Vec<NodeId> = w.doc_slot(r.slot, |d| {
        let mut sel: Vec<NodeId> = Vec::new();
        for &n in &nodes {
            if !d.is_element(n) {
                continue;
            }
            let parent_sel = d.parent(n).is_some_and(|p| sel.contains(&p));
            if parent_sel || (!inclusive_ancestor(d, n, sc) && !inclusive_ancestor(d, n, ec)) {
                sel.push(n);
            }
        }
        sel
    });
    let mut out = Vec::new();
    w.with_layout(r.slot, |l, d| {
        for &n in &nodes {
            if d.is_element(n) {
                let top =
                    selected.contains(&n) && !d.parent(n).is_some_and(|p| selected.contains(&p));
                if top {
                    out.extend(l.client_rects(d, n).into_iter().map(rect_arr));
                }
            } else if d.node_type(n) == NodeType::Text {
                let s = if n == sc { r.start.1 as usize } else { 0 };
                let e = if n == ec {
                    r.end.1 as usize
                } else {
                    usize::MAX
                };
                out.extend(l.text_rects(d, n, s, e).into_iter().map(rect_arr));
            }
        }
    });
    out
}

/// Blink's `Range::BoundingRect`: the union of the rects, empty ones too.
fn bounding(rects: &[[f64; 4]]) -> [f64; 4] {
    let Some(first) = rects.first() else {
        return [0.0; 4];
    };
    let (mut x0, mut y0) = (first[0], first[1]);
    let (mut x1, mut y1) = (first[0] + first[2], first[1] + first[3]);
    for r in &rects[1..] {
        x0 = x0.min(r[0]);
        y0 = y0.min(r[1]);
        x1 = x1.max(r[0] + r[2]);
        y1 = y1.max(r[1] + r[3]);
    }
    [x0, y0, x1 - x0, y1 - y0]
}

fn to_string(d: &Document, r: &RangeObj) -> String {
    let text = |n: NodeId| d.character_data(n).unwrap_or("");
    let sub = |n: NodeId, a: u32, b: u32| {
        let u: Vec<u16> = text(n).encode_utf16().collect();
        let (a, b) = (a as usize, (b as usize).min(u.len()));
        String::from_utf16_lossy(&u[a.min(b)..b])
    };
    if r.start.0 == r.end.0 && d.node_type(r.start.0) == NodeType::Text {
        return sub(r.start.0, r.start.1, r.end.1);
    }
    let mut out = String::new();
    if d.node_type(r.start.0) == NodeType::Text {
        out.push_str(&sub(r.start.0, r.start.1, u32::MAX));
    }
    let root = d.root(r.start.0);
    for n in d.descendants(root) {
        if d.node_type(n) == NodeType::Text && contained(d, r, n) {
            out.push_str(text(n));
        }
    }
    if d.node_type(r.end.0) == NodeType::Text {
        out.push_str(&sub(r.end.0, 0, r.end.1));
    }
    out
}

// ---------------------------------------------------------------------------
// Content algorithms
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Clone,
    Extract,
}

fn utf16_sub(s: &str, a: u32, b: u32) -> String {
    let u: Vec<u16> = s.encode_utf16().collect();
    let b = (b as usize).min(u.len());
    String::from_utf16_lossy(&u[(a as usize).min(b)..b])
}

/// "Replace data" of `n`: delete `count` units at `off`, with live range
/// updates.
fn delete_data(w: &Window, slot: u32, n: NodeId, off: u32, count: u32) {
    let new = w.doc_slot(slot, |d| {
        let s = d.character_data(n).unwrap_or("");
        let len = utf16_len(s) as u32;
        let end = (off + count).min(len);
        utf16_sub(s, 0, off) + &utf16_sub(s, end, len)
    });
    note_replace(w, slot, n, off, count, 0);
    w.doc_slot_mut(slot, |d| d.set_character_data(n, &new));
    sync(w);
}

/// A shallow copy of character data `n` holding units `a..b`.
fn clone_data(d: &mut Document, n: NodeId, a: u32, b: u32) -> NodeId {
    let s = utf16_sub(d.character_data(n).unwrap_or(""), a, b);
    let c = d.clone_node(n, false);
    d.set_character_data(c, &s);
    c
}

/// The new boundary point after removing a range's contents.
fn collapse_point(d: &Document, r: &RangeObj) -> Bp {
    let (osn, oen) = (r.start.0, r.end.0);
    if inclusive_ancestor(d, osn, oen) {
        return r.start;
    }
    let mut refn = osn;
    while let Some(p) = d.parent(refn) {
        if inclusive_ancestor(d, p, oen) {
            return (p, index(d, refn) + 1);
        }
        refn = p;
    }
    r.start
}

/// DOM "extract" / "clone the contents" of `r` into a new fragment.
fn contents(w: &Window, r: RangeObj, mode: Mode, op: &str) -> Result<NodeId, HostError> {
    let slot = r.slot;
    let frag = w.doc_slot_mut(slot, |d| d.create_document_fragment());
    if r.start == r.end {
        return Ok(frag);
    }
    let (osn, oso) = r.start;
    let (oen, oeo) = r.end;
    let same_data = w.doc_slot(slot, |d| osn == oen && is_chardata(d, osn));
    if same_data {
        w.doc_slot_mut(slot, |d| {
            let c = clone_data(d, osn, oso, oeo);
            let _ = d.append_child(frag, c);
        });
        if mode == Mode::Extract {
            delete_data(w, slot, osn, oso, oeo - oso);
        }
        return Ok(frag);
    }
    let (first, last, kids, doctype) = w.doc_slot(slot, |d| {
        let ca = common_ancestor(d, &r);
        let first = (!inclusive_ancestor(d, osn, oen))
            .then(|| d.children(ca).find(|&c| inclusive_ancestor(d, c, osn)))
            .flatten();
        let last = (!inclusive_ancestor(d, oen, osn))
            .then(|| d.children(ca).find(|&c| inclusive_ancestor(d, c, oen)))
            .flatten();
        let kids: Vec<NodeId> = d.children(ca).filter(|&c| contained(d, &r, c)).collect();
        let doctype = kids
            .iter()
            .copied()
            .find(|&c| d.node_type(c) == NodeType::DocumentType);
        (first, last, kids, doctype)
    });
    if let Some(dt) = doctype {
        return Err(match mode {
            Mode::Extract => op_err(
                w,
                "HierarchyRequestError",
                op,
                "The Range contains a doctype node.",
            ),
            Mode::Clone => w.doc_slot(slot, |d| {
                op_err(
                    w,
                    "HierarchyRequestError",
                    op,
                    format!(
                        "Nodes of type '{}' may not be inserted inside nodes of type '#document-fragment'.",
                        d.node_name(dt)
                    ),
                )
            }),
        });
    }
    if let Some(f) = first {
        if w.doc_slot(slot, |d| is_chardata(d, f)) {
            let len = w.doc_slot(slot, |d| node_len(d, osn));
            w.doc_slot_mut(slot, |d| {
                let c = clone_data(d, osn, oso, len);
                let _ = d.append_child(frag, c);
            });
            if mode == Mode::Extract {
                delete_data(w, slot, osn, oso, len - oso);
            }
        } else {
            let c = w.doc_slot_mut(slot, |d| {
                let c = d.clone_node(f, false);
                let _ = d.append_child(frag, c);
                c
            });
            let len = w.doc_slot(slot, |d| node_len(d, f));
            let sub = RangeObj {
                slot,
                start: (osn, oso),
                end: (f, len),
                kind: Kind::Live,
            };
            let sf = contents(w, sub, mode, op)?;
            w.doc_slot_mut(slot, |d| d.append_child(c, sf).map(|_| ()))
                .map_err(|e| super::dom::dom_err(w, op, "Range", e))?;
        }
    }
    for k in kids {
        w.doc_slot_mut(slot, |d| {
            let n = match mode {
                Mode::Clone => d.clone_node(k, true),
                Mode::Extract => k,
            };
            let _ = d.append_child(frag, n);
        });
        sync(w);
    }
    if let Some(l) = last {
        if w.doc_slot(slot, |d| is_chardata(d, l)) {
            w.doc_slot_mut(slot, |d| {
                let c = clone_data(d, oen, 0, oeo);
                let _ = d.append_child(frag, c);
            });
            if mode == Mode::Extract {
                delete_data(w, slot, oen, 0, oeo);
            }
        } else {
            let c = w.doc_slot_mut(slot, |d| {
                let c = d.clone_node(l, false);
                let _ = d.append_child(frag, c);
                c
            });
            let sub = RangeObj {
                slot,
                start: (l, 0),
                end: (oen, oeo),
                kind: Kind::Live,
            };
            let sf = contents(w, sub, mode, op)?;
            w.doc_slot_mut(slot, |d| d.append_child(c, sf).map(|_| ()))
                .map_err(|e| super::dom::dom_err(w, op, "Range", e))?;
        }
    }
    Ok(frag)
}

/// DOM "delete the contents".
fn delete_contents(w: &Window, r: RangeObj) -> Bp {
    let slot = r.slot;
    if r.start == r.end {
        return r.start;
    }
    let (osn, oso) = r.start;
    let (oen, oeo) = r.end;
    if w.doc_slot(slot, |d| osn == oen && is_chardata(d, osn)) {
        delete_data(w, slot, osn, oso, oeo - oso);
        return r.start;
    }
    let (remove, new_bp) = w.doc_slot(slot, |d| {
        let root = d.root(osn);
        let all: Vec<NodeId> = d
            .descendants(root)
            .filter(|&n| contained(d, &r, n))
            .collect();
        let remove: Vec<NodeId> = all
            .iter()
            .copied()
            .filter(|&n| !d.parent(n).is_some_and(|p| all.contains(&p)))
            .collect();
        (remove, collapse_point(d, &r))
    });
    if w.doc_slot(slot, |d| is_chardata(d, osn)) {
        let len = w.doc_slot(slot, |d| node_len(d, osn));
        delete_data(w, slot, osn, oso, len - oso);
    }
    for n in remove {
        w.doc_slot_mut(slot, |d| d.detach(n));
        sync(w);
    }
    if w.doc_slot(slot, |d| is_chardata(d, oen)) {
        delete_data(w, slot, oen, 0, oeo);
    }
    new_bp
}

/// Blink's texts for a node that may not go into `parent`.
fn hierarchy_msg(
    d: &Document,
    parent: NodeId,
    node: NodeId,
    child: Option<NodeId>,
) -> Option<String> {
    use NodeType as T;
    let pt = d.node_type(parent);
    let nt = d.node_type(node);
    let names = || {
        Some(format!(
            "Nodes of type '{}' may not be inserted inside nodes of type '{}'.",
            d.node_name(node),
            d.node_name(parent)
        ))
    };
    if !matches!(pt, T::Document | T::DocumentFragment | T::Element) {
        return names();
    }
    if d.shadow_including_contains(node, parent) {
        return Some("The new child element contains the parent.".into());
    }
    match nt {
        T::Document => return names(),
        T::DocumentType if pt != T::Document => return names(),
        T::Text if pt == T::Document => return names(),
        _ => {}
    }
    if pt != T::Document {
        return None;
    }
    let elements = |n: NodeId| {
        if d.node_type(n) == T::DocumentFragment {
            d.element_children(n).count()
        } else {
            usize::from(d.is_element(n))
        }
    };
    let new_elems = elements(node);
    if new_elems > 0 {
        let doctype_after = std::iter::successors(child, |&c| d.next_sibling(c))
            .any(|c| d.node_type(c) == T::DocumentType);
        if doctype_after {
            return Some("Can't insert an element before a doctype.".into());
        }
        if new_elems > 1 || d.element_children(parent).next().is_some() {
            return Some("Only one element on document allowed.".into());
        }
    }
    if nt == T::DocumentType {
        if d.doctype_node().is_some() {
            return Some("Only one doctype on document allowed.".into());
        }
        let element_before = match child {
            Some(c) => std::iter::successors(d.prev_sibling(c), |&p| d.prev_sibling(p))
                .any(|p| d.is_element(p)),
            None => d.element_children(parent).next().is_some(),
        };
        if element_before {
            return Some("Can't insert a doctype after an element.".into());
        }
    }
    None
}

/// `Text.splitText(off)` with its live range updates; the new node.
fn split_text(w: &Window, slot: u32, n: NodeId, off: u32) -> NodeId {
    let (tail, head, parent, len) = w.doc_slot(slot, |d| {
        let s = d.character_data(n).unwrap_or("");
        let len = utf16_len(s) as u32;
        (
            utf16_sub(s, off, len),
            utf16_sub(s, 0, off),
            d.parent(n),
            len,
        )
    });
    let t = w.doc_slot_mut(slot, |d| d.create_text(&tail));
    note_split(w, slot, n, t, off);
    note_replace(w, slot, n, off, len - off, 0);
    w.doc_slot_mut(slot, |d| {
        if let Some(p) = parent {
            let next = d.next_sibling(n);
            let _ = d.insert_before(p, t, next);
        }
        d.set_character_data(n, &head);
    });
    sync(w);
    t
}

/// DOM "insert" `node` (of document `ns`) into range `id`.
fn insert_node(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    id: u32,
    ns: u32,
    node: NodeId,
    op: &str,
) -> R {
    let r = current(w, id)?;
    let slot = r.slot;
    let sn = r.start.0;
    let pre = w.doc_slot(slot, |d| {
        let st = d.node_type(sn);
        let name = w.doc_slot(ns, |x| x.node_name(node));
        if matches!(st, NodeType::Comment | NodeType::ProcessingInstruction) {
            return Some(format!(
                "Nodes of type '{name}' may not be inserted inside nodes of type '{}'.",
                d.node_name(sn)
            ));
        }
        if st == NodeType::Text && d.parent(sn).is_none() {
            return Some(
                "This operation would split a text node, but there's no parent into which to insert."
                    .into(),
            );
        }
        if ns == slot && sn == node {
            return Some("Unable to insert a node into a Range starting from the node itself.".into());
        }
        None
    });
    if let Some(m) = pre {
        return Err(op_err(w, "HierarchyRequestError", op, m));
    }
    let node = super::dom::bring(cx, w, ns, node, slot);
    sync(w);
    let (sn, so) = current(w, id)?.start;
    let is_text = w.doc_slot(slot, |d| d.node_type(sn) == NodeType::Text);
    let (mut reference, parent) = w.doc_slot(slot, |d| {
        let reference = if is_text {
            Some(sn)
        } else {
            child_at(d, sn, so)
        };
        let parent = reference.and_then(|x| d.parent(x)).unwrap_or(sn);
        (reference, parent)
    });
    if let Some(m) = w.doc_slot(slot, |d| hierarchy_msg(d, parent, node, reference)) {
        return Err(op_err(w, "HierarchyRequestError", op, m));
    }
    if is_text {
        reference = Some(split_text(w, slot, sn, so));
    }
    if reference == Some(node) {
        reference = w.doc_slot(slot, |d| d.next_sibling(node));
    }
    if w.doc_slot(slot, |d| d.parent(node).is_some()) {
        w.doc_slot_mut(slot, |d| d.detach(node));
        sync(w);
    }
    let new_off = w.doc_slot(slot, |d| {
        let base = reference.map_or_else(|| node_len(d, parent), |x| index(d, x));
        base + if d.node_type(node) == NodeType::DocumentFragment {
            d.child_count(node) as u32
        } else {
            1
        }
    });
    w.doc_slot_mut(slot, |d| d.insert_before(parent, node, reference))
        .map_err(|e| super::dom::dom_err(w, op, "Range", e))?;
    sync(w);
    let mut r = current(w, id)?;
    if r.start == r.end {
        r.end = (parent, new_off);
        put(w, id, r);
    }
    Ok(())
}

/// The range `id` now (pending records applied).
fn current(w: &Window, id: u32) -> Result<RangeObj, HostError> {
    sync(w);
    let r = w
        .with_obj(id, |o| match o {
            Obj::Range(r) => Some(**r),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)?;
    Ok(w.doc_slot(r.slot, |d| fixed(d, r)))
}

fn surround(cx: &mut CallCtx<'_, '_, '_>, w: &Window, id: u32, ns: u32, n: NodeId) -> R {
    const OP: &str = "surroundContents";
    let r = current(w, id)?;
    let partial = w.doc_slot(r.slot, |d| {
        let (s, e) = (r.start.0, r.end.0);
        let bad = |a: NodeId, b: NodeId| {
            std::iter::once(a)
                .chain(d.ancestors(a))
                .take_while(|&x| !inclusive_ancestor(d, x, b))
                .any(|x| d.node_type(x) != NodeType::Text)
        };
        bad(s, e) || bad(e, s)
    });
    if partial {
        return Err(op_err(
            w,
            "InvalidStateError",
            OP,
            "The Range has partially selected a non-Text node.",
        ));
    }
    let bad_type = w.doc_slot(ns, |d| {
        matches!(
            d.node_type(n),
            NodeType::Document | NodeType::DocumentType | NodeType::DocumentFragment
        )
        .then(|| type_err(w, d, "Range", OP, n))
    });
    if let Some(e) = bad_type {
        return Err(e);
    }
    let new_bp = w.doc_slot(r.slot, |d| collapse_point(d, &r));
    let frag = contents(w, r, Mode::Extract, OP)?;
    let mut r = current(w, id)?;
    r.start = new_bp;
    r.end = new_bp;
    put(w, id, r);
    let slot = r.slot;
    let n = super::dom::bring(cx, w, ns, n, slot);
    sync(w);
    if w.doc_slot(slot, |d| d.first_child(n).is_some()) {
        w.doc_slot_mut(slot, |d| d.remove_all_children(n));
        sync(w);
    }
    insert_node(cx, w, id, slot, n, OP)?;
    w.doc_slot_mut(slot, |d| d.append_child(n, frag).map(|_| ()))
        .map_err(|e| super::dom::dom_err(w, OP, "Range", e))?;
    sync(w);
    let mut r = current(w, id)?;
    let (p, i) = w
        .doc_slot(slot, |d| d.parent(n).map(|p| (p, index(d, n))))
        .ok_or_else(|| no_parent(w, OP))?;
    r.start = (p, i);
    r.end = (p, i + 1);
    put(w, id, r);
    Ok(())
}

fn register_basic(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "Document", "createRange", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        if w.doc_slot(slot, |d| d.node_type(n)) != NodeType::Document {
            return Err(illegal());
        }
        let id = new_range(w, slot, (n, 0));
        cx.ret(w.obj_arg(id))
    });
    ctor(reg, rt, "Range", |cx, w| {
        let id = new_range(w, 0, (Document::DOCUMENT, 0));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    ctor(reg, rt, "StaticRange", static_ctor);
    for (prop, start) in [("startContainer", true), ("endContainer", false)] {
        getter(reg, rt, "AbstractRange", prop, move |cx, w| {
            let (_, r) = this_range(cx, w, false)?;
            let bp = if start { r.start } else { r.end };
            cx.ret(w.node_arg(r.slot, bp.0))
        });
    }
    for (prop, start) in [("startOffset", true), ("endOffset", false)] {
        getter(reg, rt, "AbstractRange", prop, move |cx, w| {
            let (_, r) = this_range(cx, w, false)?;
            cx.ret(if start { r.start.1 } else { r.end.1 })
        });
    }
    getter(reg, rt, "AbstractRange", "collapsed", |cx, w| {
        let (_, r) = this_range(cx, w, false)?;
        cx.ret(r.start == r.end)
    });
    getter(reg, rt, "Range", "commonAncestorContainer", |cx, w| {
        let (_, r) = this_range(cx, w, true)?;
        let c = w.doc_slot(r.slot, |d| common_ancestor(d, &r));
        cx.ret(w.node_arg(r.slot, c))
    });
    for (op, start) in [("setStart", true), ("setEnd", false)] {
        method(reg, rt, "Range", op, move |cx, w| {
            let (id, r) = this_range(cx, w, true)?;
            let (s, n, off) = node_off(cx, w, op)?;
            w.doc_slot(s, |d| check_bp(w, d, op, n, off))?;
            put(w, id, set_bp(w, r, s, (n, off), start));
            Ok(())
        });
    }
    for (op, start, after) in [
        ("setStartBefore", true, false),
        ("setStartAfter", true, true),
        ("setEndBefore", false, false),
        ("setEndAfter", false, true),
    ] {
        method(reg, rt, "Range", op, move |cx, w| {
            let (id, r) = this_range(cx, w, true)?;
            need_args(cx, 1, op, "Range")?;
            let (s, n) = arg_node(cx, w, 0, op, "Range")?;
            let bp = w
                .doc_slot(s, |d| {
                    d.parent(n).map(|p| (p, index(d, n) + u32::from(after)))
                })
                .ok_or_else(|| no_parent(w, op))?;
            put(w, id, set_bp(w, r, s, bp, start));
            Ok(())
        });
    }
    method(reg, rt, "Range", "collapse", |cx, w| {
        let (id, mut r) = this_range(cx, w, true)?;
        if cx.arg_bool(0) {
            r.end = r.start;
        } else {
            r.start = r.end;
        }
        put(w, id, r);
        Ok(())
    });
    method(reg, rt, "Range", "selectNode", |cx, w| {
        let (id, mut r) = this_range(cx, w, true)?;
        need_args(cx, 1, "selectNode", "Range")?;
        let (s, n) = arg_node(cx, w, 0, "selectNode", "Range")?;
        let (p, i) = w
            .doc_slot(s, |d| d.parent(n).map(|p| (p, index(d, n))))
            .ok_or_else(|| no_parent(w, "selectNode"))?;
        r.slot = s;
        r.start = (p, i);
        r.end = (p, i + 1);
        put(w, id, r);
        Ok(())
    });
    method(reg, rt, "Range", "selectNodeContents", |cx, w| {
        let (id, mut r) = this_range(cx, w, true)?;
        need_args(cx, 1, "selectNodeContents", "Range")?;
        let (s, n) = arg_node(cx, w, 0, "selectNodeContents", "Range")?;
        let len = w.doc_slot(s, |d| {
            if d.node_type(n) == NodeType::DocumentType {
                Err(type_err(w, d, "Range", "selectNodeContents", n))
            } else {
                Ok(node_len(d, n))
            }
        })?;
        r.slot = s;
        r.start = (n, 0);
        r.end = (n, len);
        put(w, id, r);
        Ok(())
    });
    method(reg, rt, "Range", "cloneRange", |cx, w| {
        let (_, r) = this_range(cx, w, true)?;
        let id = new_range(w, r.slot, r.start);
        put(w, id, r);
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "Range", "detach", |cx, w| {
        this_range(cx, w, true)?;
        Ok(())
    });
    method(reg, rt, "Range", "toString", |cx, w| {
        let (_, r) = this_range(cx, w, true)?;
        cx.ret(w.doc_slot(r.slot, |d| to_string(d, &r)))
    });
}

fn register_compare(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "Range", "compareBoundaryPoints", |cx, w| {
        let (_, r) = this_range(cx, w, true)?;
        need_args(cx, 2, "compareBoundaryPoints", "Range")?;
        let how = cx.arg_u32(0)? & 0xffff;
        let other = cx
            .arg_host(1)
            .filter(|h| h.kind == K_OBJ)
            .and_then(|h| {
                w.with_obj(h.id, |o| match o {
                    Obj::Range(x) if x.kind == Kind::Live => Some(h.id),
                    _ => None,
                })
                .flatten()
            })
            .ok_or_else(|| {
                HostError::type_error(
                    "Failed to execute 'compareBoundaryPoints' on 'Range': parameter 2 is not of type 'Range'.",
                )
            })?;
        if how > 3 {
            return Err(op_err(
                w,
                "NotSupportedError",
                "compareBoundaryPoints",
                "The comparison method provided must be one of 'START_TO_START', 'START_TO_END', 'END_TO_END', or 'END_TO_START'.",
            ));
        }
        let o = w
            .with_obj(other, |o| match o {
                Obj::Range(x) => Some(**x),
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal)?;
        let o = w.doc_slot(o.slot, |d| fixed(d, o));
        let same =
            o.slot == r.slot && w.doc_slot(r.slot, |d| d.root(o.start.0) == d.root(r.start.0));
        if !same {
            return Err(op_err(
                w,
                "WrongDocumentError",
                "compareBoundaryPoints",
                "The source range is in a different document than this range.",
            ));
        }
        let (a, b) = match how {
            0 => (r.start, o.start),
            1 => (r.end, o.start),
            2 => (r.end, o.end),
            _ => (r.start, o.end),
        };
        let c = w.doc_slot(r.slot, |d| bp_cmp(d, a, b));
        cx.ret(c as i32)
    });
    method(reg, rt, "Range", "comparePoint", |cx, w| {
        let (_, r) = this_range(cx, w, true)?;
        let (s, n, off) = node_off(cx, w, "comparePoint")?;
        let c = w.doc_slot(s, |d| {
            if s != r.slot || d.root(n) != d.root(r.start.0) {
                return Err(op_err(
                    w,
                    "WrongDocumentError",
                    "comparePoint",
                    "The node provided and the Range are not in the same tree.",
                ));
            }
            check_bp(w, d, "comparePoint", n, off)?;
            Ok(if bp_cmp(d, (n, off), r.start) == Ordering::Less {
                -1
            } else if bp_cmp(d, (n, off), r.end) == Ordering::Greater {
                1
            } else {
                0
            })
        })?;
        cx.ret(c)
    });
    method(reg, rt, "Range", "isPointInRange", |cx, w| {
        let (_, r) = this_range(cx, w, true)?;
        let (s, n, off) = node_off(cx, w, "isPointInRange")?;
        let v = w.doc_slot(s, |d| {
            if s != r.slot || d.root(n) != d.root(r.start.0) {
                return Ok(false);
            }
            check_bp(w, d, "isPointInRange", n, off)?;
            Ok(bp_cmp(d, (n, off), r.start) != Ordering::Less
                && bp_cmp(d, (n, off), r.end) != Ordering::Greater)
        })?;
        cx.ret(v)
    });
    method(reg, rt, "Range", "intersectsNode", |cx, w| {
        let (_, r) = this_range(cx, w, true)?;
        need_args(cx, 1, "intersectsNode", "Range")?;
        let (s, n) = arg_node(cx, w, 0, "intersectsNode", "Range")?;
        let v = w.doc_slot(s, |d| {
            if s != r.slot || d.root(n) != d.root(r.start.0) {
                return false;
            }
            let Some(p) = d.parent(n) else { return true };
            let i = index(d, n);
            bp_cmp(d, (p, i), r.end) == Ordering::Less
                && bp_cmp(d, (p, i + 1), r.start) == Ordering::Greater
        });
        cx.ret(v)
    });
}

fn register_geometry(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "Range", "getClientRects", |cx, w| {
        let (_, r) = this_range(cx, w, true)?;
        let rects = client_rects(w, &r);
        let id = w.new_obj(Obj::RectList(rects));
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "Range", "getBoundingClientRect", |cx, w| {
        let (_, r) = this_range(cx, w, true)?;
        let b = bounding(&client_rects(w, &r));
        let id = w.new_obj(Obj::Rect(b));
        cx.ret(w.obj_arg(id))
    });
}

fn register_contents(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "Range", "deleteContents", |cx, w| {
        let (id, r) = this_range(cx, w, true)?;
        let bp = delete_contents(w, r);
        let mut r = r;
        r.start = bp;
        r.end = bp;
        put(w, id, r);
        w.after_mutation(cx);
        Ok(())
    });
    for (op, mode) in [
        ("extractContents", Mode::Extract),
        ("cloneContents", Mode::Clone),
    ] {
        method(reg, rt, "Range", op, move |cx, w| {
            let (id, r) = this_range(cx, w, true)?;
            let new_bp = w.doc_slot(r.slot, |d| collapse_point(d, &r));
            let f = contents(w, r, mode, op);
            if mode == Mode::Extract && f.is_ok() {
                let mut r = r;
                r.start = new_bp;
                r.end = new_bp;
                put(w, id, r);
            }
            w.after_mutation(cx);
            cx.ret(w.node_arg(r.slot, f?))
        });
    }
    method(reg, rt, "Range", "insertNode", |cx, w| {
        let (id, _) = this_range(cx, w, true)?;
        need_args(cx, 1, "insertNode", "Range")?;
        let (s, n) = arg_node(cx, w, 0, "insertNode", "Range")?;
        let res = insert_node(cx, w, id, s, n, "insertNode");
        w.after_mutation(cx);
        res
    });
    method(reg, rt, "Range", "surroundContents", |cx, w| {
        let (id, _) = this_range(cx, w, true)?;
        need_args(cx, 1, "surroundContents", "Range")?;
        let (s, n) = arg_node(cx, w, 0, "surroundContents", "Range")?;
        let res = surround(cx, w, id, s, n);
        w.after_mutation(cx);
        res
    });
    method(reg, rt, "Range", "createContextualFragment", |cx, w| {
        let (_, r) = this_range(cx, w, true)?;
        need_args(cx, 1, "createContextualFragment", "Range")?;
        let html = cx.arg_string(0)?;
        let f = w.doc_slot_mut(r.slot, |d| {
            let n = r.start.0;
            let el = match d.node_type(n) {
                NodeType::Element => Some(n),
                NodeType::Text | NodeType::Comment | NodeType::ProcessingInstruction => {
                    d.parent_element(n)
                }
                _ => None,
            };
            let el = el.filter(|&e| {
                !(d.is_html() && d.is_html_element_named(e, &LocalName::from("html")))
            });
            let ctx = el.unwrap_or_else(|| {
                d.create_element(
                    QualName::new(None, ns!(html), LocalName::from("body")),
                    Vec::new(),
                )
            });
            let f = d.parse_fragment(ctx, &html);
            // Scripts of a contextual fragment run when inserted.
            let scripts: Vec<NodeId> = d
                .descendants(f)
                .filter(|&x| d.is_html_element_named(x, &LocalName::from("script")))
                .collect();
            for s in scripts {
                d.set_flags(
                    s,
                    NodeFlags::ALREADY_STARTED | NodeFlags::PARSER_INSERTED,
                    false,
                );
            }
            f
        });
        w.after_mutation(cx);
        cx.ret(w.node_arg(r.slot, f))
    });
}

/// `new StaticRange(init)`.
fn static_ctor(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> R {
    need_args(cx, 1, "StaticRange", "StaticRange").map_err(|_| {
        HostError::type_error(
            "Failed to construct 'StaticRange': 1 argument required, but only 0 present.",
        )
    })?;
    if !matches!(
        cx.arg_kind(0),
        ValueKind::Object | ValueKind::Function | ValueKind::Undefined
    ) {
        return Err(HostError::type_error(
            "Failed to construct 'StaticRange': The provided value is not of type 'StaticRangeInit'.",
        ));
    }
    let dict = Dict::arg(cx, 0);
    let member = |m: &str, what: &str| {
        HostError::type_error(format!(
            "Failed to construct 'StaticRange': Failed to read the '{m}' property from 'StaticRangeInit': {what}"
        ))
    };
    // Members in lexicographic order, as WebIDL reads them.
    let node = |cx: &mut CallCtx<'_, '_, '_>, m: &str| -> Result<(u32, NodeId), HostError> {
        let h = dict
            .get(cx, m)?
            .ok_or_else(|| member(m, "Required member is undefined."))?;
        let n = cx.handle_host(h).and_then(|x| w.decode_node(x));
        cx.release(h);
        n.ok_or_else(|| member(m, "Failed to convert value to 'Node'."))
    };
    let end = node(cx, "endContainer")?;
    let end_off = offset(cx, &dict, "endOffset", &member)?;
    let start = node(cx, "startContainer")?;
    let start_off = offset(cx, &dict, "startOffset", &member)?;
    let bad = w.doc_slot(start.0, |d| d.node_type(start.1) == NodeType::DocumentType)
        || w.doc_slot(end.0, |d| d.node_type(end.1) == NodeType::DocumentType);
    if bad {
        return Err(w.rt.err(
            "InvalidNodeTypeError",
            "Failed to construct 'StaticRange': Neither startContainer nor endContainer can be a DocumentType or Attribute node.",
        ));
    }
    let id = w.new_obj(Obj::Range(Box::new(RangeObj {
        slot: start.0,
        start: (start.1, start_off),
        end: (end.1, end_off),
        kind: Kind::StaticRange,
    })));
    cx.init_this(HostRef::new(K_OBJ, id))
}

/// A required `unsigned long` dictionary member.
fn offset(
    cx: &mut CallCtx<'_, '_, '_>,
    dict: &Dict,
    m: &str,
    member: &dyn Fn(&str, &str) -> HostError,
) -> Result<u32, HostError> {
    let v = dict
        .number(cx, m)?
        .ok_or_else(|| member(m, "Required member is undefined."))?;
    Ok(to_u32(v))
}

/// WebIDL `unsigned long` conversion (ToUint32).
fn to_u32(v: f64) -> u32 {
    if !v.is_finite() {
        return 0;
    }
    v.trunc().rem_euclid(4_294_967_296.0) as u32
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    register_basic(reg, rt);
    register_compare(reg, rt);
    register_geometry(reg, rt);
    register_contents(reg, rt);
    selection::register(reg, rt);
}

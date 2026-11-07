//! `Selection` (`window.getSelection()`, `document.getSelection()`): one
//! per window, for its document, on top of a live [`super::RangeObj`].
//! Behavior and error texts follow Blink's `DOMSelection` as recorded in
//! Chrome: at most one range, `addRange` keeps the first, nodes outside
//! the document are ignored, every change but `addRange` makes a new
//! `Range`, `direction` is kept only while the range is unchanged, and
//! `containsNode` uses the range normalized to its first and last text.
//! `selectionchange` fires on the document once per task in which the
//! selection changed through these methods (not when script changes the
//! selection's `Range` directly, nor for text control selections).
//! `modify` moves by characters and words within text; other
//! granularities (lines, sentences, paragraphs, boundaries) are no-ops.

use super::*;
use crate::rt::opt_string;

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
}

/// The selection's range now, `None` without one.
fn sel_range(w: &Window) -> Option<(u32, RangeObj)> {
    let id = w.ranges.borrow().sel.range?;
    current(w, id).ok().map(|r| (id, r))
}

fn dir_now(w: &Window, r: &RangeObj) -> Dir {
    let st = w.ranges.borrow();
    if r.start == r.end || st.sel.snap != Some((r.start, r.end)) {
        Dir::None
    } else {
        st.sel.dir
    }
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

fn changed(w: &Rc<Window>) {
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

fn clear(w: &Rc<Window>) {
    w.ranges.borrow_mut().sel.range = None;
    changed(w);
}

/// Select `anchor` to `focus` with a new range (both checked).
fn select(w: &Rc<Window>, anchor: Bp, focus: Bp, directional: bool) {
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
    }
    changed(w);
}

/// Blink's `IsValidForPosition`: a node of this document, connected.
fn valid(w: &Window, slot: u32, n: NodeId) -> bool {
    slot == 0 && w.doc_slot(0, |d| d.shadow_including_root(n) == Document::DOCUMENT)
}

fn this_sel(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> R {
    let id = this_obj(cx)?;
    let ok = w
        .with_obj(
            id,
            |o| matches!(o, Obj::Range(r) if r.kind == Kind::Selection),
        )
        .unwrap_or(false);
    if ok { Ok(()) } else { Err(illegal()) }
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

/// Blink's `DOMSelection::containsNode`.
fn contains_node(d: &Document, r: &RangeObj, n: NodeId, partial: bool) -> bool {
    let Some(p) = d.parent(n) else { return false };
    if d.root(n) != d.root(r.start.0) {
        return false;
    }
    let r = normalized(d, r);
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
    Break(u8),
}

/// The selection's text as `innerText` renders it (block breaks, `<br>`,
/// collapsed white space, `display: none` left out).
fn rendered_text(w: &Window, r: &RangeObj) -> String {
    let mut pieces = Vec::new();
    w.with_layout(r.slot, |l, d| {
        let ca = common_ancestor(d, r);
        let hidden = std::iter::once(ca)
            .chain(d.ancestors(ca))
            .any(|n| d.is_element(n) && l.computed_style(d, n).is_none_or(|s| s.is_display_none()));
        if !hidden {
            walk(l, d, r, ca, &mut pieces);
        }
    });
    let mut out = String::new();
    let mut breaks: u8 = 0;
    for p in pieces {
        match p {
            Piece::Break(n) => breaks = breaks.max(n),
            Piece::Text(t) => {
                let mut t = t.as_str();
                if breaks > 0 {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    if !out.is_empty() {
                        out.extend(std::iter::repeat_n('\n', breaks as usize));
                    }
                    breaks = 0;
                }
                if out.is_empty() || out.ends_with(' ') || out.ends_with('\n') {
                    t = t.trim_start_matches(' ');
                }
                out.push_str(t);
            }
        }
    }
    while out.ends_with(' ') {
        out.pop();
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
            if d.is_html_element_named(n, &LocalName::from("br")) {
                out.push(Piece::Text("\n".into()));
                return;
            }
            let block = !css.display.is_inline_level() && css.display != lb_css::Display::Contents;
            let p = d.is_html_element_named(n, &LocalName::from("p"));
            let brk = if p { 2 } else { 1 };
            if block {
                out.push(Piece::Break(brk));
            }
            for c in d.children(n) {
                walk(l, d, r, c, out);
            }
            if block {
                out.push(Piece::Break(brk));
            }
        }
        _ => {
            for c in d.children(n) {
                walk(l, d, r, c, out);
            }
        }
    }
}

fn is_word(c: u16) -> bool {
    char::from_u32(u32::from(c)).is_some_and(char::is_alphanumeric) || c == u16::from(b'_')
}

/// The caret position `modify` moves `p` to, `None` to stay.
fn step(d: &Document, p: Bp, forward: bool, word: bool) -> Option<Bp> {
    let (n, off) = p;
    if d.node_type(n) != NodeType::Text {
        return None;
    }
    let u: Vec<u16> = d.character_data(n).unwrap_or("").encode_utf16().collect();
    let len = u.len() as u32;
    let surrogate = |i: u32| {
        u.get(i as usize)
            .is_some_and(|c| (0xDC00..0xE000).contains(c))
    };
    if word {
        let mut i = off as usize;
        if forward {
            while i < u.len() && !is_word(u[i]) {
                i += 1;
            }
            while i < u.len() && is_word(u[i]) {
                i += 1;
            }
        } else {
            while i > 0 && !is_word(u[i - 1]) {
                i -= 1;
            }
            while i > 0 && is_word(u[i - 1]) {
                i -= 1;
            }
        }
        return (i as u32 != off).then_some((n, i as u32));
    }
    if forward {
        if off < len {
            let mut o = off + 1;
            if surrogate(o) {
                o += 1;
            }
            return Some((n, o));
        }
        // Into the next text node.
        let mut cur = next_skipping(d, n);
        while let Some(x) = cur {
            if d.node_type(x) == NodeType::Text && node_len(d, x) > 0 {
                return Some((x, 1));
            }
            cur = next_node(d, x);
        }
        None
    } else if off > 0 {
        let mut o = off - 1;
        if o > 0 && surrogate(o) {
            o -= 1;
        }
        Some((n, o))
    } else {
        None
    }
}

fn register_selection(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "Window", "getSelection", |cx, w| {
        let id = selection_obj(w);
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "Document", "getSelection", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        if slot != 0 || n != Document::DOCUMENT {
            return cx.ret(lb_js::Arg::Null);
        }
        let id = selection_obj(w);
        cx.ret(w.obj_arg(id))
    });
    for (prop, anchor) in [
        ("anchorNode", true),
        ("baseNode", true),
        ("focusNode", false),
        ("extentNode", false),
    ] {
        getter(reg, rt, IFACE, prop, move |cx, w| {
            this_sel(cx, w)?;
            let n = ends(w).map(|(a, f)| if anchor { a.0 } else { f.0 });
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
            this_sel(cx, w)?;
            let o = ends(w).map_or(0, |(a, f)| if anchor { a.1 } else { f.1 });
            cx.ret(o)
        });
    }
    getter(reg, rt, IFACE, "isCollapsed", |cx, w| {
        this_sel(cx, w)?;
        cx.ret(sel_range(w).is_none_or(|(_, r)| r.start == r.end))
    });
    getter(reg, rt, IFACE, "rangeCount", |cx, w| {
        this_sel(cx, w)?;
        cx.ret(u32::from(sel_range(w).is_some()))
    });
    getter(reg, rt, IFACE, "type", |cx, w| {
        this_sel(cx, w)?;
        let t = match sel_range(w) {
            None => "None",
            Some((_, r)) if r.start == r.end && !w.ranges.borrow().sel.deleted => "Caret",
            Some(_) => "Range",
        };
        cx.ret(t)
    });
    getter(reg, rt, IFACE, "direction", |cx, w| {
        this_sel(cx, w)?;
        let d = sel_range(w).map_or(Dir::None, |(_, r)| dir_now(w, &r));
        cx.ret(match d {
            Dir::None => "none",
            Dir::Forward => "forward",
            Dir::Backward => "backward",
        })
    });
    method(reg, rt, IFACE, "getRangeAt", |cx, w| {
        this_sel(cx, w)?;
        need_args(cx, 1, "getRangeAt", IFACE)?;
        let i = cx.arg_u32(0)?;
        match sel_range(w) {
            Some((id, _)) if i == 0 => cx.ret(w.obj_arg(id)),
            _ => Err(sel_err(
                w,
                "IndexSizeError",
                "getRangeAt",
                format!("{i} is not a valid index."),
            )),
        }
    });
    method(reg, rt, IFACE, "getComposedRanges", |cx, w| {
        this_sel(cx, w)?;
        let mut ids = Vec::new();
        if let Some((_, r)) = sel_range(w) {
            ids.push(w.new_obj(Obj::Range(Box::new(RangeObj {
                kind: Kind::StaticRange,
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
        let Some((anchor, _)) = ends(w) else {
            return Err(sel_err(
                w,
                "InvalidStateError",
                "extend",
                "This Selection object doesn't have any Ranges.",
            ));
        };
        check(w, "extend", n, off)?;
        if valid(w, n.0, n.1) {
            select(w, anchor, (n.1, off), true);
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
            select(w, (base.1, bo), (ext.1, eo), true);
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
        let bp = delete_contents(w, r);
        w.after_mutation(cx);
        select(w, bp, bp, false);
        w.ranges.borrow_mut().sel.deleted = true;
        Ok(())
    });
    method(reg, rt, IFACE, "containsNode", |cx, w| {
        this_sel(cx, w)?;
        need_args(cx, 1, "containsNode", IFACE)?;
        let (s, n) = arg_node(cx, w, 0, "containsNode", IFACE)?;
        let partial = cx.arg_bool(1);
        let v = match sel_range(w) {
            Some((_, r)) if s == r.slot => w.doc_slot(s, |d| contains_node(d, &r, n, partial)),
            _ => false,
        };
        cx.ret(v)
    });
    method(reg, rt, IFACE, "toString", |cx, w| {
        this_sel(cx, w)?;
        let s = sel_range(w).map_or_else(String::new, |(_, r)| rendered_text(w, &r));
        cx.ret(s)
    });
    method(reg, rt, IFACE, "modify", |cx, w| {
        this_sel(cx, w)?;
        let alter = opt_string(cx, 0)?.unwrap_or_default().to_ascii_lowercase();
        let dir = opt_string(cx, 1)?.unwrap_or_default().to_ascii_lowercase();
        let gran = opt_string(cx, 2)?.unwrap_or_default().to_ascii_lowercase();
        let Some((anchor, focus)) = ends(w) else {
            return Ok(());
        };
        let extend = match alter.as_str() {
            "move" => false,
            "extend" => true,
            _ => return Ok(()),
        };
        // Left and right are forward and backward in left-to-right text.
        let forward = match dir.as_str() {
            "forward" | "right" => true,
            "backward" | "left" => false,
            _ => return Ok(()),
        };
        let word = match gran.as_str() {
            "character" => false,
            "word" => true,
            _ => return Ok(()),
        };
        let Some(to) = w.doc_slot(0, |d| step(d, focus, forward, word)) else {
            return Ok(());
        };
        if extend {
            select(w, anchor, to, true);
        } else {
            select(w, to, to, false);
        }
        Ok(())
    });
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    register_selection(reg, rt);
}

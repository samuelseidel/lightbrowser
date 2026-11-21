//! Table DOM: `HTMLTableElement` (`caption`, `tHead`, `tFoot`, `tBodies`,
//! `rows`, create/delete helpers, `insertRow`/`deleteRow`),
//! `HTMLTableSectionElement` (`rows`, `insertRow`/`deleteRow`) and
//! `HTMLTableRowElement` (`cells`, `rowIndex`, `sectionRowIndex`,
//! `insertCell`/`deleteCell`). Collections are live HTMLCollections
//! (`coll::Filter::TableRows`, `ChildLocal`, `Cells`). Algorithms follow the
//! HTML spec ("tabular data"); error texts follow Blink.

use std::rc::Rc;

use lb_dom::{Document, LocalName, NodeId, QualName, ns};
use lb_js::{CallCtx, HostError, ValueKind};

use super::coll::{Filter, collection};
use super::{Runtime, Window, getter, is_elem, method, need_args, setter, this_node};
use crate::registry::Registry;

type R = Result<(), HostError>;

const TABLE: &str = "HTMLTableElement";
const SECTION: &str = "HTMLTableSectionElement";
const ROW: &str = "HTMLTableRowElement";

fn is_html(d: &Document, n: NodeId, local: &str) -> bool {
    d.qual_name(n)
        .is_some_and(|q| q.ns == ns!(html) && &*q.local == local)
}

/// HTML element children of `root` with one of these local names.
pub(crate) fn html_children(d: &Document, root: NodeId, names: &[&str]) -> Vec<NodeId> {
    d.element_children(root)
        .filter(|&c| names.iter().any(|l| is_html(d, c, l)))
        .collect()
}

/// `table.rows` in the spec's order: rows of `thead` children, then rows
/// that are children of the table or of its `tbody` children (tree order),
/// then rows of `tfoot` children.
pub(crate) fn table_rows(d: &Document, table: NodeId) -> Vec<NodeId> {
    let kids: Vec<NodeId> = d.element_children(table).collect();
    let mut out = Vec::new();
    for &k in &kids {
        if is_html(d, k, "thead") {
            out.extend(html_children(d, k, &["tr"]));
        }
    }
    for &k in &kids {
        if is_html(d, k, "tr") {
            out.push(k);
        } else if is_html(d, k, "tbody") {
            out.extend(html_children(d, k, &["tr"]));
        }
    }
    for &k in &kids {
        if is_html(d, k, "tfoot") {
            out.extend(html_children(d, k, &["tr"]));
        }
    }
    out
}

fn first_child(d: &Document, parent: NodeId, local: &str) -> Option<NodeId> {
    d.element_children(parent).find(|&c| is_html(d, c, local))
}

fn new_element(d: &mut Document, local: &str) -> NodeId {
    d.create_element(
        QualName::new(None, ns!(html), LocalName::from(local)),
        Vec::new(),
    )
}

/// `this` as an HTML element with one of these local names.
fn this_of(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    names: &[&str],
) -> Result<(u32, NodeId), HostError> {
    let (slot, n) = this_node(cx, w)?;
    let ok = w.doc_slot(slot, |d| {
        is_elem(d, n) && names.iter().any(|l| is_html(d, n, l))
    });
    if ok {
        Ok((slot, n))
    } else {
        Err(super::illegal())
    }
}

fn this_table(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
    this_of(cx, w, &["table"])
}

fn this_section(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
    this_of(cx, w, &["thead", "tbody", "tfoot"])
}

fn this_row(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
    this_of(cx, w, &["tr"])
}

/// Optional `long index = -1`.
fn opt_index(cx: &mut CallCtx<'_, '_, '_>) -> Result<i32, HostError> {
    if cx.is_empty() || cx.arg_kind(0) == ValueKind::Undefined {
        Ok(-1)
    } else {
        cx.arg_i32(0)
    }
}

/// The element set as `caption` / `tHead` / `tFoot`: `None` for null,
/// a TypeError when it is not an element of `iface` (local names `ok`).
fn setter_value(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    slot: u32,
    prop: &str,
    iface: &str,
    ok: &[&str],
) -> Result<Option<NodeId>, HostError> {
    if matches!(cx.arg_kind(0), ValueKind::Null | ValueKind::Undefined) {
        return Ok(None);
    }
    let bad = || {
        HostError::type_error(format!(
            "Failed to set the '{prop}' property on 'HTMLTableElement': The provided value is not of type '{iface}'."
        ))
    };
    let (s2, v) = cx
        .arg_host(0)
        .and_then(|h| w.decode_node(h))
        .ok_or_else(bad)?;
    if !w.doc_slot(s2, |d| ok.iter().any(|l| is_html(d, v, l))) {
        return Err(bad());
    }
    Ok(Some(super::dom::bring(cx, w, s2, v, slot)))
}

fn remove_first(d: &mut Document, parent: NodeId, local: &str) {
    if let Some(c) = first_child(d, parent, local) {
        d.detach(c);
    }
}

/// Where a `thead` goes: before the first element child that is neither a
/// `caption` nor a `colgroup`.
fn thead_slot(d: &Document, table: NodeId) -> Option<NodeId> {
    d.element_children(table)
        .find(|&c| !is_html(d, c, "caption") && !is_html(d, c, "colgroup"))
}

fn index_error(w: &Window, op: &str, iface: &str, msg: String) -> HostError {
    w.rt.err(
        "IndexSizeError",
        format!("Failed to execute '{op}' on '{iface}': {msg}"),
    )
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    register_table(reg, rt);
    register_section(reg, rt);
    register_row(reg, rt);
}

fn register_table(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, TABLE, "rows", |cx, w| {
        let (slot, n) = this_table(cx, w)?;
        let id = collection(w, slot, n, Filter::TableRows);
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, TABLE, "tBodies", |cx, w| {
        let (slot, n) = this_table(cx, w)?;
        let id = collection(w, slot, n, Filter::ChildLocal("tbody"));
        cx.ret(w.obj_arg(id))
    });
    for (prop, local) in [
        ("caption", "caption"),
        ("tHead", "thead"),
        ("tFoot", "tfoot"),
    ] {
        getter(reg, rt, TABLE, prop, move |cx, w| {
            let (slot, n) = this_table(cx, w)?;
            let c = w.doc_slot(slot, |d| first_child(d, n, local));
            cx.ret(w.opt_node_arg(slot, c))
        });
    }
    setter(reg, rt, TABLE, "caption", |cx, w| {
        let (slot, n) = this_table(cx, w)?;
        let v = setter_value(
            cx,
            w,
            slot,
            "caption",
            "HTMLTableCaptionElement",
            &["caption"],
        )?;
        let r = w.doc_slot_mut(slot, |d| {
            remove_first(d, n, "caption");
            match v {
                Some(v) => {
                    let first = d.first_child(n);
                    d.insert_before(n, v, first).map(|_| ())
                }
                None => Ok(()),
            }
        });
        w.after_mutation(cx);
        r.map_err(|e| super::dom::dom_err(w, "caption", TABLE, e))
    });
    for (prop, local) in [("tHead", "thead"), ("tFoot", "tfoot")] {
        setter(reg, rt, TABLE, prop, move |cx, w| {
            let (slot, n) = this_table(cx, w)?;
            let v = setter_value(cx, w, slot, prop, SECTION, &["thead", "tbody", "tfoot"])?;
            if let Some(v) = v
                && !w.doc_slot(slot, |d| is_html(d, v, local))
            {
                return Err(w.rt.err(
                    "HierarchyRequestError",
                    format!(
                        "Failed to set the '{prop}' property on 'HTMLTableElement': Not a {local} element."
                    ),
                ));
            }
            let r = w.doc_slot_mut(slot, |d| {
                remove_first(d, n, local);
                match v {
                    Some(v) => {
                        let before = if local == "thead" {
                            thead_slot(d, n)
                        } else {
                            None
                        };
                        d.insert_before(n, v, before).map(|_| ())
                    }
                    None => Ok(()),
                }
            });
            w.after_mutation(cx);
            r.map_err(|e| super::dom::dom_err(w, prop, TABLE, e))
        });
    }
    method(reg, rt, TABLE, "createCaption", |cx, w| {
        let (slot, n) = this_table(cx, w)?;
        let c = w.doc_slot_mut(slot, |d| {
            if let Some(c) = first_child(d, n, "caption") {
                return c;
            }
            let c = new_element(d, "caption");
            let first = d.first_child(n);
            let _ = d.insert_before(n, c, first);
            c
        });
        w.after_mutation(cx);
        cx.ret(w.node_arg(slot, c))
    });
    method(reg, rt, TABLE, "createTHead", |cx, w| {
        let (slot, n) = this_table(cx, w)?;
        let c = w.doc_slot_mut(slot, |d| {
            if let Some(c) = first_child(d, n, "thead") {
                return c;
            }
            let c = new_element(d, "thead");
            let before = thead_slot(d, n);
            let _ = d.insert_before(n, c, before);
            c
        });
        w.after_mutation(cx);
        cx.ret(w.node_arg(slot, c))
    });
    method(reg, rt, TABLE, "createTFoot", |cx, w| {
        let (slot, n) = this_table(cx, w)?;
        let c = w.doc_slot_mut(slot, |d| {
            if let Some(c) = first_child(d, n, "tfoot") {
                return c;
            }
            let c = new_element(d, "tfoot");
            let _ = d.append_child(n, c);
            c
        });
        w.after_mutation(cx);
        cx.ret(w.node_arg(slot, c))
    });
    method(reg, rt, TABLE, "createTBody", |cx, w| {
        let (slot, n) = this_table(cx, w)?;
        let c = w.doc_slot_mut(slot, |d| {
            let c = new_element(d, "tbody");
            let before = html_children(d, n, &["tbody"])
                .last()
                .and_then(|&last| d.next_sibling(last));
            let _ = d.insert_before(n, c, before);
            c
        });
        w.after_mutation(cx);
        cx.ret(w.node_arg(slot, c))
    });
    for (op, local) in [
        ("deleteCaption", "caption"),
        ("deleteTHead", "thead"),
        ("deleteTFoot", "tfoot"),
    ] {
        method(reg, rt, TABLE, op, move |cx, w| {
            let (slot, n) = this_table(cx, w)?;
            w.doc_slot_mut(slot, |d| remove_first(d, n, local));
            w.after_mutation(cx);
            Ok(())
        });
    }
    method(reg, rt, TABLE, "insertRow", |cx, w| {
        let (slot, n) = this_table(cx, w)?;
        let index = opt_index(cx)?;
        let rows = w.doc_slot(slot, |d| table_rows(d, n));
        let len = rows.len() as i32;
        if index < -1 {
            return Err(index_error(
                w,
                "insertRow",
                TABLE,
                format!("The index provided ({index}) is less than -1."),
            ));
        }
        if index > len {
            return Err(index_error(
                w,
                "insertRow",
                TABLE,
                format!(
                    "The index provided ({index}) is greater than the number of rows in the table ({len})."
                ),
            ));
        }
        let tr = w.doc_slot_mut(slot, |d| {
            let tr = new_element(d, "tr");
            let bodies = html_children(d, n, &["tbody"]);
            if rows.is_empty() {
                match bodies.last() {
                    Some(&b) => {
                        let _ = d.append_child(b, tr);
                    }
                    None => {
                        let b = new_element(d, "tbody");
                        let _ = d.append_child(b, tr);
                        let _ = d.append_child(n, b);
                    }
                }
            } else if index == -1 || index == len {
                let last = rows[rows.len() - 1];
                if let Some(p) = d.parent(last) {
                    let _ = d.append_child(p, tr);
                }
            } else {
                let at = rows[index as usize];
                if let Some(p) = d.parent(at) {
                    let _ = d.insert_before(p, tr, Some(at));
                }
            }
            tr
        });
        w.after_mutation(cx);
        cx.ret(w.node_arg(slot, tr))
    });
    method(reg, rt, TABLE, "deleteRow", |cx, w| {
        need_args(cx, 1, "deleteRow", TABLE)?;
        let (slot, n) = this_table(cx, w)?;
        let index = cx.arg_i32(0)?;
        let rows = w.doc_slot(slot, |d| table_rows(d, n));
        let len = rows.len() as i32;
        if index < -1 {
            return Err(index_error(
                w,
                "deleteRow",
                TABLE,
                format!("The index provided ({index}) is less than -1."),
            ));
        }
        if index >= len && !(index == -1 && len == 0) {
            return Err(index_error(
                w,
                "deleteRow",
                TABLE,
                format!(
                    "The index provided ({index}) is greater than the number of rows in the table ({len})."
                ),
            ));
        }
        let victim = if index == -1 {
            rows.last().copied()
        } else {
            Some(rows[index as usize])
        };
        if let Some(r) = victim {
            w.doc_slot_mut(slot, |d| d.detach(r));
            w.after_mutation(cx);
        }
        Ok(())
    });
}

/// Inserts a new `local` element into `parent` at `index` among `items`
/// (-1 or `items.len()`: append).
fn insert_at(
    d: &mut Document,
    parent: NodeId,
    items: &[NodeId],
    index: i32,
    local: &str,
) -> NodeId {
    let el = new_element(d, local);
    let before = if index == -1 || index as usize == items.len() {
        None
    } else {
        Some(items[index as usize])
    };
    let _ = d.insert_before(parent, el, before);
    el
}

fn register_section(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, SECTION, "rows", |cx, w| {
        let (slot, n) = this_section(cx, w)?;
        let id = collection(w, slot, n, Filter::ChildLocal("tr"));
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, SECTION, "insertRow", |cx, w| {
        let (slot, n) = this_section(cx, w)?;
        let index = opt_index(cx)?;
        let rows = w.doc_slot(slot, |d| html_children(d, n, &["tr"]));
        let len = rows.len() as i32;
        if index < -1 || index > len {
            return Err(index_error(
                w,
                "insertRow",
                SECTION,
                format!("The provided index ({index} is outside the range [-1, {len}]."),
            ));
        }
        let tr = w.doc_slot_mut(slot, |d| insert_at(d, n, &rows, index, "tr"));
        w.after_mutation(cx);
        cx.ret(w.node_arg(slot, tr))
    });
    method(reg, rt, SECTION, "deleteRow", |cx, w| {
        need_args(cx, 1, "deleteRow", SECTION)?;
        let (slot, n) = this_section(cx, w)?;
        let index = cx.arg_i32(0)?;
        let rows = w.doc_slot(slot, |d| html_children(d, n, &["tr"]));
        delete_at(cx, w, slot, &rows, index, "deleteRow", SECTION, |i, len| {
            format!("The provided index ({i} is outside the range [-1, {len}].")
        })
    });
}

/// `deleteRow` / `deleteCell`: -1 removes the last item (if any).
#[allow(clippy::too_many_arguments)]
fn delete_at(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    slot: u32,
    items: &[NodeId],
    index: i32,
    op: &str,
    iface: &str,
    msg: impl Fn(i32, i32) -> String,
) -> R {
    let len = items.len() as i32;
    let victim = if index == -1 {
        items.last().copied()
    } else if (0..len).contains(&index) {
        Some(items[index as usize])
    } else {
        return Err(index_error(w, op, iface, msg(index, len)));
    };
    if let Some(v) = victim {
        w.doc_slot_mut(slot, |d| d.detach(v));
        w.after_mutation(cx);
    }
    Ok(())
}

fn register_row(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, ROW, "cells", |cx, w| {
        let (slot, n) = this_row(cx, w)?;
        let id = collection(w, slot, n, Filter::Cells);
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, ROW, "rowIndex", |cx, w| {
        let (slot, n) = this_row(cx, w)?;
        let i = w.doc_slot(slot, |d| {
            let p = d.parent_element(n)?;
            let table = if is_html(d, p, "table") {
                p
            } else if ["thead", "tbody", "tfoot"].iter().any(|l| is_html(d, p, l)) {
                d.parent_element(p).filter(|&t| is_html(d, t, "table"))?
            } else {
                return None;
            };
            table_rows(d, table).iter().position(|&r| r == n)
        });
        cx.ret(i.map_or(-1, |i| i as i32))
    });
    getter(reg, rt, ROW, "sectionRowIndex", |cx, w| {
        let (slot, n) = this_row(cx, w)?;
        let i = w.doc_slot(slot, |d| {
            let p = d.parent_element(n)?;
            let rows = if is_html(d, p, "table") {
                table_rows(d, p)
            } else if ["thead", "tbody", "tfoot"].iter().any(|l| is_html(d, p, l)) {
                html_children(d, p, &["tr"])
            } else {
                return None;
            };
            rows.iter().position(|&r| r == n)
        });
        cx.ret(i.map_or(-1, |i| i as i32))
    });
    method(reg, rt, ROW, "insertCell", |cx, w| {
        let (slot, n) = this_row(cx, w)?;
        let index = opt_index(cx)?;
        let cells = w.doc_slot(slot, |d| html_children(d, n, &["td", "th"]));
        let len = cells.len() as i32;
        if index < -1 || index > len {
            return Err(index_error(
                w,
                "insertCell",
                ROW,
                format!("The value provided ({index}) is outside the range [-1, {len}]."),
            ));
        }
        let td = w.doc_slot_mut(slot, |d| insert_at(d, n, &cells, index, "td"));
        w.after_mutation(cx);
        cx.ret(w.node_arg(slot, td))
    });
    method(reg, rt, ROW, "deleteCell", |cx, w| {
        need_args(cx, 1, "deleteCell", ROW)?;
        let (slot, n) = this_row(cx, w)?;
        let index = cx.arg_i32(0)?;
        let cells = w.doc_slot(slot, |d| html_children(d, n, &["td", "th"]));
        delete_at(cx, w, slot, &cells, index, "deleteCell", ROW, |i, len| {
            format!("The value provided ({i}) is outside the range [0, {len}).")
        })
    });
}

//! Shadow DOM bindings (ADR 0026): `attachShadow`, `ShadowRoot`, slots
//! (`assignedNodes`, `assign`, `slotchange`), `getRootNode`, `getHTML` /
//! `setHTMLUnsafe` / `parseHTMLUnsafe`, focus and hit-test retargeting.
//!
//! The tree model (shadow roots, assignment, the flat tree) is in `lb-dom`;
//! style scoping in `lb-css`; event paths in `events.rs`.

use std::collections::HashMap;
use std::rc::Rc;

use lb_dom::{
    AttachShadowError, Document, Mutation, NodeId, ShadowRootInit, ShadowRootMode,
    ShadowSerialization, SlotAssignment, local_name, ns,
};
use lb_js::{Arg, CallCtx, HostError, ValueKind};

use super::{
    Dict, Js, Runtime, Window, getter, illegal, is_elem, method, need_args, setter, this_node,
};
use crate::registry::{ImplKind, Registry};

/// `slotchange` bookkeeping of the main document.
#[derive(Default)]
pub(crate) struct SlotState {
    /// Per shadow root: its slots in tree order with their assigned nodes,
    /// as last seen.
    seen: HashMap<NodeId, Vec<(NodeId, Vec<NodeId>)>>,
    /// Slots to fire `slotchange` at, in signal order.
    signals: Vec<NodeId>,
}

fn this_elem(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
    let (slot, n) = this_node(cx, w)?;
    if w.doc_slot(slot, |d| is_elem(d, n)) {
        Ok((slot, n))
    } else {
        Err(illegal())
    }
}

fn this_sr(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
    let (slot, n) = this_node(cx, w)?;
    if w.doc_slot(slot, |d| d.is_shadow_root(n)) {
        Ok((slot, n))
    } else {
        Err(illegal())
    }
}

fn enum_err(op: &str, iface: &str, dict: &str, member: &str, v: &str, ty: &str) -> HostError {
    HostError::type_error(format!(
        "Failed to execute '{op}' on '{iface}': Failed to read the '{member}' property from '{dict}': The provided value '{v}' is not a valid enum value of type {ty}."
    ))
}

/// The shadow root `Element.shadowRoot` exposes (open ones only).
fn open_root(d: &Document, n: NodeId) -> Option<NodeId> {
    let sr = d.shadow_root(n)?;
    (d.shadow_root_data(sr)?.mode == ShadowRootMode::Open).then_some(sr)
}

/// Retarget node `n` against the document (out of every shadow tree).
pub(crate) fn retarget_to_document(d: &Document, mut n: NodeId) -> NodeId {
    if d.shadow_roots().is_empty() {
        return n;
    }
    while let Some(sr) = d.containing_shadow_root(n) {
        match d.shadow_host(sr) {
            Some(h) => n = h,
            None => break,
        }
    }
    n
}

/// Retarget node `n` against tree root `root` (a document or shadow root):
/// the node itself if it is in that tree or a tree inside it, else the
/// nearest host that is.
pub(crate) fn retarget_to(d: &Document, mut n: NodeId, root: NodeId) -> Option<NodeId> {
    loop {
        let r = d.root(n);
        if r == root {
            return Some(n);
        }
        n = d.shadow_host(r)?;
    }
}

/// The focus a document or shadow root reports as `activeElement`.
pub(crate) fn active_element_in(w: &Window, root: NodeId) -> Option<NodeId> {
    let f = w.focused.get()?;
    w.with_doc(|d| {
        if !d.is_connected(f) {
            return None;
        }
        retarget_to(d, f, root)
    })
}

/// The `ShadowRootInit` dictionary (members in IDL order).
fn read_init(cx: &mut CallCtx<'_, '_, '_>) -> Result<ShadowRootInit, HostError> {
    const OP: &str = "attachShadow";
    let d = Dict::arg(cx, 0);
    let clonable = d.bool(cx, "clonable")?;
    let delegates_focus = d.bool(cx, "delegatesFocus")?;
    let mode = match d.string(cx, "mode")?.as_deref() {
        Some("open") => ShadowRootMode::Open,
        Some("closed") => ShadowRootMode::Closed,
        Some(v) => {
            d.release(cx);
            return Err(enum_err(
                OP,
                "Element",
                "ShadowRootInit",
                "mode",
                v,
                "ShadowRootMode",
            ));
        }
        None => {
            d.release(cx);
            return Err(HostError::type_error(
                "Failed to execute 'attachShadow' on 'Element': Failed to read the 'mode' property from 'ShadowRootInit': Required member is undefined.",
            ));
        }
    };
    let serializable = d.bool(cx, "serializable")?;
    let slot_assignment = match d.string(cx, "slotAssignment")?.as_deref() {
        None | Some("named") => SlotAssignment::Named,
        Some("manual") => SlotAssignment::Manual,
        Some(v) => {
            d.release(cx);
            return Err(enum_err(
                OP,
                "Element",
                "ShadowRootInit",
                "slotAssignment",
                v,
                "SlotAssignmentMode",
            ));
        }
    };
    d.release(cx);
    Ok(ShadowRootInit {
        mode,
        delegates_focus,
        slot_assignment,
        clonable,
        serializable,
    })
}

/// `GetHTMLOptions`: (serializableShadowRoots, shadowRoots).
fn read_get_html(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    slot: u32,
) -> Result<(bool, Vec<NodeId>), HostError> {
    let d = Dict::arg(cx, 0);
    let serializable = d.bool(cx, "serializableShadowRoots")?;
    let mut roots = Vec::new();
    if let Some(list) = d.get(cx, "shadowRoots")? {
        let len = cx.get_property(list, "length")?;
        let n = match cx.handle_value(len) {
            lb_js::JsValue::Number(n) if n >= 0.0 => n as u32,
            _ => 0,
        };
        cx.release(len);
        for i in 0..n {
            let item = cx.get_property(list, &i.to_string())?;
            let node = cx.handle_host(item).and_then(|h| w.decode_node(h));
            cx.release(item);
            match node {
                Some((s, r)) if s == slot && w.doc_slot(s, |d| d.is_shadow_root(r)) => {
                    roots.push(r)
                }
                _ => {
                    cx.release(list);
                    d.release(cx);
                    return Err(HostError::type_error(
                        "Failed to execute 'getHTML' on 'Element': Failed to read the 'shadowRoots' property from 'GetHTMLOptions': Failed to convert value to 'ShadowRoot'.",
                    ));
                }
            }
        }
        cx.release(list);
    }
    d.release(cx);
    Ok((serializable, roots))
}

// ---------------------------------------------------------------------------
// slotchange
// ---------------------------------------------------------------------------

/// Shadow roots whose slot assignment a mutation may change.
fn affected_roots(d: &Document, m: &Mutation, out: &mut Vec<NodeId>) {
    let mut add = |r: NodeId| {
        if !out.contains(&r) {
            out.push(r);
        }
    };
    match m {
        Mutation::ChildInserted { parent, .. } | Mutation::ChildRemoved { parent, .. } => {
            if let Some(sr) = d.shadow_root(*parent) {
                add(sr);
            }
            let r = d.root(*parent);
            if d.is_shadow_root(r) {
                add(r);
            }
        }
        Mutation::Attribute { element, name, .. } if name.ns == ns!() => {
            if &*name.local == "slot"
                && let Some(sr) = d.parent(*element).and_then(|p| d.shadow_root(p))
            {
                add(sr);
            }
            if &*name.local == "name" && d.is_slot(*element) {
                let r = d.root(*element);
                if d.is_shadow_root(r) {
                    add(r);
                }
            }
        }
        _ => {}
    }
}

/// Compare `sr`'s slot assignments with the last seen ones and signal the
/// slots that changed.
fn update_root(js: &mut dyn Js, w: &Rc<Window>, sr: NodeId) {
    let (now, old_slots): (Vec<(NodeId, Vec<NodeId>)>, Vec<(NodeId, Vec<NodeId>)>) = {
        let now = w.with_doc(|d| {
            d.slots_in(sr)
                .map(|s| (s, d.assigned_nodes(s)))
                .collect::<Vec<_>>()
        });
        let old = w
            .slot_state
            .borrow()
            .seen
            .get(&sr)
            .cloned()
            .unwrap_or_default();
        (now, old)
    };
    let mut changed = Vec::new();
    for (s, nodes) in &now {
        let before = old_slots.iter().find(|(o, _)| o == s).map(|(_, v)| v);
        if before.map_or(!nodes.is_empty(), |b| b != nodes) {
            changed.push(*s);
        }
    }
    for (s, nodes) in &old_slots {
        if !now.iter().any(|(n, _)| n == s) && !nodes.is_empty() {
            changed.push(*s);
        }
    }
    w.slot_state.borrow_mut().seen.insert(sr, now);
    for s in changed {
        signal_slot(js, w, s);
    }
}

/// "Signal a slot change": queue `slotchange` for the mutation observer
/// microtask.
pub(crate) fn signal_slot(js: &mut dyn Js, w: &Rc<Window>, slot: NodeId) {
    {
        let mut st = w.slot_state.borrow_mut();
        if st.signals.contains(&slot) {
            return;
        }
        st.signals.push(slot);
    }
    if !w.mo_queued.get() {
        w.mo_queued.set(true);
        let f = w.helpers.borrow().mo_deliver;
        if let Some(f) = f {
            js.microtask(f);
        }
    }
}

/// Track slot assignments after a batch of the main document's mutations.
pub(crate) fn on_mutations(js: &mut dyn Js, w: &Rc<Window>, batch: &[(u32, Mutation)]) {
    let roots = w.with_doc(|d| {
        let mut roots = Vec::new();
        if d.shadow_roots().is_empty() {
            return roots;
        }
        for (slot, m) in batch {
            if *slot == 0 {
                affected_roots(d, m, &mut roots);
            }
        }
        roots
    });
    for sr in roots {
        update_root(js, w, sr);
    }
}

/// The signal slots, emptied (taken before observers are notified).
pub(crate) fn take_signals(w: &Window) -> Vec<NodeId> {
    std::mem::take(&mut w.slot_state.borrow_mut().signals)
}

/// Fire `slotchange` at each slot.
pub(crate) fn fire_slotchange(js: &mut dyn Js, w: &Rc<Window>, slots: Vec<NodeId>) {
    for s in slots {
        super::events::fire_event(js, w, w.node_ref(0, s), "slotchange", true, false);
    }
}

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    method(reg, rt, "Element", "attachShadow", |cx, w| {
        need_args(cx, 1, "attachShadow", "Element")?;
        let (slot, n) = this_elem(cx, w)?;
        let init = read_init(cx)?;
        let not_supported = |w: &Window| {
            w.rt.err(
                "NotSupportedError",
                "Failed to execute 'attachShadow' on 'Element': This element does not support attachShadow",
            )
        };
        // Customized built-ins (an `is` value) cannot host.
        if super::custom::is_value(w, slot, n).is_some() {
            return Err(not_supported(w));
        }
        if super::custom::shadow_disabled(w, slot, n) {
            return Err(w.rt.err(
                "NotSupportedError",
                "Failed to execute 'attachShadow' on 'Element': attachShadow() is disabled by disabledFeatures static field.",
            ));
        }
        let r = w.doc_slot_mut(slot, |d| d.attach_shadow(n, &init));
        w.after_mutation(cx);
        match r {
            Ok(sr) => cx.ret(w.node_arg(slot, sr)),
            Err(AttachShadowError::NotSupportedElement) => Err(not_supported(w)),
            Err(AttachShadowError::AlreadyHost) => Err(w.rt.err(
                "NotSupportedError",
                "Failed to execute 'attachShadow' on 'Element': Shadow root cannot be created on a host which already hosts a shadow tree.",
            )),
        }
    });
    getter(reg, rt, "Element", "shadowRoot", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let r = w.doc_slot(slot, |d| open_root(d, n));
        cx.ret(w.opt_node_arg(slot, r))
    });
    for iface in ["Element", "Text"] {
        getter(reg, rt, iface, "assignedSlot", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let r = w.doc_slot(slot, |d| d.find_slot(n, true));
            cx.ret(w.opt_node_arg(slot, r))
        });
    }

    // ShadowRoot
    getter(reg, rt, "ShadowRoot", "mode", |cx, w| {
        let (slot, n) = this_sr(cx, w)?;
        let m = w.doc_slot(slot, |d| d.shadow_root_data(n).map(|x| x.mode));
        cx.ret(match m {
            Some(ShadowRootMode::Closed) => "closed",
            _ => "open",
        })
    });
    getter(reg, rt, "ShadowRoot", "host", |cx, w| {
        let (slot, n) = this_sr(cx, w)?;
        let h = w.doc_slot(slot, |d| d.shadow_host(n));
        cx.ret(w.opt_node_arg(slot, h))
    });
    macro_rules! flag {
        ($name:literal, $f:expr) => {
            getter(reg, rt, "ShadowRoot", $name, |cx, w| {
                let (slot, n) = this_sr(cx, w)?;
                let v = w.doc_slot(slot, |d| d.shadow_root_data(n).map($f).unwrap_or(false));
                cx.ret(v)
            });
        };
    }
    flag!("delegatesFocus", |x: &lb_dom::ShadowRootData| x
        .delegates_focus);
    flag!("clonable", |x: &lb_dom::ShadowRootData| x.clonable);
    flag!("serializable", |x: &lb_dom::ShadowRootData| x.serializable);
    getter(reg, rt, "ShadowRoot", "slotAssignment", |cx, w| {
        let (slot, n) = this_sr(cx, w)?;
        let m = w.doc_slot(slot, |d| d.shadow_root_data(n).map(|x| x.slot_assignment));
        cx.ret(match m {
            Some(SlotAssignment::Manual) => "manual",
            _ => "named",
        })
    });
    getter(reg, rt, "ShadowRoot", "innerHTML", |cx, w| {
        let (slot, n) = this_sr(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.inner_html(n)))
    });
    setter(reg, rt, "ShadowRoot", "innerHTML", |cx, w| {
        let (slot, n) = this_sr(cx, w)?;
        let html = if cx.arg_kind(0) == ValueKind::Null {
            String::new()
        } else {
            cx.arg_string(0)?
        };
        w.doc_slot_mut(slot, |d| d.set_inner_html(n, &html));
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "ShadowRoot", "activeElement", |cx, w| {
        let (slot, n) = this_sr(cx, w)?;
        let r = if slot == 0 {
            active_element_in(w, n)
        } else {
            None
        };
        cx.ret(w.opt_node_arg(slot, r))
    });
    // Constructable sheets are not modelled: an empty list.
    getter(reg, rt, "ShadowRoot", "adoptedStyleSheets", |cx, w| {
        this_sr(cx, w)?;
        cx.ret(Arg::Array(Vec::new()))
    });
    setter(reg, rt, "ShadowRoot", "adoptedStyleSheets", |cx, w| {
        this_sr(cx, w)?;
        Ok(())
    });
    for name in ["elementFromPoint", "elementsFromPoint"] {
        let many = name == "elementsFromPoint";
        method(reg, rt, "ShadowRoot", name, move |cx, w| {
            need_args(cx, 2, name, "ShadowRoot")?;
            let (slot, sr) = this_sr(cx, w)?;
            let (x, y) = (cx.arg_f64(0)? as f32, cx.arg_f64(1)? as f32);
            let hits = w.with_layout(slot, |l, d| l.elements_from_point(d, x, y));
            let mut out: Vec<NodeId> = Vec::new();
            w.doc_slot(slot, |d| {
                for h in hits {
                    if let Some(t) =
                        retarget_to(d, h, sr).or_else(|| Some(retarget_to_document(d, h)))
                        && !out.contains(&t)
                    {
                        out.push(t);
                    }
                }
            });
            if many {
                cx.ret(Arg::Array(
                    out.into_iter().map(|n| w.node_arg(slot, n)).collect(),
                ))
            } else {
                cx.ret(w.opt_node_arg(slot, out.first().copied()))
            }
        });
    }

    // getHTML / setHTMLUnsafe / parseHTMLUnsafe
    for iface in ["Element", "ShadowRoot"] {
        method(reg, rt, iface, "getHTML", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let (serializable, roots) = read_get_html(cx, w, slot)?;
            let s = w.doc_slot(slot, |d| {
                d.get_html(
                    n,
                    ShadowSerialization {
                        serializable,
                        roots: &roots,
                    },
                )
            });
            cx.ret(s)
        });
        method(reg, rt, iface, "setHTMLUnsafe", move |cx, w| {
            need_args(cx, 1, "setHTMLUnsafe", iface)?;
            let (slot, n) = this_node(cx, w)?;
            let html = cx.arg_string(0)?;
            w.doc_slot_mut(slot, |d| d.set_html_unsafe(n, &html));
            w.after_mutation(cx);
            Ok(())
        });
    }
    reg.register(
        "Document",
        "parseHTMLUnsafe",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 1, "parseHTMLUnsafe", "Document")?;
            let s = cx.arg_string(0)?;
            let mut d = Document::parse_html(&s);
            let scripts: Vec<NodeId> = d
                .shadow_including_descendants(Document::DOCUMENT)
                .into_iter()
                .filter(|&n| d.is_html_element_named(n, &local_name!("script")))
                .collect();
            for n in scripts {
                d.set_flags(n, lb_dom::NodeFlags::ALREADY_STARTED, true);
            }
            let slot = w.add_doc(d);
            cx.ret(w.node_arg(slot, Document::DOCUMENT))
        }),
    );

    // Node.getRootNode({composed})
    method(reg, rt, "Node", "getRootNode", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let d = Dict::arg(cx, 0);
        let composed = d.bool(cx, "composed")?;
        d.release(cx);
        let r = w.doc_slot(slot, |d| {
            if composed {
                d.shadow_including_root(n)
            } else {
                d.root(n)
            }
        });
        cx.ret(w.node_arg(slot, r))
    });

    // HTMLSlotElement
    for (name, elements) in [("assignedNodes", false), ("assignedElements", true)] {
        method(reg, rt, "HTMLSlotElement", name, move |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            let d = Dict::arg(cx, 0);
            let flatten = d.bool(cx, "flatten")?;
            d.release(cx);
            let nodes = w.doc_slot(slot, |d| {
                let v = if flatten {
                    d.flattened_assigned_nodes(n)
                } else {
                    d.assigned_nodes(n)
                };
                if elements {
                    v.into_iter().filter(|&x| d.is_element(x)).collect()
                } else {
                    v
                }
            });
            cx.ret(Arg::Array(
                nodes.into_iter().map(|x| w.node_arg(slot, x)).collect(),
            ))
        });
    }
    method(reg, rt, "HTMLSlotElement", "assign", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let mut nodes = Vec::with_capacity(cx.len());
        for i in 0..cx.len() {
            let node = cx.arg_host(i).and_then(|h| w.decode_node(h));
            let ok = node.filter(|&(s, x)| w.doc_slot(s, |d| d.is_element(x) || d.is_text(x)));
            match ok {
                Some((s, x)) if s == slot => nodes.push(x),
                Some(_) => {}
                None => {
                    return Err(HostError::type_error(
                        "Failed to execute 'assign' on 'HTMLSlotElement': The provided value is not of type '(Element or Text)'.",
                    ));
                }
            }
        }
        let changed = w.doc_slot_mut(slot, |d| {
            let before = d.manual_assigned(n);
            d.assign_slot(n, &nodes);
            before != d.manual_assigned(n)
        });
        if slot == 0 {
            if changed {
                signal_slot(cx, w, n);
            }
            let sr = w.with_doc(|d| {
                let r = d.root(n);
                d.is_shadow_root(r).then_some(r)
            });
            if let Some(sr) = sr {
                update_root(cx, w, sr);
            }
        }
        Ok(())
    });

    // HTMLTemplateElement shadowRoot* reflection
    getter(reg, rt, "HTMLTemplateElement", "shadowRootMode", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = w.doc_slot(slot, |d| {
            d.get_attribute(n, "shadowrootmode")
                .map(str::to_ascii_lowercase)
                .filter(|v| v == "open" || v == "closed")
                .unwrap_or_default()
        });
        cx.ret(v)
    });
    setter(reg, rt, "HTMLTemplateElement", "shadowRootMode", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_string(0)?;
        w.doc_slot_mut(slot, |d| d.set_attribute(n, "shadowrootmode", &v))
            .map_err(|e| super::dom::dom_err(w, "shadowRootMode", "HTMLTemplateElement", e))?;
        w.after_mutation(cx);
        Ok(())
    });
    for (prop, attr) in [
        ("shadowRootDelegatesFocus", "shadowrootdelegatesfocus"),
        ("shadowRootClonable", "shadowrootclonable"),
        ("shadowRootSerializable", "shadowrootserializable"),
    ] {
        getter(reg, rt, "HTMLTemplateElement", prop, move |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            cx.ret(w.doc_slot(slot, |d| d.has_attribute(n, attr)))
        });
        setter(reg, rt, "HTMLTemplateElement", prop, move |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            let on = super::truthy(&cx.arg_value(0));
            w.doc_slot_mut(slot, |d| {
                if on {
                    let _ = d.set_attribute(n, attr, "");
                } else {
                    d.remove_attribute(n, attr);
                }
            });
            w.after_mutation(cx);
            Ok(())
        });
    }
}

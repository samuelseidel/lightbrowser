//! Node, Element, Document, CharacterData, Text and the HTML element
//! members backed by `lb-dom` (tree, attributes, markup, selectors).

use std::rc::Rc;

use lb_dom::{
    Document, DomError, LocalName, Namespace, NodeFlags, NodeId, NodeType, QualName, local_name, ns,
};
use lb_js::{Arg, CallCtx, HostError, HostRef, ValueKind};

use super::tags::{REFLECTED, Reflect};
use super::{
    K_OBJ, Obj, Runtime, Window, arg_node, ctor, getter, illegal, is_elem, method, need_args,
    opt_string, setter, this_node,
};
use crate::registry::Registry;

type R = Result<(), HostError>;

/// The `is` member of `createElement`'s options (a string argument is
/// ignored, as in Chrome).
fn element_creation_is(
    cx: &mut CallCtx<'_, '_, '_>,
    i: usize,
) -> Result<Option<String>, HostError> {
    if i >= cx.len() || !matches!(cx.arg_kind(i), ValueKind::Object) {
        return Ok(None);
    }
    let d = super::Dict::arg(cx, i);
    let is = d.string(cx, "is")?;
    d.release(cx);
    Ok(is)
}

/// A freshly created element `n` as a custom element: constructed or
/// upgraded when a definition exists (the new element replaces `n`),
/// else given its `is` value.
fn custom_create(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    slot: u32,
    n: NodeId,
    is: Option<&str>,
) -> NodeId {
    let html_local = w.doc_slot(slot, |d| {
        d.qual_name(n)
            .filter(|q| q.ns == ns!(html))
            .map(|q| q.local.to_string())
    });
    let Some(local) = html_local else { return n };
    if let Some(x) = super::custom::create_element(cx, w, slot, &local, is) {
        super::custom::process(cx, w);
        return x;
    }
    if let Some(is) = is
        && !lb_dom::is_valid_custom_element_name(&local)
    {
        w.doc_slot_mut(slot, |d| {
            d.set_is_value(n, Some(is));
            d.set_custom_state(n, lb_dom::CustomState::Undefined);
        });
    }
    n
}

/// DOMException for a DOM error, with Chrome-like wording.
pub(crate) fn dom_err(w: &Window, op: &str, iface: &str, e: DomError) -> HostError {
    let detail = match &e {
        DomError::HierarchyRequest => "The new child element contains the parent.".to_owned(),
        DomError::NotFound => {
            "The node before which the new node is to be inserted is not a child of this node."
                .to_owned()
        }
        DomError::InvalidCharacter => "The string contains invalid characters.".to_owned(),
        DomError::Namespace => "The namespace is invalid.".to_owned(),
        DomError::NoModificationAllowed => "This node may not be modified.".to_owned(),
        DomError::NotSupported => "The operation is not supported.".to_owned(),
        DomError::Syntax(s) => format!("'{s}' is not a valid selector."),
    };
    w.rt.err(
        e.name(),
        format!("Failed to execute '{op}' on '{iface}': {detail}"),
    )
}

fn selector_err(w: &Window, op: &str, iface: &str, sel: &str) -> HostError {
    w.rt.err(
        "SyntaxError",
        format!("Failed to execute '{op}' on '{iface}': '{sel}' is not a valid selector."),
    )
}

/// Move `node` of document `from` into document `to` (adopt). Each
/// document is its own arena, so the subtree is copied into `to` and
/// detached from `from`; the copies take over the originals' JS wrappers
/// (in every realm), event listeners, inline handlers, form values and
/// selections, so the moved nodes keep their identity, as in Chrome.
pub(crate) fn bring(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    from: u32,
    node: NodeId,
    to: u32,
) -> NodeId {
    if from == to {
        return node;
    }
    let (src, dst) = (w.doc_rc(from), w.doc_rc(to));
    if Rc::ptr_eq(&src, &dst) {
        src.borrow_mut().detach(node);
        return node;
    }
    let (copy, map) = dst.borrow_mut().import_node_mapped(&src.borrow(), node);
    let was_connected = src.borrow().is_connected(node);
    super::custom::adopted(w, from, to, &map, was_connected);
    src.borrow_mut().detach(node);
    let pairs: Vec<(HostRef, HostRef)> = map
        .iter()
        .map(|&(o, n)| (w.node_ref(from, o), w.node_ref(to, n)))
        .collect();
    cx.rehost(&pairs);
    super::move_node_state(w, from, to, &map, &pairs);
    // The source document's window sees the removal now (observers,
    // frames inside the moved subtree), not on its next turn.
    if let Some((src_w, _)) = w.rt.doc_owner(w.doc_key(from))
        && !std::ptr::eq(&*src_w, w)
    {
        src_w.after_mutation(cx);
    }
    copy
}

/// The nodes of a `(Node or DOMString)...` argument list as one node
/// (a fragment when there are several).
fn convert_nodes(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    slot: u32,
) -> Result<Option<NodeId>, HostError> {
    let mut nodes = Vec::with_capacity(cx.len());
    for i in 0..cx.len() {
        match cx.arg_host(i).and_then(|h| w.decode_node(h)) {
            Some((s, n)) => nodes.push(bring(cx, w, s, n, slot)),
            None => {
                let t = cx.arg_string(i)?;
                nodes.push(w.doc_slot_mut(slot, |d| d.create_text(&t)));
            }
        }
    }
    Ok(match nodes.len() {
        0 => None,
        1 => Some(nodes[0]),
        _ => Some(w.doc_slot_mut(slot, |d| {
            let f = d.create_document_fragment();
            for n in nodes {
                let _ = d.append_child(f, n);
            }
            f
        })),
    })
}

fn ret_node(cx: &mut CallCtx<'_, '_, '_>, w: &Window, slot: u32, n: Option<NodeId>) -> R {
    cx.ret(w.opt_node_arg(slot, n))
}

fn this_elem(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
    let (slot, n) = this_node(cx, w)?;
    if w.doc_slot(slot, |d| is_elem(d, n)) {
        Ok((slot, n))
    } else {
        Err(illegal())
    }
}

/// UTF-16 length of a string.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Replace `count` UTF-16 units at `offset` with `data`.
fn utf16_splice(s: &str, offset: usize, count: usize, data: &str) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    let end = (offset + count).min(units.len());
    let mut out: Vec<u16> = units[..offset].to_vec();
    out.extend(data.encode_utf16());
    out.extend_from_slice(&units[end..]);
    String::from_utf16_lossy(&out)
}

fn utf16_sub(s: &str, offset: usize, count: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    let end = (offset + count).min(units.len());
    String::from_utf16_lossy(&units[offset.min(units.len())..end])
}

pub(crate) fn input_type(d: &Document, n: NodeId) -> String {
    const TYPES: &[&str] = &[
        "hidden",
        "text",
        "search",
        "tel",
        "url",
        "email",
        "password",
        "date",
        "month",
        "week",
        "time",
        "datetime-local",
        "number",
        "range",
        "color",
        "checkbox",
        "radio",
        "file",
        "submit",
        "image",
        "reset",
        "button",
    ];
    let t = d
        .get_attribute(n, "type")
        .unwrap_or("text")
        .to_ascii_lowercase();
    if TYPES.contains(&t.as_str()) {
        t
    } else {
        "text".into()
    }
}

/// Current checkedness of a checkbox/radio/option.
pub(crate) fn checked(d: &Document, n: NodeId, attr: &str) -> bool {
    let f = d.flags(n);
    if f.contains(NodeFlags::CHECKED_DIRTY) {
        f.contains(NodeFlags::CHECKED)
    } else {
        d.has_attribute(n, attr)
    }
}

fn set_checked(d: &mut Document, n: NodeId, v: bool) {
    d.set_flags(n, NodeFlags::CHECKED_DIRTY, true);
    d.set_flags(n, NodeFlags::CHECKED, v);
}

/// Text of an `<option>` (descendant text, whitespace collapsed).
pub(crate) fn option_text(d: &Document, n: NodeId) -> String {
    d.text_content(n)
        .unwrap_or_default()
        .split_ascii_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn select_options(d: &Document, sel: NodeId) -> Vec<NodeId> {
    d.descendants(sel)
        .filter(|&n| d.is_html_element_named(n, &local_name!("option")))
        .collect()
}

pub(crate) fn selected_index(d: &Document, sel: NodeId) -> i32 {
    let opts = select_options(d, sel);
    if let Some(i) = opts.iter().position(|&o| checked(d, o, "selected")) {
        return i as i32;
    }
    if !d.has_attribute(sel, "multiple") && !opts.is_empty() {
        0
    } else {
        -1
    }
}

/// `node.textContent` setter semantics including Document (no-op).
fn set_text(d: &mut Document, n: NodeId, v: Option<&str>) {
    d.set_text_content(n, v);
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    register_node(reg, rt);
    register_attr_node(reg, rt);
    register_parent_child(reg, rt);
    register_element(reg, rt);
    register_document(reg, rt);
    register_chardata(reg, rt);
    register_reflected(reg, rt);
    register_html(reg, rt);
}

/// `this` as an Attr: `(slot, owner, qualified name, value)`.
fn this_attr(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
) -> Option<(u32, Option<NodeId>, String, String)> {
    let h = cx.this_host().filter(|h| h.kind == K_OBJ)?;
    w.with_obj(h.id, |o| match o {
        Obj::Attr {
            slot,
            owner,
            name,
            value,
        } => {
            let q = match &name.prefix {
                Some(p) => format!("{}:{}", &**p, &*name.local),
                None => name.local.to_string(),
            };
            let v = match owner {
                Some(n) => w.doc_slot(*slot, |d| {
                    d.get_attribute_ns(*n, &name.ns, &name.local)
                        .unwrap_or("")
                        .to_owned()
                }),
                None => value.clone(),
            };
            Some((*slot, *owner, q, v))
        }
        _ => None,
    })
    .flatten()
}

/// Node members on Attr objects (Attr is a Node without children).
fn register_attr_node(reg: &mut Registry, rt: &Runtime) {
    let wrap = |rt: &Runtime,
                base: Option<lb_js::HostFn>,
                f: fn(
        &mut CallCtx<'_, '_, '_>,
        &Window,
        (u32, Option<NodeId>, String, String),
    ) -> R| {
        let rt = rt.clone();
        lb_js::host_fn(move |cx| {
            let w = rt.win(cx.realm())?;
            if let Some(a) = this_attr(cx, &w) {
                return f(cx, &w, a);
            }
            match &base {
                Some(b) => b(cx),
                None => Err(illegal()),
            }
        })
    };
    use crate::registry::ImplKind::{Getter, Method};
    let members: [(
        &'static str,
        crate::registry::ImplKind,
        fn(&mut CallCtx<'_, '_, '_>, &Window, (u32, Option<NodeId>, String, String)) -> R,
    ); 13] = [
        ("nodeType", Getter, |cx, _, _| cx.ret(2)),
        ("nodeName", Getter, |cx, _, a| cx.ret(a.2)),
        ("nodeValue", Getter, |cx, _, a| cx.ret(a.3)),
        ("textContent", Getter, |cx, _, a| cx.ret(a.3)),
        ("ownerDocument", Getter, |cx, w, a| {
            cx.ret(w.node_arg(a.0, Document::DOCUMENT))
        }),
        ("parentNode", Getter, |cx, _, _| cx.ret(Arg::Null)),
        ("parentElement", Getter, |cx, _, _| cx.ret(Arg::Null)),
        ("firstChild", Getter, |cx, _, _| cx.ret(Arg::Null)),
        ("lastChild", Getter, |cx, _, _| cx.ret(Arg::Null)),
        ("previousSibling", Getter, |cx, _, _| cx.ret(Arg::Null)),
        ("nextSibling", Getter, |cx, _, _| cx.ret(Arg::Null)),
        ("isConnected", Getter, |cx, _, _| cx.ret(false)),
        ("hasChildNodes", Method, |cx, _, _| cx.ret(false)),
    ];
    for (name, kind, f) in members {
        let base = reg.get("Node", name, kind).cloned();
        reg.register("Node", name, kind, wrap(rt, base, f));
    }
}

fn register_node(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Node", "nodeType", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.node_type(n) as u32))
    });
    getter(reg, rt, "Node", "nodeName", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.node_name(n)))
    });
    getter(reg, rt, "Node", "baseURI", |cx, w| {
        this_node(cx, w)?;
        cx.ret(w.base_url().to_string())
    });
    getter(reg, rt, "Node", "isConnected", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.is_connected(n)))
    });
    getter(reg, rt, "Node", "ownerDocument", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        if n == Document::DOCUMENT {
            return cx.ret(Arg::Null);
        }
        cx.ret(w.node_arg(slot, Document::DOCUMENT))
    });
    getter(reg, rt, "Node", "parentNode", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let p = w.doc_slot(slot, |d| d.parent(n));
        ret_node(cx, w, slot, p)
    });
    getter(reg, rt, "Node", "parentElement", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let p = w.doc_slot(slot, |d| d.parent_element(n));
        ret_node(cx, w, slot, p)
    });
    method(reg, rt, "Node", "hasChildNodes", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.first_child(n).is_some()))
    });
    getter(reg, rt, "Node", "childNodes", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let id = w.cached_obj((slot, n.0, "childNodes", String::new()), || {
            Obj::NodeList(super::coll::NodeListSrc::Children(slot, n))
        });
        cx.ret(w.obj_arg(id))
    });
    macro_rules! nav {
        ($name:literal, $f:ident) => {
            getter(reg, rt, "Node", $name, |cx, w| {
                let (slot, n) = this_node(cx, w)?;
                let r = w.doc_slot(slot, |d| d.$f(n));
                ret_node(cx, w, slot, r)
            });
        };
    }
    nav!("firstChild", first_child);
    nav!("lastChild", last_child);
    nav!("previousSibling", prev_sibling);
    nav!("nextSibling", next_sibling);
    getter(reg, rt, "Node", "nodeValue", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        match w.doc_slot(slot, |d| d.character_data(n).map(str::to_owned)) {
            Some(s) => cx.ret(s),
            None => cx.ret(Arg::Null),
        }
    });
    setter(reg, rt, "Node", "nodeValue", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let v = if matches!(cx.arg_kind(0), ValueKind::Null | ValueKind::Undefined) {
            String::new()
        } else {
            cx.arg_string(0)?
        };
        w.doc_slot_mut(slot, |d| d.set_character_data(n, &v));
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "Node", "textContent", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        match w.doc_slot(slot, |d| d.text_content(n)) {
            Some(s) => cx.ret(s),
            None => cx.ret(Arg::Null),
        }
    });
    setter(reg, rt, "Node", "textContent", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let v = if matches!(cx.arg_kind(0), ValueKind::Null | ValueKind::Undefined) {
            None
        } else {
            Some(cx.arg_string(0)?)
        };
        w.doc_slot_mut(slot, |d| set_text(d, n, v.as_deref()));
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "Node", "normalize", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        w.doc_slot_mut(slot, |d| normalize(d, n));
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "Node", "cloneNode", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        if w.doc_slot(slot, |d| d.is_shadow_root(n)) {
            return Err(w.rt.err(
                "NotSupportedError",
                "Failed to execute 'cloneNode' on 'Node': ShadowRoot nodes are not clonable.",
            ));
        }
        let deep = cx.arg_bool(0);
        let c = w.doc_slot_mut(slot, |d| {
            if n == Document::DOCUMENT {
                None
            } else {
                Some(d.clone_node(n, deep))
            }
        });
        match c {
            Some(c) => {
                // Clones of form controls keep their dirty value / checkedness.
                let v = w.values.borrow().get(&(slot, n)).cloned();
                if let Some(v) = v {
                    w.values.borrow_mut().insert((slot, c), v);
                }
                // Custom elements among the copies are upgraded now.
                w.after_mutation(cx);
                ret_node(cx, w, slot, Some(c))
            }
            None => Err(w.rt.err(
                "NotSupportedError",
                "Failed to execute 'cloneNode' on 'Node': Cloning documents is not supported.",
            )),
        }
    });
    method(reg, rt, "Node", "isEqualNode", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let Some((s2, m)) = cx.arg_host(0).and_then(|h| w.decode_node(h)) else {
            return cx.ret(false);
        };
        if s2 != slot {
            let a = w.doc_slot(slot, |d| d.outer_html(n));
            let b = w.doc_slot(s2, |d| d.outer_html(m));
            return cx.ret(a == b);
        }
        cx.ret(w.doc_slot(slot, |d| nodes_equal(d, n, m)))
    });
    method(reg, rt, "Node", "isSameNode", |cx, w| {
        let a = this_node(cx, w)?;
        let b = cx.arg_host(0).and_then(|h| w.decode_node(h));
        cx.ret(Some(a) == b)
    });
    method(reg, rt, "Node", "contains", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        match cx.arg_host(0).and_then(|h| w.decode_node(h)) {
            Some((s2, m)) if s2 == slot => cx.ret(w.doc_slot(slot, |d| d.contains(n, m))),
            _ => cx.ret(false),
        }
    });
    method(reg, rt, "Node", "compareDocumentPosition", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let (s2, m) = arg_node(cx, w, 0, "compareDocumentPosition", "Node")?;
        if s2 != slot {
            // Disconnected: implementation-specific but consistent order.
            // Chrome orders by allocation; documents made later follow.
            let later = w.doc_key(s2) > w.doc_key(slot);
            return cx.ret(1 + 32 + if later { 4 } else { 2 });
        }
        cx.ret(w.doc_slot(slot, |d| compare_position(d, n, m)))
    });
    method(reg, rt, "Node", "lookupNamespaceURI", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let prefix = opt_string(cx, 0)?.filter(|p| !p.is_empty());
        let ns = w.doc_slot(slot, |d| {
            let el = if is_elem(d, n) {
                Some(n)
            } else {
                d.parent_element(n)
            };
            let el = el.or(if n == Document::DOCUMENT {
                d.document_element()
            } else {
                None
            })?;
            let q = d.qual_name(el)?;
            if prefix.is_none() && q.prefix.is_none() && !q.ns.is_empty() {
                return Some(q.ns.to_string());
            }
            if prefix.as_deref() == Some("xml") {
                return Some("http://www.w3.org/XML/1998/namespace".into());
            }
            None
        });
        cx.ret(ns.map_or(Arg::Null, Arg::String))
    });
    method(reg, rt, "Node", "lookupPrefix", |cx, w| {
        this_node(cx, w)?;
        cx.ret(Arg::Null)
    });
    method(reg, rt, "Node", "isDefaultNamespace", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let ns = opt_string(cx, 0)?.unwrap_or_default();
        let r = w.doc_slot(slot, |d| {
            let el = if is_elem(d, n) {
                Some(n)
            } else {
                d.document_element()
            };
            el.and_then(|e| d.qual_name(e).map(|q| *q.ns == *ns))
                .unwrap_or(ns.is_empty())
        });
        cx.ret(r)
    });
    method(reg, rt, "Node", "appendChild", |cx, w| {
        need_args(cx, 1, "appendChild", "Node")?;
        let (slot, p) = this_node(cx, w)?;
        let (s2, c) = arg_node(cx, w, 0, "appendChild", "Node")?;
        let c = bring(cx, w, s2, c, slot);
        let r = w.doc_slot_mut(slot, |d| d.append_child(p, c));
        w.after_mutation(cx);
        match r {
            Ok(n) => ret_node(cx, w, slot, Some(n)),
            Err(e) => Err(dom_err(w, "appendChild", "Node", e)),
        }
    });
    method(reg, rt, "Node", "insertBefore", |cx, w| {
        need_args(cx, 2, "insertBefore", "Node")?;
        let (slot, p) = this_node(cx, w)?;
        let (s2, c) = arg_node(cx, w, 0, "insertBefore", "Node")?;
        let before = match cx.arg_kind(1) {
            ValueKind::Null | ValueKind::Undefined => None,
            _ => {
                let (s3, b) = arg_node(cx, w, 1, "insertBefore", "Node")?;
                if s3 != slot {
                    return Err(dom_err(w, "insertBefore", "Node", DomError::NotFound));
                }
                Some(b)
            }
        };
        let c = bring(cx, w, s2, c, slot);
        let r = w.doc_slot_mut(slot, |d| d.insert_before(p, c, before));
        w.after_mutation(cx);
        match r {
            Ok(n) => ret_node(cx, w, slot, Some(n)),
            Err(e) => Err(dom_err(w, "insertBefore", "Node", e)),
        }
    });
    method(reg, rt, "Node", "replaceChild", |cx, w| {
        need_args(cx, 2, "replaceChild", "Node")?;
        let (slot, p) = this_node(cx, w)?;
        let (s2, node) = arg_node(cx, w, 0, "replaceChild", "Node")?;
        let (s3, child) = arg_node(cx, w, 1, "replaceChild", "Node")?;
        if s3 != slot {
            return Err(w.rt.err(
                "NotFoundError",
                "Failed to execute 'replaceChild' on 'Node': The node to be replaced is not a child of this node.",
            ));
        }
        let node = bring(cx, w, s2, node, slot);
        let r = w.doc_slot_mut(slot, |d| d.replace_child(p, node, child));
        w.after_mutation(cx);
        match r {
            Ok(n) => ret_node(cx, w, slot, Some(n)),
            Err(DomError::NotFound) => Err(w.rt.err(
                "NotFoundError",
                "Failed to execute 'replaceChild' on 'Node': The node to be replaced is not a child of this node.",
            )),
            Err(e) => Err(dom_err(w, "replaceChild", "Node", e)),
        }
    });
    method(reg, rt, "Node", "removeChild", |cx, w| {
        need_args(cx, 1, "removeChild", "Node")?;
        let (slot, p) = this_node(cx, w)?;
        let (s2, c) = arg_node(cx, w, 0, "removeChild", "Node")?;
        if s2 != slot {
            return Err(w.rt.err(
                "NotFoundError",
                "Failed to execute 'removeChild' on 'Node': The node to be removed is not a child of this node.",
            ));
        }
        let r = w.doc_slot_mut(slot, |d| d.remove_child(p, c));
        w.after_mutation(cx);
        match r {
            Ok(n) => ret_node(cx, w, slot, Some(n)),
            Err(_) => Err(w.rt.err(
                "NotFoundError",
                "Failed to execute 'removeChild' on 'Node': The node to be removed is not a child of this node.",
            )),
        }
    });
}

fn normalize(d: &mut Document, n: NodeId) {
    let texts: Vec<NodeId> = d
        .inclusive_descendants(n)
        .filter(|&x| d.node_type(x) == NodeType::Text)
        .collect();
    for t in texts {
        if d.parent(t).is_none() && t != n {
            continue;
        }
        let data = d.character_data(t).unwrap_or("").to_owned();
        if data.is_empty() {
            if let Some(p) = d.parent(t) {
                let _ = d.remove_child(p, t);
            }
            continue;
        }
        // Merge following contiguous text siblings into this one.
        let mut merged = data.clone();
        let mut next = d.next_sibling(t);
        let mut to_remove = Vec::new();
        while let Some(x) = next {
            if d.node_type(x) != NodeType::Text {
                break;
            }
            merged.push_str(d.character_data(x).unwrap_or(""));
            to_remove.push(x);
            next = d.next_sibling(x);
        }
        if merged != data {
            d.set_character_data(t, &merged);
        }
        for x in to_remove {
            if let Some(p) = d.parent(x) {
                let _ = d.remove_child(p, x);
            }
        }
    }
}

fn nodes_equal(d: &Document, a: NodeId, b: NodeId) -> bool {
    if d.node_type(a) != d.node_type(b) {
        return false;
    }
    match d.node_type(a) {
        NodeType::Element => {
            let (ea, eb) = (d.element(a).unwrap(), d.element(b).unwrap());
            if ea.name != eb.name || ea.attrs.len() != eb.attrs.len() {
                return false;
            }
            if !ea.attrs.iter().all(|x| {
                eb.attrs
                    .iter()
                    .any(|y| y.name == x.name && y.value == x.value)
            }) {
                return false;
            }
        }
        NodeType::DocumentType => {
            let (x, y) = (d.doctype(a).unwrap(), d.doctype(b).unwrap());
            if x.name != y.name || x.public_id != y.public_id || x.system_id != y.system_id {
                return false;
            }
        }
        NodeType::ProcessingInstruction => {
            let (x, y) = (
                d.processing_instruction(a).unwrap(),
                d.processing_instruction(b).unwrap(),
            );
            if x.target != y.target || x.data != y.data {
                return false;
            }
        }
        NodeType::Text | NodeType::Comment if d.character_data(a) != d.character_data(b) => {
            return false;
        }
        _ => {}
    }
    let ka: Vec<NodeId> = d.children(a).collect();
    let kb: Vec<NodeId> = d.children(b).collect();
    ka.len() == kb.len() && ka.iter().zip(&kb).all(|(x, y)| nodes_equal(d, *x, *y))
}

fn compare_position(d: &Document, a: NodeId, b: NodeId) -> u32 {
    const DISCONNECTED: u32 = 1;
    const PRECEDING: u32 = 2;
    const FOLLOWING: u32 = 4;
    const CONTAINS: u32 = 8;
    const CONTAINED_BY: u32 = 16;
    const IMPL: u32 = 32;
    if a == b {
        return 0;
    }
    if d.root(a) != d.root(b) {
        return DISCONNECTED | IMPL | if a.0 < b.0 { FOLLOWING } else { PRECEDING };
    }
    if d.contains(b, a) {
        return CONTAINS | PRECEDING;
    }
    if d.contains(a, b) {
        return CONTAINED_BY | FOLLOWING;
    }
    // Tree order: find which comes first in a pre-order walk from the root.
    let root = d.root(a);
    for n in d.inclusive_descendants(root) {
        if n == a {
            return FOLLOWING;
        }
        if n == b {
            return PRECEDING;
        }
    }
    DISCONNECTED
}

/// ParentNode / ChildNode / NonElementParentNode members, shared by
/// Document, DocumentFragment, Element, CharacterData and DocumentType.
fn register_parent_child(reg: &mut Registry, rt: &Runtime) {
    for iface in ["Document", "DocumentFragment", "Element"] {
        getter(reg, rt, iface, "children", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let id = super::coll::collection(w, slot, n, super::coll::Filter::Children);
            cx.ret(w.obj_arg(id))
        });
        getter(reg, rt, iface, "firstElementChild", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let r = w.doc_slot(slot, |d| d.first_element_child(n));
            ret_node(cx, w, slot, r)
        });
        getter(reg, rt, iface, "lastElementChild", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let r = w.doc_slot(slot, |d| d.last_element_child(n));
            ret_node(cx, w, slot, r)
        });
        getter(reg, rt, iface, "childElementCount", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            cx.ret(w.doc_slot(slot, |d| d.element_children(n).count() as u32))
        });
        method(reg, rt, iface, "querySelector", move |cx, w| {
            need_args(cx, 1, "querySelector", iface)?;
            let (slot, n) = this_node(cx, w)?;
            let sel = cx.arg_string(0)?;
            let r = w.doc_slot(slot, |d| d.query_selector(n, &sel));
            match r {
                Ok(r) => ret_node(cx, w, slot, r),
                Err(_) => Err(selector_err(w, "querySelector", iface, &sel)),
            }
        });
        method(reg, rt, iface, "querySelectorAll", move |cx, w| {
            need_args(cx, 1, "querySelectorAll", iface)?;
            let (slot, n) = this_node(cx, w)?;
            let sel = cx.arg_string(0)?;
            let r = w.doc_slot(slot, |d| d.query_selector_all(n, &sel));
            match r {
                Ok(list) => {
                    let id = w.new_obj(Obj::NodeList(super::coll::NodeListSrc::Static(slot, list)));
                    cx.ret(w.obj_arg(id))
                }
                Err(_) => Err(selector_err(w, "querySelectorAll", iface, &sel)),
            }
        });
        method(reg, rt, iface, "append", |cx, w| {
            let (slot, p) = this_node(cx, w)?;
            if let Some(n) = convert_nodes(cx, w, slot)? {
                let r = w.doc_slot_mut(slot, |d| d.append_child(p, n));
                w.after_mutation(cx);
                r.map_err(|e| dom_err(w, "append", "Element", e))?;
            }
            Ok(())
        });
        method(reg, rt, iface, "prepend", |cx, w| {
            let (slot, p) = this_node(cx, w)?;
            if let Some(n) = convert_nodes(cx, w, slot)? {
                let r = w.doc_slot_mut(slot, |d| {
                    let first = d.first_child(p);
                    d.insert_before(p, n, first)
                });
                w.after_mutation(cx);
                r.map_err(|e| dom_err(w, "prepend", "Element", e))?;
            }
            Ok(())
        });
        method(reg, rt, iface, "replaceChildren", |cx, w| {
            let (slot, p) = this_node(cx, w)?;
            let n = convert_nodes(cx, w, slot)?;
            w.doc_slot_mut(slot, |d| {
                d.remove_all_children(p);
                if let Some(n) = n {
                    let _ = d.append_child(p, n);
                }
            });
            w.after_mutation(cx);
            Ok(())
        });
        method(reg, rt, iface, "getElementById", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let id = cx.arg_string(0)?;
            let r = w.doc_slot(slot, |d| d.get_element_by_id_in(n, &id));
            ret_node(cx, w, slot, r)
        });
    }
    for iface in ["Document", "Element"] {
        method(reg, rt, iface, "getElementsByTagName", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let name = cx.arg_string(0)?;
            let id = super::coll::collection(w, slot, n, super::coll::Filter::TagName(name));
            cx.ret(w.obj_arg(id))
        });
        method(reg, rt, iface, "getElementsByTagNameNS", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let ns = opt_string(cx, 0)?.unwrap_or_default();
            let name = cx.arg_string(1)?;
            let id = super::coll::collection(w, slot, n, super::coll::Filter::TagNameNs(ns, name));
            cx.ret(w.obj_arg(id))
        });
        method(reg, rt, iface, "getElementsByClassName", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let names = cx.arg_string(0)?;
            let id = super::coll::collection(w, slot, n, super::coll::Filter::ClassName(names));
            cx.ret(w.obj_arg(id))
        });
    }
    for iface in ["Element", "CharacterData", "DocumentType"] {
        method(reg, rt, iface, "remove", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            w.doc_slot_mut(slot, |d| d.detach(n));
            w.after_mutation(cx);
            Ok(())
        });
        method(reg, rt, iface, "before", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let Some(p) = w.doc_slot(slot, |d| d.parent(n)) else {
                return Ok(());
            };
            if let Some(x) = convert_nodes(cx, w, slot)? {
                let r = w.doc_slot_mut(slot, |d| {
                    // The node itself may have moved into the fragment.
                    let before = if d.parent(n) == Some(p) {
                        Some(n)
                    } else {
                        None
                    };
                    d.insert_before(p, x, before)
                });
                w.after_mutation(cx);
                r.map_err(|e| dom_err(w, "before", "Element", e))?;
            }
            Ok(())
        });
        method(reg, rt, iface, "after", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let Some(p) = w.doc_slot(slot, |d| d.parent(n)) else {
                return Ok(());
            };
            let next = w.doc_slot(slot, |d| d.next_sibling(n));
            if let Some(x) = convert_nodes(cx, w, slot)? {
                let r = w.doc_slot_mut(slot, |d| {
                    let before = next.filter(|&s| d.parent(s) == Some(p));
                    d.insert_before(p, x, before)
                });
                w.after_mutation(cx);
                r.map_err(|e| dom_err(w, "after", "Element", e))?;
            }
            Ok(())
        });
        method(reg, rt, iface, "replaceWith", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let Some(p) = w.doc_slot(slot, |d| d.parent(n)) else {
                return Ok(());
            };
            let next = w.doc_slot(slot, |d| d.next_sibling(n));
            let x = convert_nodes(cx, w, slot)?;
            let r = w.doc_slot_mut(slot, |d| match x {
                Some(x) if d.parent(n) == Some(p) => d.replace_child(p, x, n).map(|_| ()),
                Some(x) => {
                    let before = next.filter(|&s| d.parent(s) == Some(p));
                    d.insert_before(p, x, before).map(|_| ())
                }
                None => {
                    d.detach(n);
                    Ok(())
                }
            });
            w.after_mutation(cx);
            r.map_err(|e| dom_err(w, "replaceWith", "Element", e))
        });
    }
    for iface in ["Element", "CharacterData"] {
        getter(reg, rt, iface, "previousElementSibling", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let r = w.doc_slot(slot, |d| d.prev_element_sibling(n));
            ret_node(cx, w, slot, r)
        });
        getter(reg, rt, iface, "nextElementSibling", |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let r = w.doc_slot(slot, |d| d.next_element_sibling(n));
            ret_node(cx, w, slot, r)
        });
    }
    getter(reg, rt, "DocumentType", "name", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| {
            d.doctype(n).map(|t| t.name.to_string()).unwrap_or_default()
        }))
    });
    getter(reg, rt, "DocumentType", "publicId", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| {
            d.doctype(n)
                .map(|t| t.public_id.to_string())
                .unwrap_or_default()
        }))
    });
    getter(reg, rt, "DocumentType", "systemId", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| {
            d.doctype(n)
                .map(|t| t.system_id.to_string())
                .unwrap_or_default()
        }))
    });
}

fn register_element(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Element", "tagName", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.tag_name(n).unwrap_or_default()))
    });
    getter(reg, rt, "Element", "localName", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| {
            d.local_name(n).map(|l| l.to_string()).unwrap_or_default()
        }))
    });
    getter(reg, rt, "Element", "namespaceURI", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let ns = w.doc_slot(slot, |d| d.qual_name(n).map(|q| q.ns.to_string()));
        cx.ret(ns.filter(|s| !s.is_empty()).map_or(Arg::Null, Arg::String))
    });
    getter(reg, rt, "Element", "prefix", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let p = w.doc_slot(slot, |d| {
            d.qual_name(n)
                .and_then(|q| q.prefix.as_ref().map(|p| p.to_string()))
        });
        cx.ret(p.map_or(Arg::Null, Arg::String))
    });
    method(reg, rt, "Element", "getAttribute", |cx, w| {
        need_args(cx, 1, "getAttribute", "Element")?;
        let (slot, n) = this_elem(cx, w)?;
        let name = cx.arg_string(0)?;
        match w.doc_slot(slot, |d| d.get_attribute(n, &name).map(str::to_owned)) {
            Some(v) => cx.ret(v),
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "Element", "getAttributeNS", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let ns = Namespace::from(super::nullable_string(cx, 0)?.unwrap_or_default());
        let local = cx.arg_string(1)?;
        match w.doc_slot(slot, |d| {
            d.get_attribute_ns(n, &ns, &local).map(str::to_owned)
        }) {
            Some(v) => cx.ret(v),
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "Element", "setAttribute", |cx, w| {
        need_args(cx, 2, "setAttribute", "Element")?;
        let (slot, n) = this_elem(cx, w)?;
        let name = cx.arg_string(0)?;
        let value = cx.arg_string(1)?;
        let r = w.doc_slot_mut(slot, |d| d.set_attribute(n, &name, &value));
        w.after_mutation(cx);
        r.map_err(|_| {
            w.rt.err(
                "InvalidCharacterError",
                format!("Failed to execute 'setAttribute' on 'Element': '{name}' is not a valid attribute name."),
            )
        })
    });
    method(reg, rt, "Element", "setAttributeNS", |cx, w| {
        need_args(cx, 3, "setAttributeNS", "Element")?;
        let (slot, n) = this_elem(cx, w)?;
        let ns = Namespace::from(super::nullable_string(cx, 0)?.unwrap_or_default());
        let name = cx.arg_string(1)?;
        let value = cx.arg_string(2)?;
        let r = w.doc_slot_mut(slot, |d| d.set_attribute_ns(n, ns, &name, &value));
        w.after_mutation(cx);
        r.map_err(|e| dom_err(w, "setAttributeNS", "Element", e))
    });
    method(reg, rt, "Element", "removeAttribute", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let name = cx.arg_string(0)?;
        w.doc_slot_mut(slot, |d| d.remove_attribute(n, &name));
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "Element", "removeAttributeNS", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let ns = Namespace::from(super::nullable_string(cx, 0)?.unwrap_or_default());
        let local = cx.arg_string(1)?;
        w.doc_slot_mut(slot, |d| d.remove_attribute_ns(n, &ns, &local));
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "Element", "hasAttribute", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let name = cx.arg_string(0)?;
        cx.ret(w.doc_slot(slot, |d| d.has_attribute(n, &name)))
    });
    method(reg, rt, "Element", "hasAttributeNS", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let ns = Namespace::from(super::nullable_string(cx, 0)?.unwrap_or_default());
        let local = cx.arg_string(1)?;
        cx.ret(w.doc_slot(slot, |d| d.get_attribute_ns(n, &ns, &local).is_some()))
    });
    method(reg, rt, "Element", "hasAttributes", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| !d.attributes(n).is_empty()))
    });
    method(reg, rt, "Element", "getAttributeNames", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let names: Vec<Arg> = w.doc_slot(slot, |d| {
            d.attributes(n)
                .iter()
                .map(|a| {
                    Arg::String(match &a.name.prefix {
                        Some(p) => format!("{}:{}", &**p, &*a.name.local),
                        None => a.name.local.to_string(),
                    })
                })
                .collect()
        });
        cx.ret(Arg::Array(names))
    });
    method(reg, rt, "Element", "toggleAttribute", |cx, w| {
        need_args(cx, 1, "toggleAttribute", "Element")?;
        let (slot, n) = this_elem(cx, w)?;
        let name = cx.arg_string(0)?;
        let force =
            (cx.len() > 1 && cx.arg_kind(1) != ValueKind::Undefined).then(|| cx.arg_bool(1));
        let r = w.doc_slot_mut(slot, |d| {
            let has = d.has_attribute(n, &name);
            match (has, force) {
                (true, Some(true)) => Ok(true),
                (true, _) => {
                    d.remove_attribute(n, &name);
                    Ok(false)
                }
                (false, Some(false)) => Ok(false),
                (false, _) => d.set_attribute(n, &name, "").map(|_| true),
            }
        });
        w.after_mutation(cx);
        match r {
            Ok(b) => cx.ret(b),
            Err(e) => Err(dom_err(w, "toggleAttribute", "Element", e)),
        }
    });
    method(reg, rt, "Element", "getAttributeNode", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let name = cx.arg_string(0)?;
        let q = w.doc_slot(slot, |d| {
            let lname = if d.is_html() {
                name.to_ascii_lowercase()
            } else {
                name.clone()
            };
            d.attributes(n)
                .iter()
                .find(|a| a.name.local.as_ref() == lname && a.name.prefix.is_none())
                .map(|a| a.name.clone())
        });
        match q {
            Some(q) => {
                let id = super::coll::attr_obj(w, slot, n, q);
                cx.ret(w.obj_arg(id))
            }
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "Element", "matches", |cx, w| {
        need_args(cx, 1, "matches", "Element")?;
        let (slot, n) = this_elem(cx, w)?;
        let sel = cx.arg_string(0)?;
        match w.doc_slot(slot, |d| d.matches(n, &sel)) {
            Ok(b) => cx.ret(b),
            Err(_) => Err(selector_err(w, "matches", "Element", &sel)),
        }
    });
    method(reg, rt, "Element", "webkitMatchesSelector", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let sel = cx.arg_string(0)?;
        match w.doc_slot(slot, |d| d.matches(n, &sel)) {
            Ok(b) => cx.ret(b),
            Err(_) => Err(selector_err(w, "webkitMatchesSelector", "Element", &sel)),
        }
    });
    method(reg, rt, "Element", "closest", |cx, w| {
        need_args(cx, 1, "closest", "Element")?;
        let (slot, n) = this_elem(cx, w)?;
        let sel = cx.arg_string(0)?;
        match w.doc_slot(slot, |d| d.closest(n, &sel)) {
            Ok(r) => ret_node(cx, w, slot, r),
            Err(_) => Err(selector_err(w, "closest", "Element", &sel)),
        }
    });
    getter(reg, rt, "Element", "innerHTML", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.inner_html(n)))
    });
    setter(reg, rt, "Element", "innerHTML", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let html = if cx.arg_kind(0) == ValueKind::Null {
            String::new()
        } else {
            cx.arg_string(0)?
        };
        w.doc_slot_mut(slot, |d| d.set_inner_html(n, &html));
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "Element", "outerHTML", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.outer_html(n)))
    });
    setter(reg, rt, "Element", "outerHTML", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let html = if cx.arg_kind(0) == ValueKind::Null {
            String::new()
        } else {
            cx.arg_string(0)?
        };
        let r = w.doc_slot_mut(slot, |d| d.set_outer_html(n, &html));
        w.after_mutation(cx);
        r.map_err(|e| dom_err(w, "outerHTML", "Element", e))
    });
    for iface in ["DocumentFragment", "Document"] {
        // `innerHTML` only exists on ShadowRoot/Element; nothing here.
        let _ = iface;
    }
    method(reg, rt, "Element", "insertAdjacentHTML", |cx, w| {
        need_args(cx, 2, "insertAdjacentHTML", "Element")?;
        let (slot, n) = this_elem(cx, w)?;
        let pos = cx.arg_string(0)?.to_ascii_lowercase();
        let html = cx.arg_string(1)?;
        let r = w.doc_slot_mut(slot, |d| {
            let parent = d.parent(n);
            let (target, before) = match pos.as_str() {
                "beforebegin" => (parent, Some(n)),
                "afterbegin" => (Some(n), d.first_child(n)),
                "beforeend" => (Some(n), None),
                "afterend" => (parent, d.next_sibling(n)),
                _ => return Err(None),
            };
            let Some(target) = target else {
                return Err(Some(DomError::NoModificationAllowed));
            };
            if d.node_type(target) == NodeType::Document {
                return Err(Some(DomError::NoModificationAllowed));
            }
            let context = if matches!(pos.as_str(), "beforebegin" | "afterend") {
                target
            } else {
                n
            };
            let frag = d.parse_fragment(context, &html);
            d.insert_before(target, frag, before)
                .map(|_| ())
                .map_err(Some)
        });
        w.after_mutation(cx);
        match r {
            Ok(()) => Ok(()),
            Err(None) => Err(w.rt.err(
                "SyntaxError",
                format!("Failed to execute 'insertAdjacentHTML' on 'Element': The value provided ('{pos}') is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'."),
            )),
            Err(Some(e)) => Err(dom_err(w, "insertAdjacentHTML", "Element", e)),
        }
    });
    for (name, is_text) in [
        ("insertAdjacentElement", false),
        ("insertAdjacentText", true),
    ] {
        method(reg, rt, "Element", name, move |cx, w| {
            need_args(cx, 2, name, "Element")?;
            let (slot, n) = this_elem(cx, w)?;
            let pos = cx.arg_string(0)?.to_ascii_lowercase();
            let node = if is_text {
                let t = cx.arg_string(1)?;
                w.doc_slot_mut(slot, |d| d.create_text(&t))
            } else {
                let (s2, e) = arg_node(cx, w, 1, name, "Element")?;
                bring(cx, w, s2, e, slot)
            };
            let r = w.doc_slot_mut(slot, |d| {
                let parent = d.parent(n);
                let (target, before) = match pos.as_str() {
                    "beforebegin" => (parent, Some(n)),
                    "afterbegin" => (Some(n), d.first_child(n)),
                    "beforeend" => (Some(n), None),
                    "afterend" => (parent, d.next_sibling(n)),
                    _ => return Err(None),
                };
                match target {
                    Some(t) => d.insert_before(t, node, before).map(Some).map_err(Some),
                    None => Ok(None),
                }
            });
            w.after_mutation(cx);
            match r {
                Ok(Some(x)) if !is_text => ret_node(cx, w, slot, Some(x)),
                Ok(_) => cx.ret(if is_text { Arg::Undefined } else { Arg::Null }),
                Err(None) => Err(w.rt.err("SyntaxError", format!("Failed to execute '{name}' on 'Element': The value provided ('{pos}') is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'."))),
                Err(Some(e)) => Err(dom_err(w, name, "Element", e)),
            }
        });
    }
    getter(reg, rt, "Element", "classList", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let id = w.cached_obj((slot, n.0, "classList", String::new()), || Obj::TokenList {
            slot,
            node: n,
            attr: "class",
        });
        cx.ret(w.obj_arg(id))
    });
    setter(reg, rt, "Element", "classList", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_string(0)?;
        w.doc_slot_mut(slot, |d| d.set_attribute(n, "class", &v))
            .ok();
        w.after_mutation(cx);
        Ok(())
    });
    for (iface, attr) in [
        ("HTMLAnchorElement", "rel"),
        ("HTMLLinkElement", "rel"),
        ("HTMLAreaElement", "rel"),
        ("HTMLIFrameElement", "sandbox"),
        ("HTMLOutputElement", "for"),
        ("HTMLLinkElement", "sizes"),
    ] {
        let prop: &'static str = match (iface, attr) {
            (_, "rel") => "relList",
            (_, "for") => "htmlFor",
            (_, a) => a,
        };
        getter(reg, rt, iface, prop, move |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            let id = w.cached_obj((slot, n.0, attr, String::new()), || Obj::TokenList {
                slot,
                node: n,
                attr,
            });
            cx.ret(w.obj_arg(id))
        });
    }
    getter(reg, rt, "Element", "attributes", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let id = w.cached_obj((slot, n.0, "attributes", String::new()), || {
            Obj::NamedNodeMap { slot, node: n }
        });
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "HTMLElement", "dataset", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let id = w.cached_obj((slot, n.0, "dataset", String::new()), || Obj::StringMap {
            slot,
            node: n,
        });
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "SVGElement", "dataset", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let id = w.cached_obj((slot, n.0, "dataset", String::new()), || Obj::StringMap {
            slot,
            node: n,
        });
        cx.ret(w.obj_arg(id))
    });
}

fn register_document(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Document", "documentElement", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let r = w.doc_slot(slot, |d| d.document_element());
        ret_node(cx, w, slot, r)
    });
    getter(reg, rt, "Document", "head", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let r = w.doc_slot(slot, |d| d.head());
        ret_node(cx, w, slot, r)
    });
    getter(reg, rt, "Document", "body", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let r = w.doc_slot(slot, |d| d.body());
        ret_node(cx, w, slot, r)
    });
    setter(reg, rt, "Document", "body", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let (s2, b) = arg_node(cx, w, 0, "body", "Document")?;
        let b = bring(cx, w, s2, b, slot);
        let r = w.doc_slot_mut(slot, |d| {
            let Some(html) = d.document_element() else {
                return Err(DomError::HierarchyRequest);
            };
            match d.body() {
                Some(old) => d.replace_child(html, b, old).map(|_| ()),
                None => d.append_child(html, b).map(|_| ()),
            }
        });
        w.after_mutation(cx);
        r.map_err(|e| dom_err(w, "body", "Document", e))
    });
    getter(reg, rt, "Document", "title", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.title()))
    });
    setter(reg, rt, "Document", "title", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let v = cx.arg_string(0)?;
        w.doc_slot_mut(slot, |d| {
            let title = d
                .descendants(Document::DOCUMENT)
                .find(|&n| d.is_html_element_named(n, &local_name!("title")));
            let title = match title {
                Some(t) => Some(t),
                None => d.head().map(|h| {
                    let t = d.create_element(
                        QualName::new(None, ns!(html), local_name!("title")),
                        Vec::new(),
                    );
                    let _ = d.append_child(h, t);
                    t
                }),
            };
            if let Some(t) = title {
                d.set_text_content(t, Some(&v));
            }
        });
        w.after_mutation(cx);
        Ok(())
    });
    for name in ["URL", "documentURI"] {
        getter(reg, rt, "Document", name, |cx, w| {
            let (slot, _) = this_node(cx, w)?;
            cx.ret(if slot == 0 {
                w.url().to_string()
            } else {
                "about:blank".to_owned()
            })
        });
    }
    getter(reg, rt, "Document", "readyState", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        cx.ret(if slot == 0 {
            w.ready_state().as_str()
        } else {
            "complete"
        })
    });
    for name in ["characterSet", "charset", "inputEncoding"] {
        getter(reg, rt, "Document", name, |cx, w| {
            let (slot, _) = this_node(cx, w)?;
            cx.ret(if slot == 0 { w.charset.get() } else { "UTF-8" })
        });
    }
    getter(reg, rt, "Document", "contentType", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        cx.ret(if slot == 0 {
            w.content_type.clone()
        } else if w.doc_slot(slot, |d| d.is_html()) {
            "text/html".into()
        } else {
            "application/xml".into()
        })
    });
    getter(reg, rt, "Document", "compatMode", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let q = w.doc_slot(slot, |d| d.quirks_mode());
        cx.ret(if q == lb_dom::QuirksMode::Quirks {
            "BackCompat"
        } else {
            "CSS1Compat"
        })
    });
    getter(reg, rt, "Document", "doctype", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let r = w.doc_slot(slot, |d| d.doctype_node());
        ret_node(cx, w, slot, r)
    });
    getter(reg, rt, "Document", "defaultView", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        // A removed frame's document has no browsing context (null).
        if slot == 0 && !super::frames::is_detached(w) {
            cx.ret(w.host_arg(super::WINDOW_REF))
        } else {
            cx.ret(Arg::Null)
        }
    });
    getter(reg, rt, "Document", "referrer", |cx, w| {
        this_node(cx, w)?;
        cx.ret(w.referrer.clone())
    });
    getter(reg, rt, "Document", "domain", |cx, w| {
        this_node(cx, w)?;
        cx.ret(w.url().host_str().unwrap_or("").to_owned())
    });
    getter(reg, rt, "Document", "lastModified", |cx, w| {
        this_node(cx, w)?;
        cx.ret(super::window::last_modified_now())
    });
    getter(reg, rt, "Document", "visibilityState", |cx, w| {
        this_node(cx, w)?;
        cx.ret(if w.hidden.get() { "hidden" } else { "visible" })
    });
    getter(reg, rt, "Document", "webkitVisibilityState", |cx, w| {
        this_node(cx, w)?;
        cx.ret(if w.hidden.get() { "hidden" } else { "visible" })
    });
    getter(reg, rt, "Document", "hidden", |cx, w| {
        this_node(cx, w)?;
        cx.ret(w.hidden.get())
    });
    getter(reg, rt, "Document", "webkitHidden", |cx, w| {
        this_node(cx, w)?;
        cx.ret(w.hidden.get())
    });
    method(reg, rt, "Document", "hasFocus", |cx, w| {
        this_node(cx, w)?;
        cx.ret(!super::frames::is_detached(w))
    });
    getter(reg, rt, "Document", "activeElement", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let f = if slot == 0 {
            super::shadow::active_element_in(w, Document::DOCUMENT)
        } else {
            None
        };
        let r = f.or_else(|| w.doc_slot(slot, |d| d.body()));
        ret_node(cx, w, slot, r)
    });
    getter(reg, rt, "Document", "currentScript", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        // Scripts in shadow trees are not exposed.
        let r = if slot == 0 {
            w.current_script
                .get()
                .filter(|&n| w.with_doc(|d| !d.is_shadow_root(d.root(n))))
        } else {
            None
        };
        ret_node(cx, w, slot, r)
    });
    getter(reg, rt, "Document", "cookie", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        if slot != 0 {
            return cx.ret("");
        }
        cx.ret(w.net.cookie_string(&w.url()))
    });
    setter(reg, rt, "Document", "cookie", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let v = cx.arg_string(0)?;
        if slot == 0 {
            w.net.set_cookie_string(&w.url(), &v);
        }
        Ok(())
    });
    method(reg, rt, "Document", "createElement", |cx, w| {
        need_args(cx, 1, "createElement", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let name = cx.arg_string(0)?;
        let is = element_creation_is(cx, 1)?;
        match w.doc_slot_mut(slot, |d| d.create_element_by_name(&name)) {
            Ok(n) => {
                let n = custom_create(cx, w, slot, n, is.as_deref());
                ret_node(cx, w, slot, Some(n))
            }
            Err(_) => Err(w.rt.err(
                "InvalidCharacterError",
                format!("Failed to execute 'createElement' on 'Document': The tag name provided ('{name}') is not a valid name."),
            )),
        }
    });
    method(reg, rt, "Document", "createElementNS", |cx, w| {
        need_args(cx, 2, "createElementNS", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let ns = Namespace::from(super::nullable_string(cx, 0)?.unwrap_or_default());
        let name = cx.arg_string(1)?;
        let is = element_creation_is(cx, 2)?;
        match w.doc_slot_mut(slot, |d| d.create_element_ns(ns, &name)) {
            Ok(n) => {
                let n = custom_create(cx, w, slot, n, is.as_deref());
                ret_node(cx, w, slot, Some(n))
            }
            Err(e) => Err(dom_err(w, "createElementNS", "Document", e)),
        }
    });
    method(reg, rt, "Document", "createTextNode", |cx, w| {
        need_args(cx, 1, "createTextNode", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let t = cx.arg_string(0)?;
        let n = w.doc_slot_mut(slot, |d| d.create_text(&t));
        ret_node(cx, w, slot, Some(n))
    });
    method(reg, rt, "Document", "createComment", |cx, w| {
        need_args(cx, 1, "createComment", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let t = cx.arg_string(0)?;
        let n = w.doc_slot_mut(slot, |d| d.create_comment(&t));
        ret_node(cx, w, slot, Some(n))
    });
    method(reg, rt, "Document", "createDocumentFragment", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let n = w.doc_slot_mut(slot, |d| d.create_document_fragment());
        ret_node(cx, w, slot, Some(n))
    });
    method(
        reg,
        rt,
        "Document",
        "createProcessingInstruction",
        |cx, w| {
            need_args(cx, 2, "createProcessingInstruction", "Document")?;
            let (slot, _) = this_node(cx, w)?;
            let t = cx.arg_string(0)?;
            let data = cx.arg_string(1)?;
            let n = w.doc_slot_mut(slot, |d| d.create_processing_instruction(&t, &data));
            ret_node(cx, w, slot, Some(n))
        },
    );
    method(reg, rt, "Document", "createAttribute", |cx, w| {
        need_args(cx, 1, "createAttribute", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let name = cx.arg_string(0)?;
        let local = if w.doc_slot(slot, |d| d.is_html()) {
            name.to_ascii_lowercase()
        } else {
            name
        };
        let id = w.new_obj(Obj::Attr {
            slot,
            owner: None,
            name: QualName::new(None, ns!(), LocalName::from(local)),
            value: String::new(),
        });
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "Document", "createEvent", |cx, w| {
        need_args(cx, 1, "createEvent", "Document")?;
        this_node(cx, w)?;
        let kind = cx.arg_string(0)?;
        match super::events::create_event(w, &kind) {
            Some(id) => cx.ret(w.obj_arg(id)),
            None => Err(w.rt.err(
                "NotSupportedError",
                format!("Failed to execute 'createEvent' on 'Document': The provided event type ('{kind}') is invalid."),
            )),
        }
    });
    method(reg, rt, "Document", "getElementsByName", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let name = cx.arg_string(0)?;
        let id = super::coll::collection_nodelist(w, slot, n, super::coll::Filter::Name(name));
        cx.ret(w.obj_arg(id))
    });
    for (prop, f) in [
        ("forms", super::coll::Filter::Local("form")),
        ("images", super::coll::Filter::Local("img")),
        ("scripts", super::coll::Filter::Local("script")),
        ("embeds", super::coll::Filter::Local("embed")),
        ("plugins", super::coll::Filter::Local("embed")),
        ("links", super::coll::Filter::Links),
        ("anchors", super::coll::Filter::Anchors),
        ("all", super::coll::Filter::All),
    ] {
        getter(reg, rt, "Document", prop, move |cx, w| {
            let (slot, n) = this_node(cx, w)?;
            let id = super::coll::collection(w, slot, n, f.clone());
            cx.ret(w.obj_arg(id))
        });
    }
    method(reg, rt, "Document", "importNode", |cx, w| {
        need_args(cx, 1, "importNode", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let (s2, n) = arg_node(cx, w, 0, "importNode", "Document")?;
        if w.doc_slot(s2, |d| d.is_shadow_root(n)) {
            return Err(w.rt.err(
                "NotSupportedError",
                "Failed to execute 'importNode' on 'Document': The node provided is a shadow root, which may not be imported.",
            ));
        }
        let deep = cx.arg_bool(1);
        let (src, dst) = (w.doc_rc(s2), w.doc_rc(slot));
        let c = if Rc::ptr_eq(&src, &dst) {
            dst.borrow_mut().clone_node(n, deep)
        } else {
            dst.borrow_mut().import_node(&src.borrow(), n, deep)
        };
        w.after_mutation(cx);
        ret_node(cx, w, slot, Some(c))
    });
    method(reg, rt, "Document", "adoptNode", |cx, w| {
        need_args(cx, 1, "adoptNode", "Document")?;
        let (slot, _) = this_node(cx, w)?;
        let (s2, n) = arg_node(cx, w, 0, "adoptNode", "Document")?;
        if n == Document::DOCUMENT {
            return Err(w.rt.err("NotSupportedError", "Failed to execute 'adoptNode' on 'Document': The node provided is of type '#document', which may not be adopted."));
        }
        if w.doc_slot(s2, |d| d.is_shadow_root(n)) {
            return Err(w.rt.err(
                "HierarchyRequestError",
                "Failed to execute 'adoptNode' on 'Document': The node provided is a shadow root, which may not be adopted.",
            ));
        }
        let c = if s2 == slot {
            w.doc_slot_mut(slot, |d| d.detach(n));
            n
        } else {
            bring(cx, w, s2, n, slot)
        };
        w.after_mutation(cx);
        ret_node(cx, w, slot, Some(c))
    });
    for (name, newline) in [("write", false), ("writeln", true)] {
        method(reg, rt, "Document", name, move |cx, w| {
            let (slot, _) = this_node(cx, w)?;
            let mut text = String::new();
            for i in 0..cx.len() {
                text.push_str(&cx.arg_string(i)?);
            }
            if newline {
                text.push('\n');
            }
            if slot != 0 {
                return Ok(());
            }
            super::script::document_write(cx, w, &text);
            Ok(())
        });
    }
    method(reg, rt, "Document", "open", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        if slot == 0 && w.parser.borrow().is_none() {
            super::script::document_open(w);
            w.after_mutation(cx);
        }
        cx.ret(w.node_arg(slot, Document::DOCUMENT))
    });
    method(reg, rt, "Document", "close", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        if slot == 0 {
            super::script::document_close(cx, w);
        }
        Ok(())
    });
    getter(reg, rt, "Document", "implementation", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let id = w.cached_obj((slot, 0, "implementation", String::new()), || {
            Obj::DomImplementation { slot }
        });
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "Document", "location", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        if slot != 0 {
            return cx.ret(Arg::Null);
        }
        let id = w.cached_obj((0, 0, "location", String::new()), || Obj::Location);
        cx.ret(w.obj_arg(id))
    });
    setter(reg, rt, "Document", "location", |cx, w| {
        let (slot, _) = this_node(cx, w)?;
        let v = cx.arg_string(0)?;
        if slot == 0
            && let Some(u) = w.resolve_url(&v)
        {
            w.request_navigation(super::NavRequest::get(u, false));
        }
        Ok(())
    });
    method(reg, rt, "Document", "getSelection", |cx, w| {
        this_node(cx, w)?;
        cx.ret(Arg::Null)
    });
    method(reg, rt, "Document", "execCommand", |cx, w| {
        this_node(cx, w)?;
        cx.ret(false)
    });
    ctor(reg, rt, "Document", |cx, w| {
        let mut d = Document::with_kind(false);
        d.set_quirks_mode(lb_dom::QuirksMode::NoQuirks);
        let slot = w.add_doc(d);
        cx.init_this(w.node_ref(slot, Document::DOCUMENT))
    });
    method(
        reg,
        rt,
        "DOMImplementation",
        "createHTMLDocument",
        |cx, w| {
            let title = opt_string(cx, 0)?;
            let mut html = String::from("<!DOCTYPE html><html><head>");
            if let Some(t) = &title {
                html.push_str("<title></title>");
                let _ = t;
            }
            html.push_str("</head><body></body></html>");
            let mut d = Document::parse_html(&html);
            if let Some(t) = title {
                let title_el = d
                    .descendants(Document::DOCUMENT)
                    .find(|&n| d.is_html_element_named(n, &local_name!("title")));
                if let Some(te) = title_el {
                    d.set_text_content(te, Some(&t));
                }
            }
            let slot = w.add_doc(d);
            cx.ret(w.node_arg(slot, Document::DOCUMENT))
        },
    );
    method(reg, rt, "DOMImplementation", "createDocument", |cx, w| {
        let ns = opt_string(cx, 0)?;
        let qname = if cx.arg_kind(1) == ValueKind::Null {
            String::new()
        } else {
            cx.arg_string(1)?
        };
        let mut d = Document::with_kind(false);
        if !qname.is_empty() {
            match d.create_element_ns(Namespace::from(ns.unwrap_or_default()), &qname) {
                Ok(e) => {
                    let _ = d.append_child(Document::DOCUMENT, e);
                }
                Err(e) => return Err(dom_err(w, "createDocument", "DOMImplementation", e)),
            }
        }
        let slot = w.add_doc(d);
        cx.ret(w.node_arg(slot, Document::DOCUMENT))
    });
    method(
        reg,
        rt,
        "DOMImplementation",
        "createDocumentType",
        |cx, w| {
            need_args(cx, 3, "createDocumentType", "DOMImplementation")?;
            let id = this_obj_slot(cx, w)?;
            let (name, p, s) = (cx.arg_string(0)?, cx.arg_string(1)?, cx.arg_string(2)?);
            let n = w.doc_slot_mut(id, |d| d.create_doctype(&name, &p, &s));
            ret_node(cx, w, id, Some(n))
        },
    );
    method(reg, rt, "DOMImplementation", "hasFeature", |cx, _w| {
        cx.ret(true)
    });
}

fn this_obj_slot(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = super::this_obj(cx)?;
    w.with_obj(id, |o| match o {
        Obj::DomImplementation { slot } => Some(*slot),
        _ => None,
    })
    .flatten()
    .ok_or_else(illegal)
}

fn register_chardata(reg: &mut Registry, rt: &Runtime) {
    fn data(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId, String), HostError> {
        let (slot, n) = this_node(cx, w)?;
        let s = w
            .doc_slot(slot, |d| d.character_data(n).map(str::to_owned))
            .ok_or_else(illegal)?;
        Ok((slot, n, s))
    }
    fn index_err(w: &Window, op: &str, off: usize, len: usize) -> HostError {
        w.rt.err(
            "IndexSizeError",
            format!("Failed to execute '{op}' on 'CharacterData': The offset {off} is greater than the node's length ({len})."),
        )
    }
    getter(reg, rt, "CharacterData", "data", |cx, w| {
        let (_, _, s) = data(cx, w)?;
        cx.ret(s)
    });
    setter(reg, rt, "CharacterData", "data", |cx, w| {
        let (slot, n, _) = data(cx, w)?;
        let v = if cx.arg_kind(0) == ValueKind::Null {
            String::new()
        } else {
            cx.arg_string(0)?
        };
        w.doc_slot_mut(slot, |d| d.set_character_data(n, &v));
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "CharacterData", "length", |cx, w| {
        let (_, _, s) = data(cx, w)?;
        cx.ret(utf16_len(&s) as u32)
    });
    method(reg, rt, "CharacterData", "substringData", |cx, w| {
        need_args(cx, 2, "substringData", "CharacterData")?;
        let (_, _, s) = data(cx, w)?;
        let (off, count) = (cx.arg_u32(0)? as usize, cx.arg_u32(1)? as usize);
        let len = utf16_len(&s);
        if off > len {
            return Err(index_err(w, "substringData", off, len));
        }
        cx.ret(utf16_sub(&s, off, count))
    });
    method(reg, rt, "CharacterData", "appendData", |cx, w| {
        need_args(cx, 1, "appendData", "CharacterData")?;
        let (slot, n, mut s) = data(cx, w)?;
        s.push_str(&cx.arg_string(0)?);
        w.doc_slot_mut(slot, |d| d.set_character_data(n, &s));
        w.after_mutation(cx);
        Ok(())
    });
    for (name, nargs) in [("insertData", 2), ("deleteData", 2), ("replaceData", 3)] {
        method(reg, rt, "CharacterData", name, move |cx, w| {
            need_args(cx, nargs, name, "CharacterData")?;
            let (slot, n, s) = data(cx, w)?;
            let off = cx.arg_u32(0)? as usize;
            let len = utf16_len(&s);
            if off > len {
                return Err(index_err(w, name, off, len));
            }
            let new = match name {
                "insertData" => utf16_splice(&s, off, 0, &cx.arg_string(1)?),
                "deleteData" => utf16_splice(&s, off, cx.arg_u32(1)? as usize, ""),
                _ => {
                    let count = cx.arg_u32(1)? as usize;
                    utf16_splice(&s, off, count, &cx.arg_string(2)?)
                }
            };
            w.doc_slot_mut(slot, |d| d.set_character_data(n, &new));
            w.after_mutation(cx);
            Ok(())
        });
    }
    method(reg, rt, "Text", "splitText", |cx, w| {
        need_args(cx, 1, "splitText", "Text")?;
        let (slot, n, s) = data(cx, w)?;
        let off = cx.arg_u32(0)? as usize;
        let len = utf16_len(&s);
        if off > len {
            return Err(w.rt.err("IndexSizeError", format!("Failed to execute 'splitText' on 'Text': The offset {off} is larger than the Text node's length.")));
        }
        let tail = utf16_sub(&s, off, len - off);
        let head = utf16_sub(&s, 0, off);
        let new = w.doc_slot_mut(slot, |d| {
            let t = d.create_text(&tail);
            if let Some(p) = d.parent(n) {
                let next = d.next_sibling(n);
                let _ = d.insert_before(p, t, next);
            }
            d.set_character_data(n, &head);
            t
        });
        w.after_mutation(cx);
        ret_node(cx, w, slot, Some(new))
    });
    getter(reg, rt, "Text", "wholeText", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        let s = w.doc_slot(slot, |d| {
            let mut first = n;
            while let Some(p) = d
                .prev_sibling(first)
                .filter(|&p| d.node_type(p) == NodeType::Text)
            {
                first = p;
            }
            let mut out = String::new();
            let mut cur = Some(first);
            while let Some(c) = cur.filter(|&c| d.node_type(c) == NodeType::Text) {
                out.push_str(d.character_data(c).unwrap_or(""));
                cur = d.next_sibling(c);
            }
            out
        });
        cx.ret(s)
    });
    for (iface, kind) in [("Text", 0), ("Comment", 1)] {
        ctor(reg, rt, iface, move |cx, w| {
            let t = opt_string(cx, 0)?.unwrap_or_default();
            let n = w.doc_slot_mut(0, |d| {
                if kind == 0 {
                    d.create_text(&t)
                } else {
                    d.create_comment(&t)
                }
            });
            cx.init_this(w.node_ref(0, n))
        });
    }
    ctor(reg, rt, "DocumentFragment", |cx, w| {
        let n = w.doc_slot_mut(0, |d| d.create_document_fragment());
        cx.init_this(w.node_ref(0, n))
    });
    getter(reg, rt, "ProcessingInstruction", "target", |cx, w| {
        let (slot, n) = this_node(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| {
            d.processing_instruction(n)
                .map(|p| p.target.to_string())
                .unwrap_or_default()
        }))
    });
}

/// Reflected content attributes ([`REFLECTED`]).
fn register_reflected(reg: &mut Registry, rt: &Runtime) {
    for &(iface, prop, attr, kind) in REFLECTED {
        getter(reg, rt, iface, prop, move |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            let v = w.doc_slot(slot, |d| d.get_attribute(n, attr).map(str::to_owned));
            match kind {
                Reflect::Str => cx.ret(v.unwrap_or_default()),
                Reflect::Bool => cx.ret(v.is_some()),
                Reflect::Url => match v {
                    Some(v) => cx.ret(w.resolve_url(&v).map_or(v, |u| u.to_string())),
                    None => cx.ret(""),
                },
                Reflect::Long(def) => cx.ret(v.and_then(|s| parse_int(&s)).unwrap_or(def)),
                Reflect::ULong(def) => cx.ret(
                    v.and_then(|s| parse_int(&s))
                        .filter(|&x| x >= 0)
                        .map_or(def, |x| x as u32),
                ),
                Reflect::Enum(def, values) => {
                    let s = v.map(|s| s.to_ascii_lowercase());
                    cx.ret(match s {
                        Some(s) if values.contains(&s.as_str()) => s,
                        _ => def.to_owned(),
                    })
                }
            }
        });
        setter(reg, rt, iface, prop, move |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            match kind {
                Reflect::Bool => {
                    let on = cx.arg_bool(0);
                    w.doc_slot_mut(slot, |d| {
                        if on {
                            let _ = d.set_attribute(n, attr, "");
                        } else {
                            d.remove_attribute(n, attr);
                        }
                    });
                }
                Reflect::Long(_) => {
                    let v = cx.arg_i32(0)?;
                    let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, attr, &v.to_string()));
                }
                Reflect::ULong(_) => {
                    let v = cx.arg_u32(0)?;
                    let v = if v > i32::MAX as u32 { 0 } else { v };
                    let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, attr, &v.to_string()));
                }
                _ => {
                    let v = if cx.arg_kind(0) == ValueKind::Null && kind == Reflect::Str {
                        "null".to_owned()
                    } else {
                        cx.arg_string(0)?
                    };
                    let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, attr, &v));
                }
            }
            w.after_mutation(cx);
            Ok(())
        });
    }
}

/// HTML "rules for parsing integers".
pub(crate) fn parse_int(s: &str) -> Option<i32> {
    let s = s.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let (neg, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let v: i64 = digits.parse().ok()?;
    let v = if neg { -v } else { v };
    i32::try_from(v).ok()
}

fn register_html(reg: &mut Registry, rt: &Runtime) {
    // innerText / outerText: textContent-based (no layout-aware rendering).
    getter(reg, rt, "HTMLElement", "innerText", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| super::window::inner_text(d, n)))
    });
    getter(reg, rt, "HTMLElement", "outerText", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| super::window::inner_text(d, n)))
    });
    for prop in ["innerText", "outerText"] {
        setter(reg, rt, "HTMLElement", prop, move |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            let v = cx.arg_string(0)?;
            w.doc_slot_mut(slot, |d| {
                d.remove_all_children(n);
                let br = QualName::new(None, ns!(html), local_name!("br"));
                for (i, line) in v.split('\n').enumerate() {
                    if i > 0 {
                        let b = d.create_element(br.clone(), Vec::new());
                        let _ = d.append_child(n, b);
                    }
                    if !line.is_empty() {
                        let t = d.create_text(line);
                        let _ = d.append_child(n, t);
                    }
                }
                if prop == "outerText"
                    && let Some(p) = d.parent(n)
                {
                    let kids: Vec<NodeId> = d.children(n).collect();
                    for k in kids {
                        let _ = d.insert_before(p, k, Some(n));
                    }
                    d.detach(n);
                }
            });
            w.after_mutation(cx);
            Ok(())
        });
    }
    getter(reg, rt, "HTMLElement", "tabIndex", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = w.doc_slot(slot, |d| {
            d.get_attribute(n, "tabindex")
                .and_then(parse_int)
                .unwrap_or_else(|| {
                    let l = d.local_name(n).map(|l| l.to_string()).unwrap_or_default();
                    let focusable = matches!(
                        l.as_str(),
                        "input" | "select" | "textarea" | "button" | "iframe"
                    ) || (l == "a" && d.has_attribute(n, "href"));
                    if focusable { 0 } else { -1 }
                })
        });
        cx.ret(v)
    });
    setter(reg, rt, "HTMLElement", "tabIndex", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_i32(0)?;
        let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, "tabindex", &v.to_string()));
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "HTMLElement", "click", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        super::window::synthetic_click(cx, w, slot, n);
        Ok(())
    });
    for iface in ["HTMLElement", "SVGElement"] {
        method(reg, rt, iface, "focus", |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            if slot == 0 {
                // A host that delegates focus passes it to the first
                // focusable element of its shadow tree.
                let target = w.with_doc(|d| super::input::focus_delegate(d, n));
                super::window::focus(cx, w, Some(target));
            }
            Ok(())
        });
        method(reg, rt, iface, "blur", |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            // blur() on a host whose shadow tree has the focus blurs it.
            let has_focus = w.focused.get().is_some_and(|f| {
                f == n
                    || w.with_doc(|d| {
                        d.shadow_root(n).is_some() && d.shadow_including_contains(n, f)
                    })
            });
            if slot == 0 && has_focus {
                super::window::focus(cx, w, None);
            }
            Ok(())
        });
    }

    // <input>
    getter(reg, rt, "HTMLInputElement", "type", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| input_type(d, n)))
    });
    setter(reg, rt, "HTMLInputElement", "type", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_string(0)?;
        let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, "type", &v));
        w.after_mutation(cx);
        Ok(())
    });
    for iface in ["HTMLInputElement", "HTMLTextAreaElement"] {
        getter(reg, rt, iface, "value", |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            if let Some(v) = w.values.borrow().get(&(slot, n)) {
                return cx.ret(v.clone());
            }
            let v = w.doc_slot(slot, |d| {
                if d.is_html_element_named(n, &local_name!("textarea")) {
                    d.text_content(n).unwrap_or_default()
                } else {
                    let t = input_type(d, n);
                    let v = d.get_attribute(n, "value").map(str::to_owned);
                    match t.as_str() {
                        "checkbox" | "radio" => v.unwrap_or_else(|| "on".into()),
                        _ => v.unwrap_or_default().replace(['\r', '\n'], ""),
                    }
                }
            });
            cx.ret(v)
        });
        setter(reg, rt, iface, "value", |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            let v = if cx.arg_kind(0) == ValueKind::Null {
                String::new()
            } else {
                cx.arg_string(0)?
            };
            w.values.borrow_mut().insert((slot, n), v);
            // A value set by script puts the caret at the end.
            w.selections.borrow_mut().remove(&(slot, n));
            Ok(())
        });
        getter(reg, rt, iface, "defaultValue", |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            cx.ret(w.doc_slot(slot, |d| {
                if d.is_html_element_named(n, &local_name!("textarea")) {
                    d.text_content(n).unwrap_or_default()
                } else {
                    d.get_attribute(n, "value").unwrap_or("").to_owned()
                }
            }))
        });
        getter(reg, rt, iface, "form", |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            let f = w.doc_slot(slot, |d| {
                d.ancestors(n)
                    .find(|&a| d.is_html_element_named(a, &local_name!("form")))
            });
            ret_node(cx, w, slot, f)
        });
    }
    getter(reg, rt, "HTMLTextAreaElement", "type", |cx, _w| {
        cx.ret("textarea")
    });
    getter(reg, rt, "HTMLInputElement", "checked", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| checked(d, n, "checked")))
    });
    setter(reg, rt, "HTMLInputElement", "checked", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_bool(0);
        w.doc_slot_mut(slot, |d| set_checked(d, n, v));
        Ok(())
    });
    getter(reg, rt, "HTMLInputElement", "valueAsNumber", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = w
            .values
            .borrow()
            .get(&(slot, n))
            .cloned()
            .or_else(|| w.doc_slot(slot, |d| d.get_attribute(n, "value").map(str::to_owned)));
        cx.ret(
            v.and_then(|s| s.trim().parse::<f64>().ok())
                .unwrap_or(f64::NAN),
        )
    });
    // <option> / <select>
    getter(reg, rt, "HTMLOptionElement", "value", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| {
            d.get_attribute(n, "value")
                .map(str::to_owned)
                .unwrap_or_else(|| option_text(d, n))
        }))
    });
    setter(reg, rt, "HTMLOptionElement", "value", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_string(0)?;
        let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, "value", &v));
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "HTMLOptionElement", "text", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| option_text(d, n)))
    });
    setter(reg, rt, "HTMLOptionElement", "text", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_string(0)?;
        w.doc_slot_mut(slot, |d| d.set_text_content(n, Some(&v)));
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "HTMLOptionElement", "selected", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = w.doc_slot(slot, |d| {
            if checked(d, n, "selected") {
                return true;
            }
            // The first option of a single select is selected by default.
            let sel = d
                .ancestors(n)
                .find(|&a| d.is_html_element_named(a, &local_name!("select")));
            sel.is_some_and(|s| {
                let opts = select_options(d, s);
                selected_index(d, s) >= 0 && opts.get(selected_index(d, s) as usize) == Some(&n)
            })
        });
        cx.ret(v)
    });
    setter(reg, rt, "HTMLOptionElement", "selected", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_bool(0);
        w.doc_slot_mut(slot, |d| {
            let sel = d
                .ancestors(n)
                .find(|&a| d.is_html_element_named(a, &local_name!("select")));
            if v && let Some(s) = sel
                && !d.has_attribute(s, "multiple")
            {
                for o in select_options(d, s) {
                    set_checked(d, o, false);
                }
            }
            set_checked(d, n, v);
        });
        Ok(())
    });
    getter(reg, rt, "HTMLOptionElement", "index", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = w.doc_slot(slot, |d| {
            d.ancestors(n)
                .find(|&a| d.is_html_element_named(a, &local_name!("select")))
                .and_then(|s| select_options(d, s).iter().position(|&o| o == n))
                .unwrap_or(0) as u32
        });
        cx.ret(v)
    });
    getter(reg, rt, "HTMLSelectElement", "selectedIndex", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| selected_index(d, n)))
    });
    setter(reg, rt, "HTMLSelectElement", "selectedIndex", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let i = cx.arg_i32(0)?;
        w.doc_slot_mut(slot, |d| {
            for (k, o) in select_options(d, n).into_iter().enumerate() {
                set_checked(d, o, k as i32 == i);
            }
        });
        Ok(())
    });
    getter(reg, rt, "HTMLSelectElement", "value", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = w.doc_slot(slot, |d| {
            let i = selected_index(d, n);
            select_options(d, n)
                .get(i.max(0) as usize)
                .filter(|_| i >= 0)
                .map(|&o| {
                    d.get_attribute(o, "value")
                        .map(str::to_owned)
                        .unwrap_or_else(|| option_text(d, o))
                })
                .unwrap_or_default()
        });
        cx.ret(v)
    });
    setter(reg, rt, "HTMLSelectElement", "value", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_string(0)?;
        w.doc_slot_mut(slot, |d| {
            let mut found = false;
            for o in select_options(d, n) {
                let ov = d
                    .get_attribute(o, "value")
                    .map(str::to_owned)
                    .unwrap_or_else(|| option_text(d, o));
                let hit = !found && ov == v;
                found |= hit;
                set_checked(d, o, hit);
            }
        });
        Ok(())
    });
    getter(reg, rt, "HTMLSelectElement", "type", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(if w.doc_slot(slot, |d| d.has_attribute(n, "multiple")) {
            "select-multiple"
        } else {
            "select-one"
        })
    });
    getter(reg, rt, "HTMLSelectElement", "length", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| select_options(d, n).len() as u32))
    });
    getter(reg, rt, "HTMLSelectElement", "options", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let id = super::coll::collection(w, slot, n, super::coll::Filter::Local("option"));
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "HTMLFormElement", "elements", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let id = super::coll::collection(w, slot, n, super::coll::Filter::FormControls);
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "HTMLFormElement", "length", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let id = super::coll::collection(w, slot, n, super::coll::Filter::FormControls);
        cx.ret(super::coll::collection_len(w, id) as u32)
    });
    getter(reg, rt, "HTMLFormElement", "action", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let a = w.doc_slot(slot, |d| d.get_attribute(n, "action").map(str::to_owned));
        cx.ret(match a.filter(|s| !s.is_empty()) {
            Some(a) => w.resolve_url(&a).map_or(a, |u| u.to_string()),
            None => w.url().to_string(),
        })
    });
    setter(reg, rt, "HTMLFormElement", "action", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_string(0)?;
        let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, "action", &v));
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "HTMLFormElement", "submit", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        if slot == 0 {
            super::form::submit_form(w, n, None, false);
        }
        Ok(())
    });
    method(reg, rt, "HTMLFormElement", "reset", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let ids: Vec<NodeId> = w.doc_slot(slot, |d| d.descendants(n).collect());
        {
            let mut vals = w.values.borrow_mut();
            for i in ids {
                vals.remove(&(slot, i));
            }
        }
        super::internals::form_reset(cx, w, slot, n);
        Ok(())
    });
    // <script>
    getter(reg, rt, "HTMLScriptElement", "text", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.child_text_content(n)))
    });
    setter(reg, rt, "HTMLScriptElement", "text", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = cx.arg_string(0)?;
        w.doc_slot_mut(slot, |d| d.set_text_content(n, Some(&v)));
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "HTMLScriptElement", "async", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let v = w.doc_slot(slot, |d| {
            d.has_attribute(n, "async")
                || (!d.flags(n).contains(NodeFlags::PARSER_INSERTED)
                    && !d.flags(n).contains(super::script::NOT_FORCE_ASYNC))
        });
        cx.ret(v)
    });
    setter(reg, rt, "HTMLScriptElement", "async", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let on = cx.arg_bool(0);
        w.doc_slot_mut(slot, |d| {
            d.set_flags(n, super::script::NOT_FORCE_ASYNC, true);
            if on {
                let _ = d.set_attribute(n, "async", "");
            } else {
                d.remove_attribute(n, "async");
            }
        });
        w.after_mutation(cx);
        Ok(())
    });
    getter(reg, rt, "HTMLScriptElement", "crossOrigin", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        match w.doc_slot(slot, |d| {
            d.get_attribute(n, "crossorigin").map(str::to_owned)
        }) {
            Some(v) if v.eq_ignore_ascii_case("use-credentials") => cx.ret("use-credentials"),
            Some(_) => cx.ret("anonymous"),
            None => cx.ret(Arg::Null),
        }
    });
    // <template>
    getter(reg, rt, "HTMLTemplateElement", "content", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let c = w.doc_slot(slot, |d| d.template_contents(n));
        ret_node(cx, w, slot, c)
    });
    // <a> URL decomposition
    for prop in [
        "protocol", "host", "hostname", "port", "pathname", "search", "hash", "origin", "username",
        "password",
    ] {
        for iface in ["HTMLAnchorElement", "HTMLAreaElement"] {
            getter(reg, rt, iface, prop, move |cx, w| {
                let (slot, n) = this_elem(cx, w)?;
                let href = w.doc_slot(slot, |d| d.get_attribute(n, "href").map(str::to_owned));
                let Some(u) = href.and_then(|h| w.resolve_url(&h)) else {
                    return cx.ret("");
                };
                cx.ret(super::misc::url_component(&u, prop))
            });
            if prop != "origin" {
                setter(reg, rt, iface, prop, move |cx, w| {
                    let (slot, n) = this_elem(cx, w)?;
                    let v = cx.arg_string(0)?;
                    let href = w.doc_slot(slot, |d| d.get_attribute(n, "href").map(str::to_owned));
                    if let Some(mut u) = href.and_then(|h| w.resolve_url(&h)) {
                        super::misc::set_url_component(&mut u, prop, &v);
                        let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, "href", u.as_str()));
                        w.after_mutation(cx);
                    }
                    Ok(())
                });
            }
        }
    }
    for iface in ["HTMLAnchorElement", "HTMLAreaElement"] {
        method(reg, rt, iface, "toString", |cx, w| {
            let (slot, n) = this_elem(cx, w)?;
            let href = w.doc_slot(slot, |d| d.get_attribute(n, "href").map(str::to_owned));
            cx.ret(
                href.map(|h| w.resolve_url(&h).map_or(h, |u| u.to_string()))
                    .unwrap_or_default(),
            )
        });
    }
    getter(reg, rt, "HTMLAnchorElement", "text", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.text_content(n).unwrap_or_default()))
    });
    // <img>
    getter(reg, rt, "HTMLImageElement", "complete", |cx, _w| {
        cx.ret(true)
    });
    getter(reg, rt, "HTMLImageElement", "naturalWidth", |cx, _w| {
        cx.ret(0)
    });
    getter(reg, rt, "HTMLImageElement", "naturalHeight", |cx, _w| {
        cx.ret(0)
    });
    getter(reg, rt, "HTMLImageElement", "currentSrc", |cx, w| {
        let (slot, n) = this_elem(cx, w)?;
        let s = w.doc_slot(slot, |d| d.get_attribute(n, "src").map(str::to_owned));
        cx.ret(
            s.and_then(|s| w.resolve_url(&s))
                .map(|u| u.to_string())
                .unwrap_or_default(),
        )
    });
    reg.register(
        "HTMLImageElement",
        "Image",
        crate::registry::ImplKind::LegacyFactory,
        super::wf(rt, |cx, w| {
            let n = w.doc_slot_mut(0, |d| {
                d.create_element(
                    QualName::new(None, ns!(html), local_name!("img")),
                    Vec::new(),
                )
            });
            for (i, attr) in ["width", "height"].into_iter().enumerate() {
                if i < cx.len() && cx.arg_kind(i) != ValueKind::Undefined {
                    let v = cx.arg_u32(i)?;
                    let _ = w.doc_slot_mut(0, |d| d.set_attribute(n, attr, &v.to_string()));
                }
            }
            cx.ret_constructed(w.node_arg(0, n))
        }),
    );
    reg.register(
        "HTMLAudioElement",
        "Audio",
        crate::registry::ImplKind::LegacyFactory,
        super::wf(rt, |cx, w| {
            let src = opt_string(cx, 0)?;
            let n = w.doc_slot_mut(0, |d| {
                let n = d.create_element(
                    QualName::new(None, ns!(html), local_name!("audio")),
                    Vec::new(),
                );
                let _ = d.set_attribute(n, "preload", "auto");
                if let Some(s) = src {
                    let _ = d.set_attribute(n, "src", &s);
                }
                n
            });
            cx.ret_constructed(w.node_arg(0, n))
        }),
    );
    reg.register(
        "HTMLOptionElement",
        "Option",
        crate::registry::ImplKind::LegacyFactory,
        super::wf(rt, |cx, w| {
            let text = opt_string(cx, 0)?;
            let value = opt_string(cx, 1)?;
            let (ds, sel) = (cx.arg_bool(2), cx.arg_bool(3));
            let n = w.doc_slot_mut(0, |d| {
                let n = d.create_element(
                    QualName::new(None, ns!(html), local_name!("option")),
                    Vec::new(),
                );
                if let Some(t) = text.filter(|t| !t.is_empty()) {
                    let tn = d.create_text(&t);
                    let _ = d.append_child(n, tn);
                }
                if let Some(v) = value {
                    let _ = d.set_attribute(n, "value", &v);
                }
                if ds {
                    let _ = d.set_attribute(n, "selected", "");
                }
                if sel {
                    set_checked(d, n, true);
                }
                n
            });
            cx.ret_constructed(w.node_arg(0, n))
        }),
    );
    // Frames are a skeleton in M2: no nested browsing contexts yet.
    for prop in ["contentWindow", "contentDocument"] {
        getter(reg, rt, "HTMLIFrameElement", prop, |cx, _w| {
            cx.ret(Arg::Null)
        });
    }
    method(reg, rt, "HTMLCanvasElement", "getContext", |cx, _w| {
        cx.ret(Arg::Null)
    });
    let _ = K_OBJ;
    let _: Option<Rc<()>> = None;
}

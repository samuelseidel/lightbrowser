//! URL, URLSearchParams, TextEncoder/TextDecoder, DOMParser, Blob and
//! MutationObserver, plus mutation processing (observer records, scripts
//! inserted by script, inline event handler attributes).

use std::rc::Rc;

use lb_dom::{Document, Mutation, NodeFlags, NodeId, local_name, ns};
use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, ValueKind};
use url::Url;

use super::events::{body_forwards_to_window, set_handler};
use super::{
    Dict, HandlerValue, Js, K_OBJ, Obj, Runtime, WINDOW_REF, Window, ctor, getter, illegal, method,
    need_args, opt_string, setter, this_obj,
};
use crate::registry::{ImplKind, Registry};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Return an array iterator over `items` (`Array.prototype.values` of the
/// realm, so `next()` behaves natively).
pub(crate) fn ret_iterator(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    items: Vec<Arg>,
) -> Result<(), HostError> {
    let values = w.helpers.borrow().array_values;
    let Some(values) = values else {
        return cx.ret(Arg::Array(items));
    };
    let it = cx
        .try_call_handle(values, Arg::Array(items), &[])
        .map_err(|e| HostError::Error(e.message))?;
    cx.ret(Arg::Handle(it))
}

/// URL getter by component name (URL Standard API).
pub(crate) fn url_component(u: &Url, which: &str) -> String {
    use url::quirks as q;
    match which {
        "href" => q::href(u).to_owned(),
        "origin" => q::origin(u),
        "protocol" => q::protocol(u).to_owned(),
        "username" => q::username(u).to_owned(),
        "password" => q::password(u).to_owned(),
        "host" => q::host(u).to_owned(),
        "hostname" => q::hostname(u).to_owned(),
        "port" => q::port(u).to_owned(),
        "pathname" => q::pathname(u).to_owned(),
        "search" => q::search(u).to_owned(),
        "hash" => q::hash(u).to_owned(),
        _ => String::new(),
    }
}

/// URL setter by component name.
pub(crate) fn set_url_component(u: &mut Url, which: &str, v: &str) {
    use url::quirks as q;
    let _ = match which {
        "href" => q::set_href(u, v).map_err(|_| ()),
        "protocol" => q::set_protocol(u, v),
        "username" => q::set_username(u, v),
        "password" => q::set_password(u, v),
        "host" => q::set_host(u, v),
        "hostname" => q::set_hostname(u, v),
        "port" => q::set_port(u, v),
        "pathname" => {
            q::set_pathname(u, v);
            Ok(())
        }
        "search" => {
            q::set_search(u, v);
            Ok(())
        }
        "hash" => {
            q::set_hash(u, v);
            Ok(())
        }
        _ => Ok(()),
    };
}

pub(crate) fn serialize_params(list: &[(String, String)]) -> String {
    let mut s = form_urlencoded::Serializer::new(String::new());
    for (k, v) in list {
        s.append_pair(k, v);
    }
    s.finish()
}

fn parse_params(s: &str) -> Vec<(String, String)> {
    let s = s.strip_prefix('?').unwrap_or(s);
    form_urlencoded::parse(s.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// URL / URLSearchParams
// ---------------------------------------------------------------------------

pub(crate) struct UrlData {
    pub url: Url,
    pub params: Option<u32>,
}

pub(crate) struct ParamsData {
    pub list: Vec<(String, String)>,
    /// The URL object these params belong to.
    pub owner: Option<u32>,
}

fn this_url(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, Url), HostError> {
    let id = this_obj(cx)?;
    let u = w
        .with_obj(id, |o| match o {
            Obj::Url(u) => Some(u.url.clone()),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)?;
    Ok((id, u))
}

fn store_url(w: &Window, id: u32, url: Url) {
    let params = w
        .with_obj(id, |o| match o {
            Obj::Url(u) => {
                u.url = url.clone();
                u.params
            }
            _ => None,
        })
        .flatten();
    if let Some(p) = params {
        let list = parse_params(url.query().unwrap_or(""));
        w.with_obj(p, |o| {
            if let Obj::Params(pd) = o {
                pd.list = list;
            }
        });
    }
}

fn parse_url(cx: &mut CallCtx<'_, '_, '_>, op: &str) -> Result<Option<Url>, HostError> {
    need_args(cx, 1, op, "URL").map_err(|_| {
        HostError::type_error(format!(
            "Failed to {op} 'URL': 1 argument required, but only 0 present."
        ))
    })?;
    let input = cx.arg_string(0)?;
    let base = opt_string(cx, 1)?;
    let base = match base {
        Some(b) => match Url::parse(&b) {
            Ok(u) => Some(u),
            Err(_) => return Ok(None),
        },
        None => None,
    };
    Ok(match base {
        Some(b) => b.join(&input).ok(),
        None => Url::parse(&input).ok(),
    })
}

fn this_params(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    match w.with_obj(id, |o| matches!(o, Obj::Params(_))) {
        Some(true) => Ok(id),
        _ => Err(illegal()),
    }
}

fn params_list(w: &Window, id: u32) -> Vec<(String, String)> {
    w.with_obj(id, |o| match o {
        Obj::Params(p) => p.list.clone(),
        _ => Vec::new(),
    })
    .unwrap_or_default()
}

/// Update params and push the change to the owning URL.
fn params_update(w: &Window, id: u32, f: impl FnOnce(&mut Vec<(String, String)>)) {
    let (owner, query) = w
        .with_obj(id, |o| match o {
            Obj::Params(p) => {
                f(&mut p.list);
                (p.owner, serialize_params(&p.list))
            }
            _ => (None, String::new()),
        })
        .unwrap_or((None, String::new()));
    if let Some(u) = owner {
        w.with_obj(u, |o| {
            if let Obj::Url(ud) = o {
                ud.url
                    .set_query(if query.is_empty() { None } else { Some(&query) });
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Blob, TextDecoder, MutationObserver data
// ---------------------------------------------------------------------------

pub(crate) struct BlobData {
    pub bytes: Vec<u8>,
    pub type_: String,
}

pub(crate) struct DecoderData {
    enc: &'static encoding_rs::Encoding,
    fatal: bool,
    ignore_bom: bool,
}

#[derive(Clone, Default, Debug)]
pub(crate) struct MoOptions {
    child_list: bool,
    attributes: bool,
    character_data: bool,
    subtree: bool,
    attribute_old_value: bool,
    character_data_old_value: bool,
    attribute_filter: Option<Vec<String>>,
}

pub(crate) struct MoData {
    callback: Handle,
    targets: Vec<(u32, NodeId, MoOptions)>,
    records: Vec<u32>,
}

pub(crate) struct MoRecord {
    type_: &'static str,
    slot: u32,
    target: NodeId,
    added: Vec<NodeId>,
    removed: Vec<NodeId>,
    prev: Option<NodeId>,
    next: Option<NodeId>,
    attr_name: Option<String>,
    attr_ns: Option<String>,
    old_value: Option<String>,
}

// ---------------------------------------------------------------------------
// Mutation processing
// ---------------------------------------------------------------------------

/// See [`Window::after_mutation`].
pub(crate) fn process_mutations(js: &mut dyn Js, w: &Rc<Window>) {
    // Style elements the parser closed: their sheets start now.
    let closed = w
        .parser
        .try_borrow_mut()
        .ok()
        .and_then(|mut p| p.as_mut().map(|p| p.take_closed_styles()))
        .unwrap_or_default();
    for n in closed {
        super::sheets::style_changed(w, n);
    }
    loop {
        let batch = std::mem::take(&mut *w.mutations.borrow_mut());
        if batch.is_empty() {
            return;
        }
        let mut scripts = Vec::new();
        let mut images = Vec::new();
        let mut links: Vec<NodeId> = Vec::new();
        let mut styles: Vec<NodeId> = Vec::new();
        let mut removed = false;
        for (slot, m) in coalesce(w, &batch) {
            observe_record(js, w, slot, &m);
        }
        super::frames::on_mutations(js, w, &batch);
        super::shadow::on_mutations(js, w, &batch);
        for (slot, m) in batch {
            if slot != 0 {
                continue;
            }
            super::image::candidates(w, &m, &mut images);
            sheet_candidates(w, &m, &mut links, &mut styles, &mut removed);
            match m {
                Mutation::ChildInserted { node, .. } => {
                    let (handlers, found) = w.with_doc(|d| {
                        let mut handlers = Vec::new();
                        let mut found = Vec::new();
                        if !d.is_connected(node) && d.parent(node).is_none() {
                            return (handlers, found);
                        }
                        for n in d.inclusive_descendants(node) {
                            let Some(e) = d.element(n) else { continue };
                            if !e.is_html() {
                                continue;
                            }
                            for a in &e.attrs {
                                if a.name.ns == ns!()
                                    && a.name.local.starts_with("on")
                                    && a.name.local.len() > 2
                                {
                                    handlers.push((
                                        n,
                                        a.name.local.to_string(),
                                        Some(a.value.to_string()),
                                    ));
                                }
                            }
                            if e.name.local == local_name!("script")
                                && !d.flags(n).intersects(
                                    NodeFlags::PARSER_INSERTED | NodeFlags::ALREADY_STARTED,
                                )
                                && d.is_connected(n)
                            {
                                found.push(n);
                            }
                        }
                        (handlers, found)
                    });
                    for (n, name, v) in handlers {
                        content_handler(w, n, &name, v);
                    }
                    scripts.extend(found);
                }
                Mutation::Attribute { element, name, .. }
                    if name.ns == ns!() && name.local.starts_with("on") =>
                {
                    let v = w.with_doc(|d| {
                        d.element(element)
                            .filter(|e| e.is_html())
                            .map(|_| d.get_attribute(element, &name.local).map(str::to_owned))
                    });
                    if let Some(v) = v {
                        content_handler(w, element, &name.local, v);
                    }
                }
                _ => {}
            }
        }
        if removed {
            super::sheets::nodes_removed(w);
        }
        links.dedup();
        for n in links {
            super::sheets::link_changed(w, n);
        }
        styles.dedup();
        for n in styles {
            super::sheets::style_changed(w, n);
        }
        if !images.is_empty() {
            super::image::process(js, w, images);
        }
        for s in scripts {
            super::script::prepare_dynamic(js, w, s);
        }
    }
}

/// Style sheet owners a mutation may change: links (inserted, `rel` /
/// `href` changes) and style elements (inserted, text changes). A style
/// element the parser is still filling waits for its end tag.
fn sheet_candidates(
    w: &Window,
    m: &Mutation,
    links: &mut Vec<NodeId>,
    styles: &mut Vec<NodeId>,
    removed: &mut bool,
) {
    let style_parent = |d: &lb_dom::Document, n: NodeId| {
        d.parent(n)
            .filter(|&p| d.is_html_element_named(p, &local_name!("style")))
    };
    let parsing = |d: &lb_dom::Document, n: NodeId| {
        d.flags(n).contains(NodeFlags::PARSER_INSERTED)
            && !w.css.borrow().has_owner(n)
            && w.parser.borrow().is_some()
    };
    match m {
        Mutation::ChildInserted { node, .. } => w.with_doc(|d| {
            if !d.is_connected(*node) {
                return;
            }
            if let Some(p) = style_parent(d, *node) {
                if !parsing(d, p) {
                    styles.push(p);
                }
                return;
            }
            for n in d.inclusive_descendants(*node) {
                if d.is_html_element_named(n, &local_name!("link")) {
                    links.push(n);
                } else if d.is_html_element_named(n, &local_name!("style")) && !parsing(d, n) {
                    styles.push(n);
                }
            }
        }),
        Mutation::ChildRemoved { parent, .. } => w.with_doc(|d| {
            *removed = true;
            if d.is_html_element_named(*parent, &local_name!("style")) && d.is_connected(*parent) {
                styles.push(*parent);
            }
        }),
        Mutation::Attribute { element, name, .. } if name.ns == ns!() => w.with_doc(|d| {
            if d.is_html_element_named(*element, &local_name!("link"))
                && matches!(&*name.local, "rel" | "href")
            {
                links.push(*element);
            } else if d.is_html_element_named(*element, &local_name!("style"))
                && &*name.local == "type"
            {
                styles.push(*element);
            }
        }),
        Mutation::CharacterData { node, .. } => w.with_doc(|d| {
            if let Some(p) = style_parent(d, *node)
                && d.is_connected(p)
            {
                styles.push(p);
            }
        }),
        _ => {}
    }
}

fn content_handler(w: &Window, n: NodeId, name: &str, value: Option<String>) {
    let is_body = w.with_doc(|d| {
        d.is_html_element_named(n, &local_name!("body"))
            || d.is_html_element_named(n, &local_name!("frameset"))
    });
    let target = if is_body && body_forwards_to_window(name) {
        WINDOW_REF
    } else {
        w.node_ref(0, n)
    };
    set_handler(w, target, name, value.map(HandlerValue::Uncompiled));
}

/// One observer-visible change: a DOM mutation, or the child-list
/// changes of one operation on one parent merged into one record.
enum Change {
    Single(Mutation),
    ChildList {
        parent: NodeId,
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
        prev: Option<NodeId>,
        next: Option<NodeId>,
    },
}

/// Merge consecutive child-list mutations of the same parent made by one
/// script operation (`textContent =`, `replaceChild`, fragments) into one
/// record, as the DOM spec queues them. Parser insertions stay one record
/// per node.
fn coalesce(w: &Window, batch: &[(u32, Mutation)]) -> Vec<(u32, Change)> {
    if w.observers.borrow().is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(u32, Change)> = Vec::new();
    for (slot, m) in batch {
        let (parent, node, prev, next, inserted) = match m {
            Mutation::ChildInserted {
                parent,
                node,
                prev_sibling,
                next_sibling,
            } => (*parent, *node, *prev_sibling, *next_sibling, true),
            Mutation::ChildRemoved {
                parent,
                node,
                prev_sibling,
                next_sibling,
            } => (*parent, *node, *prev_sibling, *next_sibling, false),
            other => {
                out.push((*slot, Change::Single(other.clone())));
                continue;
            }
        };
        let parser = inserted
            && w.parser.borrow().is_some()
            && w.doc_slot(*slot, |d| {
                d.flags(node).contains(NodeFlags::PARSER_INSERTED)
            });
        let last_same = out.iter_mut().rev().find(|(s, c)| {
            *s == *slot && matches!(c, Change::ChildList { parent: p, .. } if *p == parent)
        });
        if !parser
            && let Some((
                _,
                Change::ChildList {
                    added,
                    removed,
                    next: n,
                    ..
                },
            )) = last_same
        {
            // Removals then insertions (replace), or runs of one kind.
            if inserted {
                added.push(node);
                *n = next;
                continue;
            } else if added.is_empty() {
                removed.push(node);
                *n = next;
                continue;
            }
        }
        out.push((
            *slot,
            Change::ChildList {
                parent,
                added: if inserted { vec![node] } else { Vec::new() },
                removed: if inserted { Vec::new() } else { vec![node] },
                prev,
                next,
            },
        ));
    }
    out
}

fn observe_record(js: &mut dyn Js, w: &Rc<Window>, slot: u32, c: &Change) {
    let single;
    let m = match c {
        Change::Single(m) => m,
        Change::ChildList {
            parent, prev, next, ..
        } => {
            // Stand-in for target/type selection below.
            single = Mutation::ChildInserted {
                parent: *parent,
                node: *parent,
                prev_sibling: *prev,
                next_sibling: *next,
            };
            &single
        }
    };
    if w.observers.borrow().is_empty() {
        return;
    }
    let (target, type_) = match m {
        Mutation::ChildInserted { parent, .. } | Mutation::ChildRemoved { parent, .. } => {
            (*parent, "childList")
        }
        Mutation::Attribute { element, .. } => (*element, "attributes"),
        Mutation::CharacterData { node, .. } => (*node, "characterData"),
    };
    let chain: Vec<NodeId> = w.doc_slot(slot, |d| {
        std::iter::once(target).chain(d.ancestors(target)).collect()
    });
    let observers = w.observers.borrow().clone();
    let mut queued = false;
    for ob in observers {
        let opts = w
            .with_obj(ob, |o| match o {
                Obj::MutationObserver(m) => m
                    .targets
                    .iter()
                    .filter(|(s, n, o)| {
                        *s == slot && chain.contains(n) && (*n == target || o.subtree)
                    })
                    .map(|(_, _, o)| o.clone())
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        let mut interested = false;
        let mut old = false;
        for o in &opts {
            match m {
                Mutation::ChildInserted { .. } | Mutation::ChildRemoved { .. } => {
                    interested |= o.child_list
                }
                Mutation::Attribute { name, .. } => {
                    if o.attributes
                        && o.attribute_filter
                            .as_ref()
                            .is_none_or(|f| name.ns == ns!() && f.iter().any(|x| *x == *name.local))
                    {
                        interested = true;
                        old |= o.attribute_old_value;
                    }
                }
                Mutation::CharacterData { .. } => {
                    if o.character_data {
                        interested = true;
                        old |= o.character_data_old_value;
                    }
                }
            }
        }
        if !interested {
            continue;
        }
        let rec = match (c, m) {
            (
                Change::ChildList {
                    added,
                    removed,
                    prev,
                    next,
                    ..
                },
                _,
            ) => MoRecord {
                type_,
                slot,
                target,
                added: added.clone(),
                removed: removed.clone(),
                prev: *prev,
                next: *next,
                attr_name: None,
                attr_ns: None,
                old_value: None,
            },
            (
                _,
                Mutation::ChildInserted {
                    node,
                    prev_sibling,
                    next_sibling,
                    ..
                },
            ) => MoRecord {
                type_,
                slot,
                target,
                added: vec![*node],
                removed: Vec::new(),
                prev: *prev_sibling,
                next: *next_sibling,
                attr_name: None,
                attr_ns: None,
                old_value: None,
            },
            (
                _,
                Mutation::ChildRemoved {
                    node,
                    prev_sibling,
                    next_sibling,
                    ..
                },
            ) => MoRecord {
                type_,
                slot,
                target,
                added: Vec::new(),
                removed: vec![*node],
                prev: *prev_sibling,
                next: *next_sibling,
                attr_name: None,
                attr_ns: None,
                old_value: None,
            },
            (
                _,
                Mutation::Attribute {
                    name, old_value, ..
                },
            ) => MoRecord {
                type_,
                slot,
                target,
                added: Vec::new(),
                removed: Vec::new(),
                prev: None,
                next: None,
                attr_name: Some(name.local.to_string()),
                attr_ns: (!name.ns.is_empty()).then(|| name.ns.to_string()),
                old_value: if old {
                    old_value.as_ref().map(|v| v.to_string())
                } else {
                    None
                },
            },
            (_, Mutation::CharacterData { old_value, .. }) => MoRecord {
                type_,
                slot,
                target,
                added: Vec::new(),
                removed: Vec::new(),
                prev: None,
                next: None,
                attr_name: None,
                attr_ns: None,
                old_value: old.then(|| old_value.to_string()),
            },
        };
        let rid = w.new_obj(Obj::MutationRecord(Box::new(rec)));
        w.with_obj(ob, |o| {
            if let Obj::MutationObserver(m) = o {
                m.records.push(rid);
            }
        });
        queued = true;
    }
    if queued && !w.mo_queued.get() {
        w.mo_queued.set(true);
        let f = w.helpers.borrow().mo_deliver;
        if let Some(f) = f {
            js.microtask(f);
        }
    }
}

/// The mutation observer microtask: deliver pending records.
pub(crate) fn deliver_mutation_records(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) {
    w.mo_queued.set(false);
    let observers = w.observers.borrow().clone();
    let signals = super::shadow::take_signals(w);
    for ob in observers {
        let Some(Some((cb, records))) = w.with_obj(ob, |o| match o {
            Obj::MutationObserver(m) if !m.records.is_empty() => {
                Some((m.callback, std::mem::take(&mut m.records)))
            }
            _ => None,
        }) else {
            continue;
        };
        let arr = Arg::Array(records.into_iter().map(|r| w.obj_arg(r)).collect());
        let me = w.obj_arg(ob);
        if let Err(e) = cx.try_call(cb, me.clone(), &[arr, me]) {
            super::events::report_exception(cx, w, &e);
        }
    }
    super::shadow::fire_slotchange(cx, w, signals);
    process_mutations(cx, w);
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // URL
    ctor(reg, rt, "URL", |cx, w| {
        let url = parse_url(cx, "construct")?;
        let Some(url) = url else {
            let s = cx.arg_string(0)?;
            let msg = match opt_string(cx, 1)? {
                Some(b) => format!("Failed to construct 'URL': Invalid URL '{s}' with base '{b}'"),
                None => format!("Failed to construct 'URL': Invalid URL '{s}'"),
            };
            return Err(HostError::type_error(msg));
        };
        let id = w.new_obj(Obj::Url(Box::new(UrlData { url, params: None })));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    reg.register(
        "URL",
        "canParse",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, _w| {
            let ok = parse_url(cx, "execute 'canParse' on")?.is_some();
            cx.ret(ok)
        }),
    );
    reg.register(
        "URL",
        "parse",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| match parse_url(cx, "execute 'parse' on")? {
            Some(url) => {
                let id = w.new_obj(Obj::Url(Box::new(UrlData { url, params: None })));
                cx.ret(w.obj_arg(id))
            }
            None => cx.ret(Arg::Null),
        }),
    );
    reg.register(
        "URL",
        "createObjectURL",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, w| {
            need_args(cx, 1, "createObjectURL", "URL")?;
            let blob = cx
                .arg_host(0)
                .filter(|h| h.kind == K_OBJ)
                .and_then(|h| {
                    w.with_obj(h.id, |o| match o {
                        Obj::Blob(b) => Some((b.bytes.clone(), b.type_.clone())),
                        _ => None,
                    })
                })
                .flatten();
            let Some((bytes, type_)) = blob else {
                return Err(HostError::type_error(
                    "Failed to execute 'createObjectURL' on 'URL': Overload resolution failed.",
                ));
            };
            cx.ret(super::worker::create_blob_url(w, bytes, type_))
        }),
    );
    reg.register(
        "URL",
        "revokeObjectURL",
        ImplKind::StaticMethod,
        super::wf(rt, |cx, _w| {
            need_args(cx, 1, "revokeObjectURL", "URL")?;
            let u = cx.arg_string(0)?;
            super::worker::revoke_blob_url(&u);
            Ok(())
        }),
    );
    for prop in [
        "href", "origin", "protocol", "username", "password", "host", "hostname", "port",
        "pathname", "search", "hash",
    ] {
        getter(reg, rt, "URL", prop, move |cx, w| {
            let (_, u) = this_url(cx, w)?;
            cx.ret(url_component(&u, prop))
        });
        if prop != "origin" {
            setter(reg, rt, "URL", prop, move |cx, w| {
                let (id, mut u) = this_url(cx, w)?;
                let v = cx.arg_string(0)?;
                if prop == "href" {
                    match Url::parse(&v) {
                        Ok(nu) => u = nu,
                        Err(_) => {
                            return Err(HostError::type_error(format!(
                                "Failed to set the 'href' property on 'URL': Invalid URL '{v}'"
                            )));
                        }
                    }
                } else {
                    set_url_component(&mut u, prop, &v);
                }
                store_url(w, id, u);
                Ok(())
            });
        }
    }
    for name in ["toString", "toJSON"] {
        method(reg, rt, "URL", name, |cx, w| {
            let (_, u) = this_url(cx, w)?;
            cx.ret(u.to_string())
        });
    }
    getter(reg, rt, "URL", "searchParams", |cx, w| {
        let (id, u) = this_url(cx, w)?;
        let existing = w
            .with_obj(id, |o| match o {
                Obj::Url(d) => d.params,
                _ => None,
            })
            .flatten();
        let p = match existing {
            Some(p) => p,
            None => {
                let p = w.new_obj(Obj::Params(Box::new(ParamsData {
                    list: parse_params(u.query().unwrap_or("")),
                    owner: Some(id),
                })));
                w.with_obj(id, |o| {
                    if let Obj::Url(d) = o {
                        d.params = Some(p);
                    }
                });
                p
            }
        };
        cx.ret(w.obj_arg(p))
    });

    // URLSearchParams
    ctor(reg, rt, "URLSearchParams", |cx, w| {
        let mut list = Vec::new();
        if !cx.is_empty() {
            match cx.arg_kind(0) {
                ValueKind::Undefined | ValueKind::Null => {}
                ValueKind::Object => {
                    if let Some(src) = cx.arg_host(0).filter(|h| h.kind == K_OBJ)
                        && let Some(Some(l)) = w.with_obj(src.id, |o| match o {
                            Obj::Params(p) => Some(p.list.clone()),
                            _ => None,
                        })
                    {
                        list = l;
                    } else if let lb_js::JsValue::Json(s) = cx.arg_value(0) {
                        match serde_json::from_str::<serde_json::Value>(&s) {
                            Ok(serde_json::Value::Array(items)) => {
                                for it in items {
                                    let p = it.as_array().cloned().unwrap_or_default();
                                    if p.len() != 2 {
                                        return Err(HostError::type_error(
                                            "Failed to construct 'URLSearchParams': The provided value cannot be converted to a sequence.",
                                        ));
                                    }
                                    list.push((js_s(&p[0]), js_s(&p[1])));
                                }
                            }
                            Ok(serde_json::Value::Object(m)) => {
                                for (k, v) in m {
                                    list.push((k, js_s(&v)));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => list = parse_params(&cx.arg_string(0)?),
            }
        }
        let id = w.new_obj(Obj::Params(Box::new(ParamsData { list, owner: None })));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    getter(reg, rt, "URLSearchParams", "size", |cx, w| {
        let id = this_params(cx, w)?;
        cx.ret(params_list(w, id).len() as u32)
    });
    method(reg, rt, "URLSearchParams", "append", |cx, w| {
        need_args(cx, 2, "append", "URLSearchParams")?;
        let id = this_params(cx, w)?;
        let (k, v) = (cx.arg_string(0)?, cx.arg_string(1)?);
        params_update(w, id, |l| l.push((k, v)));
        Ok(())
    });
    method(reg, rt, "URLSearchParams", "delete", |cx, w| {
        need_args(cx, 1, "delete", "URLSearchParams")?;
        let id = this_params(cx, w)?;
        let k = cx.arg_string(0)?;
        let v = opt_string(cx, 1)?;
        params_update(w, id, |l| {
            l.retain(|(a, b)| !(a == &k && v.as_ref().is_none_or(|v| v == b)))
        });
        Ok(())
    });
    method(reg, rt, "URLSearchParams", "get", |cx, w| {
        need_args(cx, 1, "get", "URLSearchParams")?;
        let id = this_params(cx, w)?;
        let k = cx.arg_string(0)?;
        match params_list(w, id).into_iter().find(|(a, _)| *a == k) {
            Some((_, v)) => cx.ret(v),
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "URLSearchParams", "getAll", |cx, w| {
        need_args(cx, 1, "getAll", "URLSearchParams")?;
        let id = this_params(cx, w)?;
        let k = cx.arg_string(0)?;
        let v = params_list(w, id)
            .into_iter()
            .filter(|(a, _)| *a == k)
            .map(|(_, v)| Arg::String(v))
            .collect();
        cx.ret(Arg::Array(v))
    });
    method(reg, rt, "URLSearchParams", "has", |cx, w| {
        need_args(cx, 1, "has", "URLSearchParams")?;
        let id = this_params(cx, w)?;
        let k = cx.arg_string(0)?;
        let v = opt_string(cx, 1)?;
        cx.ret(
            params_list(w, id)
                .iter()
                .any(|(a, b)| *a == k && v.as_ref().is_none_or(|v| v == b)),
        )
    });
    method(reg, rt, "URLSearchParams", "set", |cx, w| {
        need_args(cx, 2, "set", "URLSearchParams")?;
        let id = this_params(cx, w)?;
        let (k, v) = (cx.arg_string(0)?, cx.arg_string(1)?);
        params_update(w, id, |l| match l.iter().position(|(a, _)| *a == k) {
            Some(i) => {
                l[i].1 = v;
                let mut j = i + 1;
                while j < l.len() {
                    if l[j].0 == k {
                        l.remove(j);
                    } else {
                        j += 1;
                    }
                }
            }
            None => l.push((k, v)),
        });
        Ok(())
    });
    method(reg, rt, "URLSearchParams", "sort", |cx, w| {
        let id = this_params(cx, w)?;
        params_update(w, id, |l| {
            l.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()))
        });
        Ok(())
    });
    method(reg, rt, "URLSearchParams", "toString", |cx, w| {
        let id = this_params(cx, w)?;
        cx.ret(serialize_params(&params_list(w, id)))
    });
    for (name, part) in [("entries", 0), ("keys", 1), ("values", 2)] {
        method(reg, rt, "URLSearchParams", name, move |cx, w| {
            let id = this_params(cx, w)?;
            let items = params_list(w, id)
                .into_iter()
                .map(|(k, v)| match part {
                    0 => Arg::Array(vec![Arg::String(k), Arg::String(v)]),
                    1 => Arg::String(k),
                    _ => Arg::String(v),
                })
                .collect();
            ret_iterator(cx, w, items)
        });
    }
    method(reg, rt, "URLSearchParams", "forEach", |cx, w| {
        need_args(cx, 1, "forEach", "URLSearchParams")?;
        let id = this_params(cx, w)?;
        let f = cx.arg_handle(0);
        let me = w.obj_arg(id);
        for (k, v) in params_list(w, id) {
            cx.call(
                f,
                Arg::Undefined,
                &[Arg::String(v), Arg::String(k), me.clone()],
            )?;
        }
        Ok(())
    });

    // TextEncoder / TextDecoder
    ctor(reg, rt, "TextEncoder", |cx, w| {
        let id = w.new_obj(Obj::TextEncoder);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    getter(reg, rt, "TextEncoder", "encoding", |cx, _w| cx.ret("utf-8"));
    method(reg, rt, "TextEncoder", "encode", |cx, _w| {
        let s = opt_string(cx, 0)?.unwrap_or_default();
        cx.ret(Arg::Uint8Array(s.into_bytes()))
    });
    ctor(reg, rt, "TextDecoder", |cx, w| {
        let label = opt_string(cx, 0)?.unwrap_or_else(|| "utf-8".into());
        let Some(enc) = encoding_rs::Encoding::for_label(label.trim().as_bytes())
            .filter(|e| *e != encoding_rs::REPLACEMENT)
        else {
            return Err(HostError::RangeError(format!(
                "Failed to construct 'TextDecoder': The encoding label provided ('{label}') is invalid."
            )));
        };
        let d = Dict::arg(cx, 1);
        let fatal = d.bool(cx, "fatal")?;
        let ignore_bom = d.bool(cx, "ignoreBOM")?;
        d.release(cx);
        let id = w.new_obj(Obj::TextDecoder(Box::new(DecoderData {
            enc,
            fatal,
            ignore_bom,
        })));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    fn dec(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
    ) -> Result<(&'static encoding_rs::Encoding, bool, bool), HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::TextDecoder(d) => Some((d.enc, d.fatal, d.ignore_bom)),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    getter(reg, rt, "TextDecoder", "encoding", |cx, w| {
        let (e, _, _) = dec(cx, w)?;
        cx.ret(e.name().to_ascii_lowercase())
    });
    getter(reg, rt, "TextDecoder", "fatal", |cx, w| {
        let d = dec(cx, w)?;
        cx.ret(d.1)
    });
    getter(reg, rt, "TextDecoder", "ignoreBOM", |cx, w| {
        let d = dec(cx, w)?;
        cx.ret(d.2)
    });
    method(reg, rt, "TextDecoder", "decode", |cx, w| {
        let (enc, fatal, ignore_bom) = dec(cx, w)?;
        let bytes = if cx.is_empty() || cx.arg_kind(0) == ValueKind::Undefined {
            Vec::new()
        } else {
            cx.arg_bytes(0).ok_or_else(|| HostError::type_error("Failed to execute 'decode' on 'TextDecoder': The provided value is not of type '(ArrayBuffer or ArrayBufferView)'."))?
        };
        let (text, had_errors) = if ignore_bom {
            let (t, e) = enc.decode_without_bom_handling(&bytes);
            (t, e)
        } else {
            let (t, e) = enc.decode_with_bom_removal(&bytes);
            (t, e)
        };
        if fatal && had_errors {
            return Err(HostError::type_error(
                "Failed to execute 'decode' on 'TextDecoder': The encoded data was not valid.",
            ));
        }
        cx.ret(text.into_owned())
    });

    // DOMParser
    ctor(reg, rt, "DOMParser", |cx, w| {
        let id = w.new_obj(Obj::DomParser);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    method(reg, rt, "DOMParser", "parseFromString", |cx, w| {
        need_args(cx, 2, "parseFromString", "DOMParser")?;
        let s = cx.arg_string(0)?;
        let ty = cx.arg_string(1)?;
        if !matches!(
            ty.as_str(),
            "text/html"
                | "text/xml"
                | "application/xml"
                | "application/xhtml+xml"
                | "image/svg+xml"
        ) {
            return Err(HostError::type_error(format!(
                "Failed to execute 'parseFromString' on 'DOMParser': The provided value '{ty}' is not a valid enum value of type DOMParserSupportedType."
            )));
        }
        if ty != "text/html" {
            w.warn(format!(
                "DOMParser: {ty} is parsed as HTML (no XML parser yet)"
            ));
        }
        let mut d = Document::parse_html_with(
            &s,
            lb_dom::ParserOptions {
                declarative_shadow: false,
                ..Default::default()
            },
        );
        // Scripts in parsed documents never run.
        let scripts: Vec<NodeId> = d
            .descendants(Document::DOCUMENT)
            .filter(|&n| d.is_html_element_named(n, &local_name!("script")))
            .collect();
        for n in scripts {
            d.set_flags(n, NodeFlags::ALREADY_STARTED, true);
        }
        let slot = w.add_doc(d);
        cx.ret(w.node_arg(slot, Document::DOCUMENT))
    });

    // Blob
    ctor(reg, rt, "Blob", |cx, w| {
        let mut bytes = Vec::new();
        if !cx.is_empty() && cx.arg_kind(0) == ValueKind::Object {
            let list = cx.arg_handle(0);
            let len = cx.get_property(list, "length")?;
            let n = match cx.handle_value(len) {
                lb_js::JsValue::Number(n) => n as u32,
                _ => 0,
            };
            cx.release(len);
            for i in 0..n {
                let item = cx.get_property(list, &i.to_string())?;
                if let Some(b) = cx.handle_bytes(item) {
                    bytes.extend(b);
                } else if let Some(Some(b)) =
                    cx.handle_host(item).filter(|h| h.kind == K_OBJ).map(|h| {
                        w.with_obj(h.id, |o| match o {
                            Obj::Blob(b) => Some(b.bytes.clone()),
                            _ => None,
                        })
                        .flatten()
                    })
                {
                    bytes.extend(b);
                } else {
                    bytes.extend(cx.handle_string(item)?.into_bytes());
                }
                cx.release(item);
            }
            cx.release(list);
        }
        let d = Dict::arg(cx, 1);
        let ty = d
            .string(cx, "type")?
            .unwrap_or_default()
            .to_ascii_lowercase();
        d.release(cx);
        let id = w.new_obj(Obj::Blob(Box::new(BlobData { bytes, type_: ty })));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    fn blob(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(Vec<u8>, String), HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::Blob(b) => Some((b.bytes.clone(), b.type_.clone())),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    getter(reg, rt, "Blob", "size", |cx, w| {
        let b = blob(cx, w)?;
        cx.ret(b.0.len() as f64)
    });
    getter(reg, rt, "Blob", "type", |cx, w| {
        let b = blob(cx, w)?;
        cx.ret(b.1)
    });
    method(reg, rt, "Blob", "text", |cx, w| {
        let (b, _) = blob(cx, w)?;
        let r = cx.ret_new_promise()?;
        cx.resolve_promise(r, Arg::String(String::from_utf8_lossy(&b).into_owned()))
    });
    method(reg, rt, "Blob", "arrayBuffer", |cx, w| {
        let (b, _) = blob(cx, w)?;
        let r = cx.ret_new_promise()?;
        cx.resolve_promise(r, Arg::ArrayBuffer(b))
    });
    method(reg, rt, "Blob", "slice", |cx, w| {
        let (b, ty) = blob(cx, w)?;
        let len = b.len() as i64;
        let norm = |v: f64| -> usize {
            let v = v as i64;
            (if v < 0 { (len + v).max(0) } else { v.min(len) }) as usize
        };
        let start = if cx.is_empty() || cx.arg_kind(0) == ValueKind::Undefined {
            0
        } else {
            norm(cx.arg_f64(0)?)
        };
        let end = if cx.len() < 2 || cx.arg_kind(1) == ValueKind::Undefined {
            len as usize
        } else {
            norm(cx.arg_f64(1)?)
        };
        let ct = opt_string(cx, 2)?.unwrap_or(ty);
        let bytes = if start < end {
            b[start..end].to_vec()
        } else {
            Vec::new()
        };
        let id = w.new_obj(Obj::Blob(Box::new(BlobData { bytes, type_: ct })));
        cx.ret(w.obj_arg(id))
    });

    // MutationObserver
    ctor(reg, rt, "MutationObserver", |cx, w| {
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(
                "Failed to construct 'MutationObserver': parameter 1 is not of type 'Function'.",
            ));
        }
        let cb = cx.arg_handle(0);
        let id = w.new_obj(Obj::MutationObserver(Box::new(MoData {
            callback: cb,
            targets: Vec::new(),
            records: Vec::new(),
        })));
        w.observers.borrow_mut().push(id);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    method(reg, rt, "MutationObserver", "observe", |cx, w| {
        need_args(cx, 1, "observe", "MutationObserver")?;
        let id = this_obj(cx)?;
        let Some((slot, node)) = cx.arg_host(0).and_then(|h| w.decode_node(h)) else {
            return Err(HostError::type_error(
                "Failed to execute 'observe' on 'MutationObserver': parameter 1 is not of type 'Node'.",
            ));
        };
        let d = Dict::arg(cx, 1);
        let mut o = MoOptions {
            child_list: d.bool(cx, "childList")?,
            subtree: d.bool(cx, "subtree")?,
            attribute_old_value: d.bool(cx, "attributeOldValue")?,
            character_data_old_value: d.bool(cx, "characterDataOldValue")?,
            ..Default::default()
        };
        let attrs = d.get(cx, "attributes")?;
        let cdata = d.get(cx, "characterData")?;
        if let Some(fh) = d.get(cx, "attributeFilter")? {
            if let lb_js::JsValue::Json(s) = cx.handle_value(fh) {
                o.attribute_filter = serde_json::from_str::<Vec<serde_json::Value>>(&s)
                    .ok()
                    .map(|v| v.iter().map(js_s).collect());
            }
            cx.release(fh);
        }
        o.attributes = match attrs {
            Some(h) => {
                let b = super::truthy(&cx.handle_value(h));
                cx.release(h);
                b
            }
            None => o.attribute_old_value || o.attribute_filter.is_some(),
        };
        o.character_data = match cdata {
            Some(h) => {
                let b = super::truthy(&cx.handle_value(h));
                cx.release(h);
                b
            }
            None => o.character_data_old_value,
        };
        d.release(cx);
        if !o.child_list && !o.attributes && !o.character_data {
            return Err(HostError::type_error(
                "Failed to execute 'observe' on 'MutationObserver': The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.",
            ));
        }
        if o.attribute_old_value && !o.attributes {
            return Err(HostError::type_error(
                "Failed to execute 'observe' on 'MutationObserver': The options object may only set 'attributeOldValue' to true when 'attributes' is true or not present.",
            ));
        }
        if o.attribute_filter.is_some() && !o.attributes {
            return Err(HostError::type_error(
                "Failed to execute 'observe' on 'MutationObserver': The options object may only set 'attributeFilter' when 'attributes' is true or not present.",
            ));
        }
        if o.character_data_old_value && !o.character_data {
            return Err(HostError::type_error(
                "Failed to execute 'observe' on 'MutationObserver': The options object may only set 'characterDataOldValue' to true when 'characterData' is true or not present.",
            ));
        }
        w.with_obj(id, |ob| {
            if let Obj::MutationObserver(m) = ob {
                match m
                    .targets
                    .iter_mut()
                    .find(|(s, n, _)| *s == slot && *n == node)
                {
                    Some(t) => t.2 = o,
                    None => m.targets.push((slot, node, o)),
                }
            }
        })
        .ok_or_else(illegal)?;
        Ok(())
    });
    method(reg, rt, "MutationObserver", "disconnect", |cx, w| {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| {
            if let Obj::MutationObserver(m) = o {
                m.targets.clear();
                m.records.clear();
            }
        });
        Ok(())
    });
    method(reg, rt, "MutationObserver", "takeRecords", |cx, w| {
        let id = this_obj(cx)?;
        let recs = w
            .with_obj(id, |o| match o {
                Obj::MutationObserver(m) => std::mem::take(&mut m.records),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        cx.ret(Arg::Array(recs.into_iter().map(|r| w.obj_arg(r)).collect()))
    });
    fn rec<R>(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
        f: impl FnOnce(&MoRecord) -> R,
    ) -> Result<R, HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::MutationRecord(r) => Some(f(r)),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    getter(reg, rt, "MutationRecord", "type", |cx, w| {
        let v = rec(cx, w, |r| r.type_)?;
        cx.ret(v)
    });
    getter(reg, rt, "MutationRecord", "target", |cx, w| {
        let (s, t) = rec(cx, w, |r| (r.slot, r.target))?;
        cx.ret(w.node_arg(s, t))
    });
    for (name, added) in [("addedNodes", true), ("removedNodes", false)] {
        getter(reg, rt, "MutationRecord", name, move |cx, w| {
            let id = this_obj(cx)?;
            let (s, v) = rec(cx, w, |r| {
                (
                    r.slot,
                    if added {
                        r.added.clone()
                    } else {
                        r.removed.clone()
                    },
                )
            })?;
            let l = w.cached_obj((id, 0, name, String::new()), || {
                Obj::NodeList(super::coll::NodeListSrc::Static(s, v))
            });
            cx.ret(w.obj_arg(l))
        });
    }
    getter(reg, rt, "MutationRecord", "previousSibling", |cx, w| {
        let (s, n) = rec(cx, w, |r| (r.slot, r.prev))?;
        cx.ret(w.opt_node_arg(s, n))
    });
    getter(reg, rt, "MutationRecord", "nextSibling", |cx, w| {
        let (s, n) = rec(cx, w, |r| (r.slot, r.next))?;
        cx.ret(w.opt_node_arg(s, n))
    });
    getter(reg, rt, "MutationRecord", "attributeName", |cx, w| {
        let v = rec(cx, w, |r| r.attr_name.clone())?;
        cx.ret(v.map_or(Arg::Null, Arg::String))
    });
    getter(reg, rt, "MutationRecord", "attributeNamespace", |cx, w| {
        let v = rec(cx, w, |r| r.attr_ns.clone())?;
        cx.ret(v.map_or(Arg::Null, Arg::String))
    });
    getter(reg, rt, "MutationRecord", "oldValue", |cx, w| {
        let v = rec(cx, w, |r| r.old_value.clone())?;
        cx.ret(v.map_or(Arg::Null, Arg::String))
    });
}

fn js_s(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

//! NodeList, HTMLCollection (live, recomputed on DOM generation change),
//! DOMTokenList, NamedNodeMap, Attr and DOMStringMap.

use std::rc::Rc;

use lb_dom::{Document, LocalName, NodeId, QualName, local_name, ns};
use lb_js::{Arg, HostError, IndexedGetter, NamedGetter, PropCtx, ValueKind};

use super::{K_OBJ, Obj, Runtime, Window, getter, illegal, method, need_args, setter, this_obj};
use crate::registry::Registry;

/// What a NodeList holds.
pub(crate) enum NodeListSrc {
    /// `querySelectorAll` results.
    Static(u32, Vec<NodeId>),
    /// `node.childNodes` (live).
    Children(u32, NodeId),
    /// `getElementsByName` (live).
    Live(Box<Collection>),
}

/// Which elements a live collection holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Filter {
    Children,
    TagName(String),
    TagNameNs(String, String),
    ClassName(String),
    Name(String),
    Local(&'static str),
    Links,
    Anchors,
    All,
    FormControls,
    /// `table.rows`: rows of `thead` children, then of the table and its
    /// `tbody` children, then of `tfoot` children.
    TableRows,
    /// HTML element children with this local name (`tBodies`, section
    /// `rows`).
    ChildLocal(&'static str),
    /// `tr.cells`: `td` and `th` children.
    Cells,
    /// Window named items for a name (`window[name]`, ADR 0040).
    WindowNamed(String),
    /// Document named items for a name (`document[name]`).
    DocumentNamed(String),
    /// A form's elements for a name (`RadioNodeList`): listed elements,
    /// else `img` elements.
    FormNamed(String),
    /// `document.all[name]`: elements with that `id`, or "all named"
    /// elements with that `name`.
    AllNamed(String),
}

/// A live collection: recomputed when the document generation changes.
pub(crate) struct Collection {
    pub iface: &'static str,
    pub slot: u32,
    pub root: NodeId,
    pub filter: Filter,
    generation: u64,
    items: Vec<NodeId>,
}

impl Collection {
    fn new(iface: &'static str, slot: u32, root: NodeId, filter: Filter) -> Self {
        Collection {
            iface,
            slot,
            root,
            filter,
            generation: u64::MAX,
            items: Vec::new(),
        }
    }

    fn refresh(&mut self, d: &Document) {
        if self.generation == d.generation() {
            return;
        }
        self.generation = d.generation();
        self.items = compute(d, self.root, &self.filter);
    }
}

fn compute(d: &Document, root: NodeId, f: &Filter) -> Vec<NodeId> {
    match f {
        Filter::Children => return d.element_children(root).collect(),
        Filter::TableRows => return super::table::table_rows(d, root),
        Filter::ChildLocal(l) => return super::table::html_children(d, root, &[l]),
        Filter::Cells => return super::table::html_children(d, root, &["td", "th"]),
        Filter::WindowNamed(name) => {
            return d
                .descendants(root)
                .filter(|&n| d.is_element(n) && super::named::window_matches(d, n, name))
                .collect();
        }
        Filter::DocumentNamed(name) => {
            return d
                .descendants(root)
                .filter(|&n| d.is_element(n) && super::named::document_matches(d, n, name))
                .collect();
        }
        Filter::FormNamed(name) => return super::named::form_named_elements(d, root, name, None),
        Filter::AllNamed(name) => {
            return d
                .descendants(root)
                .filter(|&n| d.is_element(n) && all_named_matches(d, n, name))
                .collect();
        }
        Filter::FormControls => return super::named::listed_elements(d, root),
        _ => {}
    }
    let html_doc = d.is_html();
    let classes: Vec<&str> = match f {
        Filter::ClassName(c) => c.split_ascii_whitespace().collect(),
        _ => Vec::new(),
    };
    let quirks = d.quirks_mode() == lb_dom::QuirksMode::Quirks;
    d.descendants(root)
        .filter(|&n| d.is_element(n))
        .filter(|&n| {
            let q = d.qual_name(n).expect("element");
            let html = q.ns == ns!(html);
            match f {
                Filter::Children
                | Filter::TableRows
                | Filter::ChildLocal(_)
                | Filter::Cells
                | Filter::WindowNamed(_)
                | Filter::DocumentNamed(_)
                | Filter::FormNamed(_)
                | Filter::AllNamed(_)
                | Filter::FormControls => unreachable!(),
                Filter::All => true,
                Filter::TagName(t) => {
                    if t == "*" {
                        return true;
                    }
                    let qn = d.tag_name(n).unwrap_or_default();
                    if html && html_doc {
                        // Lowercase match for HTML elements, exact otherwise.
                        let qualified = match &q.prefix {
                            Some(p) => format!("{}:{}", &**p, &*q.local),
                            None => q.local.to_string(),
                        };
                        qualified == t.to_ascii_lowercase()
                    } else {
                        let qualified = match &q.prefix {
                            Some(p) => format!("{}:{}", &**p, &*q.local),
                            None => q.local.to_string(),
                        };
                        qualified == *t || (qn == *t && !html)
                    }
                }
                Filter::TagNameNs(ns_, local) => {
                    (ns_ == "*" || *q.ns == **ns_) && (local == "*" || *q.local == **local)
                }
                Filter::ClassName(_) => {
                    !classes.is_empty()
                        && classes.iter().all(|c| {
                            if quirks {
                                d.classes(n).any(|x| x.eq_ignore_ascii_case(c))
                            } else {
                                d.has_class(n, c)
                            }
                        })
                }
                Filter::Name(name) => html && d.get_attribute(n, "name") == Some(name.as_str()),
                Filter::Local(l) => html && &*q.local == *l,
                Filter::Links => {
                    html && (q.local == local_name!("a") || q.local == local_name!("area"))
                        && d.has_attribute(n, "href")
                }
                Filter::Anchors => {
                    html && q.local == local_name!("a") && d.has_attribute(n, "name")
                }
            }
        })
        .collect()
}

/// A listed element (`form.elements`): `button`, `fieldset`, `input`
/// (not an image button), `object`, `output`, `select`, `textarea`, or a
/// form-associated custom element.
pub(crate) fn is_listed(d: &Document, n: NodeId) -> bool {
    let Some(q) = d.qual_name(n) else {
        return false;
    };
    d.flags(n).contains(lb_dom::NodeFlags::FORM_ASSOCIATED)
        || q.ns == ns!(html)
            && matches!(
                &*q.local,
                "button" | "fieldset" | "input" | "object" | "output" | "select" | "textarea"
            )
            && !(q.local == local_name!("input")
                && d.get_attribute(n, "type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("image")))
}

/// HTML's "all named" elements (`document.all` by `name`).
fn is_all_named(d: &Document, n: NodeId) -> bool {
    d.qual_name(n).is_some_and(|q| {
        q.ns == ns!(html)
            && matches!(
                &*q.local,
                "a" | "button"
                    | "embed"
                    | "form"
                    | "frame"
                    | "frameset"
                    | "iframe"
                    | "img"
                    | "input"
                    | "map"
                    | "meta"
                    | "object"
                    | "select"
                    | "textarea"
            )
    })
}

fn all_named_matches(d: &Document, n: NodeId, name: &str) -> bool {
    d.element_id(n) == Some(name) || is_all_named(d, n) && d.get_attribute(n, "name") == Some(name)
}

/// The (cached) live HTMLCollection for `root` and `filter`.
pub(crate) fn collection(w: &Window, slot: u32, root: NodeId, filter: Filter) -> u32 {
    let iface = match filter {
        Filter::All => "HTMLAllCollection",
        Filter::FormControls => "HTMLFormControlsCollection",
        Filter::Local("option") => "HTMLOptionsCollection",
        _ => "HTMLCollection",
    };
    w.cached_obj((slot, root.0, "coll", format!("{filter:?}")), || {
        Obj::Collection(Box::new(Collection::new(iface, slot, root, filter)))
    })
}

/// The (cached) live collection for `root` and `filter` as interface
/// `iface` (`RadioNodeList`).
pub(crate) fn collection_as(
    w: &Window,
    slot: u32,
    root: NodeId,
    filter: Filter,
    iface: &'static str,
) -> u32 {
    w.cached_obj((slot, root.0, iface, format!("{filter:?}")), || {
        Obj::Collection(Box::new(Collection::new(iface, slot, root, filter)))
    })
}

/// A live NodeList over a filter (`getElementsByName`).
pub(crate) fn collection_nodelist(w: &Window, slot: u32, root: NodeId, filter: Filter) -> u32 {
    w.cached_obj((slot, root.0, "nodelist", format!("{filter:?}")), || {
        Obj::NodeList(NodeListSrc::Live(Box::new(Collection::new(
            "NodeList", slot, root, filter,
        ))))
    })
}

/// Current items of a NodeList or HTMLCollection object: `(slot, nodes)`.
pub(crate) fn items(w: &Window, id: u32) -> Option<(u32, Vec<NodeId>)> {
    w.with_obj(id, |o| match o {
        Obj::NodeList(NodeListSrc::Static(slot, v)) => Some((*slot, v.clone())),
        Obj::NodeList(NodeListSrc::Children(slot, n)) => {
            Some((*slot, w.doc_slot(*slot, |d| d.children(*n).collect())))
        }
        Obj::NodeList(NodeListSrc::Live(c)) | Obj::Collection(c) => {
            w.doc_slot(c.slot, |d| c.refresh(d));
            Some((c.slot, c.items.clone()))
        }
        _ => None,
    })
    .flatten()
}

fn item_at(w: &Window, id: u32, i: u32) -> Option<(u32, Option<NodeId>)> {
    w.with_obj(id, |o| match o {
        Obj::NodeList(NodeListSrc::Static(slot, v)) => Some((*slot, v.get(i as usize).copied())),
        Obj::NodeList(NodeListSrc::Children(slot, n)) => {
            Some((*slot, w.doc_slot(*slot, |d| d.children(*n).nth(i as usize))))
        }
        Obj::NodeList(NodeListSrc::Live(c)) | Obj::Collection(c) => {
            w.doc_slot(c.slot, |d| c.refresh(d));
            Some((c.slot, c.items.get(i as usize).copied()))
        }
        _ => None,
    })
    .flatten()
}

pub(crate) fn collection_len(w: &Window, id: u32) -> usize {
    items(w, id).map_or(0, |(_, v)| v.len())
}

/// Named item of an HTMLCollection: first element with that id, or an
/// HTML element with that name.
fn named_item(w: &Window, id: u32, name: &str) -> Option<(u32, NodeId)> {
    if name.is_empty() {
        return None;
    }
    let (slot, list) = items(w, id)?;
    w.doc_slot(slot, |d| {
        list.iter()
            .copied()
            .find(|&n| {
                d.element_id(n) == Some(name)
                    || (d.qual_name(n).is_some_and(|q| q.ns == ns!(html))
                        && d.get_attribute(n, "name") == Some(name))
            })
            .map(|n| (slot, n))
    })
}

/// What a collection's `namedItem(name)` (and named getter) returns:
/// `document.all` gives an `HTMLCollection` and a form's `elements` a
/// `RadioNodeList` when several elements match; others the first match.
fn named_value(w: &Window, id: u32, name: &str) -> Option<Arg> {
    if name.is_empty() {
        return None;
    }
    let kind = w
        .with_obj(id, |o| match o {
            Obj::Collection(c) => Some((c.iface, c.root, c.filter.clone())),
            _ => None,
        })
        .flatten();
    match kind {
        Some(("HTMLAllCollection", root, _)) => {
            let (slot, list) = items(w, id)?;
            let found: Vec<NodeId> = w.doc_slot(slot, |d| {
                list.into_iter()
                    .filter(|&n| all_named_matches(d, n, name))
                    .collect()
            });
            match found.as_slice() {
                [] => None,
                [n] => Some(w.node_arg(slot, *n)),
                _ => Some(w.obj_arg(collection(w, slot, root, Filter::AllNamed(name.to_owned())))),
            }
        }
        Some(("HTMLFormControlsCollection", form, Filter::FormControls)) => {
            let (slot, list) = items(w, id)?;
            let found: Vec<NodeId> = w.doc_slot(slot, |d| {
                list.into_iter()
                    .filter(|&n| {
                        d.element_id(n) == Some(name) || d.get_attribute(n, "name") == Some(name)
                    })
                    .collect()
            });
            match found.as_slice() {
                [] => None,
                [n] => Some(w.node_arg(slot, *n)),
                _ => Some(w.obj_arg(super::named::radio_node_list(w, slot, form, name))),
            }
        }
        _ => named_item(w, id, name).map(|(slot, n)| w.node_arg(slot, n)),
    }
}

/// Supported property names of a collection: each element's `id`, then
/// its `name` (HTML elements; for `document.all` the "all named" ones),
/// in order, without repeats.
fn supported_names(w: &Window, id: u32) -> Vec<String> {
    let all = w
        .with_obj(
            id,
            |o| matches!(o, Obj::Collection(c) if c.iface == "HTMLAllCollection"),
        )
        .unwrap_or(false);
    let Some((slot, list)) = items(w, id) else {
        return Vec::new();
    };
    w.doc_slot(slot, |d| {
        let mut out: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for n in list {
            if let Some(i) = d.element_id(n).filter(|s| !s.is_empty())
                && seen.insert(i.to_owned())
            {
                out.push(i.to_owned());
            }
            let html = d.qual_name(n).is_some_and(|q| q.ns == ns!(html));
            if html
                && (!all || is_all_named(d, n))
                && let Some(nm) = d.get_attribute(n, "name").filter(|s| !s.is_empty())
                && seen.insert(nm.to_owned())
            {
                out.push(nm.to_owned());
            }
        }
        out
    })
}

fn indexed(
    rt: &Runtime,
    f: impl Fn(&mut PropCtx<'_, '_, '_>, &Rc<Window>, u32, u32) -> Result<Option<Arg>, HostError>
    + 'static,
) -> IndexedGetter {
    let rt = rt.clone();
    Rc::new(move |cx, i| {
        let Some(w) = rt.window(cx.holder_realm()) else {
            return Ok(None);
        };
        let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
            return Ok(None);
        };
        f(cx, &w, h.id, i)
    })
}

fn named(
    rt: &Runtime,
    f: impl Fn(&mut PropCtx<'_, '_, '_>, &Rc<Window>, u32, &str) -> Result<Option<Arg>, HostError>
    + 'static,
) -> NamedGetter {
    let rt = rt.clone();
    Rc::new(move |cx, name| {
        let Some(w) = rt.window(cx.holder_realm()) else {
            return Ok(None);
        };
        let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
            return Ok(None);
        };
        f(cx, &w, h.id, name)
    })
}

/// Number of items of the collection an interceptor's holder is.
fn length_of(rt: &Runtime) -> lb_js::IndexedLength {
    let r = rt.clone();
    Rc::new(move |cx| {
        let Some(w) = r.window(cx.holder_realm()) else {
            return 0;
        };
        match cx.holder_host().filter(|h| h.kind == K_OBJ) {
            Some(h) => collection_len(&w, h.id) as u32,
            None => 0,
        }
    })
}

/// Names that must keep resolving to prototype members, not named items.
fn reserved(name: &str) -> bool {
    matches!(
        name,
        "length"
            | "item"
            | "namedItem"
            | "constructor"
            | "toString"
            | "valueOf"
            | "hasOwnProperty"
            | "toLocaleString"
            | "isPrototypeOf"
            | "propertyIsEnumerable"
            | "__proto__"
            | "forEach"
            | "entries"
            | "keys"
            | "values"
            | "add"
            | "remove"
            | "selectedIndex"
            | "getNamedItem"
            | "getNamedItemNS"
            | "setNamedItem"
            | "setNamedItemNS"
            | "removeNamedItem"
            | "removeNamedItemNS"
            | "then"
    )
}

pub(crate) fn token_list_values(d: &Document, n: NodeId, attr: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in d
        .get_attribute(n, attr)
        .unwrap_or("")
        .split_ascii_whitespace()
    {
        if !out.iter().any(|x| x == t) {
            out.push(t.to_owned());
        }
    }
    out
}

/// Attr object for an attribute of an element (same object every time).
pub(crate) fn attr_obj(w: &Window, slot: u32, owner: NodeId, name: QualName) -> u32 {
    let key = format!("{}|{}", &*name.ns, &*name.local);
    w.cached_obj((slot, owner.0, "attr", key), || Obj::Attr {
        slot,
        owner: Some(owner),
        name,
        value: String::new(),
    })
}

fn camel_to_data(name: &str) -> Option<String> {
    let mut out = String::from("data-");
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '-' && chars.peek().is_some_and(|n| n.is_ascii_lowercase()) {
            return None;
        }
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    Some(out)
}

fn data_to_camel(attr: &str) -> Option<String> {
    let rest = attr.strip_prefix("data-")?;
    let mut out = String::new();
    let mut up = false;
    for c in rest.chars() {
        if c == '-' {
            up = true;
            continue;
        }
        if up && c.is_ascii_lowercase() {
            out.push(c.to_ascii_uppercase());
        } else {
            if up {
                out.push('-');
            }
            out.push(c);
        }
        up = false;
    }
    if up {
        out.push('-');
    }
    Some(out)
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // NodeList / HTMLCollection
    for iface in [
        "NodeList",
        "HTMLCollection",
        "HTMLAllCollection",
        "HTMLFormControlsCollection",
        "HTMLOptionsCollection",
    ] {
        getter(reg, rt, iface, "length", |cx, w| {
            let id = this_obj(cx)?;
            cx.ret(collection_len(w, id) as u32)
        });
        method(reg, rt, iface, "item", |cx, w| {
            let id = this_obj(cx)?;
            let i = cx.arg_u32(0)?;
            match item_at(w, id, i) {
                Some((slot, n)) => cx.ret(w.opt_node_arg(slot, n)),
                None => Err(illegal()),
            }
        });
        reg.indexed_getter(
            iface,
            indexed(rt, |_cx, w, id, i| {
                Ok(match item_at(w, id, i) {
                    Some((slot, Some(n))) => Some(w.node_arg(slot, n)),
                    _ => None,
                })
            }),
        );
        // Indices enumerate, read-only (ADR 0040). `HTMLOptionsCollection`
        // has an indexed setter in Chrome, not implemented here.
        if iface != "HTMLOptionsCollection" {
            reg.indexed_length(iface, length_of(rt));
        }
        if iface != "NodeList" {
            method(reg, rt, iface, "namedItem", |cx, w| {
                let id = this_obj(cx)?;
                let name = cx.arg_string(0)?;
                match named_value(w, id, &name) {
                    Some(v) => cx.ret(v),
                    None => cx.ret(Arg::Null),
                }
            });
            // Blink's interceptors (ADR 0040): prototype members win, the
            // names are read-only and listed by `getOwnPropertyNames`.
            reg.named_getter(
                iface,
                named(rt, |_cx, w, id, name| Ok(named_value(w, id, name))),
            );
            let r = rt.clone();
            reg.named_enumerator(
                iface,
                Rc::new(move |cx| {
                    let Some(w) = r.window(cx.holder_realm()) else {
                        return Vec::new();
                    };
                    match cx.holder_host().filter(|h| h.kind == K_OBJ) {
                        Some(h) => supported_names(&w, h.id),
                        None => Vec::new(),
                    }
                }),
            );
            reg.named_unenumerable(iface);
            // Blink's error messages name the interface owning the getter.
            let owner = match iface {
                "HTMLFormControlsCollection" | "HTMLAllCollection" => iface,
                _ => "HTMLCollection",
            };
            reg.named_legacy(
                iface,
                lb_js::LegacyNamed {
                    iface: owner,
                    override_builtins: false,
                    not_enumerable: false,
                    properties_object: false,
                },
            );
        }
    }
    // NodeList entries/keys/values/forEach are Array.prototype intrinsics.

    // RadioNodeList (a form's elements for a name, ADR 0040): NodeList
    // members plus `value`.
    reg.indexed_getter(
        "RadioNodeList",
        indexed(rt, |_cx, w, id, i| {
            Ok(match item_at(w, id, i) {
                Some((slot, Some(n))) => Some(w.node_arg(slot, n)),
                _ => None,
            })
        }),
    );
    reg.indexed_length("RadioNodeList", length_of(rt));
    fn radio_value(d: &Document, n: NodeId) -> String {
        d.get_attribute(n, "value").unwrap_or("on").to_owned()
    }
    fn is_radio(d: &Document, n: NodeId) -> bool {
        d.is_html_element_named(n, &local_name!("input"))
            && d.get_attribute(n, "type")
                .is_some_and(|t| t.eq_ignore_ascii_case("radio"))
    }
    getter(reg, rt, "RadioNodeList", "value", |cx, w| {
        let id = this_obj(cx)?;
        let (slot, list) = items(w, id).ok_or_else(illegal)?;
        let v = w.doc_slot(slot, |d| {
            list.iter()
                .copied()
                .find(|&n| is_radio(d, n) && super::dom::checked(d, n, "checked"))
                .map(|n| radio_value(d, n))
        });
        cx.ret(v.unwrap_or_default())
    });
    setter(reg, rt, "RadioNodeList", "value", |cx, w| {
        let id = this_obj(cx)?;
        let v = cx.arg_string(0)?;
        let (slot, list) = items(w, id).ok_or_else(illegal)?;
        w.doc_slot_mut(slot, |d| {
            if let Some(n) = list
                .iter()
                .copied()
                .find(|&n| is_radio(d, n) && radio_value(d, n) == v)
            {
                d.set_flags(n, lb_dom::NodeFlags::CHECKED_DIRTY, true);
                d.set_flags(n, lb_dom::NodeFlags::CHECKED, true);
            }
        });
        Ok(())
    });

    // DOMTokenList
    fn tl(
        cx: &mut lb_js::CallCtx<'_, '_, '_>,
        w: &Window,
    ) -> Result<(u32, NodeId, &'static str), HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::TokenList { slot, node, attr } => Some((*slot, *node, *attr)),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    fn check_token(w: &Window, op: &str, t: &str) -> Result<(), HostError> {
        if t.is_empty() {
            return Err(w.rt.err("SyntaxError", format!("Failed to execute '{op}' on 'DOMTokenList': The token provided must not be empty.")));
        }
        if t.chars().any(|c| c.is_ascii_whitespace()) {
            return Err(w.rt.err("InvalidCharacterError", format!("Failed to execute '{op}' on 'DOMTokenList': The token provided ('{t}') contains HTML space characters, which are not valid in tokens.")));
        }
        Ok(())
    }
    fn write_tokens(w: &Window, slot: u32, n: NodeId, attr: &str, v: &[String]) {
        let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, attr, &v.join(" ")));
    }
    getter(reg, rt, "DOMTokenList", "length", |cx, w| {
        let (slot, n, a) = tl(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| token_list_values(d, n, a).len() as u32))
    });
    getter(reg, rt, "DOMTokenList", "value", |cx, w| {
        let (slot, n, a) = tl(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.get_attribute(n, a).unwrap_or("").to_owned()))
    });
    setter(reg, rt, "DOMTokenList", "value", |cx, w| {
        let (slot, n, a) = tl(cx, w)?;
        let v = cx.arg_string(0)?;
        let _ = w.doc_slot_mut(slot, |d| d.set_attribute(n, a, &v));
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "DOMTokenList", "item", |cx, w| {
        let (slot, n, a) = tl(cx, w)?;
        let i = cx.arg_u32(0)? as usize;
        match w.doc_slot(slot, |d| token_list_values(d, n, a).get(i).cloned()) {
            Some(t) => cx.ret(t),
            None => cx.ret(Arg::Null),
        }
    });
    reg.indexed_getter(
        "DOMTokenList",
        indexed(rt, |_cx, w, id, i| {
            let Some(Some((slot, n, a))) = w.with_obj(id, |o| match o {
                Obj::TokenList { slot, node, attr } => Some((*slot, *node, *attr)),
                _ => None,
            }) else {
                return Ok(None);
            };
            Ok(w.doc_slot(slot, |d| {
                token_list_values(d, n, a).get(i as usize).cloned()
            })
            .map(Arg::String))
        }),
    );
    method(reg, rt, "DOMTokenList", "contains", |cx, w| {
        let (slot, n, a) = tl(cx, w)?;
        let t = cx.arg_string(0)?;
        cx.ret(w.doc_slot(slot, |d| token_list_values(d, n, a).contains(&t)))
    });
    method(reg, rt, "DOMTokenList", "add", |cx, w| {
        let (slot, n, a) = tl(cx, w)?;
        let mut toks = Vec::new();
        for i in 0..cx.len() {
            let t = cx.arg_string(i)?;
            check_token(w, "add", &t)?;
            toks.push(t);
        }
        let mut v = w.doc_slot(slot, |d| token_list_values(d, n, a));
        for t in toks {
            if !v.contains(&t) {
                v.push(t);
            }
        }
        write_tokens(w, slot, n, a, &v);
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "DOMTokenList", "remove", |cx, w| {
        let (slot, n, a) = tl(cx, w)?;
        let mut toks = Vec::new();
        for i in 0..cx.len() {
            let t = cx.arg_string(i)?;
            check_token(w, "remove", &t)?;
            toks.push(t);
        }
        let has = w.doc_slot(slot, |d| d.has_attribute(n, a));
        let mut v = w.doc_slot(slot, |d| token_list_values(d, n, a));
        v.retain(|x| !toks.contains(x));
        if has {
            write_tokens(w, slot, n, a, &v);
        }
        w.after_mutation(cx);
        Ok(())
    });
    method(reg, rt, "DOMTokenList", "toggle", |cx, w| {
        need_args(cx, 1, "toggle", "DOMTokenList")?;
        let (slot, n, a) = tl(cx, w)?;
        let t = cx.arg_string(0)?;
        check_token(w, "toggle", &t)?;
        let force =
            (cx.len() > 1 && cx.arg_kind(1) != ValueKind::Undefined).then(|| cx.arg_bool(1));
        let mut v = w.doc_slot(slot, |d| token_list_values(d, n, a));
        let has = v.contains(&t);
        let result = match (has, force) {
            (true, Some(true)) => true,
            (true, _) => {
                v.retain(|x| *x != t);
                write_tokens(w, slot, n, a, &v);
                false
            }
            (false, Some(false)) => false,
            (false, _) => {
                v.push(t);
                write_tokens(w, slot, n, a, &v);
                true
            }
        };
        w.after_mutation(cx);
        cx.ret(result)
    });
    method(reg, rt, "DOMTokenList", "replace", |cx, w| {
        need_args(cx, 2, "replace", "DOMTokenList")?;
        let (slot, n, a) = tl(cx, w)?;
        let old = cx.arg_string(0)?;
        let new = cx.arg_string(1)?;
        check_token(w, "replace", &old)?;
        check_token(w, "replace", &new)?;
        let mut v = w.doc_slot(slot, |d| token_list_values(d, n, a));
        let Some(i) = v.iter().position(|x| *x == old) else {
            return cx.ret(false);
        };
        if v.contains(&new) {
            v.remove(i);
            if let Some(j) = v.iter().position(|x| *x == new) {
                let _ = j;
            }
        } else {
            v[i] = new;
        }
        write_tokens(w, slot, n, a, &v);
        w.after_mutation(cx);
        cx.ret(true)
    });
    // Supported tokens as in Blink's `RelList` and `HTMLIFrameElementSandbox`
    // (recorded in Chrome, `lb-page` tests `public_bench`). Other lists (`classList`, `htmlFor`, `sizes`) have none.
    method(reg, rt, "DOMTokenList", "supports", |cx, w| {
        need_args(cx, 1, "supports", "DOMTokenList")?;
        let (slot, n, attr) = tl(cx, w)?;
        let token = cx.arg_string(0)?.to_ascii_lowercase();
        let tag = w.doc_slot(slot, |d| {
            d.local_name(n).map(|l| l.to_string()).unwrap_or_default()
        });
        const LINK_REL: &[&str] = &[
            "preload",
            "preconnect",
            "dns-prefetch",
            "stylesheet",
            "icon",
            "alternate",
            "prefetch",
            "prerender",
            "next",
            "manifest",
            "apple-touch-icon",
            "apple-touch-icon-precomposed",
            "canonical",
            "modulepreload",
            "compression-dictionary",
        ];
        const ANCHOR_REL: &[&str] = &["noreferrer", "noopener", "opener"];
        const SANDBOX: &[&str] = &[
            "allow-downloads",
            "allow-forms",
            "allow-modals",
            "allow-orientation-lock",
            "allow-pointer-lock",
            "allow-popups",
            "allow-popups-to-escape-sandbox",
            "allow-presentation",
            "allow-same-origin",
            "allow-scripts",
            "allow-storage-access-by-user-activation",
            "allow-top-navigation",
            "allow-top-navigation-by-user-activation",
        ];
        let supported: &[&str] = match (attr, tag.as_str()) {
            ("rel", "link") => LINK_REL,
            ("rel", "a" | "area") => ANCHOR_REL,
            ("sandbox", _) => SANDBOX,
            _ => {
                return Err(HostError::type_error(
                    "Failed to execute 'supports' on 'DOMTokenList': DOMTokenList has no supported tokens.",
                ));
            }
        };
        cx.ret(supported.contains(&token.as_str()))
    });

    // NamedNodeMap / Attr
    fn nnm(cx: &mut lb_js::CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, NodeId), HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::NamedNodeMap { slot, node } => Some((*slot, *node)),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    getter(reg, rt, "NamedNodeMap", "length", |cx, w| {
        let (slot, n) = nnm(cx, w)?;
        cx.ret(w.doc_slot(slot, |d| d.attributes(n).len() as u32))
    });
    method(reg, rt, "NamedNodeMap", "item", |cx, w| {
        let (slot, n) = nnm(cx, w)?;
        let i = cx.arg_u32(0)? as usize;
        match w.doc_slot(slot, |d| d.attributes(n).get(i).map(|a| a.name.clone())) {
            Some(q) => {
                let id = attr_obj(w, slot, n, q);
                cx.ret(w.obj_arg(id))
            }
            None => cx.ret(Arg::Null),
        }
    });
    reg.indexed_getter(
        "NamedNodeMap",
        indexed(rt, |_cx, w, id, i| {
            let Some(Some((slot, n))) = w.with_obj(id, |o| match o {
                Obj::NamedNodeMap { slot, node } => Some((*slot, *node)),
                _ => None,
            }) else {
                return Ok(None);
            };
            Ok(w.doc_slot(slot, |d| {
                d.attributes(n).get(i as usize).map(|a| a.name.clone())
            })
            .map(|q| w.obj_arg(attr_obj(w, slot, n, q))))
        }),
    );
    fn find_attr(d: &Document, n: NodeId, name: &str) -> Option<QualName> {
        let lname = if d.is_html() && d.qual_name(n).is_some_and(|q| q.ns == ns!(html)) {
            name.to_ascii_lowercase()
        } else {
            name.to_owned()
        };
        d.attributes(n)
            .iter()
            .find(|a| match &a.name.prefix {
                Some(p) => format!("{}:{}", &**p, &*a.name.local) == lname,
                None => *a.name.local == *lname,
            })
            .map(|a| a.name.clone())
    }
    method(reg, rt, "NamedNodeMap", "getNamedItem", |cx, w| {
        let (slot, n) = nnm(cx, w)?;
        let name = cx.arg_string(0)?;
        match w.doc_slot(slot, |d| find_attr(d, n, &name)) {
            Some(q) => cx.ret(w.obj_arg(attr_obj(w, slot, n, q))),
            None => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "NamedNodeMap", "removeNamedItem", |cx, w| {
        let (slot, n) = nnm(cx, w)?;
        let name = cx.arg_string(0)?;
        match w.doc_slot(slot, |d| find_attr(d, n, &name)) {
            Some(q) => {
                let id = attr_obj(w, slot, n, q.clone());
                let v = w.doc_slot(slot, |d| d.get_attribute_ns(n, &q.ns, &q.local).unwrap_or("").to_owned());
                w.doc_slot_mut(slot, |d| d.remove_attribute_ns(n, &q.ns, &q.local));
                w.with_obj(id, |o| {
                    if let Obj::Attr { owner, value, .. } = o {
                        *owner = None;
                        *value = v;
                    }
                });
                w.after_mutation(cx);
                cx.ret(w.obj_arg(id))
            }
            None => Err(w.rt.err("NotFoundError", format!("Failed to execute 'removeNamedItem' on 'NamedNodeMap': No item with name '{name}' was found."))),
        }
    });
    reg.named_getter(
        "NamedNodeMap",
        named(rt, |_cx, w, id, name| {
            if reserved(name) {
                return Ok(None);
            }
            let Some(Some((slot, n))) = w.with_obj(id, |o| match o {
                Obj::NamedNodeMap { slot, node } => Some((*slot, *node)),
                _ => None,
            }) else {
                return Ok(None);
            };
            Ok(w.doc_slot(slot, |d| find_attr(d, n, name))
                .map(|q| w.obj_arg(attr_obj(w, slot, n, q))))
        }),
    );
    fn attr(
        cx: &mut lb_js::CallCtx<'_, '_, '_>,
        w: &Window,
    ) -> Result<(u32, Option<NodeId>, QualName, String), HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::Attr {
                slot,
                owner,
                name,
                value,
            } => Some((*slot, *owner, name.clone(), value.clone())),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    getter(reg, rt, "Attr", "name", |cx, w| {
        let (_, _, q, _) = attr(cx, w)?;
        cx.ret(match &q.prefix {
            Some(p) => format!("{}:{}", &**p, &*q.local),
            None => q.local.to_string(),
        })
    });
    getter(reg, rt, "Attr", "localName", |cx, w| {
        let (_, _, q, _) = attr(cx, w)?;
        cx.ret(q.local.to_string())
    });
    getter(reg, rt, "Attr", "namespaceURI", |cx, w| {
        let (_, _, q, _) = attr(cx, w)?;
        cx.ret(if q.ns.is_empty() {
            Arg::Null
        } else {
            Arg::String(q.ns.to_string())
        })
    });
    getter(reg, rt, "Attr", "prefix", |cx, w| {
        let (_, _, q, _) = attr(cx, w)?;
        cx.ret(q.prefix.map_or(Arg::Null, |p| Arg::String(p.to_string())))
    });
    getter(reg, rt, "Attr", "specified", |cx, _w| cx.ret(true));
    getter(reg, rt, "Attr", "ownerElement", |cx, w| {
        let (slot, owner, _, _) = attr(cx, w)?;
        cx.ret(w.opt_node_arg(slot, owner))
    });
    getter(reg, rt, "Attr", "value", |cx, w| {
        let (slot, owner, q, v) = attr(cx, w)?;
        match owner {
            Some(n) => cx.ret(w.doc_slot(slot, |d| {
                d.get_attribute_ns(n, &q.ns, &q.local)
                    .unwrap_or("")
                    .to_owned()
            })),
            None => cx.ret(v),
        }
    });
    setter(reg, rt, "Attr", "value", |cx, w| {
        let id = this_obj(cx)?;
        let (slot, owner, q, _) = attr(cx, w)?;
        let v = cx.arg_string(0)?;
        match owner {
            Some(n) => {
                let qualified = match &q.prefix {
                    Some(p) => format!("{}:{}", &**p, &*q.local),
                    None => q.local.to_string(),
                };
                let _ = w.doc_slot_mut(slot, |d| {
                    d.set_attribute_ns(n, q.ns.clone(), &qualified, &v)
                });
                w.after_mutation(cx);
            }
            None => {
                w.with_obj(id, |o| {
                    if let Obj::Attr { value, .. } = o {
                        *value = v;
                    }
                });
            }
        }
        Ok(())
    });
    method(reg, rt, "Element", "setAttributeNode", |cx, w| {
        let (slot, n) = super::this_node(cx, w)?;
        let Some(h) = cx.arg_host(0).filter(|h| h.kind == K_OBJ) else {
            return Err(HostError::type_error(
                "Failed to execute 'setAttributeNode' on 'Element': parameter 1 is not of type 'Attr'.",
            ));
        };
        let Some(Some((q, v))) = w.with_obj(h.id, |o| match o {
            Obj::Attr {
                name,
                value,
                owner: None,
                ..
            } => Some((name.clone(), value.clone())),
            _ => None,
        }) else {
            return Err(w.rt.err("InUseAttributeError", "Failed to execute 'setAttributeNode' on 'Element': The node provided is an attribute node that is already an attribute of another Element; attribute nodes must be explicitly cloned."));
        };
        let qualified = q.local.to_string();
        let _ = w.doc_slot_mut(slot, |d| {
            d.set_attribute_ns(n, q.ns.clone(), &qualified, &v)
        });
        w.with_obj(h.id, |o| {
            if let Obj::Attr { owner, slot: s, .. } = o {
                *owner = Some(n);
                *s = slot;
            }
        });
        w.cache.borrow_mut().insert(
            (slot, n.0, "attr", format!("{}|{}", &*q.ns, &*q.local)),
            h.id,
        );
        w.after_mutation(cx);
        cx.ret(Arg::Null)
    });

    // DOMStringMap (dataset)
    fn sm(w: &Window, id: u32) -> Option<(u32, NodeId)> {
        w.with_obj(id, |o| match o {
            Obj::StringMap { slot, node } => Some((*slot, *node)),
            _ => None,
        })
        .flatten()
    }
    reg.named_getter(
        "DOMStringMap",
        named(rt, |_cx, w, id, name| {
            let Some((slot, n)) = sm(w, id) else {
                return Ok(None);
            };
            let Some(attr) = camel_to_data(name) else {
                return Ok(None);
            };
            Ok(w.doc_slot(slot, |d| {
                d.get_attribute(n, &attr).map(|v| Arg::String(v.to_owned()))
            }))
        }),
    );
    let rt2 = rt.clone();
    reg.named_setter("DOMStringMap", Rc::new(move |cx, name, value| {
        let Some(w) = rt2.window(cx.holder_realm()) else { return Ok(false) };
        let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else { return Ok(false) };
        let Some((slot, n)) = sm(&w, h.id) else { return Ok(false) };
        let Some(attr) = camel_to_data(name) else {
            return Err(w.rt.err("SyntaxError", format!("Failed to set a named property '{name}' on 'DOMStringMap': '{name}' is not a valid property name.")));
        };
        let r = w.doc_slot_mut(slot, |d| d.set_attribute(n, &attr, value));
        if r.is_err() {
            return Err(w.rt.err("InvalidCharacterError", format!("Failed to set a named property '{name}' on 'DOMStringMap': '{attr}' is not a valid attribute name.")));
        }
        w.mutations_pending();
        Ok(true)
    }));
    let rt3 = rt.clone();
    reg.named_deleter(
        "DOMStringMap",
        Rc::new(move |cx, name| {
            let Some(w) = rt3.window(cx.holder_realm()) else {
                return Ok(None);
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Ok(None);
            };
            let Some((slot, n)) = sm(&w, h.id) else {
                return Ok(None);
            };
            let Some(attr) = camel_to_data(name) else {
                return Ok(None);
            };
            if !w.doc_slot(slot, |d| d.has_attribute(n, &attr)) {
                return Ok(None);
            }
            w.doc_slot_mut(slot, |d| d.remove_attribute(n, &attr));
            w.mutations_pending();
            Ok(Some(true))
        }),
    );
    let rt4 = rt.clone();
    reg.named_enumerator(
        "DOMStringMap",
        Rc::new(move |cx| {
            let Some(w) = rt4.window(cx.holder_realm()) else {
                return Vec::new();
            };
            let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                return Vec::new();
            };
            let Some((slot, n)) = sm(&w, h.id) else {
                return Vec::new();
            };
            w.doc_slot(slot, |d| {
                d.attributes(n)
                    .iter()
                    .filter(|a| a.name.ns == ns!() && a.name.prefix.is_none())
                    .filter_map(|a| data_to_camel(&a.name.local))
                    .collect()
            })
        }),
    );
    let _ = LocalName::from("");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_names() {
        assert_eq!(camel_to_data("fooBar").as_deref(), Some("data-foo-bar"));
        assert_eq!(camel_to_data("foo-bar"), None);
        assert_eq!(data_to_camel("data-foo-bar").as_deref(), Some("fooBar"));
        assert_eq!(data_to_camel("data-x").as_deref(), Some("x"));
    }
}

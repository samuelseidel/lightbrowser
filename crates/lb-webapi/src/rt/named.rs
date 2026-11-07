//! Named access as in Chrome 153 (ADR 0040).
//!
//! - **Window.** `WindowProperties`, the named properties object between
//!   `Window.prototype` and `EventTarget.prototype`, is defined by the
//!   installer; lb-js puts Blink's interceptors on its prototype object
//!   ([`lb_js::LegacyNamed`]: non-masking, so own and prototype members
//!   win; defines and deletes refused). Its getter ([`window_named`])
//!   answers, for a name: the child frame of that browsing context name
//!   (its `WindowProxy`), else the elements of the document tree with that
//!   `id` or the `img`, `form`, `embed` and `object` elements with that
//!   `name`: one element itself, several a live `HTMLCollection` (one per
//!   name). Elements in shadow trees, templates or detached subtrees do
//!   not count. Lookups use an index of the document by name, rebuilt when
//!   the document changed.
//! - **Document.** As Blink's `LocalWindowProxy::NamedItemAdded`/`Removed`,
//!   the window's (HTML) document gets an own native data property per
//!   registered name ([`on_mutations`]): added when the first element
//!   registers it (`form`, `iframe`, `embed`, `object` and `img` by
//!   `name`; `object` and named `img` by `id`), deleted when the last one
//!   leaves. So `Object.keys(document)` lists them in insertion order
//!   after `location`, assignments are ignored, and `delete` removes the
//!   property for good. Its value ([`document_named`]) follows Blink's
//!   `DocumentNameCollection` (exposed `embed`/`object` only): one element
//!   (an `iframe`'s content window instead), several an `HTMLCollection`,
//!   none the prototype chain's value.
//! - **Forms.** `HTMLFormElement`'s named getter (`[LegacyOverrideBuiltIns]`,
//!   [`form_named`]): listed elements by `id` or `name`, else `img`
//!   elements, else the past names map; several give a live
//!   `RadioNodeList` (one per form and name).

use std::collections::HashMap;
use std::rc::Rc;

use lb_dom::{Document, NodeId, local_name, ns};
use lb_js::{Arg, HostError, LegacyNamed, PropCtx};

use super::coll::{self, Filter};
use super::{Js, Runtime, Window};
use crate::registry::Registry;

/// Per-window named access state.
#[derive(Default)]
pub(crate) struct NamedState {
    /// Elements of the document tree by name, for the window, document and
    /// form getters, with the document generation it was built for.
    index: Option<(u64, Index)>,
    /// Names each element of the document registers (document named
    /// items), and how many elements register each name.
    contrib: HashMap<NodeId, Vec<String>>,
    counts: HashMap<String, u32>,
    /// Past names map of each form (slot, form) → name → element.
    past: HashMap<(u32, NodeId), HashMap<String, NodeId>>,
}

/// Elements of the document tree that have an `id` or a `name`, by each
/// (tree order, each element once per key).
type Index = HashMap<String, Vec<NodeId>>;

fn html_local(d: &Document, n: NodeId) -> Option<&'static str> {
    let q = d.qual_name(n)?;
    if q.ns != ns!(html) {
        return None;
    }
    Some(match &*q.local {
        "img" => "img",
        "form" => "form",
        "embed" => "embed",
        "object" => "object",
        "iframe" => "iframe",
        _ => return None,
    })
}

fn non_empty(v: Option<&str>) -> Option<&str> {
    v.filter(|s| !s.is_empty())
}

/// Blink's `HTMLObjectElement::IsExposed`: no exposed `object` ancestor
/// and no `object` or `embed` descendant.
fn object_exposed(d: &Document, n: NodeId) -> bool {
    if d.ancestors(n)
        .any(|a| html_local(d, a) == Some("object") && object_exposed(d, a))
    {
        return false;
    }
    !d.descendants(n)
        .any(|c| matches!(html_local(d, c), Some("object" | "embed")))
}

/// Blink's `HTMLEmbedElement::IsExposed`: no exposed `object` ancestor.
fn embed_exposed(d: &Document, n: NodeId) -> bool {
    !d.ancestors(n)
        .any(|a| html_local(d, a) == Some("object") && object_exposed(d, a))
}

/// Whether element `n` is a window named item for `name`.
pub(crate) fn window_matches(d: &Document, n: NodeId, name: &str) -> bool {
    if d.element_id(n) == Some(name) {
        return true;
    }
    matches!(html_local(d, n), Some("img" | "form" | "embed" | "object"))
        && d.get_attribute(n, "name") == Some(name)
}

/// Whether element `n` is a document named item for `name` (Blink's
/// `DocumentNameCollection::ElementMatches`).
pub(crate) fn document_matches(d: &Document, n: NodeId, name: &str) -> bool {
    let Some(tag) = html_local(d, n) else {
        return false;
    };
    let named = d.get_attribute(n, "name");
    match tag {
        "form" | "iframe" => named == Some(name),
        "embed" => named == Some(name) && embed_exposed(d, n),
        "object" => (named == Some(name) || d.element_id(n) == Some(name)) && object_exposed(d, n),
        "img" => {
            named == Some(name) || (d.element_id(n) == Some(name) && non_empty(named).is_some())
        }
        _ => false,
    }
}

fn build_index(d: &Document) -> Index {
    let mut ix = Index::default();
    for n in d.descendants(Document::DOCUMENT) {
        if !d.is_element(n) {
            continue;
        }
        let id = non_empty(d.element_id(n));
        if let Some(id) = id {
            ix.entry(id.to_owned()).or_default().push(n);
        }
        if let Some(name) = non_empty(d.get_attribute(n, "name"))
            && Some(name) != id
        {
            ix.entry(name.to_owned()).or_default().push(n);
        }
    }
    ix
}

/// Elements of `w`'s document tree with `id` or `name` `name`, in tree
/// order (from the index, rebuilt when the document changed).
fn candidates(w: &Window, name: &str) -> Vec<NodeId> {
    let generation = w.with_doc(|d| d.generation());
    let mut st = w.named.borrow_mut();
    if st.index.as_ref().is_none_or(|(g, _)| *g != generation) {
        let ix = w.with_doc(build_index);
        st.index = Some((generation, ix));
    }
    let ix = &st.index.as_ref().expect("built").1;
    ix.get(name).cloned().unwrap_or_default()
}

/// Window (`document` false) or document named items of `w`'s document
/// for `name`, in tree order.
fn items(w: &Window, name: &str, document: bool) -> Vec<NodeId> {
    let mut found = candidates(w, name);
    w.with_doc(|d| {
        found.retain(|&n| {
            if document {
                document_matches(d, n, name)
            } else {
                window_matches(d, n, name)
            }
        })
    });
    found
}

/// `window[name]` through the named properties object.
fn window_named(cx: &mut PropCtx<'_, '_, '_>, rt: &Runtime, name: &str) -> Option<Arg> {
    if name.is_empty() {
        return None;
    }
    let w = rt.window(cx.holder_realm())?;
    // A removed frame's window has no browsing context (Blink returns
    // nothing without a frame).
    if super::frames::is_detached(&w) {
        return None;
    }
    if let Some(v) = super::frames::child_by_name(&w, name, cx.realm()) {
        return Some(v);
    }
    if !w.with_doc(|d| d.is_html()) {
        return None;
    }
    let found = items(&w, name, false);
    match found.as_slice() {
        [] => None,
        [n] => Some(w.node_arg(0, *n)),
        _ => {
            let id = coll::collection(
                &w,
                0,
                Document::DOCUMENT,
                Filter::WindowNamed(name.to_owned()),
            );
            Some(w.obj_arg(id))
        }
    }
}

/// The value of document named item `name`: `None` when no element
/// matches now (the native property then reads the prototype chain).
fn document_named(cx: &mut PropCtx<'_, '_, '_>, rt: &Runtime, name: &str) -> Option<Arg> {
    let w = rt.window(cx.holder_realm())?;
    let found = items(&w, name, true);
    match found.as_slice() {
        [] => None,
        [n] => {
            let iframe = w.with_doc(|d| d.is_html_element_named(*n, &local_name!("iframe")));
            if iframe
                && let Some(child) = super::frames::child_for(&w, *n)
                && let Some(v) = super::frames::child_global(&child.window, cx.realm())
            {
                return Some(v);
            }
            Some(w.node_arg(0, *n))
        }
        _ => {
            let id = coll::collection(
                &w,
                0,
                Document::DOCUMENT,
                Filter::DocumentNamed(name.to_owned()),
            );
            Some(w.obj_arg(id))
        }
    }
}

/// Names element `n` registers as document named items, `id` first.
fn registered(d: &Document, n: NodeId) -> Vec<String> {
    let Some(tag) = html_local(d, n) else {
        return Vec::new();
    };
    let name = non_empty(d.get_attribute(n, "name"));
    let id = non_empty(d.element_id(n));
    let mut out = Vec::new();
    if let Some(id) = id
        && (tag == "object" || (tag == "img" && name.is_some()))
    {
        out.push(id.to_owned());
    }
    if let Some(name) = name
        && !out.iter().any(|x| x == name)
    {
        out.push(name.to_owned());
    }
    out
}

/// Keep the document's named item properties in step with a batch of
/// mutations (main document only, as Blink's main-world window proxy).
pub(crate) fn on_mutations(js: &mut dyn Js, w: &Rc<Window>, batch: &[(u32, lb_dom::Mutation)]) {
    use lb_dom::Mutation;
    let mut touched: Vec<NodeId> = Vec::new();
    let any_registered = !w.named.borrow().contrib.is_empty();
    w.with_doc(|d| {
        if !d.is_html() {
            return;
        }
        // Subtrees to walk: inserted ones now in the document tree (once:
        // not below another inserted node), removed ones only when some
        // element registered a name.
        let mut roots: Vec<NodeId> = Vec::new();
        let mut inserted = std::collections::HashSet::new();
        for (slot, m) in batch {
            if *slot != 0 {
                continue;
            }
            match m {
                Mutation::ChildInserted { node, .. } => {
                    if d.root(*node) == Document::DOCUMENT {
                        inserted.insert(*node);
                        roots.push(*node);
                    }
                }
                Mutation::ChildRemoved { node, .. } => {
                    if any_registered {
                        roots.push(*node);
                    }
                }
                Mutation::Attribute { element, name, .. }
                    if name.ns == ns!()
                        && (name.local == local_name!("id")
                            || name.local == local_name!("name"))
                        && html_local(d, *element).is_some() =>
                {
                    touched.push(*element);
                }
                _ => {}
            }
        }
        for r in roots {
            if d.ancestors(r).any(|a| inserted.contains(&a)) {
                continue;
            }
            touched.extend(
                d.inclusive_descendants(r)
                    .filter(|&n| html_local(d, n).is_some()),
            );
        }
    });
    if touched.is_empty() {
        return;
    }
    let (mut added, mut removed) = (Vec::new(), Vec::new());
    {
        let mut st = w.named.borrow_mut();
        let st = &mut *st;
        w.with_doc(|d| {
            for n in touched {
                let now = if d.root(n) == Document::DOCUMENT {
                    registered(d, n)
                } else {
                    Vec::new()
                };
                let old = st.contrib.remove(&n).unwrap_or_default();
                if old != now {
                    for name in old {
                        let c = st.counts.entry(name.clone()).or_default();
                        *c = c.saturating_sub(1);
                        if *c == 0 {
                            st.counts.remove(&name);
                            removed.push(name);
                        }
                    }
                    for name in &now {
                        let c = st.counts.entry(name.clone()).or_default();
                        *c += 1;
                        if *c == 1 {
                            added.push(name.clone());
                        }
                    }
                }
                if !now.is_empty() {
                    st.contrib.insert(n, now);
                }
            }
        });
        removed.retain(|n| !st.counts.contains_key(n));
        added.retain(|n| st.counts.contains_key(n));
    }
    if added.is_empty() && removed.is_empty() {
        return;
    }
    let rt = w.rt.clone();
    let getter = native_getter(js, &rt);
    let realm = w.realm();
    let iface = w.with_doc(|d| super::tags::node_interface(d, Document::DOCUMENT, 0));
    let ops = js.realm_ops();
    let Ok(doc) = ops.wrap(realm, rt.iface(iface), w.node_ref(0, Document::DOCUMENT)) else {
        return;
    };
    for name in removed {
        let _ = ops.delete_property(realm, doc, &name);
    }
    for name in added {
        let _ = ops.define_native_property(realm, doc, &name, getter);
    }
    ops.release(doc);
}

/// The engine's native getter for document named items (registered once).
fn native_getter(js: &mut dyn Js, rt: &Runtime) -> u32 {
    if let Some(id) = rt.0.named_getter.get() {
        return id;
    }
    let r = rt.clone();
    let id = js
        .realm_ops()
        .register_native_getter(Rc::new(move |cx, name| Ok(document_named(cx, &r, name))));
    rt.0.named_getter.set(Some(id));
    id
}

// ---------------------------------------------------------------------------
// Forms
// ---------------------------------------------------------------------------

/// The form owner of `n` (its `form` attribute's form, else the nearest
/// ancestor form).
pub(crate) fn form_owner(d: &Document, n: NodeId) -> Option<NodeId> {
    if let Some(id) = d.get_attribute(n, "form") {
        let root = d.root(n);
        return d
            .get_element_by_id_in(root, id)
            .filter(|&f| d.is_html_element_named(f, &local_name!("form")));
    }
    d.ancestors(n)
        .find(|&a| d.is_html_element_named(a, &local_name!("form")))
}

/// Listed elements of `form` (`form.elements`), in tree order.
pub(crate) fn listed_elements(d: &Document, form: NodeId) -> Vec<NodeId> {
    d.descendants(d.root(form))
        .filter(|&n| coll::is_listed(d, n) && form_owner(d, n) == Some(form))
        .collect()
}

fn id_or_name(d: &Document, n: NodeId, name: &str) -> bool {
    d.element_id(n) == Some(name) || d.get_attribute(n, "name") == Some(name)
}

/// Elements of `form` named `name`: listed ones, else `img`s. `among`:
/// the elements with that `id` or `name` in tree order, when known.
pub(crate) fn form_named_elements(
    d: &Document,
    form: NodeId,
    name: &str,
    among: Option<Vec<NodeId>>,
) -> Vec<NodeId> {
    if name.is_empty() {
        return Vec::new();
    }
    let among: Vec<NodeId> = match among {
        Some(v) => v,
        None => d
            .descendants(d.root(form))
            .filter(|&n| d.is_element(n) && id_or_name(d, n, name))
            .collect(),
    };
    let listed: Vec<NodeId> = among
        .iter()
        .copied()
        .filter(|&n| coll::is_listed(d, n) && form_owner(d, n) == Some(form))
        .collect();
    if !listed.is_empty() {
        return listed;
    }
    among
        .into_iter()
        .filter(|&n| {
            d.is_html_element_named(n, &local_name!("img"))
                && d.get_attribute(n, "form").is_none()
                && form_owner(d, n) == Some(form)
        })
        .collect()
}

/// [`form_named_elements`] through the window's index when the form is in
/// its document tree.
fn form_items(w: &Window, slot: u32, form: NodeId, name: &str) -> Vec<NodeId> {
    let indexed = slot == 0 && w.with_doc(|d| d.root(form) == Document::DOCUMENT);
    let among = indexed.then(|| candidates(w, name));
    w.doc_slot(slot, |d| form_named_elements(d, form, name, among))
}

/// The live `RadioNodeList` of `form` for `name` (one per form and name).
pub(crate) fn radio_node_list(w: &Window, slot: u32, form: NodeId, name: &str) -> u32 {
    coll::collection_as(
        w,
        slot,
        form,
        Filter::FormNamed(name.to_owned()),
        "RadioNodeList",
    )
}

/// `form[name]` (Blink's `HTMLFormElement::AnonymousNamedGetter` with its
/// past names map).
fn form_named(w: &Window, slot: u32, form: NodeId, name: &str) -> Option<Arg> {
    let found = form_items(w, slot, form, name);
    let mut st = w.named.borrow_mut();
    let past = st.past.entry((slot, form)).or_default();
    match found.first() {
        Some(&first) => {
            past.insert(name.to_owned(), first);
        }
        None => {
            let el = *past.get(name)?;
            // Entries leave the map when the element leaves the form.
            let still = w.doc_slot(slot, |d| {
                d.is_element(el)
                    && form_owner(d, el) == Some(form)
                    && (coll::is_listed(d, el) || d.is_html_element_named(el, &local_name!("img")))
            });
            if !still {
                past.remove(name);
                return None;
            }
            return Some(w.node_arg(slot, el));
        }
    }
    drop(st);
    if found.len() == 1 {
        return Some(w.node_arg(slot, found[0]));
    }
    Some(w.obj_arg(radio_node_list(w, slot, form, name)))
}

/// The form element an interceptor's holder is.
fn holder_form(cx: &mut PropCtx<'_, '_, '_>, rt: &Runtime) -> Option<(Rc<Window>, u32, NodeId)> {
    let w = rt.window(cx.holder_realm())?;
    let h = cx.holder_host()?;
    let (slot, form) = w.decode_node(h)?;
    Some((w, slot, form))
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    let r = rt.clone();
    reg.named_getter(
        crate::install::WINDOW_PROPERTIES,
        Rc::new(move |cx, name| Ok(window_named(cx, &r, name))),
    );

    let r = rt.clone();
    reg.named_getter(
        "HTMLFormElement",
        Rc::new(move |cx, name| -> Result<Option<Arg>, HostError> {
            let Some((w, slot, form)) = holder_form(cx, &r) else {
                return Ok(None);
            };
            Ok(form_named(&w, slot, form, name))
        }),
    );
    reg.named_unenumerable("HTMLFormElement");
    // `form[i]`: `form.elements[i]`.
    let r = rt.clone();
    reg.indexed_getter(
        "HTMLFormElement",
        Rc::new(move |cx, i| -> Result<Option<Arg>, HostError> {
            let Some((w, slot, form)) = holder_form(cx, &r) else {
                return Ok(None);
            };
            let id = coll::collection(&w, slot, form, Filter::FormControls);
            Ok(coll::items(&w, id)
                .and_then(|(slot, v)| v.get(i as usize).map(|&n| w.node_arg(slot, n))))
        }),
    );
    let r = rt.clone();
    reg.indexed_length(
        "HTMLFormElement",
        Rc::new(move |cx| {
            let Some((w, slot, form)) = holder_form(cx, &r) else {
                return 0;
            };
            let id = coll::collection(&w, slot, form, Filter::FormControls);
            coll::collection_len(&w, id) as u32
        }),
    );
    reg.named_legacy(
        "HTMLFormElement",
        LegacyNamed {
            iface: "HTMLFormElement",
            override_builtins: true,
            not_enumerable: true,
            properties_object: false,
        },
    );
}

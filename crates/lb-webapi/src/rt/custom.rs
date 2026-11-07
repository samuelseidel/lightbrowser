//! Custom elements (ADR 0026): `customElements`, definitions, the HTML
//! element constructors, upgrades, reactions, customized built-ins.
//!
//! - Definitions live in the window's [`CeState`]; an element's custom
//!   element state and `is` value live in `lb-dom` ([`CustomState`]).
//! - Reactions are queued per element and run when the operation that
//!   caused them ends (the page layer's `after_mutation`, which every
//!   DOM-changing binding calls), in element queue order, as Blink's
//!   `[CEReactions]` scopes do. Connected, disconnected and attribute
//!   reactions are derived from the DOM's mutation records; elements made
//!   by fragment parsing or cloning since the last check get their upgrade
//!   reaction first (they are created before they are inserted).
//! - The document parser pauses after each possible custom element once a
//!   definition exists ([`lb_dom::ParseStep::CustomElement`]); the element
//!   is constructed then as if before its attributes and children were
//!   added (they are taken off silently and put back after the
//!   constructor, firing `attributeChangedCallback` and
//!   `connectedCallback` as in Chrome).
//! - Only the main document has a registry (other documents have no
//!   browsing context), but custom elements moved to other documents keep
//!   their callbacks.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use lb_dom::{CustomState, Document, Mutation, NodeFlags, NodeId, QualName, ns};
use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, JsError, Resolver, ValueKind, v8};

use super::{
    Dict, Js, Obj, Runtime, WINDOW_REF, Window, ctor, getter, illegal, method, need_args, this_obj,
};
use crate::registry::Registry;

/// Element flag: the `HTMLUnknownElement` made when constructing a custom
/// element synchronously failed.
pub(crate) const CE_UNKNOWN: NodeFlags = NodeFlags::USER_1;

/// One definition.
pub(crate) struct CeDef {
    pub name: String,
    pub local: String,
    pub ctor: Handle,
    pub observed: Vec<String>,
    pub connected: Option<Handle>,
    pub disconnected: Option<Handle>,
    pub adopted: Option<Handle>,
    pub attribute_changed: Option<Handle>,
    pub form_associated_cb: Option<Handle>,
    pub form_reset: Option<Handle>,
    pub form_disabled: Option<Handle>,
    /// Never called: no form state restoration (session history).
    #[allow(dead_code)]
    pub form_state_restore: Option<Handle>,
    pub form_associated: bool,
    pub disable_internals: bool,
    pub disable_shadow: bool,
    /// Construction stack: elements being upgraded (`None`: the "already
    /// constructed" marker).
    stack: Vec<Option<NodeId>>,
}

/// A custom element reaction.
pub(crate) enum Reaction {
    Upgrade,
    Connected,
    Disconnected,
    Adopted(HostRef, HostRef),
    Attr {
        name: String,
        old: Option<String>,
        new: Option<String>,
        ns: Option<String>,
    },
    FormAssociated(Option<NodeId>),
    FormReset,
    FormDisabled(bool),
}

/// A registry (`customElements` or `new CustomElementRegistry()`).
#[derive(Default)]
pub(crate) struct Reg {
    pub defs: Vec<CeDef>,
    when: Vec<(String, Handle, Resolver)>,
    defining: bool,
}

/// The window's custom element state.
#[derive(Default)]
pub(crate) struct CeState {
    /// Registry 0 is the window's; others come from the constructor.
    pub regs: Vec<Reg>,
    queues: HashMap<(u32, NodeId), VecDeque<Reaction>>,
    element_queue: Vec<(u32, NodeId)>,
    /// Main-document nodes below this id were already looked at.
    watermark: usize,
}

impl CeState {
    fn main(&mut self) -> &mut Reg {
        if self.regs.is_empty() {
            self.regs.push(Reg::default());
        }
        &mut self.regs[0]
    }
}

/// True once the window's registry has a definition (nothing else here
/// costs anything before that).
pub(crate) fn active(w: &Window) -> bool {
    w.ce.borrow()
        .regs
        .first()
        .is_some_and(|r| !r.defs.is_empty())
}

/// The definition index for an element named `local` with `is` value.
fn lookup(w: &Window, local: &str, is: Option<&str>) -> Option<usize> {
    let st = w.ce.borrow();
    let reg = st.regs.first()?;
    reg.defs.iter().position(|d| {
        (d.name == local && d.local == local) || (Some(d.name.as_str()) == is && d.local == local)
    })
}

/// The definition of element `n` of slot `slot` (custom or not).
fn def_of(w: &Window, slot: u32, n: NodeId) -> Option<usize> {
    let (local, is) = w.doc_slot(slot, |d| {
        let e = d.element(n)?;
        if e.name.ns != ns!(html) {
            return None;
        }
        Some((
            e.name.local.to_string(),
            e.is_value.as_ref().map(|v| v.to_string()),
        ))
    })?;
    lookup(w, &local, is.as_deref())
}

/// What `attachInternals` needs from an element's definition.
pub(crate) struct DefInfo {
    pub form_associated: bool,
    pub disable_internals: bool,
}

/// The definition of element `n`, if it has one.
pub(crate) fn definition_of(w: &Window, slot: u32, n: NodeId) -> Option<DefInfo> {
    let i = def_of(w, slot, n)?;
    let st = w.ce.borrow();
    let d = &st.regs[0].defs[i];
    Some(DefInfo {
        form_associated: d.form_associated,
        disable_internals: d.disable_internals,
    })
}

/// True if `n`'s definition disables shadow roots (`disabledFeatures`).
pub(crate) fn shadow_disabled(w: &Window, slot: u32, n: NodeId) -> bool {
    def_of(w, slot, n).is_some_and(|i| w.ce.borrow().regs[0].defs[i].disable_shadow)
}

fn enqueue(w: &Window, key: (u32, NodeId), r: Reaction) {
    let mut st = w.ce.borrow_mut();
    st.queues.entry(key).or_default().push_back(r);
    if !st.element_queue.contains(&key) {
        st.element_queue.push(key);
    }
}

/// Enqueue a lifecycle callback reaction if `n` is custom and its
/// definition has that callback.
pub(crate) fn enqueue_callback(w: &Window, slot: u32, n: NodeId, r: Reaction) {
    if w.doc_slot(slot, |d| d.custom_state(n)) != CustomState::Custom {
        return;
    }
    let Some(i) = def_of(w, slot, n) else { return };
    let wanted = {
        let st = w.ce.borrow();
        let def = &st.regs[0].defs[i];
        match &r {
            Reaction::Connected => def.connected.is_some(),
            Reaction::Disconnected => def.disconnected.is_some(),
            Reaction::Adopted(..) => def.adopted.is_some(),
            Reaction::Attr { name, .. } => {
                def.attribute_changed.is_some() && def.observed.iter().any(|o| o == name)
            }
            Reaction::FormAssociated(_) => def.form_associated_cb.is_some(),
            Reaction::FormReset => def.form_reset.is_some(),
            Reaction::FormDisabled(_) => def.form_disabled.is_some(),
            Reaction::Upgrade => true,
        }
    };
    if wanted {
        enqueue(w, (slot, n), r);
    }
}

/// "Try to upgrade": queue an upgrade if a definition exists.
fn try_upgrade(w: &Window, slot: u32, n: NodeId) {
    if slot != 0 || w.doc_slot(slot, |d| d.custom_state(n)) != CustomState::Undefined {
        return;
    }
    if def_of(w, slot, n).is_some() {
        enqueue(w, (slot, n), Reaction::Upgrade);
    }
}

/// `Reflect.construct(ctor, [])`.
fn construct(js: &mut dyn Js, w: &Window, ctor: Handle) -> Result<Handle, JsError> {
    let f = w.helpers.borrow().reflect_construct;
    let Some(f) = f else {
        return Err(JsError::new(lb_js::JsErrorKind::Exception, "no Reflect"));
    };
    js.call_handle(
        f,
        Arg::Undefined,
        &[Arg::Handle(ctor), Arg::Array(Vec::new())],
    )
}

/// The custom element "upgrade" of `n` (reports its exceptions).
fn upgrade(js: &mut dyn Js, w: &Rc<Window>, slot: u32, n: NodeId) {
    let state = w.doc_slot(slot, |d| d.custom_state(n));
    if !matches!(state, CustomState::Undefined | CustomState::Uncustomized) {
        return;
    }
    let Some(i) = def_of(w, slot, n) else { return };
    w.doc_slot_mut(slot, |d| d.set_custom_state(n, CustomState::Failed));
    // Attribute and connected reactions run after the constructor.
    let (attrs, connected) = w.doc_slot(slot, |d| {
        let attrs: Vec<(String, String, Option<String>)> = d
            .attributes(n)
            .iter()
            .map(|a| {
                (
                    a.name.local.to_string(),
                    a.value.to_string(),
                    (!a.name.ns.is_empty()).then(|| a.name.ns.to_string()),
                )
            })
            .collect();
        (attrs, d.is_connected(n))
    });
    w.doc_slot_mut(slot, |d| d.set_custom_state(n, CustomState::Custom));
    for (name, value, ns) in attrs {
        enqueue_callback(
            w,
            slot,
            n,
            Reaction::Attr {
                name,
                old: None,
                new: Some(value),
                ns,
            },
        );
    }
    if connected {
        enqueue_callback(w, slot, n, Reaction::Connected);
    }
    w.doc_slot_mut(slot, |d| d.set_custom_state(n, CustomState::Precustomized));
    let ctor_h = {
        let mut st = w.ce.borrow_mut();
        let def = &mut st.regs[0].defs[i];
        def.stack.push(Some(n));
        def.ctor
    };
    let r = construct(js, w, ctor_h);
    {
        let mut st = w.ce.borrow_mut();
        st.regs[0].defs[i].stack.pop();
    }
    match r {
        Ok(h) => {
            let same = js.host_of(h).and_then(|x| w.decode_node(x)) == Some((slot, n));
            js.release(h);
            if same {
                w.doc_slot_mut(slot, |d| d.set_custom_state(n, CustomState::Custom));
                if w.ce.borrow().regs[0].defs[i].form_associated {
                    super::internals::upgraded(js, w, slot, n);
                }
            } else {
                fail(w, slot, n);
                report_new_error(
                    js,
                    w,
                    None,
                    "Failed to execute 'upgrade' on 'CustomElementRegistry': custom element constructors must call super() first and must not return a different object",
                );
            }
        }
        Err(e) => {
            fail(w, slot, n);
            super::events::report_exception(js, w, &e);
        }
    }
}

fn fail(w: &Window, slot: u32, n: NodeId) {
    w.doc_slot_mut(slot, |d| d.set_custom_state(n, CustomState::Failed));
    w.ce.borrow_mut().queues.remove(&(slot, n));
}

/// Report a new `TypeError` (or a `DOMException` named `dom`) with
/// `msg`, as an uncaught exception of the page.
fn report_new_error(js: &mut dyn Js, w: &Rc<Window>, dom: Option<&str>, msg: &str) {
    let (made, head) = match dom {
        None => {
            let (rc, te) = {
                let h = w.helpers.borrow();
                (h.reflect_construct, h.type_error)
            };
            let made = match (rc, te) {
                (Some(rc), Some(te)) => js
                    .call_handle(
                        rc,
                        Arg::Undefined,
                        &[
                            Arg::Handle(te),
                            Arg::Array(vec![Arg::String(msg.to_owned())]),
                        ],
                    )
                    .ok(),
                _ => None,
            };
            (made, format!("TypeError: {msg}"))
        }
        Some(name) => {
            let v = Arg::Construct(
                w.rt.iface("DOMException"),
                vec![Arg::String(msg.to_owned()), Arg::String(name.to_owned())],
            );
            (js.make(v), format!("{name}: {msg}"))
        }
    };
    match made {
        Some(h) => {
            super::events::report_error_value(js, w, &head, h);
            js.release(h);
        }
        None => {
            let e = JsError::new(lb_js::JsErrorKind::Exception, head);
            super::events::report_exception(js, w, &e);
        }
    }
}

/// Run queued reactions (the end of a `[CEReactions]` operation).
pub(crate) fn process(js: &mut dyn Js, w: &Rc<Window>) {
    for _ in 0..10_000 {
        let q = std::mem::take(&mut w.ce.borrow_mut().element_queue);
        if q.is_empty() {
            return;
        }
        for key in q {
            loop {
                let r = {
                    let mut st = w.ce.borrow_mut();
                    match st.queues.get_mut(&key) {
                        Some(v) => match v.pop_front() {
                            Some(r) => r,
                            None => {
                                st.queues.remove(&key);
                                break;
                            }
                        },
                        None => break,
                    }
                };
                run(js, w, key, r);
            }
        }
    }
}

fn run(js: &mut dyn Js, w: &Rc<Window>, (slot, n): (u32, NodeId), r: Reaction) {
    if let Reaction::Upgrade = r {
        upgrade(js, w, slot, n);
        return;
    }
    let Some(i) = def_of(w, slot, n) else { return };
    let (cb, args) = {
        let st = w.ce.borrow();
        let def = &st.regs[0].defs[i];
        let opt = |s: Option<String>| s.map_or(Arg::Null, Arg::String);
        match r {
            Reaction::Upgrade => unreachable!(),
            Reaction::Connected => (def.connected, Vec::new()),
            Reaction::Disconnected => (def.disconnected, Vec::new()),
            Reaction::Adopted(a, b) => (def.adopted, vec![w.host_arg(a), w.host_arg(b)]),
            Reaction::Attr { name, old, new, ns } => (
                def.attribute_changed,
                vec![Arg::String(name), opt(old), opt(new), opt(ns)],
            ),
            Reaction::FormAssociated(f) => (
                def.form_associated_cb,
                vec![f.map_or(Arg::Null, |f| w.node_arg(slot, f))],
            ),
            Reaction::FormReset => (def.form_reset, Vec::new()),
            Reaction::FormDisabled(b) => (def.form_disabled, vec![Arg::Bool(b)]),
        }
    };
    let Some(cb) = cb else { return };
    let this = w.node_arg(slot, n);
    if let Err(e) = js.call(cb, this, &args) {
        super::events::report_exception(js, w, &e);
    }
}

// ---------------------------------------------------------------------------
// Mutations → reactions
// ---------------------------------------------------------------------------

/// Reactions for a batch of DOM changes, then run them. Elements the
/// parser made that may be custom are constructed first.
pub(crate) fn on_mutations(js: &mut dyn Js, w: &Rc<Window>, batch: &[(u32, Mutation)]) {
    if !active(w) {
        return;
    }
    // Parser-made elements (the parser paused right after each).
    let cands = w
        .parser
        .try_borrow_mut()
        .ok()
        .and_then(|mut p| p.as_mut().map(|p| p.take_custom_candidates()))
        .unwrap_or_default();
    // Each element gets one connected reaction per batch: the batch is
    // read after the fact, so a node inserted into a subtree that was
    // itself inserted in this batch shows up twice.
    let mut connected: std::collections::HashSet<(u32, NodeId)> = std::collections::HashSet::new();
    for n in cands {
        if construct_parsed(js, w, n) {
            connected.insert((0, n));
        }
    }
    // Elements made since the last look (fragment parsing, cloning):
    // their upgrade reactions come first, in creation order.
    let fresh: Vec<NodeId> = w.with_doc(|d| {
        let start = w.ce.borrow().watermark;
        let end = d.node_count();
        w.ce.borrow_mut().watermark = end;
        (start..end)
            .map(|i| NodeId(i as u32))
            .filter(|&n| {
                d.custom_state(n) == CustomState::Undefined
                    && !d.flags(n).contains(NodeFlags::PARSER_INSERTED)
                    && !d.in_template_contents(n)
            })
            .collect()
    });
    for n in fresh {
        try_upgrade(w, 0, n);
    }
    for (slot, m) in batch {
        let slot = *slot;
        match m {
            Mutation::ChildInserted { node, .. } => {
                let nodes = w.doc_slot(slot, |d| {
                    if !d.is_connected(*node) {
                        return Vec::new();
                    }
                    let mut v = vec![*node];
                    v.extend(d.shadow_including_descendants(*node));
                    v.retain(|&x| d.is_element(x));
                    v
                });
                for x in nodes {
                    if !connected.insert((slot, x)) {
                        continue;
                    }
                    let st = w.doc_slot(slot, |d| d.custom_state(x));
                    if st == CustomState::Custom {
                        enqueue_callback(w, slot, x, Reaction::Connected);
                    } else {
                        try_upgrade(w, slot, x);
                    }
                }
            }
            Mutation::ChildRemoved { parent, node, .. } => {
                let nodes = w.doc_slot(slot, |d| {
                    if !d.is_connected(*parent) {
                        return Vec::new();
                    }
                    let mut v = vec![*node];
                    v.extend(d.shadow_including_descendants(*node));
                    v.retain(|&x| d.custom_state(x) == CustomState::Custom);
                    v
                });
                for x in nodes {
                    enqueue_callback(w, slot, x, Reaction::Disconnected);
                    connected.remove(&(slot, x));
                }
            }
            Mutation::Attribute {
                element,
                name,
                old_value,
            } => {
                if w.doc_slot(slot, |d| d.custom_state(*element)) != CustomState::Custom {
                    continue;
                }
                let new = w.doc_slot(slot, |d| {
                    d.get_attribute_ns(*element, &name.ns, &name.local)
                        .map(str::to_owned)
                });
                enqueue_callback(
                    w,
                    slot,
                    *element,
                    Reaction::Attr {
                        name: name.local.to_string(),
                        old: old_value.as_ref().map(|v| v.to_string()),
                        new,
                        ns: (!name.ns.is_empty()).then(|| name.ns.to_string()),
                    },
                );
            }
            Mutation::CharacterData { .. } => {}
        }
    }
    super::internals::on_mutations(w, batch);
    process(js, w);
}

/// Adoption of a subtree into another document (`bring`): disconnected
/// (if it was connected) and adopted reactions under the copies' ids;
/// the old copies stop being custom.
pub(crate) fn adopted(
    w: &Window,
    from: u32,
    to: u32,
    map: &[(NodeId, NodeId)],
    was_connected: bool,
) {
    if !active(w) {
        return;
    }
    let (old_doc, new_doc) = (
        w.node_ref(from, Document::DOCUMENT),
        w.node_ref(to, Document::DOCUMENT),
    );
    for &(o, n) in map {
        if w.doc_slot(to, |d| d.custom_state(n)) != CustomState::Custom {
            continue;
        }
        w.doc_slot_mut(from, |d| d.set_custom_state(o, CustomState::Uncustomized));
        if was_connected {
            enqueue_callback(w, to, n, Reaction::Disconnected);
        }
        enqueue_callback(w, to, n, Reaction::Adopted(old_doc, new_doc));
    }
}

/// Construct a parser-made element as the HTML parser does ("create an
/// element for a token" with synchronous custom elements): the
/// constructor sees no attributes, children or parent.
/// Returns true if it was constructed (its connected reaction is queued).
fn construct_parsed(js: &mut dyn Js, w: &Rc<Window>, n: NodeId) -> bool {
    let ok =
        w.with_doc(|d| d.custom_state(n) == CustomState::Undefined && !d.in_template_contents(n));
    if !ok || def_of(w, 0, n).is_none() {
        return false;
    }
    let stripped = w.with_doc_mut(|d| d.strip_silently(n));
    upgrade(js, w, 0, n);
    let attrs = w.with_doc_mut(|d| d.restore_silently(n, stripped));
    if w.with_doc(|d| d.custom_state(n)) != CustomState::Custom {
        return true;
    }
    for a in &attrs {
        enqueue_callback(
            w,
            0,
            n,
            Reaction::Attr {
                name: a.name.local.to_string(),
                old: None,
                new: Some(a.value.to_string()),
                ns: (!a.name.ns.is_empty()).then(|| a.name.ns.to_string()),
            },
        );
    }
    if w.with_doc(|d| d.is_connected(n)) {
        enqueue_callback(w, 0, n, Reaction::Connected);
    }
    process(js, w);
    true
}

/// Called when `customElements.define` adds a definition, or before an
/// operation that makes elements itself: elements made before now are
/// not "fresh".
fn advance_watermark(w: &Window) {
    let n = w.with_doc(|d| d.node_count());
    w.ce.borrow_mut().watermark = n;
}

// ---------------------------------------------------------------------------
// Element creation (document.createElement)
// ---------------------------------------------------------------------------

/// `createElement(local, {is})` for HTML elements of the main document:
/// a defined autonomous element is constructed synchronously, a
/// customized built-in upgraded synchronously. `None`: no definition (the
/// caller makes a plain element).
pub(crate) fn create_element(
    js: &mut dyn Js,
    w: &Rc<Window>,
    slot: u32,
    local: &str,
    is: Option<&str>,
) -> Option<NodeId> {
    if slot != 0 || !active(w) {
        return None;
    }
    let i = lookup(w, local, is)?;
    let (ctor_h, autonomous) = {
        let st = w.ce.borrow();
        let d = &st.regs[0].defs[i];
        (d.ctor, d.name == d.local)
    };
    if autonomous {
        let r = construct(js, w, ctor_h);
        // The checks of "create an element" on the constructed result.
        let made: Result<NodeId, Option<(Option<&str>, &str)>> = match r {
            Ok(h) => {
                let node = js.host_of(h).and_then(|x| w.decode_node(x));
                js.release(h);
                match node {
                    Some((0, x)) if w.with_doc(|d| d.is_element(x)) => {
                        let problem = w.with_doc(|d| {
                            let q = d.qual_name(x).expect("element");
                            if q.ns != ns!(html) {
                                Some((None, "The result must implement HTMLElement interface"))
                            } else if !d.attributes(x).is_empty() {
                                Some((
                                    Some("NotSupportedError"),
                                    "The result must not have attributes",
                                ))
                            } else if d.first_child(x).is_some() {
                                Some((
                                    Some("NotSupportedError"),
                                    "The result must not have children",
                                ))
                            } else if d.parent(x).is_some() {
                                Some((
                                    Some("NotSupportedError"),
                                    "The result must not have a parent",
                                ))
                            } else if *q.local != *local {
                                Some((
                                    Some("NotSupportedError"),
                                    "The result must have the same localName",
                                ))
                            } else {
                                None
                            }
                        });
                        match problem {
                            None => Ok(x),
                            Some(p) => Err(Some(p)),
                        }
                    }
                    Some((_, _)) => Err(Some((
                        Some("NotSupportedError"),
                        "The result must be in the same document",
                    ))),
                    None => Err(Some((
                        None,
                        "The result must implement HTMLElement interface",
                    ))),
                }
            }
            Err(e) => {
                super::events::report_exception(js, w, &e);
                Err(None)
            }
        };
        advance_watermark(w);
        return Some(match made {
            Ok(x) => x,
            Err(problem) => {
                if let Some((dom, msg)) = problem {
                    report_new_error(
                        js,
                        w,
                        dom,
                        &format!("Failed to execute 'createElement' on 'Document': {msg}"),
                    );
                }
                let x = w.with_doc_mut(|d| {
                    let x =
                        d.create_element(QualName::new(None, ns!(html), local.into()), Vec::new());
                    d.set_custom_state(x, CustomState::Failed);
                    d.set_flags(x, CE_UNKNOWN, true);
                    x
                });
                advance_watermark(w);
                x
            }
        });
    }
    let x = w.with_doc_mut(|d| {
        let x = d.create_element(QualName::new(None, ns!(html), local.into()), Vec::new());
        d.set_is_value(x, is);
        d.set_custom_state(x, CustomState::Undefined);
        x
    });
    advance_watermark(w);
    upgrade(js, w, 0, x);
    Some(x)
}

// ---------------------------------------------------------------------------
// The HTML element constructors
// ---------------------------------------------------------------------------

/// `new HTMLElement()` / `super()` from a custom element class (the HTML
/// element constructor steps); `iface` is the interface whose constructor
/// runs.
fn html_ctor(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    iface: &'static str,
) -> Result<(), HostError> {
    let illegal_ctor = || {
        HostError::type_error(format!(
            "Failed to construct '{iface}': Illegal constructor"
        ))
    };
    let nt = cx.v8_args().new_target();
    let nt = cx.handle_of(nt);
    let found = {
        let st = w.ce.borrow();
        st.regs
            .first()
            .map(|r| r.defs.iter().map(|d| d.ctor).collect::<Vec<_>>())
    }
    .unwrap_or_default();
    let mut idx = None;
    for (i, c) in found.into_iter().enumerate() {
        if cx.strict_equals(nt, c) {
            idx = Some(i);
            break;
        }
    }
    cx.release(nt);
    let Some(i) = idx else {
        return Err(illegal_ctor());
    };
    let (name, local, top) = {
        let st = w.ce.borrow();
        let d = &st.regs[0].defs[i];
        (d.name.clone(), d.local.clone(), d.stack.last().copied())
    };
    let autonomous = name == local;
    if autonomous {
        if iface != "HTMLElement" {
            return Err(illegal_ctor());
        }
    } else if super::tags::html_interface(&local) != iface {
        return Err(illegal_ctor());
    }
    match top {
        None => {
            // A new element (`new C()`, synchronous construction).
            let n = w.with_doc_mut(|d| {
                let n = d.create_element(
                    QualName::new(None, ns!(html), local.as_str().into()),
                    Vec::new(),
                );
                d.set_custom_state(n, CustomState::Custom);
                if !autonomous {
                    d.set_is_value(n, Some(&name));
                }
                n
            });
            advance_watermark(w);
            if w.ce.borrow().regs[0].defs[i].form_associated {
                super::internals::upgraded(cx, w, 0, n);
            }
            cx.init_this(w.node_ref(0, n))
        }
        Some(None) => Err(HostError::type_error(format!(
            "Failed to construct '{iface}': This instance is already constructed"
        ))),
        Some(Some(n)) => {
            {
                let mut st = w.ce.borrow_mut();
                if let Some(t) = st.regs[0].defs[i].stack.last_mut() {
                    *t = None;
                }
            }
            cx.ret_constructed(w.node_arg(0, n))
        }
    }
}

// ---------------------------------------------------------------------------
// CustomElementRegistry
// ---------------------------------------------------------------------------

fn this_reg(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<usize, HostError> {
    match cx.this_host() {
        Some(h) if h.kind == super::K_OBJ => w
            .with_obj(h.id, |o| match o {
                Obj::CustomElementRegistry(i) => Some(*i as usize),
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal),
        _ => Err(illegal()),
    }
}

/// `customElements` of `w` (registry 0).
fn registry_obj(w: &Window) -> u32 {
    w.ce.borrow_mut().main();
    w.cached_obj((0, 0, "customElements", String::new()), || {
        Obj::CustomElementRegistry(0)
    })
}

/// Get a callable member of `obj` (WebIDL callback conversion): `None`
/// when undefined, a TypeError when not callable.
fn callback_member(
    cx: &mut CallCtx<'_, '_, '_>,
    obj: Handle,
    key: &str,
) -> Result<Option<Handle>, HostError> {
    let v = cx.get_property(obj, key)?;
    match cx.handle_kind(v) {
        ValueKind::Undefined => {
            cx.release(v);
            Ok(None)
        }
        ValueKind::Function => Ok(Some(v)),
        _ => {
            cx.release(v);
            Err(HostError::type_error(format!(
                "Failed to execute 'define' on 'CustomElementRegistry': The '{key}' property of the prototype is not a function."
            )))
        }
    }
}

/// A `sequence<DOMString>` member of the constructor (`observedAttributes`,
/// `disabledFeatures`): read through the iterator protocol.
fn string_sequence(
    cx: &mut CallCtx<'_, '_, '_>,
    obj: Handle,
    key: &str,
) -> Result<Vec<String>, HostError> {
    let v = cx.get_property(obj, key)?;
    if cx.handle_kind(v) == ValueKind::Undefined {
        cx.release(v);
        return Ok(Vec::new());
    }
    let local = cx.local_of(v);
    let out = sequence_strings(cx.scope(), local);
    cx.release(v);
    out.ok_or_else(|| {
        HostError::type_error(format!(
            "Failed to execute 'define' on 'CustomElementRegistry': Failed to read the '{key}' property from 'Function': The provided value cannot be converted to a sequence."
        ))
    })
}

/// Strings of an iterable (`Array.from(v, String)`), `None` if `v` is not
/// iterable. Exceptions stay pending in the scope.
fn sequence_strings(
    scope: &mut v8::PinScope<'_, '_>,
    v: Option<v8::Local<'_, v8::Value>>,
) -> Option<Vec<String>> {
    let v = v?;
    let obj = v8::Local::<v8::Object>::try_from(v).ok()?;
    let sym = v8::Symbol::get_iterator(scope);
    let iter_fn = obj.get(scope, sym.into())?;
    let iter_fn = v8::Local::<v8::Function>::try_from(iter_fn).ok()?;
    let it = iter_fn.call(scope, obj.into(), &[])?;
    let it = v8::Local::<v8::Object>::try_from(it).ok()?;
    let next_key = v8::String::new(scope, "next")?;
    let next = it.get(scope, next_key.into())?;
    let next = v8::Local::<v8::Function>::try_from(next).ok()?;
    let done_key = v8::String::new(scope, "done")?;
    let value_key = v8::String::new(scope, "value")?;
    let mut out = Vec::new();
    for _ in 0..100_000 {
        let r = next.call(scope, it.into(), &[])?;
        let r = v8::Local::<v8::Object>::try_from(r).ok()?;
        let done = r.get(scope, done_key.into())?;
        if done.boolean_value(scope) {
            return Some(out);
        }
        let val = r.get(scope, value_key.into())?;
        let s = val.to_string(scope)?;
        out.push(s.to_rust_string_lossy(scope));
    }
    Some(out)
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Window", "customElements", |cx, w| {
        let id = registry_obj(w);
        cx.ret(w.obj_arg(id))
    });
    ctor(reg, rt, "CustomElementRegistry", |cx, w| {
        let i = {
            let mut st = w.ce.borrow_mut();
            st.main();
            st.regs.push(Reg::default());
            st.regs.len() - 1
        };
        let id = w.new_obj(Obj::CustomElementRegistry(i as u32));
        cx.init_this(HostRef::new(super::K_OBJ, id))
    });
    method(reg, rt, "CustomElementRegistry", "define", |cx, w| {
        const OP: &str = "Failed to execute 'define' on 'CustomElementRegistry'";
        need_args(cx, 2, "define", "CustomElementRegistry")?;
        let r = this_reg(cx, w)?;
        let name = cx.arg_string(0)?;
        if cx.arg_kind(1) != ValueKind::Function {
            return Err(HostError::type_error(format!(
                "{OP}: parameter 2 is not of type 'Function'."
            )));
        }
        let is_ctor = {
            let v = cx.v8_args().get(1);
            v8::Local::<v8::Object>::try_from(v).is_ok_and(|o| o.is_constructor())
        };
        if !is_ctor {
            return Err(HostError::type_error(format!(
                "{OP}: constructor argument is not a constructor"
            )));
        }
        let ctor_h = cx.arg_handle(1);
        let not_supported =
            |w: &Window, m: String| w.rt.err("NotSupportedError", format!("{OP}: {m}"));
        if !lb_dom::is_valid_custom_element_name(&name) {
            return Err(w.rt.err(
                "SyntaxError",
                format!("{OP}: \"{name}\" is not a valid custom element name"),
            ));
        }
        let (dup_name, ctors) = {
            let st = w.ce.borrow();
            let reg = &st.regs[r];
            (
                reg.defs.iter().any(|d| d.name == name),
                reg.defs.iter().map(|d| d.ctor).collect::<Vec<_>>(),
            )
        };
        if dup_name {
            return Err(not_supported(
                w,
                format!("the name \"{name}\" has already been used with this registry"),
            ));
        }
        for c in ctors {
            if cx.strict_equals(c, ctor_h) {
                return Err(not_supported(
                    w,
                    "this constructor has already been used with this registry".into(),
                ));
            }
        }
        let opts = Dict::arg(cx, 2);
        let extends = opts.string(cx, "extends")?;
        opts.release(cx);
        let mut local = name.clone();
        if let Some(e) = extends {
            if lb_dom::is_valid_custom_element_name(&e) {
                return Err(not_supported(
                    w,
                    format!("\"{e}\" is a valid custom element name"),
                ));
            }
            if super::tags::html_interface(&e) == "HTMLUnknownElement" {
                return Err(not_supported(
                    w,
                    format!("\"{e}\" is an HTMLUnknownElement"),
                ));
            }
            local = e;
        }
        if w.ce.borrow().regs[r].defining {
            return Err(not_supported(
                w,
                "this registry is already defining an element".into(),
            ));
        }
        w.ce.borrow_mut().regs[r].defining = true;
        let read = (|| -> Result<CeDef, HostError> {
            let proto = cx.get_property(ctor_h, "prototype")?;
            if !matches!(
                cx.handle_kind(proto),
                ValueKind::Object | ValueKind::Function
            ) {
                cx.release(proto);
                return Err(HostError::type_error(format!(
                    "{OP}: constructor prototype is not an object"
                )));
            }
            let connected = callback_member(cx, proto, "connectedCallback")?;
            let disconnected = callback_member(cx, proto, "disconnectedCallback")?;
            let adopted = callback_member(cx, proto, "adoptedCallback")?;
            let attribute_changed = callback_member(cx, proto, "attributeChangedCallback")?;
            let observed = if attribute_changed.is_some() {
                string_sequence(cx, ctor_h, "observedAttributes")?
            } else {
                Vec::new()
            };
            let disabled = string_sequence(cx, ctor_h, "disabledFeatures")?;
            let fa = cx.get_property(ctor_h, "formAssociated")?;
            let form_associated = super::truthy(&cx.handle_value(fa));
            cx.release(fa);
            let (mut a, mut b, mut c, mut d) = (None, None, None, None);
            if form_associated {
                a = callback_member(cx, proto, "formAssociatedCallback")?;
                b = callback_member(cx, proto, "formResetCallback")?;
                c = callback_member(cx, proto, "formDisabledCallback")?;
                d = callback_member(cx, proto, "formStateRestoreCallback")?;
            }
            cx.release(proto);
            Ok(CeDef {
                name: name.clone(),
                local: local.clone(),
                ctor: ctor_h,
                observed,
                connected,
                disconnected,
                adopted,
                attribute_changed,
                form_associated_cb: a,
                form_reset: b,
                form_disabled: c,
                form_state_restore: d,
                form_associated,
                disable_internals: disabled.iter().any(|f| f == "internals"),
                disable_shadow: disabled.iter().any(|f| f == "shadow"),
                stack: Vec::new(),
            })
        })();
        w.ce.borrow_mut().regs[r].defining = false;
        let def = read?;
        let when = {
            let mut st = w.ce.borrow_mut();
            st.regs[r].defs.push(def);
            let pos = st.regs[r].when.iter().position(|(n, _, _)| *n == name);
            pos.map(|p| st.regs[r].when.remove(p))
        };
        if r == 0 {
            // Upgrade candidates in shadow-including tree order.
            let cands: Vec<NodeId> = w.with_doc(|d| {
                d.shadow_including_descendants(Document::DOCUMENT)
                    .into_iter()
                    .filter(|&n| {
                        d.custom_state(n) == CustomState::Undefined
                            && d.qual_name(n)
                                .is_some_and(|q| q.ns == ns!(html) && *q.local == *local)
                            && (local == name || d.is_value(n) == Some(name.as_str()))
                    })
                    .collect()
            });
            advance_watermark(w);
            for n in cands {
                enqueue(w, (0, n), Reaction::Upgrade);
            }
            if let Some(p) = w.parser.borrow_mut().as_mut() {
                p.set_pause_at_custom(true);
            }
        }
        if let Some((_, h, res)) = when {
            cx.resolve_promise(res, Arg::Handle(ctor_h))?;
            cx.release(h);
        }
        process(cx, w);
        Ok(())
    });
    method(reg, rt, "CustomElementRegistry", "get", |cx, w| {
        need_args(cx, 1, "get", "CustomElementRegistry")?;
        let r = this_reg(cx, w)?;
        let name = cx.arg_string(0)?;
        let c = w.ce.borrow().regs[r]
            .defs
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.ctor);
        match c {
            Some(h) => cx.ret(Arg::Handle(h)),
            None => cx.ret(Arg::Undefined),
        }
    });
    method(reg, rt, "CustomElementRegistry", "getName", |cx, w| {
        need_args(cx, 1, "getName", "CustomElementRegistry")?;
        let r = this_reg(cx, w)?;
        let h = cx.arg_handle(0);
        let defs: Vec<(Handle, String)> = w.ce.borrow().regs[r]
            .defs
            .iter()
            .map(|d| (d.ctor, d.name.clone()))
            .collect();
        for (c, n) in defs {
            if cx.strict_equals(c, h) {
                return cx.ret(n);
            }
        }
        cx.ret(Arg::Null)
    });
    method(reg, rt, "CustomElementRegistry", "whenDefined", |cx, w| {
        need_args(cx, 1, "whenDefined", "CustomElementRegistry")?;
        let r = this_reg(cx, w)?;
        let name = cx.arg_string(0)?;
        if !lb_dom::is_valid_custom_element_name(&name) {
            let (h, res) = cx.new_promise()?;
            cx.reject_promise(
                res,
                w.rt.err(
                    "SyntaxError",
                    format!("Failed to execute 'whenDefined' on 'CustomElementRegistry': \"{name}\" is not a valid custom element name"),
                ),
            )?;
            return cx.ret(Arg::Handle(h));
        }
        let defined = w.ce.borrow().regs[r]
            .defs
            .iter()
            .find(|d| d.name == name)
            .map(|d| d.ctor);
        if let Some(c) = defined {
            let (h, res) = cx.new_promise()?;
            cx.resolve_promise(res, Arg::Handle(c))?;
            return cx.ret(Arg::Handle(h));
        }
        let existing = w.ce.borrow().regs[r]
            .when
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, h, _)| *h);
        if let Some(h) = existing {
            return cx.ret(Arg::Handle(h));
        }
        let (h, res) = cx.new_promise()?;
        w.ce.borrow_mut().regs[r].when.push((name, h, res));
        cx.ret(Arg::Handle(h))
    });
    method(reg, rt, "CustomElementRegistry", "upgrade", |cx, w| {
        need_args(cx, 1, "upgrade", "CustomElementRegistry")?;
        this_reg(cx, w)?;
        let (slot, root) = super::arg_node(cx, w, 0, "upgrade", "CustomElementRegistry")?;
        let nodes = w.doc_slot(slot, |d| {
            let mut v = vec![root];
            v.extend(d.shadow_including_descendants(root));
            v.retain(|&x| d.is_element(x));
            v
        });
        for n in nodes {
            try_upgrade(w, slot, n);
        }
        process(cx, w);
        Ok(())
    });
    method(reg, rt, "CustomElementRegistry", "initialize", |cx, w| {
        need_args(cx, 1, "initialize", "CustomElementRegistry")?;
        this_reg(cx, w)?;
        Ok(())
    });

    // The HTML element constructors ([HTMLConstructor]).
    let mut ifaces: Vec<&'static str> = vec!["HTMLElement"];
    for local in super::tags::HTML_LOCALS {
        let i = super::tags::html_interface(local);
        if i != "HTMLElement" && i != "HTMLUnknownElement" && !ifaces.contains(&i) {
            ifaces.push(i);
        }
    }
    for iface in ifaces {
        ctor(reg, rt, iface, move |cx, w| html_ctor(cx, w, iface));
    }
    let _ = (this_obj, WINDOW_REF);
}

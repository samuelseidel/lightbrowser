//! Nested browsing contexts: `<iframe>` windows.
//!
//! Each frame document is its own [`Window`] with its own realm in the same
//! engine, built from the same persona, so `navigator`, `screen` and the
//! global property list match the parent's.
//!
//! - **Attach.** When an `<iframe>` becomes connected (parser or script),
//!   its initial `about:blank` window is created on the spot, as Chrome
//!   does: `contentWindow` is readable right after `appendChild`, and an
//!   iframe without `src`/`srcdoc` fires `load` synchronously. The realm is
//!   built through [`lb_js::RealmOps`], which also works inside a callback.
//! - **Navigate.** A `src` or `srcdoc` (at insertion or later), and
//!   `location` changes inside the frame, go to the page layer through
//!   [`FrameHooks::navigate`]. It loads the document, builds the new window
//!   with [`child_window_init`] and swaps it in with [`commit_child`].
//! - **Detach.** When the element leaves the document, the frame's windows
//!   (and their descendants) are closed and their realms dropped.
//! - **Access.** `contentWindow`, `contentDocument`, `frameElement`,
//!   `parent`, `top`, `length`, `frames[i]`, `window[i]` and
//!   `postMessage` (JSON-cloned data, `origin` and `source`) work across
//!   frames. Cross-origin access is checked against the calling realm's
//!   window: `contentDocument` and `frameElement` are `null`, and
//!   `contentWindow` is a restricted object whose members throw
//!   `SecurityError` except `postMessage`, `closed`, `length`, `window`,
//!   `self`, `frames`, `top`, `parent`, `opener`, `blur`, `focus` and
//!   `close`, like Chrome's cross-origin `WindowProxy`.
//! - `about:blank` and `about:srcdoc` documents take their creator's
//!   origin and secure-context state.
//!
//! Gaps: nodes of one frame passed to another frame's DOM methods are not
//! supported (host refs are per window); no named frame access
//! (`window.frameName`); `<frame>`, `<object>` and `<embed>` get no
//! browsing context; `sandbox` is ignored.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use lb_dom::{Document, Mutation, NodeId, local_name, ns};
use lb_js::{Arg, CallCtx, Handle, HostError, JsError, JsValue, RealmOps};
use url::Url;

use super::events::{EventExtra, fire_event, fire_event_with};
use super::{
    K_NODE, Runtime, TaskSource, WINDOW_REF, Window, WindowInit, decode_node, getter, method,
    need_args, node_ref,
};
use crate::registry::Registry;

/// Frame links of one window. Default: a top-level window.
#[derive(Default)]
pub struct FrameState {
    parent: RefCell<Weak<Window>>,
    /// The `<iframe>` in the parent's main document.
    element: Cell<Option<NodeId>>,
    /// Frame id, unique within a page (0 for the top-level window).
    id: Cell<u64>,
    children: RefCell<Vec<Child>>,
    /// Page hooks (set on the top-level window, inherited by frames).
    hooks: RefCell<Option<Rc<dyn FrameHooks>>>,
    /// Creator document URL of an `about:blank` / `about:srcdoc` window
    /// (its origin and secure-context state).
    creator: RefCell<Option<Url>>,
    /// Next frame id (top-level window only).
    next_id: Cell<u64>,
    /// This window's global in its own realm.
    global: Cell<Option<Handle>>,
    /// This window's document wrapper in its own realm.
    document: Cell<Option<Handle>>,
    /// Restricted cross-origin stand-ins handed to other windows, by the
    /// accessing window's realm.
    remote: RefCell<Vec<(lb_js::RealmId, Handle)>>,
}

/// A child frame of a window.
#[derive(Clone)]
pub struct Child {
    /// The `<iframe>` element in the parent's main document.
    pub element: NodeId,
    /// Frame id (stable across the frame's navigations).
    pub id: u64,
    /// The frame's current window.
    pub window: Rc<Window>,
}

/// A navigation the page layer should perform in a frame.
#[derive(Debug, Clone)]
pub struct FrameNavigation {
    /// Target URL (`about:srcdoc` for `srcdoc`).
    pub url: Url,
    /// Document text of an `srcdoc` iframe.
    pub srcdoc: Option<String>,
    /// URL of the document that started it (referrer, `Sec-Fetch-Site`).
    pub initiator: Option<Url>,
    /// Form POST body (`None`: GET).
    pub post: Option<super::FormBody>,
    /// Started by trusted user input (`Sec-Fetch-User`).
    pub user_activated: bool,
}

/// What the page layer provides for frames. Calls may come from inside a
/// running callback; `ops` can build or drop realms there.
pub trait FrameHooks {
    /// A frame's document committed: the initial `about:blank` window
    /// (`initial`) or a navigation's window installed by [`commit_child`].
    fn committed(&self, ops: &mut dyn RealmOps, child: &Rc<Window>, initial: bool);
    /// A frame window is going away: replaced by a navigation of its frame
    /// (`gone` false) or removed with its frame (`gone` true: element
    /// detached, or the parent document went away). Its main realm is
    /// dropped after this.
    fn detached(&self, ops: &mut dyn RealmOps, child: &Rc<Window>, gone: bool);
    /// Load `nav` in the frame whose current window is `child`.
    fn navigate(&self, child: &Rc<Window>, nav: FrameNavigation);
}

// ---------------------------------------------------------------------------
// Public helpers for the page layer
// ---------------------------------------------------------------------------

/// Install the page's frame hooks on its top-level window.
pub fn set_hooks(w: &Window, hooks: Option<Rc<dyn FrameHooks>>) {
    *w.frames.hooks.borrow_mut() = hooks;
}

fn hooks(w: &Window) -> Option<Rc<dyn FrameHooks>> {
    w.frames.hooks.borrow().clone()
}

/// The parent window of a frame.
pub fn parent(w: &Window) -> Option<Rc<Window>> {
    w.frames.parent.borrow().upgrade()
}

/// The top-level window of `w`'s page.
pub fn top(w: &Rc<Window>) -> Rc<Window> {
    let mut cur = w.clone();
    while let Some(p) = parent(&cur) {
        cur = p;
    }
    cur
}

/// Frame id (0 for a top-level window).
pub fn frame_id(w: &Window) -> u64 {
    w.frames.id.get()
}

/// The `<iframe>` element of a frame, in its parent's main document.
pub fn frame_element(w: &Window) -> Option<NodeId> {
    w.frames.element.get()
}

/// The child frames of `w`, in attach order.
pub fn children(w: &Window) -> Vec<Child> {
    w.frames.children.borrow().clone()
}

/// The child frame whose element is `element`.
pub fn child_for(w: &Window, element: NodeId) -> Option<Child> {
    w.frames
        .children
        .borrow()
        .iter()
        .find(|c| c.element == element)
        .cloned()
}

/// Every frame window below `w`, parents before children.
pub fn descendants(w: &Window) -> Vec<Rc<Window>> {
    let mut out = Vec::new();
    for c in children(w) {
        out.push(c.window.clone());
        out.extend(descendants(&c.window));
    }
    out
}

/// The window of frame `id` in `top`'s page.
pub fn find(top: &Rc<Window>, id: u64) -> Option<Rc<Window>> {
    if frame_id(top) == id {
        return Some(top.clone());
    }
    descendants(top).into_iter().find(|w| frame_id(w) == id)
}

/// `window.name` of `w`.
pub fn window_name(w: &Window) -> String {
    w.window_name.borrow().clone()
}

/// The URL whose origin `w` has: its creator's for `about:blank` and
/// `about:srcdoc` frames, else its own.
pub fn effective_url(w: &Window) -> Url {
    w.frames.creator.borrow().clone().unwrap_or_else(|| w.url())
}

/// Serialized origin of `w` (`null` for opaque origins).
pub fn origin(w: &Window) -> String {
    effective_url(w).origin().ascii_serialization()
}

/// Same origin (opaque origins match each other: they come from the same
/// creator chain in practice).
pub fn same_origin(a: &Window, b: &Window) -> bool {
    std::ptr::eq(a, b) || origin(a) == origin(b)
}

/// A [`WindowInit`] for a new document in a frame of `parent`: same
/// persona, network, clock mode and wake-up as the parent, the parent's
/// storage areas (replace them for another origin).
pub fn child_window_init(parent: &Window, url: Url) -> WindowInit {
    WindowInit {
        url,
        persona: parent.persona.clone(),
        profile: parent.profile.clone(),
        net: parent.net.clone(),
        local_storage: parent.local_storage.clone(),
        session_storage: parent.session_storage.clone(),
        virtual_time: parent.clock.is_virtual(),
        wake: parent.wake.clone(),
        referrer: parent.url().to_string(),
        content_type: "text/html".into(),
        charset: "UTF-8",
        history_len: 0,
    }
}

/// Make `new` (a window from [`child_window_init`], realm not yet created)
/// the current window of frame `frame` of `parent`. `creator` is the
/// creator URL for `about:srcdoc`. Creates the realm, drops the old
/// window's realms and reports the commit to the hooks.
pub fn commit_child(
    ops: &mut dyn RealmOps,
    parent: &Rc<Window>,
    frame: u64,
    new: &Rc<Window>,
    creator: Option<Url>,
) -> Result<(), JsError> {
    let Some(old) = parent
        .frames
        .children
        .borrow()
        .iter()
        .find(|c| c.id == frame)
        .cloned()
    else {
        return Err(JsError::new(lb_js::JsErrorKind::Host, "frame is gone"));
    };
    let bindings = parent
        .rt
        .bindings()
        .ok_or_else(|| JsError::new(lb_js::JsErrorKind::Host, "no bindings"))?;
    link(new, parent, old.element, frame, creator);
    new.set_console_sink(parent.console_sink.borrow().clone());
    *new.window_name.borrow_mut() = old.window.window_name.borrow().clone();
    new.create_realm(ops, &bindings)?;
    let _ = global_handle(ops, new);
    for c in parent.frames.children.borrow_mut().iter_mut() {
        if c.id == frame {
            c.window = new.clone();
        }
    }
    drop_window_tree(ops, &old.window, false);
    if let Some(h) = hooks(parent) {
        h.committed(ops, new, false);
    }
    Ok(())
}

/// Close and drop every frame below `w` (the page is navigating away or
/// closing). Call before dropping `w`'s own realm.
pub fn drop_children(ops: &mut dyn RealmOps, w: &Window) {
    let kids = std::mem::take(&mut *w.frames.children.borrow_mut());
    for c in kids {
        drop_window_tree(ops, &c.window, true);
    }
}

/// Fire `load` at frame `frame`'s `<iframe>` element (after the frame's
/// document loaded), from the parent's event loop.
pub fn fire_frame_load(parent: &Rc<Window>, frame: u64) {
    parent.queue_task(
        TaskSource::DomManipulation,
        Box::new(move |js, p| {
            if let Some(c) = p.frames.children.borrow().iter().find(|c| c.id == frame) {
                let el = c.element;
                fire_event(js, p, node_ref(0, el), "load", false, false);
            }
        }),
    );
}

// ---------------------------------------------------------------------------
// Attach / detach
// ---------------------------------------------------------------------------

fn link(child: &Rc<Window>, parent: &Rc<Window>, element: NodeId, id: u64, creator: Option<Url>) {
    *child.frames.parent.borrow_mut() = Rc::downgrade(parent);
    child.frames.element.set(Some(element));
    child.frames.id.set(id);
    *child.frames.hooks.borrow_mut() = hooks(parent);
    *child.frames.creator.borrow_mut() = creator;
}

fn next_frame_id(w: &Rc<Window>) -> u64 {
    let t = top(w);
    let id = t.frames.next_id.get() + 1;
    t.frames.next_id.set(id);
    id
}

/// The global of `w` as a handle in its own realm (cached).
fn global_handle(ops: &mut dyn RealmOps, w: &Window) -> Result<Handle, HostError> {
    if let Some(h) = w.frames.global.get() {
        return Ok(h);
    }
    let h = ops
        .global(w.realm())
        .map_err(|e| HostError::Error(e.message))?;
    w.frames.global.set(Some(h));
    Ok(h)
}

fn is_iframe(d: &Document, n: NodeId) -> bool {
    d.is_html_element_named(n, &local_name!("iframe"))
}

/// Tree changes of `w`'s documents: attach connected iframes, detach
/// removed ones, follow `src` / `srcdoc` changes.
pub(crate) fn on_mutations(js: &mut dyn super::Js, w: &Rc<Window>, batch: &[(u32, Mutation)]) {
    if w.is_closed() {
        return;
    }
    let mut attach = Vec::new();
    let mut removed = false;
    let mut renav = Vec::new();
    w.with_doc(|d| {
        for (slot, m) in batch {
            if *slot != 0 {
                continue;
            }
            match m {
                Mutation::ChildInserted { node, .. } => {
                    if !d.is_connected(*node) {
                        continue;
                    }
                    for n in d.inclusive_descendants(*node) {
                        if is_iframe(d, n) && !attach.contains(&n) {
                            attach.push(n);
                        }
                    }
                }
                Mutation::ChildRemoved { .. } => removed = true,
                Mutation::Attribute { element, name, .. }
                    if name.ns == ns!()
                        && (name.local == local_name!("src") || &*name.local == "srcdoc")
                        && is_iframe(d, *element) =>
                {
                    renav.push(*element);
                }
                _ => {}
            }
        }
    });
    if removed && !w.frames.children.borrow().is_empty() {
        let gone: Vec<Child> = {
            let kids = w.frames.children.borrow();
            w.with_doc(|d| {
                kids.iter()
                    .filter(|c| !d.is_connected(c.element))
                    .cloned()
                    .collect()
            })
        };
        for c in gone {
            w.frames.children.borrow_mut().retain(|x| x.id != c.id);
            drop_window_tree(js.realm_ops(), &c.window, true);
        }
    }
    for el in attach {
        let still = w.with_doc(|d| d.is_connected(el));
        if still && child_for(w, el).is_none() {
            attach_iframe(js, w, el);
        }
    }
    for el in renav {
        if let Some(c) = child_for(w, el) {
            match navigation_for(w, el) {
                Some(nav) => {
                    if let Some(h) = hooks(w) {
                        h.navigate(&c.window, nav);
                    }
                }
                None => {
                    fire_event(js, w, node_ref(0, el), "load", false, false);
                }
            }
        }
    }
}

/// What an iframe element should load: `srcdoc` wins over `src`; `None`
/// for `about:blank` (nothing to load).
fn navigation_for(w: &Window, el: NodeId) -> Option<FrameNavigation> {
    let (src, srcdoc) = w.with_doc(|d| {
        (
            d.get_attribute(el, "src").map(str::to_owned),
            d.get_attribute(el, "srcdoc").map(str::to_owned),
        )
    });
    let initiator = Some(w.url());
    if let Some(doc) = srcdoc {
        return Some(FrameNavigation {
            url: Url::parse("about:srcdoc").ok()?,
            srcdoc: Some(doc),
            initiator,
            post: None,
            user_activated: false,
        });
    }
    let src = src.filter(|s| !s.trim().is_empty())?;
    let url = w.resolve_url(&src)?;
    if url.as_str() == "about:blank" {
        return None;
    }
    Some(FrameNavigation {
        url,
        srcdoc: None,
        initiator,
        post: None,
        user_activated: false,
    })
}

/// Create the initial `about:blank` window of a newly connected iframe.
fn attach_iframe(js: &mut dyn super::Js, parent: &Rc<Window>, el: NodeId) {
    let Some(bindings) = parent.rt.bindings() else {
        return;
    };
    let id = next_frame_id(parent);
    let blank = Url::parse("about:blank").expect("about:blank parses");
    let child = Window::new(&parent.rt, child_window_init(parent, blank));
    link(&child, parent, el, id, Some(effective_url(parent)));
    child.set_console_sink(parent.console_sink.borrow().clone());
    let name = parent.with_doc(|d| d.get_attribute(el, "name").map(str::to_owned));
    *child.window_name.borrow_mut() = name.unwrap_or_default();
    let ops = js.realm_ops();
    if let Err(e) = child.create_realm(ops, &bindings) {
        parent.warn(format!("iframe: realm creation failed: {}", e.message));
        return;
    }
    let _ = global_handle(ops, &child);
    // The initial empty document: <html><head></head><body></body></html>.
    child.start_parser();
    child.parser_end();
    while let Some(step) = child.parser_run() {
        if matches!(step, lb_dom::ParseStep::Done) {
            break;
        }
    }
    child.finish_parser();
    child.mutations.borrow_mut().clear();
    child.set_ready_state(super::ReadyState::Complete);
    parent.frames.children.borrow_mut().push(Child {
        element: el,
        id,
        window: child.clone(),
    });
    if let Some(h) = hooks(parent) {
        h.committed(ops, &child, true);
    }
    match navigation_for(parent, el) {
        Some(nav) => {
            if let Some(h) = hooks(parent) {
                h.navigate(&child, nav);
            }
        }
        // Chrome fires `load` for the initial empty document while the
        // element is being inserted.
        None => {
            fire_event(js, parent, node_ref(0, el), "load", false, false);
        }
    }
}

/// Close `w` and its descendants and drop their realms.
fn drop_window_tree(ops: &mut dyn RealmOps, w: &Rc<Window>, gone: bool) {
    let kids = std::mem::take(&mut *w.frames.children.borrow_mut());
    for c in kids {
        drop_window_tree(ops, &c.window, true);
    }
    if let Some(h) = hooks(w) {
        h.detached(ops, w, gone);
    }
    w.close();
    let realm = w.realm();
    w.rt.detach(realm);
    ops.drop_realm(realm);
    w.frames.global.set(None);
    w.frames.document.set(None);
    w.frames.remote.borrow_mut().clear();
}

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

/// The window of the realm that started the current call (the accessor).
fn accessor(cx: &mut CallCtx<'_, '_, '_>, rt: &Runtime, fallback: &Rc<Window>) -> Rc<Window> {
    let r = cx.entered_realm();
    rt.window(r).unwrap_or_else(|| fallback.clone())
}

/// `w` as seen from `from`: its global when same-origin, else the
/// restricted cross-origin stand-in.
fn window_for(
    cx: &mut CallCtx<'_, '_, '_>,
    from: &Rc<Window>,
    w: &Rc<Window>,
) -> Result<Arg, HostError> {
    if same_origin(from, w) {
        return Ok(Arg::Handle(global_handle(cx, w)?));
    }
    remote_window(cx, from, w)
}

fn js_err(e: JsError) -> HostError {
    HostError::Error(e.message)
}

/// Properties a cross-origin window exposes (HTML's `CrossOriginProperties`).
const CROSS_ORIGIN_OK: &[&str] = &[
    "window",
    "self",
    "location",
    "close",
    "closed",
    "focus",
    "blur",
    "frames",
    "length",
    "top",
    "opener",
    "parent",
    "postMessage",
    "then",
];

/// A stand-in for cross-origin window `w` in `from`'s realm: a native
/// `Proxy` whose traps forward the allowed members and throw Chrome's
/// `SecurityError` for everything else. Built once per pair.
fn remote_window(
    cx: &mut CallCtx<'_, '_, '_>,
    from: &Rc<Window>,
    w: &Rc<Window>,
) -> Result<Arg, HostError> {
    let realm = from.realm();
    if let Some((_, h)) = w.frames.remote.borrow().iter().find(|(r, _)| *r == realm) {
        return Ok(Arg::Handle(*h));
    }
    let target = RealmOps::make_handle(cx, realm, Arg::Json("{}".into())).map_err(js_err)?;
    let handler = RealmOps::make_handle(cx, realm, Arg::Json("{}".into())).map_err(js_err)?;
    let origin_from = origin(from);
    let blocked = move |rt: &Runtime, what: &str| -> HostError {
        rt.err(
            "SecurityError",
            format!(
                "Failed to read a named property '{what}' from 'Window': Blocked a frame with origin \"{origin_from}\" from accessing a cross-origin frame."
            ),
        )
    };
    let weak = Rc::downgrade(w);
    let rt = w.rt.clone();
    let b = blocked.clone();
    let get = lb_js::host_fn(move |cx| {
        let key = cx.arg_string(1).unwrap_or_default();
        let Some(w) = weak.upgrade() else {
            return cx.ret(Arg::Undefined);
        };
        if !CROSS_ORIGIN_OK.contains(&key.as_str()) {
            return Err(b(&rt, &key));
        }
        let g = global_handle(cx, &w)?;
        match key.as_str() {
            "then" => cx.ret(Arg::Undefined),
            "closed" => cx.ret(w.is_closed()),
            "length" => cx.ret(w.frames.children.borrow().len() as u32),
            "location" => Err(b(&rt, "location")),
            _ => {
                let v = RealmOps::get_property(cx, w.realm(), g, &key).map_err(js_err)?;
                cx.ret(Arg::Handle(v))
            }
        }
    });
    let rt2 = w.rt.clone();
    let b2 = blocked.clone();
    let deny = lb_js::host_fn(move |cx| {
        let key = cx.arg_string(1).unwrap_or_default();
        Err(b2(&rt2, &key))
    });
    let rt3 = w.rt.clone();
    let b3 = blocked;
    let deny_all = lb_js::host_fn(move |_cx| Err(b3(&rt3, "ownKeys")));
    let traps: [(&str, lb_js::HostFn); 7] = [
        ("get", get),
        ("set", deny.clone()),
        ("has", deny.clone()),
        ("deleteProperty", deny.clone()),
        ("defineProperty", deny.clone()),
        ("getOwnPropertyDescriptor", deny),
        ("ownKeys", deny_all),
    ];
    for (name, f) in traps {
        let fh = RealmOps::new_function(cx, realm, name, 2, f).map_err(js_err)?;
        RealmOps::set_property(cx, realm, handler, name, Arg::Handle(fh)).map_err(js_err)?;
        RealmOps::release(cx, fh);
    }
    let g = global_handle(cx, from)?;
    let proxy_ctor = RealmOps::get_property(cx, realm, g, "Proxy").map_err(js_err)?;
    let reflect = RealmOps::get_property(cx, realm, g, "Reflect").map_err(js_err)?;
    let construct = RealmOps::get_property(cx, realm, reflect, "construct").map_err(js_err)?;
    let proxy = RealmOps::call_handle(
        cx,
        realm,
        construct,
        Arg::Handle(reflect),
        &[
            Arg::Handle(proxy_ctor),
            Arg::Array(vec![Arg::Handle(target), Arg::Handle(handler)]),
        ],
    )
    .map_err(js_err)?;
    for h in [proxy_ctor, reflect, construct, target, handler] {
        RealmOps::release(cx, h);
    }
    w.frames.remote.borrow_mut().push((realm, proxy));
    Ok(Arg::Handle(proxy))
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    let r = rt.clone();
    getter(
        reg,
        rt,
        "HTMLIFrameElement",
        "contentWindow",
        move |cx, w| {
            let Some(c) = this_child(cx, w) else {
                return cx.ret(Arg::Null);
            };
            let from = accessor(cx, &r, w);
            let v = window_for(cx, &from, &c.window)?;
            cx.ret(v)
        },
    );
    let r = rt.clone();
    getter(
        reg,
        rt,
        "HTMLIFrameElement",
        "contentDocument",
        move |cx, w| {
            let Some(c) = this_child(cx, w) else {
                return cx.ret(Arg::Null);
            };
            let from = accessor(cx, &r, w);
            if !same_origin(&from, &c.window) {
                return cx.ret(Arg::Null);
            }
            let h = document_handle(cx, &c.window)?;
            cx.ret(Arg::Handle(h))
        },
    );

    let r = rt.clone();
    getter(reg, rt, "Window", "parent", move |cx, w| match parent(w) {
        Some(p) => {
            let from = accessor(cx, &r, w);
            let v = window_for(cx, &from, &p)?;
            cx.ret(v)
        }
        None => self_global(cx, w),
    });
    let r = rt.clone();
    getter(reg, rt, "Window", "top", move |cx, w| {
        let t = top(w);
        if Rc::ptr_eq(&t, w) {
            return self_global(cx, w);
        }
        let from = accessor(cx, &r, w);
        let v = window_for(cx, &from, &t)?;
        cx.ret(v)
    });
    let r = rt.clone();
    getter(reg, rt, "Window", "frameElement", move |cx, w| {
        let (Some(p), Some(el)) = (parent(w), frame_element(w)) else {
            return cx.ret(Arg::Null);
        };
        let from = accessor(cx, &r, w);
        if !same_origin(&from, &p) {
            return cx.ret(Arg::Null);
        }
        let iface = p.rt.iface("HTMLIFrameElement");
        let h = RealmOps::wrap(cx, p.realm(), iface, node_ref(0, el)).map_err(js_err)?;
        cx.ret(Arg::Handle(h))
    });
    getter(reg, rt, "Window", "length", |cx, w| {
        cx.ret(w.frames.children.borrow().len() as u32)
    });
    let r = rt.clone();
    reg.indexed_getter(
        "Window",
        Rc::new(move |px: &mut lb_js::PropCtx<'_, '_, '_>, i: u32| {
            let realm = px.holder_realm();
            let Some(w) = r.window(realm) else {
                return Ok(None);
            };
            let Some(c) = w.frames.children.borrow().get(i as usize).cloned() else {
                return Ok(None);
            };
            // Another origin sees the restricted stand-in once it exists
            // (V8 denies the raw global across origins anyway).
            if let Some(from) = r.window(px.realm())
                && !same_origin(&from, &c.window)
                && let Some((_, h)) = c
                    .window
                    .frames
                    .remote
                    .borrow()
                    .iter()
                    .find(|(rr, _)| *rr == from.realm())
            {
                return Ok(Some(Arg::Handle(*h)));
            }
            Ok(c.window.frames.global.get().map(Arg::Handle))
        }),
    );
    let r = rt.clone();
    reg.indexed_length(
        "Window",
        Rc::new(move |px: &mut lb_js::PropCtx<'_, '_, '_>| {
            let realm = px.holder_realm();
            r.window(realm)
                .map_or(0, |w| w.frames.children.borrow().len() as u32)
        }),
    );

    let r = rt.clone();
    method(reg, rt, "Window", "postMessage", move |cx, target| {
        need_args(cx, 1, "postMessage", "Window")?;
        let source = accessor(cx, &r, target);
        let data = cx.arg_handle(0);
        // targetOrigin: a string, or an options dictionary.
        let wanted = if cx.len() > 1 {
            match cx.arg_kind(1) {
                lb_js::ValueKind::String => Some(cx.arg_string(1)?),
                lb_js::ValueKind::Object => {
                    let o = cx.arg_handle(1);
                    let v = cx.get_property(o, "targetOrigin")?;
                    let s = match cx.handle_value(v) {
                        JsValue::String(s) => Some(s),
                        _ => None,
                    };
                    cx.release(v);
                    s
                }
                _ => Some(cx.arg_string(1)?),
            }
        } else {
            None
        };
        let src_origin = origin(&source);
        if let Some(t) = wanted.as_deref() {
            let t = if t == "/" { src_origin.as_str() } else { t };
            if t != "*" {
                let Ok(u) = Url::parse(t) else {
                    return Err(r.err(
                        "SyntaxError",
                        format!("Failed to execute 'postMessage' on 'Window': Invalid target origin '{t}' in a call to 'postMessage'."),
                    ));
                };
                if u.origin().ascii_serialization() != origin(target) {
                    source.warn(format!(
                        "Failed to execute 'postMessage' on 'DOMWindow': The target origin provided ('{t}') does not match the recipient window's origin ('{}').",
                        origin(target)
                    ));
                    return Ok(());
                }
            }
        }
        let same = Rc::ptr_eq(&source, target);
        // Structured clone stand-in: JSON through the sender's realm.
        let json = if same {
            None
        } else {
            let stringify = source.helpers.borrow().json_stringify;
            match stringify {
                Some(f) => match RealmOps::call(
                    cx,
                    source.realm(),
                    f,
                    Arg::Undefined,
                    &[Arg::Handle(data)],
                ) {
                    Ok(JsValue::String(s)) => Some(Some(s)),
                    Ok(_) => Some(None),
                    Err(e) => {
                        return Err(r.err(
                            "DataCloneError",
                            format!("Failed to execute 'postMessage' on 'Window': {}", e.message),
                        ));
                    }
                },
                None => Some(None),
            }
        };
        let src_global = global_handle(cx, &source)?;
        target.queue_task(
            TaskSource::PostedMessage,
            Box::new(move |js, w| {
                let data = match json {
                    None => Some(data),
                    Some(None) => None,
                    Some(Some(s)) => {
                        let parse = w.helpers.borrow().json_parse;
                        parse
                            .and_then(|p| js.call_handle(p, Arg::Undefined, &[Arg::String(s)]).ok())
                    }
                };
                fire_event_with(
                    js,
                    w,
                    WINDOW_REF,
                    "MessageEvent",
                    "message",
                    false,
                    false,
                    EventExtra::Message {
                        data,
                        origin: src_origin,
                        source: Some(src_global),
                    },
                );
            }),
        );
        Ok(())
    });
}

/// `this` (an iframe element of the main document) as a child frame.
fn this_child(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Option<Child> {
    let h = cx.this_host()?;
    if h.kind & 0xff != K_NODE {
        return None;
    }
    let (slot, n) = decode_node(h)?;
    if slot != 0 {
        return None;
    }
    child_for(w, n)
}

fn self_global(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<(), HostError> {
    let h = global_handle(cx, w)?;
    cx.ret(Arg::Handle(h))
}

/// The document wrapper of `w` in its own realm (cached).
fn document_handle(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Handle, HostError> {
    if let Some(h) = w.frames.document.get() {
        return Ok(h);
    }
    let name = w.with_doc(|d| super::tags::node_interface(d, Document::DOCUMENT, 0));
    let iface = w.rt.iface(name);
    let h =
        RealmOps::wrap(cx, w.realm(), iface, node_ref(0, Document::DOCUMENT)).map_err(js_err)?;
    w.frames.document.set(Some(h));
    Ok(h)
}

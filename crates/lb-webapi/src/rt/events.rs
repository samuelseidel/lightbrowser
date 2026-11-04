//! EventTarget, Event and its common subclasses, dispatch (capture,
//! target, bubble), event handler IDL and content attributes, exception
//! reporting.

use std::cell::Cell;
use std::rc::Rc;

use lb_dom::NodeId;
use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, JsError, JsValue, RealmId, ValueKind};

use super::{
    Dict, HandlerValue, Js, K_NODE, K_OBJ, K_WINDOW, Obj, Runtime, WINDOW_REF, Window, ctor,
    getter, illegal, method, need_args, setter, this_obj,
};
use crate::desc::AttrType;
use crate::registry::Registry;

/// Extra state of event subclasses.
#[derive(Clone, Debug, Default)]
pub enum EventExtra {
    #[default]
    None,
    Custom(Option<Handle>),
    Progress {
        computable: bool,
        loaded: f64,
        total: f64,
    },
    HashChange {
        old: String,
        new: String,
    },
    /// `history.state` as JSON.
    PopState(Option<String>),
    Error {
        message: String,
        filename: String,
        lineno: u32,
        colno: u32,
        error: Option<Handle>,
    },
    Message {
        data: Option<Handle>,
        origin: String,
        /// `MessageEvent.source` (the sender's window), if any.
        source: Option<Handle>,
        /// `MessageEvent.ports`: a frozen array (made on first read for
        /// events made by script).
        ports: Option<Handle>,
    },
    PageTransition {
        persisted: bool,
    },
    /// `MouseEvent`, `PointerEvent`, `WheelEvent` (see `input`).
    Mouse(Box<super::input::MouseData>),
    /// `KeyboardEvent`.
    Keyboard(Box<super::input::KeyData>),
    /// `InputEvent`.
    Input {
        data: Option<String>,
        input_type: String,
    },
    /// `FocusEvent`.
    Focus {
        related: Option<HostRef>,
    },
    /// `UIEvent` with a `detail`.
    Ui {
        detail: i32,
    },
    /// `CloseEvent` (WebSocket).
    Close {
        code: u16,
        reason: String,
        was_clean: bool,
    },
}

/// One event object.
#[derive(Clone, Debug)]
pub struct EventData {
    pub iface: &'static str,
    pub type_: String,
    pub bubbles: bool,
    pub cancelable: bool,
    pub composed: bool,
    pub target: Option<HostRef>,
    pub current_target: Option<HostRef>,
    pub phase: u16,
    pub stop: bool,
    pub stop_immediate: bool,
    pub canceled: bool,
    pub in_passive: bool,
    pub dispatching: bool,
    pub initialized: bool,
    pub trusted: bool,
    pub time: f64,
    pub path: Vec<PathEntry>,
    /// The retargeted `relatedTarget` while (and after) dispatching;
    /// `None` reads the one the event was made with.
    pub related_view: Option<Option<HostRef>>,
    pub extra: EventExtra,
}

/// One entry of an event's path (DOM "event path" struct).
#[derive(Clone, Copy, Debug)]
pub struct PathEntry {
    /// The invocation target.
    pub item: HostRef,
    /// The shadow-adjusted target (`None` when the target is the same as
    /// at an earlier entry).
    pub target: Option<HostRef>,
    pub related: Option<HostRef>,
    pub root_of_closed_tree: bool,
    pub slot_in_closed_tree: bool,
}

/// `relatedTarget` as listeners see it now.
pub(crate) fn related_of(e: &EventData) -> Option<HostRef> {
    if let Some(v) = e.related_view {
        return v;
    }
    match &e.extra {
        EventExtra::Mouse(m) => m.related,
        EventExtra::Focus { related } => *related,
        _ => None,
    }
}

impl EventData {
    pub fn new(iface: &'static str, type_: &str, bubbles: bool, cancelable: bool) -> Self {
        EventData {
            iface,
            type_: type_.to_owned(),
            bubbles,
            cancelable,
            composed: false,
            target: None,
            current_target: None,
            phase: 0,
            stop: false,
            stop_immediate: false,
            canceled: false,
            in_passive: false,
            dispatching: false,
            initialized: true,
            trusted: false,
            time: 0.0,
            path: Vec::new(),
            related_view: None,
            extra: EventExtra::None,
        }
    }
}

pub(crate) enum ListenerKind {
    Callback(Handle),
    /// The event handler (`onclick`) of this name.
    Handler(String),
}

pub(crate) struct Listener {
    pub type_: String,
    pub kind: ListenerKind,
    pub capture: bool,
    pub once: bool,
    pub passive: bool,
    pub removed: Cell<bool>,
    #[allow(dead_code)]
    pub realm: RealmId,
}

/// Create a trusted event of `iface` and return its object id.
pub(crate) fn new_event(
    w: &Window,
    iface: &'static str,
    type_: &str,
    bubbles: bool,
    cancelable: bool,
    extra: EventExtra,
) -> u32 {
    let mut e = EventData::new(iface, type_, bubbles, cancelable);
    e.trusted = true;
    // UI events the browser fires cross shadow boundaries.
    e.composed = matches!(
        iface,
        "FocusEvent"
            | "MouseEvent"
            | "PointerEvent"
            | "WheelEvent"
            | "KeyboardEvent"
            | "InputEvent"
            | "CompositionEvent"
            | "TouchEvent"
    );
    e.time = coarse_now(w);
    e.extra = extra;
    w.new_obj(Obj::Event(Box::new(e)))
}

/// Fire a trusted `Event` named `type_` at `target`. Returns false if a
/// listener canceled it.
pub fn fire_event(
    js: &mut dyn Js,
    w: &Rc<Window>,
    target: HostRef,
    type_: &str,
    bubbles: bool,
    cancelable: bool,
) -> bool {
    let ev = new_event(w, "Event", type_, bubbles, cancelable, EventExtra::None);
    dispatch(js, w, target, ev)
}

/// Fire a trusted event of interface `iface` with subclass state.
#[allow(clippy::too_many_arguments)]
pub fn fire_event_with(
    js: &mut dyn Js,
    w: &Rc<Window>,
    target: HostRef,
    iface: &'static str,
    type_: &str,
    bubbles: bool,
    cancelable: bool,
    extra: EventExtra,
) -> bool {
    let ev = new_event(w, iface, type_, bubbles, cancelable, extra);
    dispatch(js, w, target, ev)
}

/// `performance.now()`-resolution time.
pub(crate) fn coarse_now(w: &Window) -> f64 {
    super::coarse_time(w, w.clock.now())
}

fn with_ev<R>(w: &Window, ev: u32, f: impl FnOnce(&mut EventData) -> R) -> Option<R> {
    w.with_obj(ev, |o| match o {
        Obj::Event(e) => Some(f(e)),
        _ => None,
    })
    .flatten()
}

/// "Retarget" `a` against `b`: climb out of shadow trees `b` is not in.
fn retarget(w: &Window, a: Option<HostRef>, b: HostRef) -> Option<HostRef> {
    let a = a?;
    let Some((sa, na)) = w.decode_node(a) else {
        return Some(a);
    };
    let b = w.decode_node(b).filter(|&(sb, _)| sb == sa).map(|(_, n)| n);
    let n = w.doc_slot(sa, |d| {
        if d.shadow_roots().is_empty() {
            return na;
        }
        let mut n = na;
        loop {
            let root = d.root(n);
            if !d.is_shadow_root(root) {
                break n;
            }
            if b.is_some_and(|b| d.shadow_including_contains(root, b)) {
                break n;
            }
            match d.shadow_host(root) {
                Some(h) => n = h,
                None => break n,
            }
        }
    });
    Some(if n == na { a } else { w.node_ref(sa, n) })
}

/// Facts about a path item: (assigned to a slot, is a closed shadow root,
/// its root is a closed shadow root).
fn item_facts(w: &Window, item: HostRef) -> (bool, bool, bool) {
    let Some((s, n)) = w.decode_node(item) else {
        return (false, false, false);
    };
    w.doc_slot(s, |d| {
        if d.shadow_roots().is_empty() {
            return (false, false, false);
        }
        let closed = |r: NodeId| {
            d.shadow_root_data(r)
                .is_some_and(|x| x.mode == lb_dom::ShadowRootMode::Closed)
        };
        (
            d.find_slot(n, false).is_some(),
            closed(n),
            closed(d.root(n)),
        )
    })
}

/// "Get the parent" of an event path item.
fn get_parent(
    w: &Window,
    item: HostRef,
    first_root: Option<NodeId>,
    type_: &str,
    composed: bool,
) -> Option<HostRef> {
    let (s, n) = w.decode_node(item)?;
    let r = w.doc_slot(s, |d| {
        if !d.shadow_roots().is_empty() {
            if let Some(slot) = d.find_slot(n, false) {
                return Some(Ok(slot));
            }
            if let Some(host) = d.shadow_host(n) {
                return if !composed && first_root == Some(n) {
                    None
                } else {
                    Some(Ok(host))
                };
            }
        }
        if let Some(p) = d.parent(n) {
            return Some(Ok(p));
        }
        (s == 0 && n == lb_dom::Document::DOCUMENT && type_ != "load").then_some(Err(()))
    })?;
    Some(match r {
        Ok(p) => w.node_ref(s, p),
        Err(()) => WINDOW_REF,
    })
}

/// The event path (DOM dispatch steps 5.1-5.9), target first.
fn event_path(
    w: &Window,
    target: HostRef,
    type_: &str,
    composed: bool,
    related: Option<HostRef>,
) -> Vec<PathEntry> {
    let mut path = Vec::new();
    let rel0 = retarget(w, related, target);
    if Some(target) == rel0 && related != Some(target) {
        return path;
    }
    let first_root = w
        .decode_node(target)
        .map(|(s, n)| w.doc_slot(s, |d| d.root(n)));
    // Root of the current (shadow-adjusted) target.
    let root_of = |h: HostRef| {
        w.decode_node(h)
            .map(|(s, n)| (s, w.doc_slot(s, |d| d.root(n))))
    };
    let (assigned, closed_root, _) = item_facts(w, target);
    path.push(PathEntry {
        item: target,
        target: Some(target),
        related: rel0,
        root_of_closed_tree: closed_root,
        slot_in_closed_tree: false,
    });
    let mut tgt_root = root_of(target);
    let mut slottable = assigned;
    let mut slot_in_closed = false;
    let mut parent = get_parent(w, target, first_root, type_, composed);
    while let Some(p) = parent {
        let (p_assigned, p_closed_root, p_in_closed) = item_facts(w, p);
        if slottable {
            slottable = false;
            if p_in_closed {
                slot_in_closed = true;
            }
        }
        if p_assigned {
            slottable = true;
        }
        let rel = retarget(w, related, p);
        let in_target_tree = p.kind == K_WINDOW
            || match (tgt_root, w.decode_node(p)) {
                (Some((s, r)), Some((ps, pn))) if s == ps => {
                    w.doc_slot(s, |d| d.shadow_including_contains(r, pn))
                }
                _ => false,
            };
        if in_target_tree {
            path.push(PathEntry {
                item: p,
                target: None,
                related: rel,
                root_of_closed_tree: p_closed_root,
                slot_in_closed_tree: slot_in_closed,
            });
        } else if rel == Some(p) {
            break;
        } else {
            tgt_root = root_of(p);
            path.push(PathEntry {
                item: p,
                target: Some(p),
                related: rel,
                root_of_closed_tree: p_closed_root,
                slot_in_closed_tree: slot_in_closed,
            });
        }
        parent = get_parent(w, p, first_root, type_, composed);
        slot_in_closed = false;
    }
    path
}

/// `Event.composedPath()`: the path as seen from the current target,
/// without nodes inside closed shadow trees it cannot see.
fn composed_path(path: &[PathEntry], current: Option<HostRef>) -> Vec<HostRef> {
    let Some(cur) = current else {
        return Vec::new();
    };
    if path.is_empty() {
        return Vec::new();
    }
    let mut cur_index = 0;
    let mut cur_level: i32 = 0;
    for i in (0..path.len()).rev() {
        if path[i].root_of_closed_tree {
            cur_level += 1;
        }
        if path[i].item == cur {
            cur_index = i;
            break;
        }
        if path[i].slot_in_closed_tree {
            cur_level -= 1;
        }
    }
    let mut before = Vec::new();
    let (mut level, mut max) = (cur_level, cur_level);
    for i in (0..cur_index).rev() {
        if path[i].root_of_closed_tree {
            level += 1;
        }
        if level <= max {
            before.push(path[i].item);
        }
        if path[i].slot_in_closed_tree {
            level -= 1;
            max = max.min(level);
        }
    }
    before.reverse();
    before.push(cur);
    let (mut level, mut max) = (cur_level, cur_level);
    for e in &path[cur_index + 1..] {
        if e.slot_in_closed_tree {
            level += 1;
        }
        if level <= max {
            before.push(e.item);
        }
        if e.root_of_closed_tree {
            level -= 1;
            max = max.min(level);
        }
    }
    before
}

/// The DOM dispatch algorithm. Returns false if the event was canceled.
pub(crate) fn dispatch(js: &mut dyn Js, w: &Rc<Window>, target: HostRef, ev: u32) -> bool {
    let Some((type_, composed, related)) = with_ev(w, ev, |e| {
        e.dispatching = true;
        e.target = Some(target);
        e.related_view = None;
        (e.type_.clone(), e.composed, related_of(e))
    }) else {
        return true;
    };
    let path = event_path(w, target, &type_, composed, related);
    with_ev(w, ev, |e| e.path = path.clone());
    let stopped = |w: &Window| with_ev(w, ev, |e| e.stop).unwrap_or(true);
    // The shadow-adjusted target for entry `i`: the nearest one at or
    // before it.
    let adjusted = |i: usize| path[..=i].iter().rev().find_map(|p| p.target);
    let enter = |w: &Window, i: usize, phase: u16| {
        let t = adjusted(i);
        let entry = path[i];
        with_ev(w, ev, |e| {
            e.phase = phase;
            e.current_target = Some(entry.item);
            e.target = t;
            if related.is_some() {
                e.related_view = Some(entry.related);
            }
        });
    };

    // Capture: window → target, capture listeners (AT_TARGET at targets).
    for i in (0..path.len()).rev() {
        if stopped(w) {
            break;
        }
        let phase = if path[i].target.is_some() { 2 } else { 1 };
        enter(w, i, phase);
        invoke(js, w, path[i].item, ev, true);
    }
    // Bubble: targets (non-capture listeners), then ancestors if bubbling.
    let bubbles = with_ev(w, ev, |e| e.bubbles).unwrap_or(false);
    for (i, entry) in path.iter().enumerate() {
        if stopped(w) {
            break;
        }
        let phase = if entry.target.is_some() {
            2
        } else if bubbles {
            3
        } else {
            continue;
        };
        enter(w, i, phase);
        invoke(js, w, entry.item, ev, false);
    }
    // After dispatch the target is the last shadow-adjusted one, unless it
    // (or the related target) is inside a shadow tree ("clear targets").
    let last = path.iter().rev().find(|p| p.target.is_some()).copied();
    let in_shadow = |h: Option<HostRef>| {
        h.and_then(|h| w.decode_node(h))
            .is_some_and(|(s, n)| w.doc_slot(s, |d| d.is_shadow_root(d.root(n))))
    };
    let clear = last.is_some_and(|l| in_shadow(l.target) || in_shadow(l.related));
    with_ev(w, ev, |e| {
        if let Some(l) = last {
            e.target = if clear { None } else { l.target };
            if related.is_some() {
                e.related_view = Some(if clear { None } else { l.related });
            }
        }
        e.phase = 0;
        e.current_target = None;
        e.dispatching = false;
        e.stop = false;
        e.stop_immediate = false;
        e.path.clear();
        !e.canceled
    })
    .unwrap_or(true)
}

fn invoke(js: &mut dyn Js, w: &Rc<Window>, t: HostRef, ev: u32, capture: bool) {
    let list = match w.listeners.borrow().get(&t) {
        Some(l) => l.clone(),
        None => return,
    };
    let type_ = with_ev(w, ev, |e| e.type_.clone()).unwrap_or_default();
    for l in list {
        if l.removed.get() || l.type_ != type_ || l.capture != capture {
            continue;
        }
        if l.once {
            remove_listener(w, t, &l);
        }
        with_ev(w, ev, |e| e.in_passive = l.passive);
        call_listener(js, w, t, ev, &l);
        with_ev(w, ev, |e| e.in_passive = false);
        if with_ev(w, ev, |e| e.stop_immediate).unwrap_or(true) {
            break;
        }
    }
}

pub(crate) fn remove_listener(w: &Window, t: HostRef, l: &Rc<Listener>) {
    l.removed.set(true);
    if let Some(v) = w.listeners.borrow_mut().get_mut(&t) {
        v.retain(|x| !Rc::ptr_eq(x, l));
    }
}

fn call_listener(js: &mut dyn Js, w: &Rc<Window>, t: HostRef, ev: u32, l: &Listener) {
    let this = w.host_arg(t);
    let ev_arg = w.obj_arg(ev);
    let result = match &l.kind {
        ListenerKind::Callback(h) => {
            if js.kind(*h) == ValueKind::Function {
                js.call(*h, this, &[ev_arg]).map(|_| ())
            } else {
                match js.get(*h, "handleEvent") {
                    Some(f) if js.kind(f) == ValueKind::Function => {
                        let r = js.call(f, Arg::Handle(*h), &[ev_arg]).map(|_| ());
                        js.release(f);
                        r
                    }
                    other => {
                        if let Some(f) = other {
                            js.release(f);
                        }
                        Err(JsError::new(
                            lb_js::JsErrorKind::Exception,
                            "TypeError: The provided callback is no longer runnable.",
                        ))
                    }
                }
            }
        }
        ListenerKind::Handler(name) => {
            let Some(f) = handler_function(js, w, t, name) else {
                return;
            };
            let is_error = name == "onerror" && t.kind == K_WINDOW;
            let args = if is_error {
                error_handler_args(w, ev)
            } else {
                vec![ev_arg]
            };
            match js.call(f, this, &args) {
                Ok(v) => {
                    let cancel = if is_error {
                        v == JsValue::Bool(true)
                    } else {
                        v == JsValue::Bool(false)
                    };
                    if cancel {
                        with_ev(w, ev, |e| {
                            if e.cancelable && !e.in_passive {
                                e.canceled = true;
                            }
                        });
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
    };
    if let Err(e) = result {
        report_exception(js, w, &e);
    }
}

fn error_handler_args(w: &Window, ev: u32) -> Vec<Arg> {
    let extra = with_ev(w, ev, |e| e.extra.clone()).unwrap_or_default();
    match extra {
        EventExtra::Error {
            message,
            filename,
            lineno,
            colno,
            error,
        } => vec![
            Arg::String(message),
            Arg::String(filename),
            Arg::Number(lineno.into()),
            Arg::Number(colno.into()),
            error.map_or(Arg::Null, Arg::Handle),
        ],
        _ => vec![w.obj_arg(ev)],
    }
}

/// The handler function for `name` on `t`, compiling an attribute value on
/// first use.
fn handler_function(js: &mut dyn Js, w: &Rc<Window>, t: HostRef, name: &str) -> Option<Handle> {
    let key = (t, name.to_owned());
    let src = match w.handlers.borrow().get(&key) {
        Some(HandlerValue::Compiled(h)) => return Some(*h),
        Some(HandlerValue::Uncompiled(s)) => s.clone(),
        None => return None,
    };
    let params = if name == "onerror" && t.kind == K_WINDOW {
        "event, source, lineno, colno, error"
    } else {
        "event"
    };
    let code = format!("(function {name}({params}) {{\n{src}\n}})");
    let url = w.url().to_string();
    match js.eval_handle(&code, lb_js::ScriptOrigin::new(&url)) {
        Ok(h) => {
            w.handlers
                .borrow_mut()
                .insert(key, HandlerValue::Compiled(h));
            Some(h)
        }
        Err(e) => {
            w.handlers.borrow_mut().remove(&key);
            report_exception(js, w, &e);
            None
        }
    }
}

/// Set (or clear) an event handler and make sure its listener exists.
pub(crate) fn set_handler(w: &Window, t: HostRef, name: &str, value: Option<HandlerValue>) {
    let key = (t, name.to_owned());
    match value {
        None => {
            w.handlers.borrow_mut().remove(&key);
        }
        Some(v) => {
            w.handlers.borrow_mut().insert(key, v);
            let mut ls = w.listeners.borrow_mut();
            let list = ls.entry(t).or_default();
            let exists = list
                .iter()
                .any(|l| matches!(&l.kind, ListenerKind::Handler(n) if n == name));
            if !exists {
                list.push(Rc::new(Listener {
                    type_: name[2..].to_owned(),
                    kind: ListenerKind::Handler(name.to_owned()),
                    capture: false,
                    once: false,
                    passive: false,
                    removed: Cell::new(false),
                    realm: w.realm(),
                }));
            }
        }
    }
}

/// Handlers on `<body>` / `<frameset>` that live on the window.
pub(crate) fn body_forwards_to_window(name: &str) -> bool {
    matches!(
        name,
        "onblur"
            | "onerror"
            | "onfocus"
            | "onload"
            | "onresize"
            | "onscroll"
            | "onafterprint"
            | "onbeforeprint"
            | "onbeforeunload"
            | "onhashchange"
            | "onlanguagechange"
            | "onmessage"
            | "onmessageerror"
            | "onoffline"
            | "ononline"
            | "onpagehide"
            | "onpageshow"
            | "onpopstate"
            | "onrejectionhandled"
            | "onstorage"
            | "onunhandledrejection"
            | "onunload"
            | "onpagereveal"
            | "onpageswap"
    )
}

/// Report an uncaught exception: an `error` event at the window, then the
/// console unless a listener canceled it.
/// Report an error object made by the host (not thrown by script):
/// `window.onerror` / `error` event with `error` set, else the console.
pub(crate) fn report_error_value(js: &mut dyn Js, w: &Rc<Window>, message: &str, error: Handle) {
    let message = format!("Uncaught {message}");
    let mut canceled = false;
    if w.error_depth.get() == 0 {
        w.error_depth.set(1);
        let extra = EventExtra::Error {
            message: message.clone(),
            filename: String::new(),
            lineno: 0,
            colno: 0,
            error: Some(error),
        };
        canceled = !fire_event_with(js, w, WINDOW_REF, "ErrorEvent", "error", false, true, extra);
        w.error_depth.set(0);
    }
    if !canceled {
        w.log(crate::host::ConsoleMessage {
            level: "error".into(),
            text: message,
            url: None,
            line: None,
        });
    }
}

pub(crate) fn report_exception(js: &mut dyn Js, w: &Rc<Window>, e: &JsError) {
    if e.kind == lb_js::JsErrorKind::Terminated || e.kind == lb_js::JsErrorKind::OutOfMemory {
        w.log(crate::host::ConsoleMessage {
            level: "error".into(),
            text: e.message.clone(),
            url: e.url.clone(),
            line: e.line,
        });
        return;
    }
    let head = e
        .stack
        .as_deref()
        .and_then(|s| s.lines().next())
        .map_or_else(|| e.message.clone(), str::to_owned);
    let message = format!("Uncaught {head}");
    let mut canceled = false;
    let error = if e.kind == lb_js::JsErrorKind::Exception {
        js.take_exception()
    } else {
        None
    };
    if w.error_depth.get() == 0 {
        w.error_depth.set(1);
        let extra = EventExtra::Error {
            message: message.clone(),
            filename: e.url.clone().unwrap_or_default(),
            lineno: e.line.unwrap_or(0),
            colno: e.column.map_or(0, |c| c + 1),
            error,
        };
        canceled = !fire_event_with(js, w, WINDOW_REF, "ErrorEvent", "error", false, true, extra);
        w.error_depth.set(0);
    }
    if !canceled && w.is_worker() {
        // A worker's uncaught error goes on to its `Worker` object.
        super::worker::forward_error(w, &message, e);
        return;
    }
    if !canceled {
        let text = match &e.stack {
            Some(s) => format!("Uncaught {s}"),
            None => message,
        };
        w.log(crate::host::ConsoleMessage {
            level: "error".into(),
            text,
            url: e.url.clone(),
            line: e.line,
        });
    }
}

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

/// Event targets: nodes, the window and event-target host objects.
fn this_target(cx: &mut CallCtx<'_, '_, '_>) -> Result<HostRef, HostError> {
    match cx.this_host() {
        Some(h) if h.kind & 0xff == K_NODE || h.kind == K_WINDOW || h.kind == K_OBJ => Ok(h),
        _ => Err(illegal()),
    }
}

fn this_event(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    match w.with_obj(id, |o| matches!(o, Obj::Event(_))) {
        Some(true) => Ok(id),
        _ => Err(illegal()),
    }
}

/// Parsed `addEventListener` options.
struct ListenOpts {
    capture: bool,
    once: bool,
    passive: Option<bool>,
    signal: Option<u32>,
}

fn listen_opts(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Result<ListenOpts, HostError> {
    let mut o = ListenOpts {
        capture: false,
        once: false,
        passive: None,
        signal: None,
    };
    if i >= cx.len() {
        return Ok(o);
    }
    match cx.arg_kind(i) {
        ValueKind::Object | ValueKind::Function => {
            let d = Dict::arg(cx, i);
            o.capture = d.bool(cx, "capture")?;
            o.once = d.bool(cx, "once")?;
            if let Some(h) = d.get(cx, "passive")? {
                o.passive = Some(super::truthy(&cx.handle_value(h)));
                cx.release(h);
            }
            if let Some(h) = d.get(cx, "signal")? {
                match cx.handle_host(h) {
                    Some(r) if r.kind == K_OBJ => o.signal = Some(r.id),
                    _ => {
                        cx.release(h);
                        d.release(cx);
                        return Err(HostError::type_error(
                            "Failed to execute 'addEventListener' on 'EventTarget': Failed to read the 'signal' property from 'AddEventListenerOptions': Failed to convert value to 'AbortSignal'.",
                        ));
                    }
                }
                cx.release(h);
            }
            d.release(cx);
        }
        _ => o.capture = cx.arg_bool(i),
    }
    Ok(o)
}

/// Default passive for `touchstart`/`wheel`/... on window, document and
/// root elements (Chrome intervention).
fn default_passive(w: &Window, t: HostRef, type_: &str) -> bool {
    if !matches!(type_, "touchstart" | "touchmove" | "wheel" | "mousewheel") {
        return false;
    }
    match w.decode_node(t) {
        None => t.kind == K_WINDOW,
        Some((slot, n)) => w.doc_slot(slot, |d| {
            n == lb_dom::Document::DOCUMENT
                || Some(n) == d.document_element()
                || Some(n) == d.body()
        }),
    }
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    ctor(reg, rt, "EventTarget", |cx, w| {
        let id = w.new_obj(Obj::EventTarget);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    method(reg, rt, "EventTarget", "addEventListener", |cx, w| {
        need_args(cx, 2, "addEventListener", "EventTarget")?;
        let t = this_target(cx)?;
        let type_ = cx.arg_string(0)?;
        if matches!(cx.arg_kind(1), ValueKind::Undefined | ValueKind::Null) {
            return Ok(());
        }
        if !matches!(cx.arg_kind(1), ValueKind::Object | ValueKind::Function) {
            return Err(HostError::type_error(
                "Failed to execute 'addEventListener' on 'EventTarget': parameter 2 is not of type 'Object'.",
            ));
        }
        let o = listen_opts(cx, 2)?;
        if let Some(sig) = o.signal
            && super::net::signal_aborted(w, sig)
        {
            return Ok(());
        }
        let cb = cx.arg_handle(1);
        let existing: Vec<Handle> = w
            .listeners
            .borrow()
            .get(&t)
            .map(|l| {
                l.iter()
                    .filter(|l| l.type_ == type_ && l.capture == o.capture && !l.removed.get())
                    .filter_map(|l| match l.kind {
                        ListenerKind::Callback(h) => Some(h),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if existing.into_iter().any(|h| cx.strict_equals(h, cb)) {
            cx.release(cb);
            return Ok(());
        }
        let passive = o.passive.unwrap_or_else(|| default_passive(w, t, &type_));
        let l = Rc::new(Listener {
            type_,
            kind: ListenerKind::Callback(cb),
            capture: o.capture,
            once: o.once,
            passive,
            removed: Cell::new(false),
            realm: cx.realm(),
        });
        w.listeners
            .borrow_mut()
            .entry(t)
            .or_default()
            .push(l.clone());
        if let Some(sig) = o.signal {
            super::net::signal_on_abort(w, sig, super::net::AbortAlgo::RemoveListener(t, l));
        }
        Ok(())
    });
    method(reg, rt, "EventTarget", "removeEventListener", |cx, w| {
        need_args(cx, 2, "removeEventListener", "EventTarget")?;
        let t = this_target(cx)?;
        let type_ = cx.arg_string(0)?;
        if !matches!(cx.arg_kind(1), ValueKind::Object | ValueKind::Function) {
            return Ok(());
        }
        let capture = listen_opts(cx, 2)?.capture;
        let cb = cx.arg_handle(1);
        let candidates: Vec<Rc<Listener>> = w
            .listeners
            .borrow()
            .get(&t)
            .map(|l| {
                l.iter()
                    .filter(|l| l.type_ == type_ && l.capture == capture)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for l in candidates {
            if let ListenerKind::Callback(h) = l.kind
                && cx.strict_equals(h, cb)
            {
                remove_listener(w, t, &l);
                break;
            }
        }
        cx.release(cb);
        Ok(())
    });
    method(reg, rt, "EventTarget", "dispatchEvent", |cx, w| {
        need_args(cx, 1, "dispatchEvent", "EventTarget")?;
        let t = this_target(cx)?;
        let ev = match cx.arg_host(0) {
            Some(h)
                if h.kind == K_OBJ
                    && w.with_obj(h.id, |o| matches!(o, Obj::Event(_))) == Some(true) =>
            {
                h.id
            }
            _ => {
                return Err(HostError::type_error(
                    "Failed to execute 'dispatchEvent' on 'EventTarget': parameter 1 is not of type 'Event'.",
                ));
            }
        };
        let (dispatching, initialized) =
            with_ev(w, ev, |e| (e.dispatching, e.initialized)).unwrap_or((false, true));
        if dispatching || !initialized {
            return Err(cx_err(
                w,
                "InvalidStateError",
                "Failed to execute 'dispatchEvent' on 'EventTarget': The event is already being dispatched.",
            ));
        }
        with_ev(w, ev, |e| {
            e.trusted = false;
            e.canceled = false;
        });
        let r = dispatch(cx, w, t, ev);
        w.after_mutation(cx);
        cx.ret(r)
    });

    // Event
    for iface in [
        "Event",
        "UIEvent",
        "MouseEvent",
        "KeyboardEvent",
        "FocusEvent",
        "InputEvent",
        "PointerEvent",
        "WheelEvent",
        "CompositionEvent",
        "TouchEvent",
        "SubmitEvent",
        "AnimationEvent",
        "TransitionEvent",
        "PageTransitionEvent",
        "StorageEvent",
        "BeforeUnloadEvent",
    ] {
        ctor(reg, rt, iface, move |cx, w| {
            need_args(cx, 1, "", iface).map_err(|_| {
                HostError::type_error(format!(
                    "Failed to construct '{iface}': 1 argument required, but only 0 present."
                ))
            })?;
            let type_ = cx.arg_string(0)?;
            let d = Dict::arg(cx, 1);
            let mut e = EventData::new(
                iface,
                &type_,
                d.bool(cx, "bubbles")?,
                d.bool(cx, "cancelable")?,
            );
            e.composed = d.bool(cx, "composed")?;
            if iface == "PageTransitionEvent" {
                e.extra = EventExtra::PageTransition {
                    persisted: d.bool(cx, "persisted")?,
                };
            }
            d.release(cx);
            e.time = coarse_now(w);
            let id = w.new_obj(Obj::Event(Box::new(e)));
            cx.init_this(HostRef::new(K_OBJ, id))
        });
    }
    ctor(reg, rt, "CustomEvent", |cx, w| {
        if cx.is_empty() {
            return Err(HostError::type_error(
                "Failed to construct 'CustomEvent': 1 argument required, but only 0 present.",
            ));
        }
        let type_ = cx.arg_string(0)?;
        let d = Dict::arg(cx, 1);
        let mut e = EventData::new(
            "CustomEvent",
            &type_,
            d.bool(cx, "bubbles")?,
            d.bool(cx, "cancelable")?,
        );
        e.composed = d.bool(cx, "composed")?;
        e.extra = EventExtra::Custom(d.get(cx, "detail")?);
        d.release(cx);
        e.time = coarse_now(w);
        let id = w.new_obj(Obj::Event(Box::new(e)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    ctor(reg, rt, "ProgressEvent", |cx, w| {
        if cx.is_empty() {
            return Err(HostError::type_error(
                "Failed to construct 'ProgressEvent': 1 argument required, but only 0 present.",
            ));
        }
        let type_ = cx.arg_string(0)?;
        let d = Dict::arg(cx, 1);
        let mut e = EventData::new(
            "ProgressEvent",
            &type_,
            d.bool(cx, "bubbles")?,
            d.bool(cx, "cancelable")?,
        );
        e.extra = EventExtra::Progress {
            computable: d.bool(cx, "lengthComputable")?,
            loaded: d.number(cx, "loaded")?.unwrap_or(0.0),
            total: d.number(cx, "total")?.unwrap_or(0.0),
        };
        d.release(cx);
        e.time = coarse_now(w);
        let id = w.new_obj(Obj::Event(Box::new(e)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    ctor(reg, rt, "HashChangeEvent", |cx, w| {
        let type_ = cx.arg_string(0)?;
        let d = Dict::arg(cx, 1);
        let mut e = EventData::new(
            "HashChangeEvent",
            &type_,
            d.bool(cx, "bubbles")?,
            d.bool(cx, "cancelable")?,
        );
        e.extra = EventExtra::HashChange {
            old: d.string(cx, "oldURL")?.unwrap_or_default(),
            new: d.string(cx, "newURL")?.unwrap_or_default(),
        };
        d.release(cx);
        e.time = coarse_now(w);
        let id = w.new_obj(Obj::Event(Box::new(e)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    ctor(reg, rt, "ErrorEvent", |cx, w| {
        let type_ = cx.arg_string(0)?;
        let d = Dict::arg(cx, 1);
        let mut e = EventData::new(
            "ErrorEvent",
            &type_,
            d.bool(cx, "bubbles")?,
            d.bool(cx, "cancelable")?,
        );
        e.extra = EventExtra::Error {
            message: d.string(cx, "message")?.unwrap_or_default(),
            filename: d.string(cx, "filename")?.unwrap_or_default(),
            lineno: d.number(cx, "lineno")?.unwrap_or(0.0) as u32,
            colno: d.number(cx, "colno")?.unwrap_or(0.0) as u32,
            error: d.get(cx, "error")?,
        };
        d.release(cx);
        e.time = coarse_now(w);
        let id = w.new_obj(Obj::Event(Box::new(e)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    ctor(reg, rt, "CloseEvent", |cx, w| {
        need_args(cx, 1, "construct", "CloseEvent").map_err(|_| {
            HostError::type_error(
                "Failed to construct 'CloseEvent': 1 argument required, but only 0 present.",
            )
        })?;
        let type_ = cx.arg_string(0)?;
        let d = Dict::arg(cx, 1);
        let mut e = EventData::new(
            "CloseEvent",
            &type_,
            d.bool(cx, "bubbles")?,
            d.bool(cx, "cancelable")?,
        );
        e.extra = EventExtra::Close {
            was_clean: d.bool(cx, "wasClean")?,
            code: d.number(cx, "code")?.unwrap_or(0.0) as u16,
            reason: d.string(cx, "reason")?.unwrap_or_default(),
        };
        d.release(cx);
        e.time = coarse_now(w);
        let id = w.new_obj(Obj::Event(Box::new(e)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    ctor(reg, rt, "MessageEvent", |cx, w| {
        let type_ = cx.arg_string(0)?;
        let d = Dict::arg(cx, 1);
        let mut e = EventData::new(
            "MessageEvent",
            &type_,
            d.bool(cx, "bubbles")?,
            d.bool(cx, "cancelable")?,
        );
        e.extra = EventExtra::Message {
            data: d.get(cx, "data")?,
            origin: d.string(cx, "origin")?.unwrap_or_default(),
            source: d.get(cx, "source")?,
            ports: None,
        };
        d.release(cx);
        e.time = coarse_now(w);
        let id = w.new_obj(Obj::Event(Box::new(e)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    ctor(reg, rt, "PopStateEvent", |cx, w| {
        let type_ = cx.arg_string(0)?;
        let d = Dict::arg(cx, 1);
        let mut e = EventData::new(
            "PopStateEvent",
            &type_,
            d.bool(cx, "bubbles")?,
            d.bool(cx, "cancelable")?,
        );
        let state = match d.get(cx, "state")? {
            Some(h) => {
                let v = cx.handle_value(h);
                cx.release(h);
                match v {
                    JsValue::Json(s) => Some(s),
                    JsValue::String(s) => Some(super::js_string_literal(&s)),
                    JsValue::Number(n) => Some(n.to_string()),
                    JsValue::Bool(b) => Some(b.to_string()),
                    _ => None,
                }
            }
            None => None,
        };
        e.extra = EventExtra::PopState(state);
        d.release(cx);
        e.time = coarse_now(w);
        let id = w.new_obj(Obj::Event(Box::new(e)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });

    macro_rules! ev_get {
        ($name:literal, |$cx:ident, $w:ident, $e:ident| $body:expr) => {
            getter(reg, rt, "Event", $name, |$cx, $w| {
                let id = this_event($cx, $w)?;
                let $e = with_ev($w, id, |e| e.clone()).ok_or_else(illegal)?;
                $body
            });
        };
    }
    ev_get!("type", |cx, w, e| cx.ret(e.type_));
    ev_get!("target", |cx, w, e| cx
        .ret(e.target.map_or(Arg::Null, |t| w.host_arg(t))));
    ev_get!("srcElement", |cx, w, e| cx
        .ret(e.target.map_or(Arg::Null, |t| w.host_arg(t))));
    ev_get!("currentTarget", |cx, w, e| cx
        .ret(e.current_target.map_or(Arg::Null, |t| w.host_arg(t))));
    ev_get!("eventPhase", |cx, w, e| cx.ret(u32::from(e.phase)));
    ev_get!("bubbles", |cx, w, e| cx.ret(e.bubbles));
    ev_get!("cancelable", |cx, w, e| cx.ret(e.cancelable));
    ev_get!("composed", |cx, w, e| cx.ret(e.composed));
    ev_get!("defaultPrevented", |cx, w, e| cx.ret(e.canceled));
    ev_get!("returnValue", |cx, w, e| cx.ret(!e.canceled));
    ev_get!("cancelBubble", |cx, w, e| cx.ret(e.stop));
    ev_get!("isTrusted", |cx, w, e| cx.ret(e.trusted));
    ev_get!("timeStamp", |cx, w, e| cx.ret(e.time));
    method(reg, rt, "Event", "composedPath", |cx, w| {
        let id = this_event(cx, w)?;
        let (path, cur) =
            with_ev(w, id, |e| (e.path.clone(), e.current_target)).unwrap_or_default();
        let items = composed_path(&path, cur)
            .into_iter()
            .map(|h| w.host_arg(h))
            .collect();
        cx.ret(Arg::Array(items))
    });
    method(reg, rt, "Event", "stopPropagation", |cx, w| {
        let id = this_event(cx, w)?;
        with_ev(w, id, |e| e.stop = true);
        Ok(())
    });
    method(reg, rt, "Event", "stopImmediatePropagation", |cx, w| {
        let id = this_event(cx, w)?;
        with_ev(w, id, |e| {
            e.stop = true;
            e.stop_immediate = true
        });
        Ok(())
    });
    method(reg, rt, "Event", "preventDefault", |cx, w| {
        let id = this_event(cx, w)?;
        with_ev(w, id, |e| {
            if e.cancelable && !e.in_passive {
                e.canceled = true
            }
        });
        Ok(())
    });
    setter(reg, rt, "Event", "returnValue", |cx, w| {
        let id = this_event(cx, w)?;
        let v = cx.arg_bool(0);
        with_ev(w, id, |e| {
            if !v && e.cancelable && !e.in_passive {
                e.canceled = true
            }
        });
        Ok(())
    });
    setter(reg, rt, "Event", "cancelBubble", |cx, w| {
        let id = this_event(cx, w)?;
        let v = cx.arg_bool(0);
        with_ev(w, id, |e| {
            if v {
                e.stop = true
            }
        });
        Ok(())
    });
    method(reg, rt, "Event", "initEvent", |cx, w| {
        need_args(cx, 1, "initEvent", "Event")?;
        let id = this_event(cx, w)?;
        let type_ = cx.arg_string(0)?;
        let (b, c) = (cx.arg_bool(1), cx.arg_bool(2));
        with_ev(w, id, |e| {
            if e.dispatching {
                return;
            }
            e.initialized = true;
            e.type_ = type_;
            e.bubbles = b;
            e.cancelable = c;
            e.stop = false;
            e.stop_immediate = false;
            e.canceled = false;
            e.trusted = false;
            e.target = None;
        });
        Ok(())
    });
    getter(reg, rt, "CustomEvent", "detail", |cx, w| {
        let id = this_event(cx, w)?;
        match with_ev(w, id, |e| e.extra.clone()) {
            Some(EventExtra::Custom(Some(h))) => cx.ret(Arg::Handle(h)),
            _ => cx.ret(Arg::Null),
        }
    });
    method(reg, rt, "CustomEvent", "initCustomEvent", |cx, w| {
        let id = this_event(cx, w)?;
        let type_ = cx.arg_string(0)?;
        let (b, c) = (cx.arg_bool(1), cx.arg_bool(2));
        let detail = (cx.len() > 3).then(|| cx.arg_handle(3));
        with_ev(w, id, |e| {
            if e.dispatching {
                return;
            }
            e.initialized = true;
            e.type_ = type_;
            e.bubbles = b;
            e.cancelable = c;
            e.extra = EventExtra::Custom(detail);
        });
        Ok(())
    });
    macro_rules! extra_get {
        ($iface:literal, $name:literal, |$cx:ident, $x:ident| $body:expr) => {
            getter(reg, rt, $iface, $name, |$cx, w| {
                let id = this_event($cx, w)?;
                let $x = with_ev(w, id, |e| e.extra.clone()).unwrap_or_default();
                $body
            });
        };
    }
    extra_get!("ProgressEvent", "lengthComputable", |cx, x| cx.ret(
        matches!(
            x,
            EventExtra::Progress {
                computable: true,
                ..
            }
        )
    ));
    extra_get!("ProgressEvent", "loaded", |cx, x| cx.ret(match x {
        EventExtra::Progress { loaded, .. } => loaded,
        _ => 0.0,
    }));
    extra_get!("ProgressEvent", "total", |cx, x| cx.ret(match x {
        EventExtra::Progress { total, .. } => total,
        _ => 0.0,
    }));
    extra_get!("HashChangeEvent", "oldURL", |cx, x| cx.ret(match x {
        EventExtra::HashChange { old, .. } => old,
        _ => String::new(),
    }));
    extra_get!("HashChangeEvent", "newURL", |cx, x| cx.ret(match x {
        EventExtra::HashChange { new, .. } => new,
        _ => String::new(),
    }));
    extra_get!("PopStateEvent", "state", |cx, x| cx.ret(match x {
        EventExtra::PopState(Some(s)) => Arg::Json(s),
        _ => Arg::Null,
    }));
    extra_get!("PageTransitionEvent", "persisted", |cx, x| cx
        .ret(matches!(x, EventExtra::PageTransition { persisted: true })));
    extra_get!("ErrorEvent", "message", |cx, x| cx.ret(match x {
        EventExtra::Error { message, .. } => message,
        _ => String::new(),
    }));
    extra_get!("ErrorEvent", "filename", |cx, x| cx.ret(match x {
        EventExtra::Error { filename, .. } => filename,
        _ => String::new(),
    }));
    extra_get!("ErrorEvent", "lineno", |cx, x| cx.ret(match x {
        EventExtra::Error { lineno, .. } => lineno,
        _ => 0,
    }));
    extra_get!("ErrorEvent", "colno", |cx, x| cx.ret(match x {
        EventExtra::Error { colno, .. } => colno,
        _ => 0,
    }));
    extra_get!("ErrorEvent", "error", |cx, x| cx.ret(match x {
        EventExtra::Error { error: Some(h), .. } => Arg::Handle(h),
        _ => Arg::Null,
    }));
    extra_get!("MessageEvent", "data", |cx, x| cx.ret(match x {
        EventExtra::Message { data: Some(h), .. } => Arg::Handle(h),
        _ => Arg::Null,
    }));
    extra_get!("CloseEvent", "code", |cx, x| cx.ret(match x {
        EventExtra::Close { code, .. } => u32::from(code),
        _ => 0,
    }));
    extra_get!("CloseEvent", "reason", |cx, x| cx.ret(match x {
        EventExtra::Close { reason, .. } => reason,
        _ => String::new(),
    }));
    extra_get!("CloseEvent", "wasClean", |cx, x| cx.ret(matches!(
        x,
        EventExtra::Close {
            was_clean: true,
            ..
        }
    )));
    extra_get!("MessageEvent", "origin", |cx, x| cx.ret(match x {
        EventExtra::Message { origin, .. } => origin,
        _ => String::new(),
    }));
    getter(reg, rt, "MessageEvent", "ports", |cx, w| {
        let id = this_event(cx, w)?;
        let have = with_ev(w, id, |e| match &e.extra {
            EventExtra::Message { ports, .. } => Some(*ports),
            _ => None,
        })
        .flatten();
        let ports = match have {
            Some(Some(h)) => Some(h),
            _ => {
                let h = super::worker::empty_ports(cx, w);
                with_ev(w, id, |e| {
                    if let EventExtra::Message { ports, .. } = &mut e.extra {
                        *ports = h;
                    }
                });
                h
            }
        };
        match ports {
            Some(h) => cx.ret(Arg::Handle(h)),
            None => cx.ret(Arg::Array(Vec::new())),
        }
    });
    extra_get!("MessageEvent", "source", |cx, x| cx.ret(match x {
        EventExtra::Message {
            source: Some(h), ..
        } => Arg::Handle(h),
        _ => Arg::Null,
    }));

    register_handlers(reg, rt);
}

/// `on*` event handler IDL attributes of every interface in the table.
fn register_handlers(reg: &mut Registry, rt: &Runtime) {
    for d in crate::generated::chromium::INTERFACES {
        for a in d.attributes {
            if !a.name.starts_with("on") || a.ty != AttrType::Null || a.readonly {
                continue;
            }
            let iface = d.name.get();
            let name = a.name.get();
            let forwards = matches!(iface, "HTMLBodyElement" | "HTMLFrameSetElement")
                && body_forwards_to_window(name);
            let target = move |cx: &mut CallCtx<'_, '_, '_>| -> Result<HostRef, HostError> {
                let t = this_target(cx)?;
                Ok(if forwards { WINDOW_REF } else { t })
            };
            getter(reg, rt, iface, name, move |cx, w| {
                let t = target(cx)?;
                if let Some(f) = handler_function(cx, w, t, name) {
                    cx.ret(Arg::Handle(f))
                } else {
                    cx.ret(Arg::Null)
                }
            });
            setter(reg, rt, iface, name, move |cx, w| {
                let t = target(cx)?;
                let v = match cx.arg_kind(0) {
                    ValueKind::Object | ValueKind::Function => {
                        Some(HandlerValue::Compiled(cx.arg_handle(0)))
                    }
                    _ => None,
                };
                set_handler(w, t, name, v);
                Ok(())
            });
        }
    }
}

fn cx_err(w: &Window, name: &str, msg: &str) -> HostError {
    w.rt.err(name, msg)
}

/// `document.createEvent(interface)`.
pub(crate) fn create_event(w: &Window, kind: &str) -> Option<u32> {
    let iface: &'static str = match kind.to_ascii_lowercase().as_str() {
        "event" | "events" | "htmlevents" | "svgevents" => "Event",
        "customevent" => "CustomEvent",
        "uievent" | "uievents" => "UIEvent",
        "mouseevent" | "mouseevents" => "MouseEvent",
        "keyboardevent" => "KeyboardEvent",
        "focusevent" => "FocusEvent",
        "messageevent" => "MessageEvent",
        "hashchangeevent" => "HashChangeEvent",
        "compositionevent" => "CompositionEvent",
        "storageevent" => "StorageEvent",
        "textevent" => "TextEvent",
        "touchevent" => "TouchEvent",
        "beforeunloadevent" => "BeforeUnloadEvent",
        "devicemotionevent" => "DeviceMotionEvent",
        "deviceorientationevent" => "DeviceOrientationEvent",
        _ => return None,
    };
    let mut e = EventData::new(iface, "", false, false);
    e.initialized = false;
    e.time = coarse_now(w);
    if iface == "CustomEvent" {
        e.extra = EventExtra::Custom(None);
    }
    Some(w.new_obj(Obj::Event(Box::new(e))))
}

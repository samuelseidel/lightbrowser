//! Implementations of the core Web APIs on top of `lb-dom`, `lb-css`,
//! `lb-layout` and the page layer's [`NetHost`] (chrome build).
//!
//! # Model
//!
//! - [`Runtime`]: one per engine. Registers every implementation in a
//!   [`Registry`] ([`Runtime::new`]) and maps realms to windows. Host
//!   functions look up their [`Window`] by the calling realm.
//! - [`Window`]: one per frame document (created anew on each
//!   navigation). Owns the documents (main document in slot 0, plus
//!   `DOMParser` / `createHTMLDocument` documents), host objects, event
//!   listeners, task queues, timers, animation frame callbacks, the
//!   console log, history and the lazily created layout engine.
//! - Host refs: [`K_NODE`] (`kind = K_NODE | slot << 8`, `id` = node id),
//!   [`K_WINDOW`] (the global) and [`K_OBJ`] (`id` indexes the window's
//!   object table).
//! - [`input`]: trusted user input for CDP `Input.*` (pointer, mouse and
//!   keyboard events in Chrome's order, focus, activation, text editing)
//!   and the UI event interfaces.
//! - Fingerprint surfaces from the persona: `canvas` (2D, readback noise),
//!   `webgl` (stub renderer, persona GPU, limits and extensions), `audio`
//!   (Web Audio DSP with seeded output noise), `fonts` (Font Loading API;
//!   measurement detection runs through layout's font tables).
//! - `navigator`, `rtc`, `chrome`: the persona-driven fingerprint surface
//!   (plugins, UA client hints, permissions, battery, media devices,
//!   voices, `performance.memory`, WebRTC without networking,
//!   `window.chrome`, ICU time zone and locale per realm).
//! - [`Js`]: what event dispatch and callbacks need from the engine. It is
//!   implemented for `CallCtx` (inside a host function) and [`EngineJs`]
//!   (from the event loop), so the same code runs in both places.
//!
//! The page layer (lb-page) drives the event loop: it pops tasks
//! ([`Window::take_task`]), runs due timers ([`Window::run_due_timer`]) and
//! animation frames, performs microtask checkpoints and parses documents.
//!
//! Known gaps: no weak wrappers (host objects live until the window is
//! dropped), listeners from isolated worlds are called with wrappers of
//! the dispatching realm, no shadow DOM, no synchronous XHR.

#![allow(clippy::type_complexity)]

mod audio;
mod audio_dsp;
pub mod canvas;
mod chrome;
mod coll;
mod dom;
mod events;
mod fonts;
pub mod frames;
mod gpu;
mod image;
pub mod input;
mod matrix;
mod media;
mod misc;
mod navigator;
mod net;
mod rtc;
pub mod screenshot;
pub mod script;
mod serializer;
mod style;
mod table;
mod tags;
mod webgl;
mod webgl_ext;
mod window;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use lb_dom::{Document, Mutation, NodeId, NodeType, QualName};
use lb_js::{
    Arg, CallCtx, Engine, Handle, HostError, HostRef, InterfaceId, JsError, JsValue, RealmId,
    Resolver, ScriptOrigin, ValueKind,
};
use lb_persona::{Persona, PersonaProfile};
use url::Url;

use crate::dom_exception::DomExceptions;
use crate::host::{ConsoleMessage, NetHost, StorageArea};
use crate::install::Bindings;
use crate::registry::{Registry, host_fn};

pub use canvas::{CanvasTarget, ContextFactory, ExternalContext, canvas_size};
pub use chrome::LoadInfo;
pub use events::{EventData, EventExtra, fire_event, fire_event_with};
pub use image::load_visible_lazy_images;
pub use script::ScriptKind;
pub(crate) use webgl::{extension_names, extension_spec};

/// Host ref kind of DOM nodes (`kind = K_NODE | document_slot << 8`).
pub const K_NODE: u32 = 1;
/// Host ref kind of the window global.
pub const K_WINDOW: u32 = 2;
/// Host ref kind of other host objects (`id` indexes [`Window`]'s table).
pub const K_OBJ: u32 = 3;
/// The window global's host ref.
pub const WINDOW_REF: HostRef = HostRef::new(K_WINDOW, 0);

/// Host ref of node `id` in document `slot`.
pub fn node_ref(slot: u32, id: NodeId) -> HostRef {
    HostRef::new(K_NODE | (slot << 8), id.0)
}

/// `(slot, node)` of a node host ref.
pub fn decode_node(h: HostRef) -> Option<(u32, NodeId)> {
    (h.kind & 0xff == K_NODE).then_some((h.kind >> 8, NodeId(h.id)))
}

fn obj_ref(id: u32) -> HostRef {
    HostRef::new(K_OBJ, id)
}

// ---------------------------------------------------------------------------
// Js: the engine as event dispatch sees it
// ---------------------------------------------------------------------------

/// Engine operations needed to run callbacks, from a host function or from
/// the event loop.
pub trait Js {
    fn realm(&self) -> RealmId;
    /// Call a function; exceptions are caught and returned.
    fn call(&mut self, f: Handle, this: Arg, args: &[Arg]) -> Result<JsValue, JsError>;
    /// Like `call`, keeping the result.
    fn call_handle(&mut self, f: Handle, this: Arg, args: &[Arg]) -> Result<Handle, JsError>;
    /// Run a classic script; exceptions are caught and returned.
    fn eval(&mut self, src: &str, origin: ScriptOrigin<'_>) -> Result<JsValue, JsError>;
    fn eval_handle(&mut self, src: &str, origin: ScriptOrigin<'_>) -> Result<Handle, JsError>;
    /// `obj[key]`, `None` if that threw.
    fn get(&mut self, obj: Handle, key: &str) -> Option<Handle>;
    fn kind(&mut self, h: Handle) -> ValueKind;
    fn value(&mut self, h: Handle) -> JsValue;
    fn release(&mut self, h: Handle);
    fn microtask(&mut self, f: Handle);
    fn resolve(&mut self, r: Resolver, v: Arg);
    fn reject(&mut self, r: Resolver, e: HostError);
    /// True when called from the event loop (no JS on the stack): a
    /// microtask checkpoint runs after each callback.
    fn top_level(&self) -> bool;
    /// Materialize a value (e.g. a new `DOMException`) as a handle.
    fn make(&mut self, v: Arg) -> Option<Handle>;
    /// Realm building (child frames), from the event loop or a callback.
    fn realm_ops(&mut self) -> &mut dyn lb_js::RealmOps;
}

impl Js for CallCtx<'_, '_, '_> {
    fn realm(&self) -> RealmId {
        CallCtx::realm(self)
    }
    fn call(&mut self, f: Handle, this: Arg, args: &[Arg]) -> Result<JsValue, JsError> {
        self.try_call(f, this, args)
    }
    fn call_handle(&mut self, f: Handle, this: Arg, args: &[Arg]) -> Result<Handle, JsError> {
        self.try_call_handle(f, this, args)
    }
    fn eval(&mut self, src: &str, origin: ScriptOrigin<'_>) -> Result<JsValue, JsError> {
        self.try_evaluate(src, origin)
    }
    fn eval_handle(&mut self, src: &str, origin: ScriptOrigin<'_>) -> Result<Handle, JsError> {
        self.try_evaluate_handle(src, origin)
    }
    fn get(&mut self, obj: Handle, key: &str) -> Option<Handle> {
        self.get_property(obj, key).ok()
    }
    fn kind(&mut self, h: Handle) -> ValueKind {
        self.handle_kind(h)
    }
    fn value(&mut self, h: Handle) -> JsValue {
        self.handle_value(h)
    }
    fn release(&mut self, h: Handle) {
        CallCtx::release(self, h)
    }
    fn microtask(&mut self, f: Handle) {
        let _ = self.enqueue_microtask(f);
    }
    fn resolve(&mut self, r: Resolver, v: Arg) {
        let _ = self.resolve_promise(r, v);
    }
    fn reject(&mut self, r: Resolver, e: HostError) {
        let _ = self.reject_promise(r, e);
    }
    fn top_level(&self) -> bool {
        false
    }
    fn make(&mut self, v: Arg) -> Option<Handle> {
        self.make_handle(v).ok()
    }
    fn realm_ops(&mut self) -> &mut dyn lb_js::RealmOps {
        self
    }
}

/// [`Js`] over an engine, used from the event loop.
pub struct EngineJs<'e> {
    pub engine: &'e mut Engine,
    pub realm: RealmId,
}

impl Js for EngineJs<'_> {
    fn realm(&self) -> RealmId {
        self.realm
    }
    fn call(&mut self, f: Handle, this: Arg, args: &[Arg]) -> Result<JsValue, JsError> {
        let r = self.engine.call(self.realm, f, this, args);
        self.engine.perform_microtask_checkpoint();
        r
    }
    fn call_handle(&mut self, f: Handle, this: Arg, args: &[Arg]) -> Result<Handle, JsError> {
        let r = self.engine.call_handle(self.realm, f, this, args);
        self.engine.perform_microtask_checkpoint();
        r
    }
    fn eval(&mut self, src: &str, origin: ScriptOrigin<'_>) -> Result<JsValue, JsError> {
        let r = self.engine.evaluate(self.realm, src, origin);
        self.engine.perform_microtask_checkpoint();
        r
    }
    fn eval_handle(&mut self, src: &str, origin: ScriptOrigin<'_>) -> Result<Handle, JsError> {
        self.engine.evaluate_handle(self.realm, src, origin)
    }
    fn get(&mut self, obj: Handle, key: &str) -> Option<Handle> {
        self.engine.get_property(self.realm, obj, key).ok()
    }
    fn kind(&mut self, h: Handle) -> ValueKind {
        self.engine
            .value_kind(self.realm, h)
            .unwrap_or(ValueKind::Undefined)
    }
    fn value(&mut self, h: Handle) -> JsValue {
        self.engine
            .get_value(self.realm, h)
            .unwrap_or(JsValue::Undefined)
    }
    fn release(&mut self, h: Handle) {
        self.engine.release(h)
    }
    fn microtask(&mut self, f: Handle) {
        let _ = self.engine.enqueue_microtask(self.realm, f);
    }
    fn resolve(&mut self, r: Resolver, v: Arg) {
        let _ = self.engine.resolve_promise(r, v);
    }
    fn reject(&mut self, r: Resolver, e: HostError) {
        let _ = self.engine.reject_promise(r, e);
    }
    fn top_level(&self) -> bool {
        true
    }
    fn make(&mut self, v: Arg) -> Option<Handle> {
        self.engine.make_handle(self.realm, v).ok()
    }
    fn realm_ops(&mut self) -> &mut dyn lb_js::RealmOps {
        self.engine
    }
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

struct RtInner {
    windows: RefCell<HashMap<RealmId, Rc<Window>>>,
    bindings: RefCell<Option<Bindings>>,
    ids: RefCell<HashMap<&'static str, InterfaceId>>,
    dom: DomExceptions,
    /// `getContext` factories for non-2D kinds (WebGL).
    canvas_factories: canvas::Factories,
    /// Font tables and UA stylesheet per OS, shared by the layout engines
    /// of this engine's documents (parsed once, not per document).
    layout_bases: RefCell<Vec<lb_layout::LayoutBase>>,
}

/// The implementations of one engine. Cheap to clone.
#[derive(Clone)]
pub struct Runtime(Rc<RtInner>);

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("windows", &self.0.windows.borrow().len())
            .finish()
    }
}

impl Runtime {
    /// The shared layout inputs for `os` (made on first use).
    pub(crate) fn layout_base(&self, os: lb_css::OsKind) -> lb_layout::LayoutBase {
        let mut bases = self.0.layout_bases.borrow_mut();
        if let Some(b) = bases.iter().find(|b| b.os() == os) {
            return b.clone();
        }
        let b = lb_layout::LayoutBase::for_os(os);
        bases.push(b.clone());
        b
    }

    /// Register every implementation in `reg`. Call before
    /// `Bindings::define`, then [`Runtime::set_bindings`].
    pub fn new(reg: &mut Registry) -> Runtime {
        let rt = Runtime(Rc::new(RtInner {
            windows: RefCell::new(HashMap::new()),
            bindings: RefCell::new(None),
            ids: RefCell::new(HashMap::new()),
            dom: reg.dom_exceptions(),
            canvas_factories: canvas::Factories::default(),
            layout_bases: RefCell::new(Vec::new()),
        }));
        dom::register(reg, &rt);
        coll::register(reg, &rt);
        style::register(reg, &rt);
        events::register(reg, &rt);
        input::register(reg, &rt);
        serializer::register(reg, &rt);
        window::register(reg, &rt);
        net::register(reg, &rt);
        misc::register(reg, &rt);
        canvas::register(reg, &rt);
        image::register(reg, &rt);
        webgl::register(reg, &rt);
        audio::register(reg, &rt);
        fonts::register(reg, &rt);
        navigator::register(reg, &rt);
        media::register(reg, &rt);
        gpu::register(reg, &rt);
        frames::register(reg, &rt);
        table::register(reg, &rt);
        inherit_redeclared(reg);
        rt
    }

    /// Hand over the bindings defined from the registry.
    pub fn set_bindings(&self, b: Bindings) {
        *self.0.bindings.borrow_mut() = Some(b);
        self.0.ids.borrow_mut().clear();
    }

    pub fn bindings(&self) -> Option<Bindings> {
        self.0.bindings.borrow().clone()
    }

    /// Make `w` the window of `realm` (main world or isolated world).
    pub fn attach(&self, realm: RealmId, w: Rc<Window>) {
        self.0.windows.borrow_mut().insert(realm, w);
    }

    /// Forget a realm (before dropping it).
    pub fn detach(&self, realm: RealmId) -> Option<Rc<Window>> {
        self.0.windows.borrow_mut().remove(&realm)
    }

    pub fn window(&self, realm: RealmId) -> Option<Rc<Window>> {
        self.0.windows.borrow().get(&realm).cloned()
    }

    /// Not inlined: every binding closure made by [`wf`] calls it, and
    /// one shared copy keeps the map lookup out of each of them (M6
    /// binary size).
    #[inline(never)]
    pub(crate) fn win(&self, realm: RealmId) -> Result<Rc<Window>, HostError> {
        self.window(realm)
            .ok_or_else(|| HostError::type_error("Illegal invocation"))
    }

    /// Interface id by name. Panics before `set_bindings` (a bug).
    pub fn iface(&self, name: &'static str) -> InterfaceId {
        if let Some(id) = self.0.ids.borrow().get(name) {
            return *id;
        }
        let id = self
            .0
            .bindings
            .borrow()
            .as_ref()
            .and_then(|b| b.interface_id(name))
            .or_else(|| {
                self.0
                    .bindings
                    .borrow()
                    .as_ref()
                    .and_then(|b| b.interface_id("Object"))
            })
            .unwrap_or_else(|| panic!("no interface {name}"));
        self.0.ids.borrow_mut().insert(name, id);
        id
    }

    pub fn dom(&self) -> &DomExceptions {
        &self.0.dom
    }

    /// Throwable DOMException.
    pub fn err(&self, name: &str, msg: impl Into<String>) -> HostError {
        self.0.dom.error(name, msg)
    }
}

/// Interfaces redeclare inherited members (Trusted Types makes
/// `HTMLScriptElement.textContent`, `HTMLElement.innerText` ...): such a
/// member without its own implementation uses the nearest ancestor's.
fn inherit_redeclared(reg: &mut Registry) {
    // (interface, member) pairs that must not inherit (different meaning).
    const KEEP: &[(&str, &str)] = &[
        ("HTMLSelectElement", "remove"),
        ("HTMLOptionsCollection", "remove"),
    ];
    reg.inherit_redeclared(&crate::generated::chromium::TABLE, KEEP);
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

/// What the page layer passes to create a window.
pub struct WindowInit {
    pub url: Url,
    pub persona: Arc<Persona>,
    /// Full persona, when known (navigator, screen, layout, fonts).
    pub profile: Option<Arc<PersonaProfile>>,
    pub net: Rc<dyn NetHost>,
    pub local_storage: StorageArea,
    pub session_storage: StorageArea,
    /// Timers and animation frames run on a virtual clock.
    pub virtual_time: bool,
    /// Wakes the page's event loop after a task was queued.
    pub wake: Rc<dyn Fn()>,
    pub referrer: String,
    /// `Content-Type` of the document (`text/html`).
    pub content_type: String,
    /// Encoding the document was decoded with.
    pub charset: &'static str,
    /// History of the previous document in this frame, kept across
    /// same-frame navigations.
    pub history_len: u32,
}

/// HTML task sources, highest priority first (the event loop prefers
/// user interaction, like Chrome).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskSource {
    UserInteraction = 0,
    DomManipulation = 1,
    Networking = 2,
    PostedMessage = 3,
    Timer = 4,
}

const SOURCES: usize = 5;

/// A queued task. Runs from the event loop with no JS on the stack.
pub type TaskFn = Box<dyn FnOnce(&mut dyn Js, &Rc<Window>)>;

/// `document.readyState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyState {
    Loading,
    Interactive,
    Complete,
}

impl ReadyState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadyState::Loading => "loading",
            ReadyState::Interactive => "interactive",
            ReadyState::Complete => "complete",
        }
    }
}

/// The page clock: real or virtual milliseconds since navigation start.
#[derive(Debug)]
pub struct Clock {
    start: Instant,
    /// Wall-clock ms at navigation start (`performance.timeOrigin`).
    pub time_origin: f64,
    virtual_now: Cell<Option<f64>>,
}

impl Clock {
    pub fn new(virtual_time: bool) -> Clock {
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64() * 1000.0);
        Clock {
            start: Instant::now(),
            time_origin: wall,
            virtual_now: Cell::new(virtual_time.then_some(0.0)),
        }
    }
    /// Milliseconds since navigation start.
    pub fn now(&self) -> f64 {
        match self.virtual_now.get() {
            Some(v) => v,
            None => self.start.elapsed().as_secs_f64() * 1000.0,
        }
    }
    pub fn is_virtual(&self) -> bool {
        self.virtual_now.get().is_some()
    }
    /// Virtual time only: jump forward to `t` (never back).
    pub fn advance_to(&self, t: f64) {
        if let Some(v) = self.virtual_now.get()
            && t > v
        {
            self.virtual_now.set(Some(t));
        }
    }
}

/// A navigation script asked for (`location.href = ...`, form submit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavRequest {
    pub url: Url,
    pub replace: bool,
}

pub(crate) struct Timer {
    pub due: f64,
    pub seq: u64,
    pub interval: Option<f64>,
    pub callback: TimerCallback,
    pub args: Vec<Handle>,
    pub nesting: u32,
}

pub(crate) enum TimerCallback {
    Function(Handle),
    Code(String),
    /// Host work (XHR timeout, `AbortSignal.timeout`).
    Task(Option<TaskFn>),
}

/// A same-document history entry.
pub(crate) struct HistEntry {
    pub url: Url,
    /// `history.state` as JSON (structured clone stand-in).
    pub state: Option<String>,
}

pub(crate) struct History {
    pub entries: Vec<HistEntry>,
    pub index: usize,
    /// Entries before this document (`history.length` counts them).
    pub before: u32,
    pub scroll_restoration: String,
}

/// Event handler storage (`onclick`), see `events`.
pub(crate) enum HandlerValue {
    Compiled(Handle),
    Uncompiled(String),
}

/// Per-window cached helpers created in each realm.
#[derive(Default)]
pub(crate) struct Helpers {
    pub mo_deliver: Option<Handle>,
    pub array_values: Option<Handle>,
    pub json_parse: Option<Handle>,
    pub json_stringify: Option<Handle>,
    pub freeze: Option<Handle>,
    pub languages: Option<Handle>,
}

/// One frame's document state. See the module docs.
pub struct Window {
    pub(crate) rt: Runtime,
    realm: Cell<RealmId>,
    pub persona: Arc<Persona>,
    pub profile: Option<Arc<PersonaProfile>>,
    pub(crate) net: Rc<dyn NetHost>,
    pub(crate) local_storage: StorageArea,
    pub(crate) session_storage: StorageArea,
    wake: Rc<dyn Fn()>,
    pub(crate) docs: RefCell<Vec<Document>>,
    pub(crate) url: RefCell<Url>,
    pub(crate) referrer: String,
    pub(crate) content_type: String,
    pub(crate) charset: Cell<&'static str>,
    /// The document's parser while it loads (`document.write`).
    pub parser: RefCell<Option<lb_dom::HtmlParser>>,
    pub(crate) mutations: Rc<RefCell<Vec<(u32, Mutation)>>>,
    pub(crate) objs: RefCell<Vec<Option<Obj>>>,
    pub(crate) cache: RefCell<HashMap<(u32, u32, &'static str, String), u32>>,
    pub(crate) listeners: RefCell<HashMap<HostRef, Vec<Rc<events::Listener>>>>,
    pub(crate) handlers: RefCell<HashMap<(HostRef, String), HandlerValue>>,
    tasks: RefCell<[VecDeque<TaskFn>; SOURCES]>,
    pub(crate) timers: RefCell<BTreeMap<u32, Timer>>,
    next_timer: Cell<u32>,
    timer_seq: Cell<u64>,
    pub(crate) timer_nesting: Cell<u32>,
    pub(crate) raf: RefCell<Vec<(u32, Handle)>>,
    next_raf: Cell<u32>,
    pub(crate) console: RefCell<Vec<ConsoleMessage>>,
    console_sink: RefCell<Option<Rc<dyn Fn(&ConsoleMessage)>>>,
    pub clock: Clock,
    pub(crate) layout: RefCell<Option<lb_layout::LayoutEngine>>,
    pub(crate) sheets: RefCell<HashMap<NodeId, String>>,
    ready_state: Cell<ReadyState>,
    pub(crate) history: RefCell<History>,
    pub(crate) observers: RefCell<Vec<u32>>,
    pub(crate) mo_queued: Cell<bool>,
    /// Loads that delay the `load` event (scripts, stylesheets).
    pub pending_loads: Cell<u32>,
    /// Requests in flight (for `networkidle`).
    pub in_flight: Cell<u32>,
    pub(crate) current_script: Cell<Option<NodeId>>,
    nav_request: RefCell<Option<NavRequest>>,
    pub(crate) helpers: RefCell<Helpers>,
    pub(crate) error_depth: Cell<u32>,
    /// Async and dynamically inserted scripts that finished loading, run by
    /// tasks. Kept here so `load` waits for them.
    pub(crate) closed: Cell<bool>,
    pub(crate) focused: Cell<Option<NodeId>>,
    /// Dirty values of form controls (`input.value`), by (slot, node).
    pub(crate) values: RefCell<HashMap<(u32, NodeId), String>>,
    /// Intervals cleared while their own callback ran.
    pub(crate) cleared: RefCell<std::collections::HashSet<u32>>,
    pub(crate) window_name: RefCell<String>,
    /// The parser was started by `document.open()`.
    pub(crate) opened: Cell<bool>,
    /// Prefetched ES module sources by URL (the realm's module loader reads
    /// them; the page layer fills it before evaluating a module graph).
    pub module_map: Rc<RefCell<HashMap<String, String>>>,
    /// Canvas contexts, paint objects' state and image loads.
    pub(crate) paint: canvas::PaintState,
    /// Pointer, click and focus-value state of trusted input.
    pub(crate) input: RefCell<input::InputState>,
    /// Text control selections, by (slot, node), in UTF-16 units.
    pub(crate) selections: RefCell<HashMap<(u32, NodeId), input::Sel>>,
    /// Per-realm handles of the navigator surface (`getBattery()`).
    pub(crate) nav_state: RefCell<navigator::NavState>,
    /// How the document was loaded (`chrome.loadTimes()`); the page layer
    /// may fill in the protocol and navigation type.
    pub load_info: RefCell<LoadInfo>,
    /// Nested browsing contexts: parent link, child frames, page hooks.
    pub(crate) frames: frames::FrameState,
    /// `document.visibilityState` is "hidden" (set while unloading).
    pub(crate) hidden: Cell<bool>,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window")
            .field("url", &self.url.borrow().as_str())
            .field("realm", &self.realm.get())
            .finish()
    }
}

impl Window {
    /// A window for a new, empty document at `init.url`.
    pub fn new(rt: &Runtime, init: WindowInit) -> Rc<Window> {
        let mutations: Rc<RefCell<Vec<(u32, Mutation)>>> = Rc::new(RefCell::new(Vec::new()));
        let mut doc = Document::new();
        let sink = mutations.clone();
        doc.set_mutation_hook(Some(Box::new(move |m: &Mutation| {
            sink.borrow_mut().push((0, m.clone()));
        })));
        let url = init.url.clone();
        Rc::new(Window {
            rt: rt.clone(),
            realm: Cell::new(RealmId(0)),
            persona: init.persona,
            profile: init.profile,
            net: init.net,
            local_storage: init.local_storage,
            session_storage: init.session_storage,
            wake: init.wake,
            docs: RefCell::new(vec![doc]),
            url: RefCell::new(url.clone()),
            referrer: init.referrer,
            content_type: init.content_type,
            charset: Cell::new(init.charset),
            parser: RefCell::new(None),
            mutations,
            objs: RefCell::new(Vec::new()),
            cache: RefCell::new(HashMap::new()),
            listeners: RefCell::new(HashMap::new()),
            handlers: RefCell::new(HashMap::new()),
            tasks: RefCell::new(Default::default()),
            timers: RefCell::new(BTreeMap::new()),
            next_timer: Cell::new(1),
            timer_seq: Cell::new(0),
            timer_nesting: Cell::new(0),
            raf: RefCell::new(Vec::new()),
            next_raf: Cell::new(1),
            console: RefCell::new(Vec::new()),
            console_sink: RefCell::new(None),
            clock: Clock::new(init.virtual_time),
            layout: RefCell::new(None),
            sheets: RefCell::new(HashMap::new()),
            ready_state: Cell::new(ReadyState::Loading),
            history: RefCell::new(History {
                entries: vec![HistEntry { url, state: None }],
                index: 0,
                before: init.history_len,
                scroll_restoration: "auto".into(),
            }),
            observers: RefCell::new(Vec::new()),
            mo_queued: Cell::new(false),
            pending_loads: Cell::new(0),
            in_flight: Cell::new(0),
            current_script: Cell::new(None),
            nav_request: RefCell::new(None),
            helpers: RefCell::new(Helpers::default()),
            error_depth: Cell::new(0),
            closed: Cell::new(false),
            focused: Cell::new(None),
            values: RefCell::new(HashMap::new()),
            cleared: RefCell::new(std::collections::HashSet::new()),
            window_name: RefCell::new(String::new()),
            opened: Cell::new(false),
            module_map: Rc::new(RefCell::new(HashMap::new())),
            paint: canvas::PaintState::default(),
            input: RefCell::new(input::InputState::default()),
            selections: RefCell::new(HashMap::new()),
            nav_state: RefCell::new(navigator::NavState::default()),
            load_info: RefCell::new(LoadInfo::default()),
            frames: frames::FrameState::default(),
            hidden: Cell::new(false),
        })
    }

    /// Create the main-world realm for this window, attach it and set up
    /// per-realm helpers. Returns the realm.
    pub fn create_realm(
        self: &Rc<Self>,
        engine: &mut dyn lb_js::RealmOps,
        bindings: &Bindings,
    ) -> Result<RealmId, JsError> {
        let insecure = !is_secure_url(&frames::effective_url(self));
        let map = self.module_map.clone();
        let base = self.url();
        let loader: lb_js::ModuleLoader = Rc::new(move |spec: &str, referrer: &str| {
            let from = Url::parse(referrer).unwrap_or_else(|_| base.clone());
            let url = from
                .join(spec)
                .map_err(|_| format!("Failed to resolve module specifier \"{spec}\""))?;
            match map.borrow().get(url.as_str()) {
                Some(src) => Ok(lb_js::ModuleSource {
                    url: url.to_string(),
                    source: src.clone(),
                }),
                None => Err(format!(
                    "Failed to fetch dynamically imported module: {url}"
                )),
            }
        });
        let realm = bindings.new_window_realm(
            engine,
            crate::install::WindowRealmOptions {
                window_host: Some(WINDOW_REF),
                world: None,
                module_loader: Some(loader),
                insecure,
                os_apis: self.profile.as_deref().map(PersonaProfile::os_apis),
                // `setup_realm` defines `window.chrome`.
                host_slots: true,
            },
        )?;
        self.realm.set(realm);
        self.rt.attach(realm, self.clone());
        engine.set_security_token(realm, &frames::origin(self))?;
        self.setup_realm(engine, realm)?;
        Ok(realm)
    }

    /// An isolated world sharing this window's document (CDP utility
    /// scripts). Nothing is added to the main world.
    pub fn create_isolated_world(
        self: &Rc<Self>,
        engine: &mut dyn lb_js::RealmOps,
        bindings: &Bindings,
        name: &str,
    ) -> Result<RealmId, JsError> {
        let realm = bindings.new_window_realm(
            engine,
            crate::install::WindowRealmOptions {
                window_host: Some(WINDOW_REF),
                world: Some(lb_js::World::Isolated(name.to_owned())),
                module_loader: None,
                insecure: !is_secure_url(&frames::effective_url(self)),
                os_apis: self.profile.as_deref().map(PersonaProfile::os_apis),
                host_slots: false,
            },
        )?;
        self.rt.attach(realm, self.clone());
        engine.set_security_token(realm, &frames::origin(self))?;
        window::install_console(engine, realm, &self.rt)?;
        // Per-realm navigator state (UA-CH promises, performance.memory).
        navigator::setup_realm(self, engine, realm)?;
        Ok(realm)
    }

    fn setup_realm(
        self: &Rc<Self>,
        engine: &mut dyn lb_js::RealmOps,
        realm: RealmId,
    ) -> Result<(), JsError> {
        window::install_console(engine, realm, &self.rt)?;
        let rt = self.rt.clone();
        let deliver = engine.new_function(
            realm,
            "",
            0,
            host_fn(move |cx| {
                let w = rt.win(cx.realm())?;
                misc::deliver_mutation_records(cx, &w);
                Ok(())
            }),
        )?;
        let g = engine.global(realm)?;
        let array = engine.get_property(realm, g, "Array")?;
        let proto = engine.get_property(realm, array, "prototype")?;
        let values = engine.get_property(realm, proto, "values")?;
        let json = engine.get_property(realm, g, "JSON")?;
        let parse = engine.get_property(realm, json, "parse")?;
        let stringify = engine.get_property(realm, json, "stringify")?;
        let object = engine.get_property(realm, g, "Object")?;
        let freeze = engine.get_property(realm, object, "freeze")?;
        for h in [g, array, proto, json, object] {
            engine.release(h);
        }
        let mut hp = self.helpers.borrow_mut();
        hp.mo_deliver = Some(deliver);
        hp.array_values = Some(values);
        hp.json_parse = Some(parse);
        hp.json_stringify = Some(stringify);
        hp.freeze = Some(freeze);
        drop(hp);
        navigator::setup_realm(self, engine, realm)?;
        chrome::install(self, engine, realm)?;
        Ok(())
    }

    /// The main-world realm.
    pub fn realm(&self) -> RealmId {
        self.realm.get()
    }

    pub fn runtime(&self) -> &Runtime {
        &self.rt
    }

    pub fn url(&self) -> Url {
        self.url.borrow().clone()
    }

    pub(crate) fn set_url(&self, url: Url) {
        *self.url.borrow_mut() = url;
    }

    /// Borrow the main document.
    pub fn with_doc<R>(&self, f: impl FnOnce(&Document) -> R) -> R {
        f(&self.docs.borrow()[0])
    }

    /// Borrow the main document mutably. Do not call JS inside.
    pub fn with_doc_mut<R>(&self, f: impl FnOnce(&mut Document) -> R) -> R {
        f(&mut self.docs.borrow_mut()[0])
    }

    pub(crate) fn doc_slot<R>(&self, slot: u32, f: impl FnOnce(&Document) -> R) -> R {
        f(&self.docs.borrow()[slot as usize])
    }

    pub(crate) fn doc_slot_mut<R>(&self, slot: u32, f: impl FnOnce(&mut Document) -> R) -> R {
        f(&mut self.docs.borrow_mut()[slot as usize])
    }

    /// Add a document (DOMParser, createHTMLDocument); returns its slot.
    pub(crate) fn add_doc(&self, mut doc: Document) -> u32 {
        let slot = self.docs.borrow().len() as u32;
        let sink = self.mutations.clone();
        doc.set_mutation_hook(Some(Box::new(move |m: &Mutation| {
            sink.borrow_mut().push((slot, m.clone()));
        })));
        self.docs.borrow_mut().push(doc);
        slot
    }

    /// Mutations are queued; they are processed by the next
    /// [`Window::after_mutation`] (named setters have no engine access).
    pub(crate) fn mutations_pending(&self) {}

    /// Rendered-ish text of the main document (`innerText` of the body,
    /// or of the root without a body).
    pub fn text(&self) -> String {
        self.with_doc(crate::text::document_text)
    }

    /// Start the document parser (navigation).
    pub fn start_parser(&self) {
        *self.parser.borrow_mut() = Some(lb_dom::HtmlParser::new());
    }

    /// Queue decoded document text for the parser.
    pub fn parser_push(&self, text: &str) {
        if let Some(p) = self.parser.borrow_mut().as_mut() {
            p.push_str(text);
        }
    }

    /// Mark the end of the document text.
    pub fn parser_end(&self) {
        if let Some(p) = self.parser.borrow_mut().as_mut() {
            p.end();
        }
    }

    /// Parse as far as possible (see `lb_dom::HtmlParser::run`). `None`
    /// when no parser is active.
    pub fn parser_run(&self) -> Option<lb_dom::ParseStep> {
        let mut p = self.parser.borrow_mut();
        let parser = p.as_mut()?;
        let mut docs = self.docs.borrow_mut();
        Some(parser.run(&mut docs[0]))
    }

    /// 1-based source line where a parser-inserted script started.
    pub fn parser_script_line(&self, script: NodeId) -> Option<u64> {
        self.parser.borrow().as_ref()?.script_line(script)
    }

    /// Drop the parser once the document is parsed.
    pub fn finish_parser(&self) {
        *self.parser.borrow_mut() = None;
    }

    /// Set node flags on the main document.
    pub fn set_node_flags(&self, n: NodeId, flags: lb_dom::NodeFlags, on: bool) {
        self.with_doc_mut(|d| d.set_flags(n, flags, on));
    }

    /// Same-document history entries (kept when navigating away).
    pub fn history_entry_count(&self) -> u32 {
        self.history.borrow().entries.len() as u32
    }

    /// Serialized main document.
    pub fn content(&self) -> String {
        self.with_doc(|d| d.to_html())
    }

    pub fn ready_state(&self) -> ReadyState {
        self.ready_state.get()
    }

    pub fn set_ready_state(&self, s: ReadyState) {
        self.ready_state.set(s);
        let now = self.clock.now();
        let mut li = self.load_info.borrow_mut();
        match s {
            ReadyState::Interactive => {
                li.dcl_ms.get_or_insert(now);
            }
            ReadyState::Complete => {
                li.dcl_ms.get_or_insert(now);
                li.load_ms.get_or_insert(now);
            }
            ReadyState::Loading => {}
        }
    }

    /// Forward console messages (and uncaught exceptions) to `sink`.
    pub fn set_console_sink(&self, sink: Option<Rc<dyn Fn(&ConsoleMessage)>>) {
        *self.console_sink.borrow_mut() = sink;
    }

    /// Every console message so far.
    pub fn console_messages(&self) -> Vec<ConsoleMessage> {
        self.console.borrow().clone()
    }

    pub(crate) fn log(&self, msg: ConsoleMessage) {
        let sink = self.console_sink.borrow().clone();
        if let Some(s) = sink {
            s(&msg);
        }
        self.console.borrow_mut().push(msg);
    }

    /// Log a warning to the console (parser, loader messages).
    pub fn warn(&self, text: impl Into<String>) {
        self.log(ConsoleMessage {
            level: "warning".into(),
            text: text.into(),
            url: None,
            line: None,
        });
    }

    /// Report an uncaught exception: console error and an `error` event on
    /// the window.
    pub fn report_exception(self: &Rc<Self>, js: &mut dyn Js, e: &JsError) {
        events::report_exception(js, self, e);
    }

    /// A navigation requested by script, taken by the page layer.
    pub fn take_nav_request(&self) -> Option<NavRequest> {
        self.nav_request.borrow_mut().take()
    }

    pub(crate) fn request_navigation(&self, req: NavRequest) {
        *self.nav_request.borrow_mut() = Some(req);
        self.wake();
    }

    pub fn wake(&self) {
        (self.wake)()
    }

    // ----- tasks -----

    /// Queue a task and wake the event loop.
    pub fn queue_task(&self, source: TaskSource, f: TaskFn) {
        self.tasks.borrow_mut()[source as usize].push_back(f);
        self.wake();
    }

    /// The next task by source priority.
    pub fn take_task(&self) -> Option<TaskFn> {
        let mut q = self.tasks.borrow_mut();
        q.iter_mut().find_map(|q| q.pop_front())
    }

    pub fn has_tasks(&self) -> bool {
        self.tasks.borrow().iter().any(|q| !q.is_empty())
    }

    // ----- timers -----

    pub(crate) fn add_timer(
        &self,
        callback: TimerCallback,
        delay: f64,
        args: Vec<Handle>,
        repeat: bool,
    ) -> u32 {
        let nesting = self.timer_nesting.get() + 1;
        let mut delay = if delay.is_finite() && delay > 0.0 {
            delay.min(i32::MAX as f64)
        } else {
            0.0
        };
        // Chrome: after 5 nested levels the minimum is 4 ms.
        if nesting > 5 && delay < 4.0 {
            delay = 4.0;
        }
        let id = self.next_timer.get();
        self.next_timer.set(id.wrapping_add(1).max(1));
        let seq = self.timer_seq.get() + 1;
        self.timer_seq.set(seq);
        self.timers.borrow_mut().insert(
            id,
            Timer {
                due: self.clock.now() + delay,
                seq,
                interval: repeat.then_some(delay),
                callback,
                args,
                nesting,
            },
        );
        self.wake();
        id
    }

    /// Earliest timer due time (page clock ms).
    pub fn next_timer_due(&self) -> Option<f64> {
        self.timers
            .borrow()
            .values()
            .map(|t| t.due)
            .reduce(f64::min)
    }

    pub fn has_timers(&self) -> bool {
        !self.timers.borrow().is_empty()
    }

    /// Run the earliest due timer, if any. Returns whether one ran.
    pub fn run_due_timer(self: &Rc<Self>, js: &mut dyn Js) -> bool {
        window::run_due_timer(js, self)
    }

    // ----- animation frames -----

    pub(crate) fn add_raf(&self, cb: Handle) -> u32 {
        let id = self.next_raf.get();
        self.next_raf.set(id + 1);
        self.raf.borrow_mut().push((id, cb));
        self.wake();
        id
    }

    pub fn has_animation_frames(&self) -> bool {
        !self.raf.borrow().is_empty()
    }

    /// Run the animation frame callbacks queued so far with timestamp `now`.
    pub fn run_animation_frames(self: &Rc<Self>, js: &mut dyn Js, now: f64) {
        let cbs = std::mem::take(&mut *self.raf.borrow_mut());
        for (_, cb) in cbs {
            if let Err(e) = js.call(cb, Arg::Undefined, &[Arg::Number(now)]) {
                self.report_exception(js, &e);
            }
            js.release(cb);
        }
    }

    // ----- objects -----

    pub(crate) fn new_obj(&self, o: Obj) -> u32 {
        let mut objs = self.objs.borrow_mut();
        objs.push(Some(o));
        (objs.len() - 1) as u32
    }

    /// Same-object cache: `(key a, key b, kind, extra)` → object id.
    pub(crate) fn cached_obj(
        &self,
        key: (u32, u32, &'static str, String),
        make: impl FnOnce() -> Obj,
    ) -> u32 {
        if let Some(id) = self.cache.borrow().get(&key) {
            return *id;
        }
        let id = self.new_obj(make());
        self.cache.borrow_mut().insert(key, id);
        id
    }

    pub(crate) fn obj_arg(&self, id: u32) -> Arg {
        let name = self.objs.borrow()[id as usize]
            .as_ref()
            .map_or("Object", Obj::iface_name);
        Arg::Host(self.rt.iface(name), obj_ref(id))
    }

    pub(crate) fn with_obj<R>(&self, id: u32, f: impl FnOnce(&mut Obj) -> R) -> Option<R> {
        let mut objs = self.objs.borrow_mut();
        objs.get_mut(id as usize).and_then(Option::as_mut).map(f)
    }

    // ----- nodes -----

    /// The JS value for node `id` of document `slot`.
    pub fn node_arg(&self, slot: u32, id: NodeId) -> Arg {
        let name = self.doc_slot(slot, |d| tags::node_interface(d, id, slot));
        Arg::Host(self.rt.iface(name), node_ref(slot, id))
    }

    pub(crate) fn opt_node_arg(&self, slot: u32, id: Option<NodeId>) -> Arg {
        id.map_or(Arg::Null, |n| self.node_arg(slot, n))
    }

    /// The JS value for any host ref this window owns.
    pub fn host_arg(&self, h: HostRef) -> Arg {
        if let Some((slot, n)) = decode_node(h) {
            return self.node_arg(slot, n);
        }
        match h.kind {
            K_WINDOW => Arg::Host(self.rt.iface("Window"), h),
            _ => self.obj_arg(h.id),
        }
    }

    /// Process tree changes since the last call: MutationObserver records,
    /// script elements inserted by script, inline event handler attributes.
    /// Call after any DOM change and after each parser step.
    pub fn after_mutation(self: &Rc<Self>, js: &mut dyn Js) {
        misc::process_mutations(js, self);
    }

    /// The layout engine, created on first use.
    pub(crate) fn with_layout<R>(
        &self,
        slot: u32,
        f: impl FnOnce(&mut lb_layout::LayoutEngine, &Document) -> R,
    ) -> R {
        let mut l = self.layout.borrow_mut();
        let created = l.is_none();
        let engine = l.get_or_insert_with(|| match &self.profile {
            Some(p) => {
                let base = self.rt.layout_base(lb_css::MediaEnv::from_persona(p).os);
                lb_layout::LayoutEngine::new_with(p.clone(), &base)
            }
            None => lb_layout::LayoutEngine::with_env(lb_css::MediaEnv::default()),
        });
        if created {
            image::apply_intrinsic(self, engine);
        }
        let sheets = std::mem::take(&mut *self.sheets.borrow_mut());
        for (link, text) in sheets {
            engine.styles().set_external_sheet(link, Some(&text));
        }
        let docs = self.docs.borrow();
        f(engine, &docs[slot as usize])
    }

    /// The main document's layout engine (created on first use), for the
    /// page layer (CDP box models, hit testing, viewport, AX tree).
    pub fn with_layout_engine<R>(
        &self,
        f: impl FnOnce(&mut lb_layout::LayoutEngine, &Document) -> R,
    ) -> R {
        self.with_layout(0, f)
    }

    /// Hand a fetched stylesheet to the style engine (applied lazily).
    pub fn add_stylesheet(&self, link: NodeId, text: String) {
        self.sheets.borrow_mut().insert(link, text);
    }

    /// Nodes of `<link rel=stylesheet>` elements whose sheet is not loaded.
    pub fn pending_stylesheets(&self) -> Vec<NodeId> {
        let docs = self.docs.borrow();
        let doc = &docs[0];
        doc.descendants(Document::DOCUMENT)
            .filter(|&n| {
                doc.is_html_element_named(n, &lb_dom::local_name!("link"))
                    && doc.get_attribute(n, "rel").is_some_and(|r| {
                        r.split_ascii_whitespace()
                            .any(|t| t.eq_ignore_ascii_case("stylesheet"))
                    })
                    && doc.get_attribute(n, "href").is_some()
            })
            .collect()
    }

    /// Resolve a URL against the document base URL.
    pub fn resolve_url(&self, s: &str) -> Option<Url> {
        self.base_url().join(s.trim()).ok()
    }

    pub(crate) fn base_url(&self) -> Url {
        let url = self.url();
        let href = self.with_doc(|d| {
            d.descendants(Document::DOCUMENT)
                .find(|&n| d.is_html_element_named(n, &lb_dom::local_name!("base")))
                .and_then(|n| d.get_attribute(n, "href").map(str::to_owned))
        });
        href.and_then(|h| url.join(&h).ok()).unwrap_or(url)
    }

    /// Release JS state before the realm is dropped.
    /// The unload steps of the document, as Chrome runs them when the page
    /// navigates away or closes: `pagehide` (only after `pageshow`, i.e. once
    /// `load` ran), `visibilitychange` to "hidden", then `unload`. Requests
    /// started here (`navigator.sendBeacon`, keepalive) still go out.
    pub fn unload(self: &Rc<Self>, js: &mut dyn Js) {
        if self.is_closed() {
            return;
        }
        if self.ready_state() == ReadyState::Complete {
            fire_event_with(
                js,
                self,
                WINDOW_REF,
                "PageTransitionEvent",
                "pagehide",
                false,
                false,
                EventExtra::PageTransition { persisted: false },
            );
        }
        if !self.hidden.replace(true) {
            fire_event(
                js,
                self,
                node_ref(0, lb_dom::Document::DOCUMENT),
                "visibilitychange",
                true,
                false,
            );
        }
        fire_event(js, self, WINDOW_REF, "unload", false, false);
    }

    pub fn close(&self) {
        self.closed.set(true);
        self.timers.borrow_mut().clear();
        self.raf.borrow_mut().clear();
        for q in self.tasks.borrow_mut().iter_mut() {
            q.clear();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }
}

/// `t` (page clock ms) coarsened to the persona's `performance.now()`
/// resolution (Chrome 100 µs).
///
/// Chrome (Blink's `TimeClamper`) rounds down to the resolution but moves
/// to the next tick at a pseudo-random point inside each interval:
/// `threshold = murmur3(tick ^ secret) % resolution`. The result is still a
/// multiple of the resolution and never goes backwards. The secret is per
/// document (here derived from the time origin). Firefox personas floor.
pub fn coarse_time(w: &Window, t: f64) -> f64 {
    let (res_us, jitter) = match &w.profile {
        Some(p) => (
            i64::from(p.performance_now_resolution_us()),
            p.family() == lb_persona::BrowserFamily::Chrome,
        ),
        None => (100, w.persona.family == lb_persona::BrowserFamily::Chrome),
    };
    if res_us <= 0 || !t.is_finite() {
        return t;
    }
    let us = (t * 1000.0).floor() as i64;
    let (neg, us) = (us < 0, us.abs());
    let lower = us % res_us;
    let mut clamped = us - lower;
    if jitter {
        let secret = w.clock.time_origin.to_bits().rotate_left(17) ^ 0x9e37_79b9_7f4a_7c15;
        let threshold = (murmur3(clamped as u64 ^ secret) % res_us as u64) as i64;
        if lower >= threshold {
            clamped += res_us;
        }
    }
    let clamped = if neg { -clamped } else { clamped };
    clamped as f64 / 1000.0
}

/// MurmurHash3's 64-bit finalizer (Blink `TimeClamper`).
fn murmur3(mut v: u64) -> u64 {
    v ^= v >> 33;
    v = v.wrapping_mul(0xff51_afd7_ed55_8ccd);
    v ^= v >> 33;
    v = v.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    v ^= v >> 33;
    v
}

/// `https:`, `wss:`, `file:`, loopback `http:` and `about:` count as secure
/// contexts.
pub fn is_secure_url(u: &Url) -> bool {
    match u.scheme() {
        "https" | "wss" | "file" | "about" | "data" => true,
        "http" | "ws" => match u.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(d)) => d == "localhost" || d.ends_with(".localhost"),
            None => false,
        },
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Host objects
// ---------------------------------------------------------------------------

/// Host objects other than nodes and the window.
pub(crate) enum Obj {
    Event(Box<EventData>),
    /// `new EventTarget()`.
    EventTarget,
    NodeList(coll::NodeListSrc),
    Collection(Box<coll::Collection>),
    TokenList {
        slot: u32,
        node: NodeId,
        attr: &'static str,
    },
    NamedNodeMap {
        slot: u32,
        node: NodeId,
    },
    Attr {
        slot: u32,
        owner: Option<NodeId>,
        name: QualName,
        value: String,
    },
    StringMap {
        slot: u32,
        node: NodeId,
    },
    Style {
        slot: u32,
        node: NodeId,
        computed: bool,
    },
    Location,
    History,
    Navigator,
    Storage {
        local: bool,
    },
    Performance,
    Screen,
    DomImplementation {
        slot: u32,
    },
    MediaQueryList {
        media: String,
    },
    Rect([f64; 4]),
    RectList(Vec<[f64; 4]>),
    Xhr(Box<net::XhrState>),
    XhrUpload,
    Headers(Box<net::HeadersData>),
    Request(Box<net::RequestData>),
    Response(Box<net::ResponseData>),
    AbortController {
        signal: u32,
    },
    AbortSignal(Box<net::SignalData>),
    Url(Box<misc::UrlData>),
    Params(Box<misc::ParamsData>),
    TextEncoder,
    TextDecoder(Box<misc::DecoderData>),
    DomParser,
    XmlSerializer,
    MutationObserver(Box<misc::MoData>),
    MutationRecord(Box<misc::MoRecord>),
    Blob(Box<misc::BlobData>),
    /// Canvas contexts, gradients, patterns, paths, image data, metrics,
    /// offscreen canvases and matrices.
    Paint(Box<canvas::PaintObj>),
    /// WebGL contexts, resources and extension objects.
    Gl(Box<webgl::GlObj>),
    /// Web Audio contexts, nodes, params, buffers and periodic waves.
    Audio(Box<audio::AudioObj>),
    /// `FontFaceSet` and `FontFace`.
    Font(Box<fonts::FontObj>),
    /// Navigator surface objects (plugins, UA data, permissions, ...).
    Nav(Box<navigator::NavObj>),
}

impl Obj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            Obj::Event(e) => e.iface,
            Obj::EventTarget => "EventTarget",
            Obj::NodeList(_) => "NodeList",
            Obj::Collection(c) => c.iface,
            Obj::TokenList { .. } => "DOMTokenList",
            Obj::NamedNodeMap { .. } => "NamedNodeMap",
            Obj::Attr { .. } => "Attr",
            Obj::StringMap { .. } => "DOMStringMap",
            Obj::Style { .. } => "CSSStyleDeclaration",
            Obj::Location => "Location",
            Obj::History => "History",
            Obj::Navigator => "Navigator",
            Obj::Storage { .. } => "Storage",
            Obj::Performance => "Performance",
            Obj::Screen => "Screen",
            Obj::DomImplementation { .. } => "DOMImplementation",
            Obj::MediaQueryList { .. } => "MediaQueryList",
            Obj::Rect(_) => "DOMRect",
            Obj::RectList(_) => "DOMRectList",
            Obj::Xhr(_) => "XMLHttpRequest",
            Obj::XhrUpload => "XMLHttpRequestUpload",
            Obj::Headers(_) => "Headers",
            Obj::Request(_) => "Request",
            Obj::Response(_) => "Response",
            Obj::AbortController { .. } => "AbortController",
            Obj::AbortSignal(_) => "AbortSignal",
            Obj::Url(_) => "URL",
            Obj::Params(_) => "URLSearchParams",
            Obj::TextEncoder => "TextEncoder",
            Obj::TextDecoder(_) => "TextDecoder",
            Obj::DomParser => "DOMParser",
            Obj::XmlSerializer => "XMLSerializer",
            Obj::MutationObserver(_) => "MutationObserver",
            Obj::MutationRecord(_) => "MutationRecord",
            Obj::Blob(_) => "Blob",
            Obj::Paint(p) => p.iface_name(),
            Obj::Gl(g) => g.iface_name(),
            Obj::Audio(a) => a.iface_name(),
            Obj::Font(f) => f.iface_name(),
            Obj::Nav(n) => n.iface_name(),
        }
    }
}

// ---------------------------------------------------------------------------
// Small helpers shared by the implementation modules
// ---------------------------------------------------------------------------

/// `Illegal invocation` (wrong `this`).
pub(crate) fn illegal() -> HostError {
    HostError::type_error("Illegal invocation")
}

/// Register `f` as a method of `iface`, with the window looked up first.
pub(crate) fn method(
    reg: &mut Registry,
    rt: &Runtime,
    iface: &'static str,
    name: &'static str,
    f: impl Fn(&mut CallCtx<'_, '_, '_>, &Rc<Window>) -> Result<(), HostError> + 'static,
) {
    reg.method(iface, name, wf(rt, f));
}

pub(crate) fn getter(
    reg: &mut Registry,
    rt: &Runtime,
    iface: &'static str,
    name: &'static str,
    f: impl Fn(&mut CallCtx<'_, '_, '_>, &Rc<Window>) -> Result<(), HostError> + 'static,
) {
    reg.getter(iface, name, wf(rt, f));
}

pub(crate) fn setter(
    reg: &mut Registry,
    rt: &Runtime,
    iface: &'static str,
    name: &'static str,
    f: impl Fn(&mut CallCtx<'_, '_, '_>, &Rc<Window>) -> Result<(), HostError> + 'static,
) {
    reg.setter(iface, name, wf(rt, f));
}

pub(crate) fn ctor(
    reg: &mut Registry,
    rt: &Runtime,
    iface: &'static str,
    f: impl Fn(&mut CallCtx<'_, '_, '_>, &Rc<Window>) -> Result<(), HostError> + 'static,
) {
    reg.constructor(iface, wf(rt, f));
}

/// Wrap `f` so it receives the calling realm's window.
pub(crate) fn wf(
    rt: &Runtime,
    f: impl Fn(&mut CallCtx<'_, '_, '_>, &Rc<Window>) -> Result<(), HostError> + 'static,
) -> lb_js::HostFn {
    let rt = rt.clone();
    host_fn(move |cx| {
        let w = rt.win(cx.realm())?;
        f(cx, &w)
    })
}

/// `this` as an object id of the window's table.
pub(crate) fn this_obj(cx: &mut CallCtx<'_, '_, '_>) -> Result<u32, HostError> {
    match cx.this_host() {
        Some(h) if h.kind == K_OBJ => Ok(h.id),
        _ => Err(illegal()),
    }
}

/// `this` as a node.
pub(crate) fn this_node(cx: &mut CallCtx<'_, '_, '_>) -> Result<(u32, NodeId), HostError> {
    cx.this_host().and_then(decode_node).ok_or_else(illegal)
}

/// Argument `i` as a node, or a TypeError naming the parameter.
pub(crate) fn arg_node(
    cx: &mut CallCtx<'_, '_, '_>,
    i: usize,
    op: &str,
    iface: &str,
) -> Result<(u32, NodeId), HostError> {
    cx.arg_host(i).and_then(decode_node).ok_or_else(|| {
        HostError::type_error(format!(
            "Failed to execute '{op}' on '{iface}': parameter {} is not of type 'Node'.",
            i + 1
        ))
    })
}

/// "Not enough arguments" TypeError, Chrome's wording.
pub(crate) fn need_args(
    cx: &CallCtx<'_, '_, '_>,
    n: usize,
    op: &str,
    iface: &str,
) -> Result<(), HostError> {
    if cx.len() < n {
        return Err(HostError::type_error(format!(
            "Failed to execute '{op}' on '{iface}': {n} argument{} required, but only {} present.",
            if n == 1 { "" } else { "s" },
            cx.len()
        )));
    }
    Ok(())
}

/// Argument `i` as a DOMString, `None` if missing or undefined.
pub(crate) fn opt_string(
    cx: &mut CallCtx<'_, '_, '_>,
    i: usize,
) -> Result<Option<String>, HostError> {
    if i >= cx.len() || cx.arg_kind(i) == ValueKind::Undefined {
        Ok(None)
    } else {
        cx.arg_string(i).map(Some)
    }
}

/// `node.nodeType == Element` check helper.
pub(crate) fn is_elem(doc: &Document, n: NodeId) -> bool {
    doc.node_type(n) == NodeType::Element
}

/// Escape a string for embedding in a JS string literal.
pub(crate) fn js_string_literal(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// A dictionary argument read member by member (getters run, like WebIDL
/// dictionary conversion). Missing or `undefined` members are `None`.
pub(crate) struct Dict(Option<Handle>);

impl Dict {
    pub(crate) fn arg(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Dict {
        if i < cx.len() && matches!(cx.arg_kind(i), ValueKind::Object | ValueKind::Function) {
            Dict(Some(cx.arg_handle(i)))
        } else {
            Dict(None)
        }
    }

    pub(crate) fn handle(&self) -> Option<Handle> {
        self.0
    }

    pub(crate) fn get(
        &self,
        cx: &mut CallCtx<'_, '_, '_>,
        key: &str,
    ) -> Result<Option<Handle>, HostError> {
        let Some(h) = self.0 else { return Ok(None) };
        let v = cx.get_property(h, key)?;
        if cx.handle_kind(v) == ValueKind::Undefined {
            cx.release(v);
            Ok(None)
        } else {
            Ok(Some(v))
        }
    }

    pub(crate) fn bool(&self, cx: &mut CallCtx<'_, '_, '_>, key: &str) -> Result<bool, HostError> {
        Ok(match self.get(cx, key)? {
            Some(h) => {
                let b = truthy(&cx.handle_value(h));
                cx.release(h);
                b
            }
            None => false,
        })
    }

    pub(crate) fn string(
        &self,
        cx: &mut CallCtx<'_, '_, '_>,
        key: &str,
    ) -> Result<Option<String>, HostError> {
        match self.get(cx, key)? {
            Some(h) => {
                let s = cx.handle_string(h);
                cx.release(h);
                s.map(Some)
            }
            None => Ok(None),
        }
    }

    pub(crate) fn number(
        &self,
        cx: &mut CallCtx<'_, '_, '_>,
        key: &str,
    ) -> Result<Option<f64>, HostError> {
        match self.get(cx, key)? {
            Some(h) => {
                let v = cx.handle_value(h);
                cx.release(h);
                Ok(Some(match v {
                    JsValue::Number(n) => n,
                    JsValue::Bool(b) => f64::from(u8::from(b)),
                    JsValue::Null => 0.0,
                    JsValue::String(s) => s.trim().parse().unwrap_or(f64::NAN),
                    _ => f64::NAN,
                }))
            }
            None => Ok(None),
        }
    }

    pub(crate) fn release(self, cx: &mut CallCtx<'_, '_, '_>) {
        if let Some(h) = self.0 {
            cx.release(h);
        }
    }
}

/// ToBoolean of a copied value.
pub(crate) fn truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        JsValue::Json(_) | JsValue::Opaque(_) => true,
    }
}

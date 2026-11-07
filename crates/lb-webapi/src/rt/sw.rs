//! Service workers and the Cache API (ADR 0022).
//!
//! - **Registry in the page layer.** Registrations, versions, clients and
//!   the job queue of a browser context live in `lb-page` (`sw.rs`), in
//!   memory only. Windows reach it through their [`SwHost`]; it reaches
//!   windows by sending [`RemoteTask`]s to their inboxes, built here
//!   ([`reg_state_task`], [`worker_state_task`], ...), so the order of
//!   state changes, events and promise resolutions is the order the
//!   registry sends them in (Service Workers "Update Registration State",
//!   "Update Worker State", Chrome's observed order).
//! - **Objects per window.** Each window keeps one
//!   `ServiceWorkerRegistration` object per registration and one
//!   `ServiceWorker` object per version (`r.active === controller`);
//!   their attributes change only through the registry's tasks.
//! - **Service worker scopes** are worker scopes (`WorkerKind::Service`,
//!   own thread and isolate, `lb-page` `worker.rs`) whose global is
//!   `ServiceWorkerGlobalScope`. Lifecycle, fetch and message events are
//!   extendable: `waitUntil` / `respondWith` promises are awaited through
//!   a settle helper made before the worker's script runs.
//! - **Fetch interception.** [`Window::net_fetch`] sends the requests of
//!   a controlled window (fetch, XHR, scripts, style sheets, images) to
//!   its controller first; navigations are intercepted by the page layer.
//! - **Cache API**: `caches` per context and origin ([`CacheStorageRef`]),
//!   in memory.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, ValueKind};
use url::Url;

use super::events::{EventExtra, dispatch, new_event};
use super::net::{HeadersData, RequestData, ResponseData, headers_of, new_headers, new_signal};
use super::worker::{Inbox, Payload, RemoteTask, serialize_args, unpack};
use super::{
    Dict, Js, K_OBJ, Obj, Runtime, TaskSource, WINDOW_REF, Window, getter, illegal, method,
    this_obj,
};
use crate::host::{Destination, LocalFuture, NetRequest, NetResponse};
use crate::registry::{ImplKind, Registry, host_fn};

// ---------------------------------------------------------------------------
// Data shared with the page layer
// ---------------------------------------------------------------------------

/// `ServiceWorker.state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwState {
    Parsed,
    Installing,
    Installed,
    Activating,
    Activated,
    Redundant,
}

impl SwState {
    pub fn as_str(self) -> &'static str {
        match self {
            SwState::Parsed => "parsed",
            SwState::Installing => "installing",
            SwState::Installed => "installed",
            SwState::Activating => "activating",
            SwState::Activated => "activated",
            SwState::Redundant => "redundant",
        }
    }
}

/// One service worker version as windows see it.
#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub id: u64,
    pub script_url: Url,
    pub state: SwState,
}

/// One registration as windows see it.
#[derive(Debug, Clone)]
pub struct RegInfo {
    pub id: u64,
    pub scope: Url,
    pub update_via_cache: String,
    pub installing: Option<VersionInfo>,
    pub waiting: Option<VersionInfo>,
    pub active: Option<VersionInfo>,
}

/// A window client (`Client` / `WindowClient`).
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub id: String,
    pub url: Url,
    /// `top-level` or `nested`.
    pub frame_type: &'static str,
    pub focused: bool,
}

/// A rejected job: a `TypeError` or a `DOMException` name, and the
/// message.
#[derive(Debug, Clone)]
pub struct SwError {
    pub name: &'static str,
    pub message: String,
}

impl SwError {
    pub fn new(name: &'static str, message: impl Into<String>) -> SwError {
        SwError {
            name,
            message: message.into(),
        }
    }

    fn host(&self, w: &Window) -> HostError {
        match self.name {
            "TypeError" => HostError::type_error(self.message.clone()),
            n => w.rt.err(n, self.message.clone()),
        }
    }
}

/// A callback the registry calls once, from any thread.
pub type Reply<T> = Box<dyn FnOnce(T) + Send>;

/// A window or service worker scope known to the registry: it receives
/// registration and worker state changes of its origin.
pub struct ClientReg {
    pub info: ClientInfo,
    pub origin: String,
    pub inbox: Inbox,
    pub controller: Option<u64>,
    /// A window (`Clients.matchAll` lists it); false for service worker
    /// scopes.
    pub is_window: bool,
}

/// A request handed to a service worker's `fetch` event.
#[derive(Debug, Clone)]
pub struct SwRequest {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub mode: &'static str,
    pub credentials: &'static str,
    pub cache: &'static str,
    pub redirect: &'static str,
    pub destination: &'static str,
    pub client_id: String,
    pub resulting_client_id: String,
}

impl SwRequest {
    /// The `FetchEvent.request` of a network request of a client.
    pub fn from_net(req: &NetRequest, client_id: String, resulting: String, reload: bool) -> Self {
        let navigate = req.mode == "navigate";
        SwRequest {
            method: req.method.clone(),
            url: req.url.clone(),
            headers: req.headers.clone(),
            body: req.body.clone(),
            mode: req.mode,
            credentials: if navigate || req.mode == "no-cors" {
                "include"
            } else {
                "same-origin"
            },
            cache: if navigate && reload {
                "no-cache"
            } else {
                "default"
            },
            redirect: if navigate { "manual" } else { "follow" },
            destination: match req.destination {
                Destination::Document => "document",
                Destination::Iframe => "iframe",
                Destination::Script => "script",
                Destination::Style => "style",
                Destination::Image => "image",
                Destination::Empty => "",
            },
            client_id,
            resulting_client_id: resulting,
        }
    }
}

/// What a `fetch` event decided.
#[derive(Debug)]
pub enum FetchOutcome {
    /// No `respondWith`: the request goes to the network.
    Fallback,
    /// The worker's response.
    Respond(NetResponse),
    /// A network error (`Response.error()`, a rejected promise, ...).
    Error,
}

/// What `lb-page` needs to report a new service worker's script run.
pub struct ServiceStart {
    pub version: u64,
    pub registration: u64,
    /// Called once the script ran: `Ok(has a fetch handler)` or `Err` when
    /// it threw.
    pub evaluated: Reply<Result<bool, ()>>,
}

/// The registry of a browser context, provided by the page layer.
pub trait SwHost {
    /// List a window or scope (state changes of its origin reach it).
    fn add_client(&self, c: ClientReg);
    /// The window or scope went away.
    fn remove_client(&self, id: &str);
    /// A `register()` job.
    fn register(
        &self,
        client: ClientInfo,
        script: Url,
        scope: Url,
        update_via_cache: String,
        reply: Reply<Result<RegInfo, SwError>>,
    );
    /// An `update()` job.
    fn update(&self, reg: u64, reply: Reply<Result<RegInfo, SwError>>);
    /// An `unregister()` job: true when it unregistered.
    fn unregister(&self, reg: u64, reply: Reply<bool>);
    /// The registration whose scope is the longest prefix of `url`.
    fn registration_for(&self, url: &Url) -> Option<RegInfo>;
    /// Registrations of `origin`, in creation order.
    fn registrations(&self, origin: &str) -> Vec<RegInfo>;
    fn registration(&self, id: u64) -> Option<RegInfo>;
    fn version(&self, id: u64) -> Option<VersionInfo>;
    /// Whether `origin` has a registration (windows list themselves then).
    fn has_registrations(&self, origin: &str) -> bool;
    /// Called once the registration matching `url` has an active worker.
    fn when_ready(&self, url: Url, reply: Reply<RegInfo>);
    /// The inbox of a running version.
    fn worker_inbox(&self, version: u64) -> Option<Inbox>;
    /// Dispatch a `fetch` event (waits until the version is activated).
    fn dispatch_fetch(&self, version: u64, req: SwRequest) -> LocalFuture<FetchOutcome>;
    /// `skipWaiting()`.
    fn skip_waiting(&self, version: u64, reply: Reply<()>);
    /// `clients.claim()`.
    fn claim(&self, version: u64, reply: Reply<Result<(), SwError>>);
    /// Window clients of the version's origin: those it controls, or all.
    fn clients(&self, version: u64, include_uncontrolled: bool) -> Vec<ClientInfo>;
    /// A window client by id, with its inbox.
    fn client(&self, id: &str) -> Option<(ClientInfo, String, Inbox)>;
    /// The `caches` of `origin`.
    fn caches(&self, origin: &str) -> CacheStorageRef;
    /// A controlled navigation committed: check for an update soon
    /// (Chrome: about 1.5 s later).
    fn schedule_update(&self, version: u64);
}

// ---------------------------------------------------------------------------
// Per-window state
// ---------------------------------------------------------------------------

/// Service worker state of one window or scope.
#[derive(Default)]
pub(crate) struct SwSide {
    host: RefCell<Option<Rc<dyn SwHost>>>,
    client_id: RefCell<Option<String>>,
    frame_type: Cell<Option<&'static str>>,
    controller: RefCell<Option<VersionInfo>>,
    listed: Cell<bool>,
    /// Registration id → object, version id → object.
    regs: RefCell<HashMap<u64, u32>>,
    workers: RefCell<HashMap<u64, u32>>,
    ready: Cell<Option<Handle>>,
    msg_enabled: Cell<bool>,
    msg_queue: RefCell<Vec<RemoteTask>>,
    /// Service worker scopes: (version, registration).
    scope: Cell<Option<(u64, u64)>>,
    /// `(p, f, r) => Promise.resolve(p).then(f, r)` with the realm's
    /// original functions (service worker scopes).
    settle: Cell<Option<Handle>>,
    /// Extendable events by event object.
    events: RefCell<HashMap<u32, Rc<EvState>>>,
}

impl SwSide {
    pub(crate) fn scope_set(&self) -> bool {
        self.scope.get().is_some()
    }
}

/// Service worker objects of a window.
pub(crate) enum SwObj {
    Container,
    Registration(Box<RegObj>),
    Worker {
        id: u64,
        script_url: Url,
        state: SwState,
    },
    Preload,
    CacheStorage,
    Cache(CacheRef),
    Clients,
    Client(Box<ClientInfo>),
}

pub(crate) struct RegObj {
    id: u64,
    scope: Url,
    update_via_cache: String,
    installing: Option<VersionInfo>,
    waiting: Option<VersionInfo>,
    active: Option<VersionInfo>,
    preload: Option<u32>,
}

impl SwObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            SwObj::Container => "ServiceWorkerContainer",
            SwObj::Registration(_) => "ServiceWorkerRegistration",
            SwObj::Worker { .. } => "ServiceWorker",
            SwObj::Preload => "NavigationPreloadManager",
            SwObj::CacheStorage => "CacheStorage",
            SwObj::Cache(_) => "Cache",
            SwObj::Clients => "Clients",
            SwObj::Client(_) => "WindowClient",
        }
    }
}

/// A fresh random UUID (client ids).
pub fn uuid4() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    let rnd = |salt: u64| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(N.fetch_add(1, Ordering::Relaxed) ^ salt);
        h.finish()
    };
    let (a, b) = (rnd(1), rnd(2));
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        0x8000 | ((b >> 48) & 0x3fff),
        b & 0xffff_ffff_ffff
    )
}

impl Window {
    /// Let this window use the context's service worker registry (set by
    /// the page layer).
    pub fn set_sw_host(&self, host: Option<Rc<dyn SwHost>>) {
        *self.sw.host.borrow_mut() = host;
    }

    pub fn sw_host(&self) -> Option<Rc<dyn SwHost>> {
        self.sw.host.borrow().clone()
    }

    /// The window's client id (made on first use).
    pub fn client_id(&self) -> String {
        self.sw
            .client_id
            .borrow_mut()
            .get_or_insert_with(uuid4)
            .clone()
    }

    /// The version controlling this window.
    pub fn sw_controller(&self) -> Option<VersionInfo> {
        self.sw.controller.borrow().clone()
    }

    fn client_info(&self) -> ClientInfo {
        let frame_type = self.sw.frame_type.get().unwrap_or("top-level");
        ClientInfo {
            id: self.client_id(),
            url: self.url(),
            frame_type,
            focused: frame_type == "top-level",
        }
    }

    /// Set up a new document's window as a service worker client: its
    /// reserved client id, frame type and controller. It is listed in the
    /// registry when controlled or when its origin has registrations.
    pub fn init_sw_client(
        self: &Rc<Self>,
        client_id: Option<String>,
        nested: bool,
        controller: Option<VersionInfo>,
    ) {
        if let Some(id) = client_id {
            *self.sw.client_id.borrow_mut() = Some(id);
        }
        self.sw
            .frame_type
            .set(Some(if nested { "nested" } else { "top-level" }));
        let controlled = controller.is_some();
        *self.sw.controller.borrow_mut() = controller;
        let Some(host) = self.sw_host() else { return };
        if controlled || host.has_registrations(&self.origin_string()) {
            list(self);
        }
    }

    /// A service worker scope's version and registration (call before
    /// creating its realm).
    pub fn init_sw_scope(&self, version: u64, registration: u64) {
        self.sw.scope.set(Some((version, registration)));
    }

    /// List this service worker scope in the registry (after its inbox
    /// is attached).
    pub fn sw_list(self: &Rc<Self>) {
        list(self);
    }

    /// Whether the service worker scope has a `fetch` listener or handler.
    pub fn has_fetch_handler(&self) -> bool {
        self.listeners
            .borrow()
            .get(&WINDOW_REF)
            .is_some_and(|ls| ls.iter().any(|l| l.type_ == "fetch" && !l.removed.get()))
            || self
                .handlers
                .borrow()
                .contains_key(&(WINDOW_REF, "fetch".to_owned()))
    }

    /// `DOMContentLoaded` fired: the client message queue is enabled.
    pub fn sw_enable_messages(&self) {
        enable_messages(self);
    }

    /// Send a request of this window to its controller first (fetch, XHR,
    /// subresources); not controlled, or not http(s): the network.
    pub fn net_fetch(self: &Rc<Self>, req: NetRequest) -> LocalFuture<Result<NetResponse, String>> {
        let ctrl = self.sw.controller.borrow().as_ref().map(|v| v.id);
        match (ctrl, self.sw_host()) {
            (Some(v), Some(host))
                if matches!(req.url.scheme(), "http" | "https") && !self.is_worker() =>
            {
                let w = self.clone();
                Box::pin(intercept(w, host, v, req))
            }
            _ => self.net.fetch(req),
        }
    }

    /// The window or scope goes away.
    pub(crate) fn close_sw(&self) {
        if self.sw.listed.replace(false)
            && let Some(h) = self.sw_host()
        {
            h.remove_client(&self.client_id());
        }
    }

    /// A service worker scope's realm setup: the settle helper, made from
    /// the realm's own `Promise` before any script runs.
    pub(crate) fn setup_sw_scope(
        &self,
        engine: &mut dyn lb_js::RealmOps,
        realm: lb_js::RealmId,
    ) -> Result<(), lb_js::JsError> {
        let h = engine.evaluate_handle(
            realm,
            "(function(){const P=Promise,r=P.resolve,t=P.prototype.then,a=Reflect.apply;return function(p,f,g){a(t,a(r,P,[p]),[f,g])}})()",
            lb_js::ScriptOrigin::new(""),
        )?;
        self.sw.settle.set(Some(h));
        Ok(())
    }
}

/// Follow redirects answered by the worker; other answers as they are.
async fn intercept(
    w: Rc<Window>,
    host: Rc<dyn SwHost>,
    version: u64,
    mut req: NetRequest,
) -> Result<NetResponse, String> {
    let client = w.client_id();
    let mut redirected = false;
    for _ in 0..20 {
        let sreq = SwRequest::from_net(&req, client.clone(), String::new(), false);
        match host.dispatch_fetch(version, sreq).await {
            FetchOutcome::Fallback => {
                let mut r = w.net.fetch(req).await?;
                r.redirected |= redirected;
                return Ok(r);
            }
            FetchOutcome::Error => {
                let r = Err("net::ERR_FAILED".to_string());
                w.net.sw_answered(&req, &r);
                return r;
            }
            FetchOutcome::Respond(mut r) => {
                let loc = (matches!(r.status, 301 | 302 | 303 | 307 | 308)
                    && req.mode != "navigate")
                    .then(|| r.header("location").and_then(|l| req.url.join(l).ok()))
                    .flatten();
                if let Some(loc) = loc {
                    if r.status == 303 || (matches!(r.status, 301 | 302) && req.method == "POST") {
                        req.method = "GET".into();
                        req.body.clear();
                    }
                    req.url = loc;
                    redirected = true;
                    continue;
                }
                r.redirected |= redirected;
                let r = Ok(r);
                w.net.sw_answered(&req, &r);
                return r;
            }
        }
    }
    Err("net::ERR_TOO_MANY_REDIRECTS".into())
}

/// List `w` in the registry (once).
fn list(w: &Rc<Window>) {
    if w.sw.listed.get() {
        return;
    }
    let Some(host) = w.sw_host() else { return };
    w.sw.listed.set(true);
    let controller = w.sw.controller.borrow().as_ref().map(|v| v.id);
    host.add_client(ClientReg {
        info: w.client_info(),
        origin: w.origin_string(),
        inbox: w.inbox(),
        controller,
        is_window: w.sw.scope.get().is_none() && !w.is_worker(),
    });
}

fn enable_messages(w: &Window) {
    if w.sw.msg_enabled.replace(true) {
        return;
    }
    let queued: Vec<RemoteTask> = w.sw.msg_queue.borrow_mut().drain(..).collect();
    for t in queued {
        w.queue_task(TaskSource::PostedMessage, Box::new(move |js, w| t(js, w)));
    }
}

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

fn sw_obj<R>(w: &Window, id: u32, f: impl FnOnce(&mut SwObj) -> Option<R>) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Sw(s) => f(s),
        _ => None,
    })
    .flatten()
}

fn container(w: &Window) -> u32 {
    w.cached_obj((0, 0, "swcontainer", String::new()), || {
        Obj::Sw(Box::new(SwObj::Container))
    })
}

/// The `ServiceWorker` object of version `v` in `w`.
fn worker_obj(w: &Window, v: &VersionInfo) -> u32 {
    if let Some(id) = w.sw.workers.borrow().get(&v.id) {
        return *id;
    }
    let id = w.new_obj(Obj::Sw(Box::new(SwObj::Worker {
        id: v.id,
        script_url: v.script_url.clone(),
        state: v.state,
    })));
    w.sw.workers.borrow_mut().insert(v.id, id);
    id
}

/// The `ServiceWorkerRegistration` object of `r` in `w`.
fn reg_obj(w: &Window, r: &RegInfo) -> u32 {
    if let Some(id) = w.sw.regs.borrow().get(&r.id) {
        return *id;
    }
    let id = w.new_obj(Obj::Sw(Box::new(SwObj::Registration(Box::new(RegObj {
        id: r.id,
        scope: r.scope.clone(),
        update_via_cache: r.update_via_cache.clone(),
        installing: r.installing.clone(),
        waiting: r.waiting.clone(),
        active: r.active.clone(),
        preload: None,
    })))));
    w.sw.regs.borrow_mut().insert(r.id, id);
    id
}

fn opt_worker(w: &Window, v: Option<VersionInfo>) -> Arg {
    v.map_or(Arg::Null, |v| w.obj_arg(worker_obj(w, &v)))
}

fn this_reg(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, u64), HostError> {
    let id = this_obj(cx)?;
    sw_obj(w, id, |o| match o {
        SwObj::Registration(r) => Some((id, r.id)),
        _ => None,
    })
    .ok_or_else(illegal)
}

fn with_reg<R>(w: &Window, id: u32, f: impl FnOnce(&mut RegObj) -> R) -> Option<R> {
    sw_obj(w, id, |o| match o {
        SwObj::Registration(r) => Some(f(r)),
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Tasks sent by the registry
// ---------------------------------------------------------------------------

/// "Update Registration State": the registration's slots as they are now.
pub fn reg_state_task(info: RegInfo) -> RemoteTask {
    Box::new(move |_js, w| {
        let id = w.sw.regs.borrow().get(&info.id).copied();
        if let Some(id) = id {
            with_reg(w, id, |r| {
                r.installing = info.installing.clone();
                r.waiting = info.waiting.clone();
                r.active = info.active.clone();
            });
        }
    })
}

/// "Update Worker State": the state, then `statechange` at the object.
pub fn worker_state_task(v: VersionInfo) -> RemoteTask {
    Box::new(move |js, w| {
        if let Some(c) = w.sw.controller.borrow_mut().as_mut()
            && c.id == v.id
        {
            c.state = v.state;
        }
        let id = w.sw.workers.borrow().get(&v.id).copied();
        let Some(id) = id else { return };
        sw_obj(w, id, |o| match o {
            SwObj::Worker { state, .. } => {
                *state = v.state;
                Some(())
            }
            _ => None,
        });
        super::fire_event(js, w, HostRef::new(K_OBJ, id), "statechange", false, false);
    })
}

/// `updatefound` at the registration's object.
pub fn update_found_task(reg: u64) -> RemoteTask {
    Box::new(move |js, w| {
        let id = w.sw.regs.borrow().get(&reg).copied();
        if let Some(id) = id {
            super::fire_event(js, w, HostRef::new(K_OBJ, id), "updatefound", false, false);
        }
    })
}

/// The window's controller changed: `controllerchange` at its container.
pub fn controller_task(v: VersionInfo) -> RemoteTask {
    Box::new(move |js, w| {
        *w.sw.controller.borrow_mut() = Some(v);
        let id = w
            .cache
            .borrow()
            .get(&(0, 0, "swcontainer", String::new()))
            .copied();
        if let Some(id) = id {
            super::fire_event(
                js,
                w,
                HostRef::new(K_OBJ, id),
                "controllerchange",
                false,
                false,
            );
        }
    })
}

/// A message from a service worker (`Client.postMessage`) for this
/// window's container; queued until the client message queue is enabled.
fn client_message_task(from: VersionInfo, p: Payload, origin: String) -> RemoteTask {
    Box::new(move |js, w| {
        if !w.sw.msg_enabled.get() {
            w.sw.msg_queue
                .borrow_mut()
                .push(client_message_task(from, p, origin));
            return;
        }
        let target = HostRef::new(K_OBJ, container(w));
        let source = w.obj_arg(worker_obj(w, &from));
        super::worker::deliver(js, w, target, p, origin, Some(source));
    })
}

// ---------------------------------------------------------------------------
// Extendable events (service worker scopes)
// ---------------------------------------------------------------------------

type Settled = Box<dyn FnOnce(&mut CallCtx<'_, '_, '_>, &Rc<Window>, bool, Handle)>;

/// Run `f` once `value` (a promise or not) settles: (fulfilled, value).
fn on_settled(js: &mut dyn Js, w: &Rc<Window>, value: Handle, f: Settled) {
    let Some(helper) = w.sw.settle.get() else {
        return;
    };
    let cell: Rc<RefCell<Option<Settled>>> = Rc::new(RefCell::new(Some(f)));
    let mk = |ok: bool| {
        let cell = cell.clone();
        let rt = w.rt.clone();
        host_fn(move |cx| {
            let w = rt.win(cx.realm())?;
            let v = if cx.is_empty() {
                cx.make_handle(Arg::Undefined)?
            } else {
                cx.arg_handle(0)
            };
            let f = cell.borrow_mut().take();
            if let Some(f) = f {
                f(cx, &w, ok, v);
            }
            Ok(())
        })
    };
    let realm = js.realm();
    let ff = js.realm_ops().new_function(realm, "", 1, mk(true));
    let rf = js.realm_ops().new_function(realm, "", 1, mk(false));
    if let (Ok(ff), Ok(rf)) = (ff, rf) {
        let _ = js.call(
            helper,
            Arg::Undefined,
            &[Arg::Handle(value), Arg::Handle(ff), Arg::Handle(rf)],
        );
        js.release(ff);
        js.release(rf);
    }
}

/// State of one extendable event.
pub(crate) struct EvState {
    pending: Cell<u32>,
    rejected: Cell<bool>,
    dispatching: Cell<bool>,
    done: RefCell<Option<Box<dyn FnOnce(&Rc<Window>, bool)>>>,
    fetch: Option<FetchEv>,
}

struct FetchEv {
    request: u32,
    url: Url,
    client_id: String,
    resulting: String,
    responded: Cell<bool>,
    reply: RefCell<Option<Reply<FetchOutcome>>>,
    handled: Cell<Option<Handle>>,
}

impl EvState {
    fn finish_if_done(&self, w: &Rc<Window>) {
        if self.pending.get() == 0 && !self.dispatching.get() {
            let f = self.done.borrow_mut().take();
            if let Some(f) = f {
                f(w, !self.rejected.get());
            }
        }
    }
}

fn ev_state(w: &Window, ev: u32) -> Option<Rc<EvState>> {
    w.sw.events.borrow().get(&ev).cloned()
}

/// Add `p` to the event's extend lifetime promises.
fn extend(js: &mut dyn Js, w: &Rc<Window>, st: &Rc<EvState>, p: Handle) {
    st.pending.set(st.pending.get() + 1);
    let st2 = st.clone();
    on_settled(
        js,
        w,
        p,
        Box::new(move |js, w, ok, v| {
            js.release(v);
            if !ok {
                st2.rejected.set(true);
            }
            st2.pending.set(st2.pending.get().saturating_sub(1));
            st2.finish_if_done(w);
        }),
    );
}

/// Dispatch extendable event `ev` at the scope; `done(ok)` runs once the
/// handlers returned and every `waitUntil` promise settled (ok: none
/// rejected).
fn dispatch_extendable(
    js: &mut dyn Js,
    w: &Rc<Window>,
    ev: u32,
    fetch: Option<FetchEv>,
    done: Box<dyn FnOnce(&Rc<Window>, bool)>,
) -> Rc<EvState> {
    let st = Rc::new(EvState {
        pending: Cell::new(0),
        rejected: Cell::new(false),
        dispatching: Cell::new(true),
        done: RefCell::new(Some(done)),
        fetch,
    });
    w.sw.events.borrow_mut().insert(ev, st.clone());
    dispatch(js, w, WINDOW_REF, ev);
    st.dispatching.set(false);
    st
}

/// `install` (an `InstallEvent`) or `activate` (an `ExtendableEvent`) at
/// a service worker scope; `reply(ok)` once its `waitUntil` promises
/// settled (ok: none rejected).
pub fn lifecycle_task(kind: &'static str, reply: Reply<bool>) -> RemoteTask {
    Box::new(move |js, w| {
        let iface = if kind == "install" {
            "InstallEvent"
        } else {
            "ExtendableEvent"
        };
        let ev = new_event(w, iface, kind, false, false, EventExtra::None);
        let st = dispatch_extendable(js, w, ev, None, Box::new(move |_, ok| reply(ok)));
        st.finish_if_done(w);
    })
}

/// Evaluate `src` (an expression whose value, or promise, is a string)
/// in a service worker scope and reply with the string. For the
/// fingerprint report's probe of a version started outside the registry.
pub fn eval_task(src: String, reply: Reply<Result<String, String>>) -> RemoteTask {
    Box::new(
        move |js, w| match js.eval_handle(&src, lb_js::ScriptOrigin::new("")) {
            Err(e) => reply(Err(e.message)),
            Ok(h) => on_settled(
                js,
                w,
                h,
                Box::new(move |cx, _w, ok, v| {
                    let s = match cx.handle_value(v) {
                        lb_js::JsValue::String(s) => s,
                        other => format!("{other:?}"),
                    };
                    cx.release(v);
                    reply(if ok { Ok(s) } else { Err(s) });
                }),
            ),
        },
    )
}

/// Resolve a `skipWaiting()` promise of the scope.
fn resolve_task(r: lb_js::Resolver) -> RemoteTask {
    Box::new(move |js, _w| js.resolve(r, Arg::Undefined))
}

/// A `fetch` event for `req`; `reply` gets the outcome.
pub fn fetch_task(req: SwRequest, reply: Reply<FetchOutcome>) -> RemoteTask {
    Box::new(move |js, w| {
        let h = new_headers(
            w,
            HeadersData {
                list: req
                    .headers
                    .iter()
                    .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
                    .collect(),
                immutable: true,
            },
        );
        let signal = new_signal(w);
        let rid = w.new_obj(Obj::Request(Box::new(RequestData {
            method: req.method.clone(),
            url: req.url.clone(),
            headers: h,
            body: (!req.body.is_empty()).then(|| req.body.clone()),
            used: false,
            signal,
            mode: req.mode,
            credentials: req.credentials.into(),
            cache: req.cache.into(),
            redirect: req.redirect.into(),
            destination: req.destination,
        })));
        let ev = new_event(w, "FetchEvent", "fetch", false, true, EventExtra::None);
        let fe = FetchEv {
            request: rid,
            url: req.url.clone(),
            client_id: req.client_id.clone(),
            resulting: req.resulting_client_id.clone(),
            responded: Cell::new(false),
            reply: RefCell::new(Some(reply)),
            handled: Cell::new(None),
        };
        let st = dispatch_extendable(js, w, ev, Some(fe), Box::new(|_, _| {}));
        let canceled = w
            .with_obj(ev, |o| match o {
                Obj::Event(e) => e.canceled,
                _ => false,
            })
            .unwrap_or(false);
        if let Some(f) = &st.fetch
            && !f.responded.get()
        {
            let reply = f.reply.borrow_mut().take();
            if let Some(reply) = reply {
                reply(if canceled {
                    FetchOutcome::Error
                } else {
                    FetchOutcome::Fallback
                });
            }
        }
        st.finish_if_done(w);
    })
}

/// A message from a client (`ServiceWorker.postMessage`): an
/// `ExtendableMessageEvent` at the scope.
fn sw_message_task(p: Payload, source: ClientInfo, origin: String) -> RemoteTask {
    Box::new(move |js, w| {
        let (data, ports) = unpack(js, w, p);
        let client = w.new_obj(Obj::Sw(Box::new(SwObj::Client(Box::new(source)))));
        let src = js.make(w.obj_arg(client));
        let (type_, data) = match data {
            Ok(d) => ("message", Some(d)),
            Err(()) => ("messageerror", None),
        };
        let ev = new_event(
            w,
            "ExtendableMessageEvent",
            type_,
            false,
            false,
            EventExtra::Message {
                data,
                origin,
                source: src,
                ports,
            },
        );
        let st = dispatch_extendable(js, w, ev, None, Box::new(|_, _| {}));
        st.finish_if_done(w);
    })
}

/// The `Response` object `v` of the scope as the client's response
/// (`None`: not a response, or a network error).
fn response_of(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    v: Handle,
    req_url: &Url,
) -> Option<NetResponse> {
    let host = cx.handle_host(v).filter(|h| h.kind == K_OBJ)?;
    let (status, status_text, headers, body, url, redirected, type_) = w
        .with_obj(host.id, |o| match o {
            Obj::Response(r) => {
                let out = (
                    r.status,
                    r.status_text.clone(),
                    r.headers,
                    r.body.clone(),
                    r.url.clone(),
                    r.redirected,
                    r.type_,
                );
                r.used = r.body.is_some();
                Some(out)
            }
            _ => None,
        })
        .flatten()?;
    if type_ == "error" {
        return None;
    }
    Some(NetResponse {
        status,
        status_text,
        headers: headers_of(w, headers).map(|h| h.list).unwrap_or_default(),
        body: body.unwrap_or_default().into(),
        url: Url::parse(&url).unwrap_or_else(|_| req_url.clone()),
        redirected,
    })
}

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

fn host_of(w: &Window) -> Result<Rc<dyn SwHost>, HostError> {
    w.sw_host().ok_or_else(illegal)
}

/// Send `f` to `w`'s inbox from a registry reply.
fn back<T: Send + 'static>(
    w: &Rc<Window>,
    f: impl FnOnce(&mut dyn Js, &Rc<Window>, T) + Send + 'static,
) -> Reply<T> {
    let inbox = w.inbox();
    Box::new(move |v| {
        inbox.send(Box::new(move |js, w| f(js, w, v)));
    })
}

fn has_escape(u: &Url) -> bool {
    let p = u.path().to_ascii_lowercase();
    p.contains("%2f") || p.contains("%5c")
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Navigator", "serviceWorker", |cx, w| {
        list(w);
        cx.ret(w.obj_arg(container(w)))
    });
    register_container(reg, rt);
    register_registration(reg, rt);
    register_scope(reg, rt);
    register_cache(reg, rt);
}

fn this_container(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
    let id = this_obj(cx)?;
    sw_obj(w, id, |o| matches!(o, SwObj::Container).then_some(())).ok_or_else(illegal)
}

fn register_container(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "ServiceWorkerContainer", "controller", |cx, w| {
        this_container(cx, w)?;
        let c = w.sw_controller();
        cx.ret(opt_worker(w, c))
    });
    getter(reg, rt, "ServiceWorkerContainer", "ready", |cx, w| {
        this_container(cx, w)?;
        if let Some(h) = w.sw.ready.get() {
            return cx.ret(Arg::Handle(h));
        }
        let (p, r) = cx.new_promise()?;
        w.sw.ready.set(Some(p));
        let host = host_of(w)?;
        list(w);
        host.when_ready(
            w.url(),
            back(w, move |js, w, info: RegInfo| {
                let id = reg_obj(w, &info);
                js.resolve(r, w.obj_arg(id));
            }),
        );
        cx.ret(Arg::Handle(p))
    });
    method(
        reg,
        rt,
        "ServiceWorkerContainer",
        "startMessages",
        |cx, w| {
            this_container(cx, w)?;
            enable_messages(w);
            Ok(())
        },
    );
    if let Some(set) = reg
        .get("ServiceWorkerContainer", "onmessage", ImplKind::Setter)
        .cloned()
    {
        let rt2 = rt.clone();
        reg.register(
            "ServiceWorkerContainer",
            "onmessage",
            ImplKind::Setter,
            host_fn(move |cx| {
                set(cx)?;
                if let Ok(w) = rt2.win(cx.realm()) {
                    enable_messages(&w);
                }
                Ok(())
            }),
        );
    }
    method(reg, rt, "ServiceWorkerContainer", "register", |cx, w| {
        let id = this_obj(cx)?;
        sw_obj(w, id, |o| matches!(o, SwObj::Container).then_some(())).ok_or_else(illegal)?;
        let r = cx.ret_new_promise()?;
        match start_register(cx, w, r) {
            Ok(()) => Ok(()),
            Err(e) => cx.reject_promise(r, e),
        }
    });
    method(
        reg,
        rt,
        "ServiceWorkerContainer",
        "getRegistration",
        |cx, w| {
            this_container(cx, w)?;
            let r = cx.ret_new_promise()?;
            let s = super::opt_string(cx, 0)?.unwrap_or_default();
            let Some(url) = w.resolve_url(&s) else {
                return cx.reject_promise(
                r,
                HostError::type_error(format!(
                    "Failed to get a ServiceWorkerRegistration: The provided documentURL ('{s}') is invalid."
                )),
            );
            };
            let mine = w.origin_string();
            if url.origin().ascii_serialization() != mine {
                let e = w.rt.err(
                "SecurityError",
                format!(
                    "Failed to get a ServiceWorkerRegistration: The origin of the provided documentURL ('{}') does not match the current origin ('{mine}').",
                    url.origin().ascii_serialization()
                ),
            );
                return cx.reject_promise(r, e);
            }
            let host = host_of(w)?;
            list(w);
            match host.registration_for(&url) {
                Some(info) => {
                    let id = reg_obj(w, &info);
                    let a = w.obj_arg(id);
                    cx.resolve_promise(r, a)
                }
                None => cx.resolve_promise(r, Arg::Undefined),
            }
        },
    );
    method(
        reg,
        rt,
        "ServiceWorkerContainer",
        "getRegistrations",
        |cx, w| {
            this_container(cx, w)?;
            let r = cx.ret_new_promise()?;
            let host = host_of(w)?;
            list(w);
            let regs = host.registrations(&w.origin_string());
            let arr: Vec<Arg> = regs.iter().map(|i| w.obj_arg(reg_obj(w, i))).collect();
            cx.resolve_promise(r, Arg::Array(arr))
        },
    );

    // ServiceWorker
    fn this_worker(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
    ) -> Result<(u64, Url, SwState), HostError> {
        let id = this_obj(cx)?;
        sw_obj(w, id, |o| match o {
            SwObj::Worker {
                id,
                script_url,
                state,
            } => Some((*id, script_url.clone(), *state)),
            _ => None,
        })
        .ok_or_else(illegal)
    }
    getter(reg, rt, "ServiceWorker", "scriptURL", |cx, w| {
        let (_, u, _) = this_worker(cx, w)?;
        cx.ret(u.to_string())
    });
    getter(reg, rt, "ServiceWorker", "state", |cx, w| {
        let (_, _, s) = this_worker(cx, w)?;
        cx.ret(s.as_str())
    });
    method(reg, rt, "ServiceWorker", "postMessage", |cx, w| {
        let (vid, _, state) = this_worker(cx, w)?;
        let p = serialize_args(cx, w, "postMessage", "ServiceWorker")?;
        if state == SwState::Redundant {
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'postMessage' on 'ServiceWorker': The ServiceWorker is no longer active.",
            ));
        }
        let host = host_of(w)?;
        if let Some(inbox) = host.worker_inbox(vid) {
            let origin = w.origin_string();
            let source = if w.sw.scope.get().is_some() {
                // From another service worker: no window client.
                ClientInfo {
                    id: w.client_id(),
                    url: w.url(),
                    frame_type: "none",
                    focused: false,
                }
            } else {
                list(w);
                w.client_info()
            };
            inbox.send(sw_message_task(p, source, origin));
        }
        Ok(())
    });
}

/// `register(scriptURL, options)`: checks done in the page, then the job.
fn start_register(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    r: lb_js::Resolver,
) -> Result<(), HostError> {
    if cx.is_empty() {
        return Err(HostError::type_error(
            "Failed to execute 'register' on 'ServiceWorkerContainer': 1 argument required, but only 0 present.",
        ));
    }
    let s = cx.arg_string(0)?;
    let d = Dict::arg(cx, 1);
    let scope_s = d.string(cx, "scope")?;
    let uvc = d.string(cx, "updateViaCache")?;
    let ty = d.string(cx, "type")?;
    d.release(cx);
    let uvc = match uvc.as_deref() {
        None => "imports".to_owned(),
        Some(v @ ("imports" | "all" | "none")) => v.to_owned(),
        Some(v) => {
            return Err(HostError::type_error(format!(
                "Failed to execute 'register' on 'ServiceWorkerContainer': Failed to read the 'updateViaCache' property from 'RegistrationOptions': The provided value '{v}' is not a valid enum value of type ServiceWorkerUpdateViaCache."
            )));
        }
    };
    if let Some(t) = ty.as_deref()
        && t != "classic"
        && t != "module"
    {
        return Err(HostError::type_error(format!(
            "Failed to execute 'register' on 'ServiceWorkerContainer': Failed to read the 'type' property from 'RegistrationOptions': The provided value '{t}' is not a valid enum value of type WorkerType."
        )));
    }
    let fail = |m: String| format!("Failed to register a ServiceWorker: {m}");
    let Some(script) = w.resolve_url(&s) else {
        return Err(HostError::type_error(fail(format!(
            "An invalid ScriptURL was provided ('{s}')."
        ))));
    };
    if !matches!(script.scheme(), "http" | "https") {
        return Err(HostError::type_error(fail(format!(
            "The URL protocol of the script ('{script}') is not supported."
        ))));
    }
    let scope = match &scope_s {
        Some(sc) => w.resolve_url(sc),
        None => script.join("./").ok(),
    };
    let Some(mut scope) = scope else {
        return Err(HostError::type_error(fail(format!(
            "An invalid scope was provided ('{}').",
            scope_s.unwrap_or_default()
        ))));
    };
    scope.set_fragment(None);
    if !matches!(scope.scheme(), "http" | "https") {
        return Err(HostError::type_error(fail(format!(
            "The URL protocol of the scope ('{scope}') is not supported."
        ))));
    }
    let mine = w.origin_string();
    let so = script.origin().ascii_serialization();
    if so != mine {
        return Err(w.rt.err(
            "SecurityError",
            fail(format!(
                "The origin of the provided scriptURL ('{so}') does not match the current origin ('{mine}')."
            )),
        ));
    }
    let sco = scope.origin().ascii_serialization();
    if sco != mine {
        return Err(w.rt.err(
            "SecurityError",
            fail(format!(
                "The origin of the provided scope ('{sco}') does not match the current origin ('{mine}')."
            )),
        ));
    }
    if has_escape(&scope) || has_escape(&script) {
        return Err(HostError::type_error(fail(format!(
            "The provided scope ('{scope}') or scriptURL ('{script}') includes a disallowed escape character."
        ))));
    }
    let host = host_of(w)?;
    list(w);
    host.register(
        w.client_info(),
        script,
        scope,
        uvc,
        back(w, move |js, w, res: Result<RegInfo, SwError>| match res {
            Ok(info) => {
                let id = reg_obj(w, &info);
                js.resolve(r, w.obj_arg(id));
            }
            Err(e) => js.reject(r, e.host(w)),
        }),
    );
    Ok(())
}

fn register_registration(reg: &mut Registry, rt: &Runtime) {
    for (name, slot) in [("installing", 0u8), ("waiting", 1), ("active", 2)] {
        getter(reg, rt, "ServiceWorkerRegistration", name, move |cx, w| {
            let (id, _) = this_reg(cx, w)?;
            let v = with_reg(w, id, |r| match slot {
                0 => r.installing.clone(),
                1 => r.waiting.clone(),
                _ => r.active.clone(),
            })
            .flatten();
            cx.ret(opt_worker(w, v))
        });
    }
    getter(reg, rt, "ServiceWorkerRegistration", "scope", |cx, w| {
        let (id, _) = this_reg(cx, w)?;
        let s = with_reg(w, id, |r| r.scope.to_string()).unwrap_or_default();
        cx.ret(s)
    });
    getter(
        reg,
        rt,
        "ServiceWorkerRegistration",
        "updateViaCache",
        |cx, w| {
            let (id, _) = this_reg(cx, w)?;
            let s = with_reg(w, id, |r| r.update_via_cache.clone()).unwrap_or_default();
            cx.ret(s)
        },
    );
    getter(
        reg,
        rt,
        "ServiceWorkerRegistration",
        "navigationPreload",
        |cx, w| {
            let (id, _) = this_reg(cx, w)?;
            let have = with_reg(w, id, |r| r.preload).flatten();
            let p = match have {
                Some(p) => p,
                None => {
                    let p = w.new_obj(Obj::Sw(Box::new(SwObj::Preload)));
                    with_reg(w, id, |r| r.preload = Some(p));
                    p
                }
            };
            cx.ret(w.obj_arg(p))
        },
    );
    method(reg, rt, "ServiceWorkerRegistration", "update", |cx, w| {
        let (_, rid) = this_reg(cx, w)?;
        let r = cx.ret_new_promise()?;
        let host = host_of(w)?;
        host.update(
            rid,
            back(w, move |js, w, res: Result<RegInfo, SwError>| match res {
                Ok(info) => {
                    let id = reg_obj(w, &info);
                    js.resolve(r, w.obj_arg(id));
                }
                Err(e) => js.reject(r, e.host(w)),
            }),
        );
        Ok(())
    });
    method(
        reg,
        rt,
        "ServiceWorkerRegistration",
        "unregister",
        |cx, w| {
            let (_, rid) = this_reg(cx, w)?;
            let r = cx.ret_new_promise()?;
            let host = host_of(w)?;
            host.unregister(
                rid,
                back(w, move |js, _w, ok: bool| js.resolve(r, Arg::Bool(ok))),
            );
            Ok(())
        },
    );
    // NavigationPreloadManager: off, as in a registration that never
    // enabled it.
    for name in ["enable", "disable", "setHeaderValue"] {
        method(reg, rt, "NavigationPreloadManager", name, |cx, w| {
            let id = this_obj(cx)?;
            sw_obj(w, id, |o| matches!(o, SwObj::Preload).then_some(())).ok_or_else(illegal)?;
            let r = cx.ret_new_promise()?;
            cx.resolve_promise(r, Arg::Undefined)
        });
    }
    method(reg, rt, "NavigationPreloadManager", "getState", |cx, w| {
        let id = this_obj(cx)?;
        sw_obj(w, id, |o| matches!(o, SwObj::Preload).then_some(())).ok_or_else(illegal)?;
        let r = cx.ret_new_promise()?;
        cx.resolve_promise(
            r,
            Arg::Json(r#"{"enabled":false,"headerValue":"true"}"#.into()),
        )
    });
}

// ---------------------------------------------------------------------------
// ServiceWorkerGlobalScope, clients, events
// ---------------------------------------------------------------------------

fn scope_ids(w: &Window) -> Result<(u64, u64), HostError> {
    w.sw.scope.get().ok_or_else(illegal)
}

fn client_obj(w: &Window, c: ClientInfo) -> Arg {
    let id = w.new_obj(Obj::Sw(Box::new(SwObj::Client(Box::new(c)))));
    w.obj_arg(id)
}

fn register_scope(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "ServiceWorkerGlobalScope", "clients", |cx, w| {
        scope_ids(w)?;
        let id = w.cached_obj((0, 0, "swclients", String::new()), || {
            Obj::Sw(Box::new(SwObj::Clients))
        });
        cx.ret(w.obj_arg(id))
    });
    getter(
        reg,
        rt,
        "ServiceWorkerGlobalScope",
        "registration",
        |cx, w| {
            let (_, rid) = scope_ids(w)?;
            let have = w.sw.regs.borrow().get(&rid).copied();
            let id = match have {
                Some(id) => id,
                None => {
                    let info = host_of(w)?.registration(rid).ok_or_else(illegal)?;
                    reg_obj(w, &info)
                }
            };
            cx.ret(w.obj_arg(id))
        },
    );
    getter(
        reg,
        rt,
        "ServiceWorkerGlobalScope",
        "serviceWorker",
        |cx, w| {
            let (vid, _) = scope_ids(w)?;
            let have = w.sw.workers.borrow().get(&vid).copied();
            let id = match have {
                Some(id) => id,
                None => {
                    let info = host_of(w)?.version(vid).ok_or_else(illegal)?;
                    worker_obj(w, &info)
                }
            };
            cx.ret(w.obj_arg(id))
        },
    );
    method(
        reg,
        rt,
        "ServiceWorkerGlobalScope",
        "skipWaiting",
        |cx, w| {
            let (vid, _) = scope_ids(w)?;
            let r = cx.ret_new_promise()?;
            let inbox = w.inbox();
            host_of(w)?.skip_waiting(
                vid,
                Box::new(move |()| {
                    inbox.send(resolve_task(r));
                }),
            );
            Ok(())
        },
    );

    // Clients
    fn this_clients(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u64, HostError> {
        let id = this_obj(cx)?;
        sw_obj(w, id, |o| matches!(o, SwObj::Clients).then_some(())).ok_or_else(illegal)?;
        Ok(scope_ids(w)?.0)
    }
    method(reg, rt, "Clients", "get", |cx, w| {
        let vid = this_clients(cx, w)?;
        let r = cx.ret_new_promise()?;
        let id = super::opt_string(cx, 0)?.unwrap_or_default();
        let found = host_of(w)?
            .client(&id)
            .filter(|(_, origin, _)| *origin == w.origin_string())
            .map(|(c, _, _)| c);
        let _ = vid;
        match found {
            Some(c) => {
                let a = client_obj(w, c);
                cx.resolve_promise(r, a)
            }
            None => cx.resolve_promise(r, Arg::Undefined),
        }
    });
    method(reg, rt, "Clients", "matchAll", |cx, w| {
        let vid = this_clients(cx, w)?;
        let r = cx.ret_new_promise()?;
        let d = Dict::arg(cx, 0);
        let all = d.bool(cx, "includeUncontrolled")?;
        let ty = d.string(cx, "type")?.unwrap_or_else(|| "window".into());
        d.release(cx);
        if !matches!(ty.as_str(), "window" | "worker" | "sharedworker" | "all") {
            return cx.reject_promise(
                r,
                HostError::type_error(format!(
                    "Failed to execute 'matchAll' on 'Clients': Failed to read the 'type' property from 'ClientQueryOptions': The provided value '{ty}' is not a valid enum value of type ClientType."
                )),
            );
        }
        let list = if matches!(ty.as_str(), "window" | "all") {
            host_of(w)?.clients(vid, all)
        } else {
            Vec::new()
        };
        let arr: Vec<Arg> = list.into_iter().map(|c| client_obj(w, c)).collect();
        cx.resolve_promise(r, Arg::Array(arr))
    });
    method(reg, rt, "Clients", "openWindow", |cx, w| {
        this_clients(cx, w)?;
        let r = cx.ret_new_promise()?;
        let e = w.rt.err(
            "InvalidAccessError",
            "Failed to execute 'openWindow' on 'Clients': Not allowed to open a window.",
        );
        cx.reject_promise(r, e)
    });
    method(reg, rt, "Clients", "claim", |cx, w| {
        let vid = this_clients(cx, w)?;
        let r = cx.ret_new_promise()?;
        host_of(w)?.claim(
            vid,
            back(w, move |js, w, res: Result<(), SwError>| match res {
                Ok(()) => js.resolve(r, Arg::Undefined),
                Err(e) => js.reject(r, e.host(w)),
            }),
        );
        Ok(())
    });

    // Client / WindowClient
    fn this_client(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<ClientInfo, HostError> {
        let id = this_obj(cx)?;
        sw_obj(w, id, |o| match o {
            SwObj::Client(c) => Some((**c).clone()),
            _ => None,
        })
        .ok_or_else(illegal)
    }
    getter(reg, rt, "Client", "id", |cx, w| {
        let c = this_client(cx, w)?;
        cx.ret(c.id)
    });
    getter(reg, rt, "Client", "url", |cx, w| {
        let c = this_client(cx, w)?;
        cx.ret(c.url.to_string())
    });
    getter(reg, rt, "Client", "type", |cx, w| {
        this_client(cx, w)?;
        cx.ret("window")
    });
    getter(reg, rt, "Client", "frameType", |cx, w| {
        let c = this_client(cx, w)?;
        cx.ret(c.frame_type)
    });
    getter(reg, rt, "Client", "lifecycleState", |cx, w| {
        this_client(cx, w)?;
        cx.ret("active")
    });
    getter(reg, rt, "WindowClient", "visibilityState", |cx, w| {
        this_client(cx, w)?;
        cx.ret("visible")
    });
    getter(reg, rt, "WindowClient", "focused", |cx, w| {
        let c = this_client(cx, w)?;
        cx.ret(c.focused)
    });
    getter(reg, rt, "WindowClient", "ancestorOrigins", |cx, w| {
        this_client(cx, w)?;
        cx.ret(Arg::Array(Vec::new()))
    });
    for name in ["focus", "navigate"] {
        method(reg, rt, "WindowClient", name, move |cx, w| {
            this_client(cx, w)?;
            let r = cx.ret_new_promise()?;
            let e = w.rt.err(
                "InvalidAccessError",
                format!(
                    "Failed to execute '{name}' on 'WindowClient': Not allowed to {} a window.",
                    if name == "focus" { "focus" } else { "navigate" }
                ),
            );
            cx.reject_promise(r, e)
        });
    }
    method(reg, rt, "Client", "postMessage", |cx, w| {
        let c = this_client(cx, w)?;
        let iface = "WindowClient";
        let p = serialize_args(cx, w, "postMessage", iface)?;
        let (vid, _) = scope_ids(w)?;
        let host = host_of(w)?;
        let Some(from) = host.version(vid) else {
            return Ok(());
        };
        if let Some((_, _, inbox)) = host.client(&c.id) {
            inbox.send(client_message_task(from, p, w.origin_string()));
        }
        Ok(())
    });

    // Extendable events
    fn this_ev(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, Rc<EvState>), HostError> {
        let id = this_obj(cx)?;
        let st = ev_state(w, id).ok_or_else(illegal)?;
        Ok((id, st))
    }
    method(reg, rt, "ExtendableEvent", "waitUntil", |cx, w| {
        let (_, st) = this_ev(cx, w)?;
        super::need_args(cx, 1, "waitUntil", "ExtendableEvent")?;
        if !st.dispatching.get() && st.pending.get() == 0 {
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'waitUntil' on 'ExtendableEvent': The event handler is already finished and no extend lifetime promises are outstanding.",
            ));
        }
        let p = cx.arg_handle(0);
        extend(cx, w, &st, p);
        Ok(())
    });
    fn fetch_ev<R>(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
        f: impl FnOnce(&FetchEv) -> R,
    ) -> Result<R, HostError> {
        let (_, st) = this_ev(cx, w)?;
        st.fetch.as_ref().map(f).ok_or_else(illegal)
    }
    getter(reg, rt, "FetchEvent", "request", |cx, w| {
        let id = fetch_ev(cx, w, |f| f.request)?;
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "FetchEvent", "clientId", |cx, w| {
        let s = fetch_ev(cx, w, |f| f.client_id.clone())?;
        cx.ret(s)
    });
    getter(reg, rt, "FetchEvent", "resultingClientId", |cx, w| {
        let s = fetch_ev(cx, w, |f| f.resulting.clone())?;
        cx.ret(s)
    });
    getter(reg, rt, "FetchEvent", "replacesClientId", |cx, w| {
        fetch_ev(cx, w, |_| ())?;
        cx.ret("")
    });
    getter(reg, rt, "FetchEvent", "isReload", |cx, w| {
        fetch_ev(cx, w, |_| ())?;
        cx.ret(false)
    });
    getter(reg, rt, "FetchEvent", "preloadResponse", |cx, w| {
        fetch_ev(cx, w, |_| ())?;
        let (p, r) = cx.new_promise()?;
        cx.resolve_promise(r, Arg::Undefined)?;
        cx.ret(Arg::Handle(p))
    });
    getter(reg, rt, "FetchEvent", "handled", |cx, w| {
        let (_, st) = this_ev(cx, w)?;
        let f = st.fetch.as_ref().ok_or_else(illegal)?;
        if let Some(h) = f.handled.get() {
            return cx.ret(Arg::Handle(h));
        }
        let (p, r) = cx.new_promise()?;
        cx.resolve_promise(r, Arg::Undefined)?;
        f.handled.set(Some(p));
        cx.ret(Arg::Handle(p))
    });
    method(reg, rt, "FetchEvent", "respondWith", |cx, w| {
        let (ev, st) = this_ev(cx, w)?;
        let f = st.fetch.as_ref().ok_or_else(illegal)?;
        super::need_args(cx, 1, "respondWith", "FetchEvent")?;
        if !st.dispatching.get() {
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'respondWith' on 'FetchEvent': The event handler is already finished.",
            ));
        }
        if f.responded.replace(true) {
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'respondWith' on 'FetchEvent': The event has already been responded to.",
            ));
        }
        // respondWith stops propagation (Service Workers).
        w.with_obj(ev, |o| {
            if let Obj::Event(e) = o {
                e.stop = true;
                e.stop_immediate = true;
            }
        });
        let p = cx.arg_handle(0);
        extend(cx, w, &st, p);
        let st2 = st.clone();
        on_settled(
            cx,
            w,
            p,
            Box::new(move |js, w, ok, v| {
                let Some(f) = &st2.fetch else { return };
                let out = if ok {
                    response_of(js, w, v, &f.url).map_or(FetchOutcome::Error, FetchOutcome::Respond)
                } else {
                    FetchOutcome::Error
                };
                js.release(v);
                let reply = f.reply.borrow_mut().take();
                if let Some(reply) = reply {
                    reply(out);
                }
            }),
        );
        Ok(())
    });
    // ExtendableMessageEvent (and MessageEvent.lastEventId).
    fn msg_extra(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<EventExtra, HostError> {
        let id = this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::Event(e) => Some(e.extra.clone()),
            _ => None,
        })
        .flatten()
        .ok_or_else(illegal)
    }
    getter(
        reg,
        rt,
        "ExtendableMessageEvent",
        "data",
        |cx, w| match msg_extra(cx, w)? {
            EventExtra::Message { data: Some(h), .. } => cx.ret(Arg::Handle(h)),
            _ => cx.ret(Arg::Null),
        },
    );
    getter(
        reg,
        rt,
        "ExtendableMessageEvent",
        "origin",
        |cx, w| match msg_extra(cx, w)? {
            EventExtra::Message { origin, .. } => cx.ret(origin),
            _ => cx.ret(""),
        },
    );
    getter(
        reg,
        rt,
        "ExtendableMessageEvent",
        "source",
        |cx, w| match msg_extra(cx, w)? {
            EventExtra::Message {
                source: Some(h), ..
            } => cx.ret(Arg::Handle(h)),
            _ => cx.ret(Arg::Null),
        },
    );
    getter(
        reg,
        rt,
        "ExtendableMessageEvent",
        "ports",
        |cx, w| match msg_extra(cx, w)? {
            EventExtra::Message { ports: Some(h), .. } => cx.ret(Arg::Handle(h)),
            _ => match super::worker::empty_ports(cx, w) {
                Some(h) => cx.ret(Arg::Handle(h)),
                None => cx.ret(Arg::Array(Vec::new())),
            },
        },
    );
    for iface in ["ExtendableMessageEvent", "MessageEvent"] {
        getter(reg, rt, iface, "lastEventId", |cx, w| {
            msg_extra(cx, w)?;
            cx.ret("")
        });
    }
}

// ---------------------------------------------------------------------------
// Cache API
// ---------------------------------------------------------------------------

/// A stored response.
#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub url: String,
    pub type_: &'static str,
    pub redirected: bool,
}

/// One entry of a cache: the request's URL, method and headers (for
/// `Vary`) and the response.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    pub url: Url,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub response: CachedResponse,
}

/// One named cache (shared by every `Cache` object for it).
pub type CacheRef = Arc<Mutex<Vec<CacheEntry>>>;

/// The caches of one origin, in creation order.
#[derive(Default)]
pub struct CacheStorageData {
    caches: Vec<(String, CacheRef)>,
}

/// `caches` of one origin of one context.
pub type CacheStorageRef = Arc<Mutex<CacheStorageData>>;

/// A request argument as (url, method, headers).
type ReqKey = (Url, String, Vec<(String, String)>);

/// `RequestInfo` argument `i`.
fn req_arg(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    op: &str,
    iface: &str,
) -> Result<ReqKey, HostError> {
    if let Some(h) = cx.arg_host(i).filter(|h| h.kind == K_OBJ) {
        let found = w
            .with_obj(h.id, |o| match o {
                Obj::Request(r) => Some((r.url.clone(), r.method.clone(), r.headers)),
                _ => None,
            })
            .flatten();
        if let Some((u, m, hid)) = found {
            let hs = headers_of(w, hid).map(|h| h.list).unwrap_or_default();
            return Ok((u, m, hs));
        }
    }
    let s = cx.arg_string(i)?;
    let u = w.resolve_url(&s).ok_or_else(|| {
        HostError::type_error(format!(
            "Failed to execute '{op}' on '{iface}': Failed to parse URL from {s}"
        ))
    })?;
    Ok((u, "GET".into(), Vec::new()))
}

#[derive(Default, Clone, Copy)]
struct QueryOpts {
    ignore_search: bool,
    ignore_method: bool,
    ignore_vary: bool,
}

fn query_opts(
    cx: &mut CallCtx<'_, '_, '_>,
    i: usize,
) -> Result<(QueryOpts, Option<String>), HostError> {
    let d = Dict::arg(cx, i);
    let o = QueryOpts {
        ignore_search: d.bool(cx, "ignoreSearch")?,
        ignore_method: d.bool(cx, "ignoreMethod")?,
        ignore_vary: d.bool(cx, "ignoreVary")?,
    };
    let name = d.string(cx, "cacheName")?;
    d.release(cx);
    Ok((o, name))
}

fn strip(u: &Url, search: bool) -> String {
    let mut u = u.clone();
    u.set_fragment(None);
    if search {
        u.set_query(None);
    }
    u.to_string()
}

/// Whether `e` matches request `q` (Service Workers "Request Matches
/// Cached Item").
fn matches(q: &ReqKey, e: &CacheEntry, o: QueryOpts) -> bool {
    if !o.ignore_method && q.1 != "GET" {
        return false;
    }
    if strip(&q.0, o.ignore_search) != strip(&e.url, o.ignore_search) {
        return false;
    }
    if o.ignore_vary {
        return true;
    }
    let vary = e
        .response
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("vary"))
        .flat_map(|(_, v)| v.split(',').map(|s| s.trim().to_ascii_lowercase()))
        .collect::<Vec<_>>();
    let get = |hs: &[(String, String)], n: &str| {
        hs.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.clone())
    };
    vary.iter()
        .all(|n| n != "*" && get(&q.2, n) == get(&e.headers, n))
}

fn response_arg(w: &Window, c: &CachedResponse) -> Arg {
    let h = new_headers(
        w,
        HeadersData {
            list: c.headers.clone(),
            immutable: true,
        },
    );
    let id = w.new_obj(Obj::Response(Box::new(ResponseData {
        status: c.status,
        status_text: c.status_text.clone(),
        headers: h,
        body: Some(c.body.clone()),
        used: false,
        url: c.url.clone(),
        redirected: c.redirected,
        type_: c.type_,
    })));
    w.obj_arg(id)
}

fn request_arg(w: &Window, e: &CacheEntry) -> Arg {
    let h = new_headers(
        w,
        HeadersData {
            list: e.headers.clone(),
            immutable: true,
        },
    );
    let signal = new_signal(w);
    let id = w.new_obj(Obj::Request(Box::new(RequestData {
        method: e.method.clone(),
        url: e.url.clone(),
        headers: h,
        body: None,
        used: false,
        signal,
        mode: "cors",
        credentials: "same-origin".into(),
        cache: "default".into(),
        redirect: "follow".into(),
        destination: "",
    })));
    w.obj_arg(id)
}

fn storage(w: &Window) -> Result<CacheStorageRef, HostError> {
    Ok(host_of(w)?.caches(&w.origin_string()))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn this_cache(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<CacheRef, HostError> {
    let id = this_obj(cx)?;
    sw_obj(w, id, |o| match o {
        SwObj::Cache(c) => Some(c.clone()),
        _ => None,
    })
    .ok_or_else(illegal)
}

fn this_storage(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<CacheStorageRef, HostError> {
    let id = this_obj(cx)?;
    sw_obj(w, id, |o| matches!(o, SwObj::CacheStorage).then_some(())).ok_or_else(illegal)?;
    storage(w)
}

/// A promise method: argument errors reject (Chrome) instead of throwing.
fn promise_method(
    reg: &mut Registry,
    rt: &Runtime,
    iface: &'static str,
    name: &'static str,
    f: impl Fn(&mut CallCtx<'_, '_, '_>, &Rc<Window>, lb_js::Resolver) -> Result<(), HostError>
    + 'static,
) {
    method(reg, rt, iface, name, move |cx, w| {
        let r = cx.ret_new_promise()?;
        match f(cx, w, r) {
            Ok(()) => Ok(()),
            Err(HostError::Thrown) => Err(HostError::Thrown),
            Err(e) => cx.reject_promise(r, e),
        }
    });
}

/// `add` / `addAll`: fetch every request, then store all or reject.
fn cache_add(
    w: &Rc<Window>,
    cache: CacheRef,
    reqs: Vec<ReqKey>,
    op: &'static str,
    r: lb_js::Resolver,
) {
    let w2 = w.clone();
    w.in_flight.set(w.in_flight.get() + 1);
    w.net.spawn(Box::pin(async move {
        let mut results = Vec::new();
        for (u, m, hs) in &reqs {
            let mut req = NetRequest::get(u.clone(), Destination::Empty, Some(w2.url()));
            req.method = m.clone();
            req.headers = hs.clone();
            req.mode = "cors";
            results.push(w2.net_fetch(req).await);
        }
        w2.in_flight.set(w2.in_flight.get().saturating_sub(1));
        if w2.is_closed() {
            return;
        }
        w2.queue_task(
            TaskSource::Networking,
            Box::new(move |js, w| {
                let mut entries = Vec::new();
                for ((u, m, hs), res) in reqs.into_iter().zip(results) {
                    match res {
                        Ok(resp) if (200..300).contains(&resp.status) => {
                            let cors = resp.url.origin() != w.url().origin();
                            entries.push(CacheEntry {
                                url: u,
                                method: m,
                                headers: hs,
                                response: CachedResponse {
                                    status: resp.status,
                                    status_text: resp.status_text.clone(),
                                    headers: super::net::visible_headers(&resp, cors).list,
                                    body: Vec::from(resp.body),
                                    url: resp.url.to_string(),
                                    type_: if cors { "cors" } else { "basic" },
                                    redirected: resp.redirected,
                                },
                            });
                        }
                        _ => {
                            js.reject(
                                r,
                                HostError::type_error(format!(
                                    "Failed to execute '{op}' on 'Cache': Request failed"
                                )),
                            );
                            return;
                        }
                    }
                }
                let mut c = lock(&cache);
                for e in entries {
                    c.retain(|x| {
                        !matches(
                            &(e.url.clone(), "GET".into(), e.headers.clone()),
                            x,
                            QueryOpts::default(),
                        )
                    });
                    c.push(e);
                }
                drop(c);
                js.resolve(r, Arg::Undefined);
            }),
        );
    }));
}

fn register_cache(reg: &mut Registry, rt: &Runtime) {
    for iface in ["Window", "WorkerGlobalScope"] {
        getter(reg, rt, iface, "caches", |cx, w| {
            let id = w.cached_obj((0, 0, "caches", String::new()), || {
                Obj::Sw(Box::new(SwObj::CacheStorage))
            });
            cx.ret(w.obj_arg(id))
        });
    }
    // CacheStorage
    promise_method(reg, rt, "CacheStorage", "open", |cx, w, r| {
        let st = this_storage(cx, w)?;
        super::need_args(cx, 1, "open", "CacheStorage")?;
        let name = cx.arg_string(0)?;
        let cache = {
            let mut s = lock(&st);
            match s.caches.iter().find(|(n, _)| *n == name) {
                Some((_, c)) => c.clone(),
                None => {
                    let c: CacheRef = Arc::default();
                    s.caches.push((name, c.clone()));
                    c
                }
            }
        };
        let id = w.new_obj(Obj::Sw(Box::new(SwObj::Cache(cache))));
        let a = w.obj_arg(id);
        cx.resolve_promise(r, a)
    });
    promise_method(reg, rt, "CacheStorage", "has", |cx, w, r| {
        let st = this_storage(cx, w)?;
        super::need_args(cx, 1, "has", "CacheStorage")?;
        let name = cx.arg_string(0)?;
        let has = lock(&st).caches.iter().any(|(n, _)| *n == name);
        cx.resolve_promise(r, Arg::Bool(has))
    });
    promise_method(reg, rt, "CacheStorage", "delete", |cx, w, r| {
        let st = this_storage(cx, w)?;
        super::need_args(cx, 1, "delete", "CacheStorage")?;
        let name = cx.arg_string(0)?;
        let mut s = lock(&st);
        let before = s.caches.len();
        s.caches.retain(|(n, _)| *n != name);
        let gone = s.caches.len() != before;
        drop(s);
        cx.resolve_promise(r, Arg::Bool(gone))
    });
    promise_method(reg, rt, "CacheStorage", "keys", |cx, w, r| {
        let st = this_storage(cx, w)?;
        let names: Vec<Arg> = lock(&st)
            .caches
            .iter()
            .map(|(n, _)| Arg::String(n.clone()))
            .collect();
        cx.resolve_promise(r, Arg::Array(names))
    });
    promise_method(reg, rt, "CacheStorage", "match", |cx, w, r| {
        let st = this_storage(cx, w)?;
        super::need_args(cx, 1, "match", "CacheStorage")?;
        let q = req_arg(cx, w, 0, "match", "CacheStorage")?;
        let (o, name) = query_opts(cx, 1)?;
        let caches: Vec<CacheRef> = lock(&st)
            .caches
            .iter()
            .filter(|(n, _)| name.as_ref().is_none_or(|x| x == n))
            .map(|(_, c)| c.clone())
            .collect();
        let found = caches.iter().find_map(|c| {
            lock(c)
                .iter()
                .find(|e| matches(&q, e, o))
                .map(|e| e.response.clone())
        });
        match found {
            Some(resp) => {
                let a = response_arg(w, &resp);
                cx.resolve_promise(r, a)
            }
            None => cx.resolve_promise(r, Arg::Undefined),
        }
    });

    // Cache
    promise_method(reg, rt, "Cache", "match", |cx, w, r| {
        let c = this_cache(cx, w)?;
        super::need_args(cx, 1, "match", "Cache")?;
        let q = req_arg(cx, w, 0, "match", "Cache")?;
        let (o, _) = query_opts(cx, 1)?;
        let found = lock(&c)
            .iter()
            .find(|e| matches(&q, e, o))
            .map(|e| e.response.clone());
        match found {
            Some(resp) => {
                let a = response_arg(w, &resp);
                cx.resolve_promise(r, a)
            }
            None => cx.resolve_promise(r, Arg::Undefined),
        }
    });
    promise_method(reg, rt, "Cache", "matchAll", |cx, w, r| {
        let c = this_cache(cx, w)?;
        let q = if cx.is_empty() || matches!(cx.arg_kind(0), ValueKind::Undefined) {
            None
        } else {
            Some(req_arg(cx, w, 0, "matchAll", "Cache")?)
        };
        let (o, _) = query_opts(cx, 1)?;
        let found: Vec<CachedResponse> = lock(&c)
            .iter()
            .filter(|e| q.as_ref().is_none_or(|q| matches(q, e, o)))
            .map(|e| e.response.clone())
            .collect();
        let arr: Vec<Arg> = found.iter().map(|x| response_arg(w, x)).collect();
        cx.resolve_promise(r, Arg::Array(arr))
    });
    promise_method(reg, rt, "Cache", "keys", |cx, w, r| {
        let c = this_cache(cx, w)?;
        let q = if cx.is_empty() || matches!(cx.arg_kind(0), ValueKind::Undefined) {
            None
        } else {
            Some(req_arg(cx, w, 0, "keys", "Cache")?)
        };
        let (o, _) = query_opts(cx, 1)?;
        let found: Vec<CacheEntry> = lock(&c)
            .iter()
            .filter(|e| q.as_ref().is_none_or(|q| matches(q, e, o)))
            .cloned()
            .collect();
        let arr: Vec<Arg> = found.iter().map(|e| request_arg(w, e)).collect();
        cx.resolve_promise(r, Arg::Array(arr))
    });
    promise_method(reg, rt, "Cache", "delete", |cx, w, r| {
        let c = this_cache(cx, w)?;
        super::need_args(cx, 1, "delete", "Cache")?;
        let q = req_arg(cx, w, 0, "delete", "Cache")?;
        let (o, _) = query_opts(cx, 1)?;
        let mut v = lock(&c);
        let before = v.len();
        v.retain(|e| !matches(&q, e, o));
        let gone = v.len() != before;
        drop(v);
        cx.resolve_promise(r, Arg::Bool(gone))
    });
    promise_method(reg, rt, "Cache", "put", |cx, w, r| {
        let c = this_cache(cx, w)?;
        super::need_args(cx, 2, "put", "Cache")?;
        let (url, m, hs) = req_arg(cx, w, 0, "put", "Cache")?;
        let fail =
            |m: String| HostError::type_error(format!("Failed to execute 'put' on 'Cache': {m}"));
        if !matches!(url.scheme(), "http" | "https") {
            return Err(fail(format!(
                "Request scheme '{}' is unsupported",
                url.scheme()
            )));
        }
        if m != "GET" {
            return Err(fail(format!("Request method '{m}' is unsupported")));
        }
        let rid = cx
            .arg_host(1)
            .filter(|h| h.kind == K_OBJ)
            .map(|h| h.id)
            .filter(|&id| {
                w.with_obj(id, |o| matches!(o, Obj::Response(_)))
                    .unwrap_or(false)
            })
            .ok_or_else(|| {
                HostError::type_error(
                    "Failed to execute 'put' on 'Cache': parameter 2 is not of type 'Response'.",
                )
            })?;
        let resp = w
            .with_obj(rid, |o| match o {
                Obj::Response(x) => Some((
                    x.status,
                    x.status_text.clone(),
                    x.headers,
                    x.body.clone(),
                    x.used,
                    x.url.clone(),
                    x.type_,
                    x.redirected,
                )),
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal)?;
        let (status, status_text, hid, body, used, rurl, type_, redirected) = resp;
        if status == 206 {
            return Err(fail(
                "Partial response (status code 206) is unsupported".into(),
            ));
        }
        let headers = headers_of(w, hid).map(|h| h.list).unwrap_or_default();
        if headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("vary") && v.split(',').any(|x| x.trim() == "*"))
        {
            return Err(fail("Vary header contains *".into()));
        }
        if used {
            return Err(fail("Response body is already used".into()));
        }
        w.with_obj(rid, |o| {
            if let Obj::Response(x) = o {
                x.used = x.body.is_some();
            }
        });
        let e = CacheEntry {
            url: url.clone(),
            method: m,
            headers: hs,
            response: CachedResponse {
                status,
                status_text,
                headers,
                body: body.unwrap_or_default(),
                url: rurl,
                type_,
                redirected,
            },
        };
        let mut v = lock(&c);
        let key = (url, "GET".to_owned(), e.headers.clone());
        v.retain(|x| !matches(&key, x, QueryOpts::default()));
        v.push(e);
        drop(v);
        cx.resolve_promise(r, Arg::Undefined)
    });
    promise_method(reg, rt, "Cache", "add", |cx, w, r| {
        let c = this_cache(cx, w)?;
        super::need_args(cx, 1, "add", "Cache")?;
        let q = req_arg(cx, w, 0, "add", "Cache")?;
        cache_add(w, c, vec![q], "add", r);
        Ok(())
    });
    promise_method(reg, rt, "Cache", "addAll", |cx, w, r| {
        let c = this_cache(cx, w)?;
        super::need_args(cx, 1, "addAll", "Cache")?;
        let list = cx.arg_handle(0);
        let len = cx.get_property(list, "length")?;
        let n = match cx.handle_value(len) {
            lb_js::JsValue::Number(n) => n as usize,
            _ => 0,
        };
        cx.release(len);
        let mut reqs = Vec::new();
        for i in 0..n.min(10_000) {
            let item = cx.get_property(list, &i.to_string())?;
            let host = cx.handle_host(item).filter(|h| h.kind == K_OBJ);
            let found = host.and_then(|h| {
                w.with_obj(h.id, |o| match o {
                    Obj::Request(x) => Some((x.url.clone(), x.method.clone(), x.headers)),
                    _ => None,
                })
                .flatten()
            });
            let key = match found {
                Some((u, m, hid)) => (u, m, headers_of(w, hid).map(|h| h.list).unwrap_or_default()),
                None => {
                    let s = cx.handle_string(item)?;
                    let u = w.resolve_url(&s).ok_or_else(|| {
                        HostError::type_error(format!(
                            "Failed to execute 'addAll' on 'Cache': Failed to parse URL from {s}"
                        ))
                    })?;
                    (u, "GET".to_owned(), Vec::new())
                }
            };
            cx.release(item);
            reqs.push(key);
        }
        cx.release(list);
        cache_add(w, c, reqs, "addAll", r);
        Ok(())
    });
}

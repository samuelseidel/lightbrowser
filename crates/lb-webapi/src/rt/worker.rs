//! Workers, message ports and structured clone (ADR 0021).
//!
//! - **Threads.** A dedicated or shared worker runs on its own thread with
//!   its own engine (V8 isolate), started by the page layer through the
//!   window's [`WorkerHost`]. Its global scope is a [`Window`] without a
//!   document (`WindowKind` is told apart by [`Window::is_worker`]) whose
//!   realm's global is `DedicatedWorkerGlobalScope` or
//!   `SharedWorkerGlobalScope` (`Bindings::define_worker`), so timers,
//!   `fetch`, XHR, events, `URL`, encoders and the rest are the window's
//!   implementations.
//! - **Inboxes.** Windows on other threads talk through [`Inbox`]es: a
//!   channel of [`RemoteTask`]s pumped into the receiving window's task
//!   queue (posted-message task source). A window gets one when first
//!   needed, so pages without workers or ports pay nothing.
//! - **Messages** are V8 structured clones ([`lb_js::Serialized`]) with
//!   their transferred ports and cloned blobs beside them ([`Payload`]).
//! - **Ports** ([`PortCore`]) are shared by the two entangled ends; a port
//!   owned by a window delivers to that window's inbox once started
//!   (`start()` or setting `onmessage`), and queues until then or while it
//!   is in transit.
//! - Every persona value a worker sees (`WorkerNavigator`, UA client
//!   hints, time zone, locale) comes from the creating window's
//!   `Arc<Persona>` / `Arc<PersonaProfile>`, so it equals the window's.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak as SyncWeak};

use lb_js::{
    Arg, CallCtx, Handle, HostError, HostRef, InterfaceId, JsError, Serialized, ValueKind,
};
use lb_persona::{Persona, PersonaProfile};
use url::Url;

use super::events::{EventExtra, fire_event_with};
use super::misc::BlobData;
use super::{
    Js, K_OBJ, Obj, Runtime, TaskSource, WINDOW_REF, Window, ctor, getter, illegal, method,
    need_args, this_obj,
};
use crate::host::{Destination, NetRequest, NetResponse};
use crate::registry::{ImplKind, Registry, host_fn};

/// A task sent to a window from any thread.
pub type RemoteTask = Box<dyn FnOnce(&mut dyn Js, &Rc<Window>) + Send>;

/// Sends [`RemoteTask`]s to one window (worker scope or page window).
#[derive(Clone)]
pub struct Inbox(tokio::sync::mpsc::UnboundedSender<RemoteTask>);

/// The receiving end of an [`Inbox`], pumped by [`Window::attach_inbox`].
pub struct InboxRx(tokio::sync::mpsc::UnboundedReceiver<RemoteTask>);

/// A new inbox and its receiving end.
pub fn inbox() -> (Inbox, InboxRx) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (Inbox(tx), InboxRx(rx))
}

impl Inbox {
    /// Queue `t`; false when the window is gone.
    pub fn send(&self, t: RemoteTask) -> bool {
        self.0.send(t).is_ok()
    }

    /// Whether the receiving window is gone.
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

/// Dedicated or shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerKind {
    Dedicated,
    Shared,
    /// A service worker version (ADR 0022).
    Service,
}

impl WorkerKind {
    /// The global interface of the scope.
    pub fn global(self) -> &'static str {
        match self {
            WorkerKind::Dedicated => "DedicatedWorkerGlobalScope",
            WorkerKind::Shared => "SharedWorkerGlobalScope",
            WorkerKind::Service => "ServiceWorkerGlobalScope",
        }
    }
}

/// Termination state of one worker, shared by its creator and its thread.
pub struct WorkerControl {
    terminated: AtomicBool,
    /// `close()` ran in the worker: no further task runs.
    closing: AtomicBool,
    isolate: Mutex<Option<lb_js::TerminateHandle>>,
    inbox: Inbox,
}

impl WorkerControl {
    pub fn new(inbox: Inbox) -> Arc<WorkerControl> {
        Arc::new(WorkerControl {
            terminated: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            isolate: Mutex::new(None),
            inbox,
        })
    }

    /// The worker's engine, for [`WorkerControl::terminate`].
    pub fn set_isolate(&self, h: lb_js::TerminateHandle) {
        let mut slot = self.isolate.lock().unwrap_or_else(|e| e.into_inner());
        if self.terminated.load(Ordering::SeqCst) {
            h.terminate();
        }
        *slot = Some(h);
    }

    /// Stop the worker: running script is terminated, queued tasks are
    /// dropped and the thread ends.
    pub fn terminate(&self) {
        if self.terminated.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(h) = self
            .isolate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            h.terminate();
        }
        // Wake the worker's event loop so it sees the flag.
        self.inbox.send(Box::new(|_, _| {}));
    }

    /// `close()` in the worker: no new tasks run; the running one ends
    /// (and what it posts is still delivered).
    pub fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }

    /// Terminated by its creator (or its context).
    pub fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::SeqCst)
    }

    /// Terminated or closed: the worker's event loop stops.
    pub fn is_done(&self) -> bool {
        self.is_terminated() || self.closing.load(Ordering::SeqCst)
    }

    /// The worker's inbox.
    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }
}

/// What the page layer needs to start a worker thread. `Send`.
pub struct WorkerStart {
    pub kind: WorkerKind,
    /// Script URL (a `blob:` URL stays as is; its source is in `source`).
    pub url: Url,
    /// The script when the creator already has it (`blob:`, `data:`).
    pub source: Option<String>,
    pub module: bool,
    pub name: String,
    /// `self.origin`: the creator's for `blob:` URLs, `"null"` for `data:`.
    pub origin: String,
    /// The creating document (referrer, initiator).
    pub creator: Url,
    pub persona: Arc<Persona>,
    pub profile: Option<Arc<PersonaProfile>>,
    pub virtual_time: bool,
    /// The worker scope's inbox and its receiving end.
    pub inbox: Inbox,
    pub inbox_rx: InboxRx,
    pub control: Arc<WorkerControl>,
    /// Dedicated workers: the creator's inbox and its `Worker` object.
    pub parent: Option<(Inbox, u32)>,
    /// Service workers: the version and what to report after the script
    /// ran.
    pub service: Option<super::sw::ServiceStart>,
}

/// Worker threads, provided by the page layer.
pub trait WorkerHost {
    /// Start a dedicated worker thread.
    fn start(&self, start: WorkerStart);
    /// Connect to the shared worker of `start` (same context, URL and
    /// name), starting it if needed; `connect` is its first or next task.
    fn connect_shared(&self, start: WorkerStart, connect: RemoteTask);
    /// Fetch synchronously (`importScripts` in a worker thread).
    fn fetch_sync(&self, req: NetRequest) -> Result<NetResponse, String>;
}

/// A worker scope's own settings.
pub struct ScopeInit {
    pub kind: WorkerKind,
    pub name: String,
    pub module: bool,
    pub origin: String,
    pub control: Arc<WorkerControl>,
    pub parent: Option<(Inbox, u32)>,
}

/// Per-window worker state (see the module docs).
#[derive(Default)]
pub(crate) struct WorkerSide {
    inbox: RefCell<Option<Inbox>>,
    /// Workers this window started (terminated when it closes).
    children: RefCell<Vec<Arc<WorkerControl>>>,
    scope: RefCell<Option<Rc<ScopeInit>>>,
    host: RefCell<Option<Rc<dyn WorkerHost>>>,
    /// Blob URLs this window made (revoked when it closes).
    blob_urls: RefCell<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Blob URLs
// ---------------------------------------------------------------------------

/// Blob URLs of the process, by URL. Not fingerprint data: the URLs are
/// random, scoped by origin on lookup, and revoked with their document.
fn blob_store() -> &'static Mutex<HashMap<String, (Vec<u8>, String)>> {
    static STORE: std::sync::OnceLock<Mutex<HashMap<String, (Vec<u8>, String)>>> =
        std::sync::OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A fresh `blob:` URL for `bytes` created by `w`.
pub(crate) fn create_blob_url(w: &Window, bytes: Vec<u8>, type_: String) -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    let rnd = |salt: u64| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(N.fetch_add(1, Ordering::Relaxed) ^ salt);
        h.finish()
    };
    let (a, b) = (rnd(1), rnd(2));
    let uuid = format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        0x8000 | ((b >> 48) & 0x3fff),
        b & 0xffff_ffff_ffff
    );
    let origin = w.origin_string();
    let url = format!("blob:{origin}/{uuid}");
    blob_store()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(url.clone(), (bytes, type_));
    w.wk.blob_urls.borrow_mut().push(url.clone());
    url
}

pub(crate) fn revoke_blob_url(url: &str) {
    blob_store()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(url);
}

/// The contents of a live blob URL (fragment ignored).
pub fn resolve_blob_url(url: &str) -> Option<(Vec<u8>, String)> {
    let key = url.split('#').next().unwrap_or(url);
    blob_store()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
        .cloned()
}

// ---------------------------------------------------------------------------
// Payloads and ports
// ---------------------------------------------------------------------------

/// What a host object in a clone stream stands for.
enum HostEntry {
    /// `ports[i]`.
    Port(usize),
    Blob(Vec<u8>, String),
    /// A `CryptoKey` (keys are immutable; both sides share it).
    Key(std::sync::Arc<super::crypto::CryptoKey>),
}

/// One posted message: the clone and what travels beside it.
pub struct Payload {
    data: Serialized,
    entries: Vec<HostEntry>,
    ports: Vec<Arc<PortCore>>,
}

/// The shared state of one end of a message channel.
pub struct PortCore {
    st: Mutex<PortSt>,
}

struct PortSt {
    peer: SyncWeak<PortCore>,
    /// Where messages for this end go: a window's inbox and the port
    /// object there. `None` while in transit.
    owner: Option<(Inbox, u32)>,
    started: bool,
    closed: bool,
    queue: VecDeque<Payload>,
}

impl PortCore {
    fn new() -> Arc<PortCore> {
        Arc::new(PortCore {
            st: Mutex::new(PortSt {
                peer: SyncWeak::new(),
                owner: None,
                started: false,
                closed: false,
                queue: VecDeque::new(),
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PortSt> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Two entangled ports.
    fn pair() -> (Arc<PortCore>, Arc<PortCore>) {
        let (a, b) = (PortCore::new(), PortCore::new());
        a.lock().peer = Arc::downgrade(&b);
        b.lock().peer = Arc::downgrade(&a);
        (a, b)
    }

    fn post(&self, p: Payload) {
        let peer = {
            let st = self.lock();
            if st.closed {
                return;
            }
            st.peer.upgrade()
        };
        if let Some(peer) = peer {
            peer.enqueue(p);
        }
    }

    fn enqueue(&self, p: Payload) {
        let mut st = self.lock();
        if st.closed {
            return;
        }
        st.queue.push_back(p);
        Self::flush(&mut st);
    }

    fn flush(st: &mut PortSt) {
        if !st.started {
            return;
        }
        let Some((inbox, id)) = st.owner.clone() else {
            return;
        };
        while let Some(p) = st.queue.pop_front() {
            inbox.send(Box::new(move |js, w| {
                deliver(js, w, HostRef::new(K_OBJ, id), p, String::new(), None)
            }));
        }
    }

    fn start(&self) {
        let mut st = self.lock();
        st.started = true;
        Self::flush(&mut st);
    }

    fn set_owner(&self, owner: Option<(Inbox, u32)>) {
        let mut st = self.lock();
        st.owner = owner;
        st.started = false;
    }

    fn close(&self) {
        let peer = {
            let mut st = self.lock();
            st.closed = true;
            st.queue.clear();
            std::mem::take(&mut st.peer).upgrade()
        };
        if let Some(p) = peer {
            p.lock().peer = SyncWeak::new();
        }
    }
}

/// A port object of a window.
pub(crate) struct PortObj {
    core: Arc<PortCore>,
}

/// A `Worker` object.
pub(crate) struct WorkerObj {
    control: Arc<WorkerControl>,
}

/// Host objects of this module.
pub(crate) enum WorkerHostObj {
    Port(PortObj),
    Channel { port1: u32, port2: u32 },
    Worker(WorkerObj),
    SharedWorker { port: u32 },
    Location,
    Navigator,
}

impl WorkerHostObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            WorkerHostObj::Port(_) => "MessagePort",
            WorkerHostObj::Channel { .. } => "MessageChannel",
            WorkerHostObj::Worker(_) => "Worker",
            WorkerHostObj::SharedWorker { .. } => "SharedWorker",
            WorkerHostObj::Location => "WorkerLocation",
            WorkerHostObj::Navigator => "WorkerNavigator",
        }
    }
}

fn port_core(w: &Window, id: u32) -> Option<Arc<PortCore>> {
    w.with_obj(id, |o| match o {
        Obj::Worker(b) => match &**b {
            WorkerHostObj::Port(p) => Some(p.core.clone()),
            _ => None,
        },
        _ => None,
    })
    .flatten()
}

/// A new port object of `w` for `core`, owned by `w`.
fn adopt_port(w: &Rc<Window>, core: Arc<PortCore>) -> u32 {
    let id = w.new_obj(Obj::Worker(Box::new(WorkerHostObj::Port(PortObj {
        core: core.clone(),
    }))));
    core.set_owner(Some((w.inbox(), id)));
    id
}

// ---------------------------------------------------------------------------
// Serialize / deliver
// ---------------------------------------------------------------------------

/// The `postMessage(message, transfer)` / `(message, {transfer})`
/// arguments as a payload; ports in the transfer list are taken from `w`
/// (the sending objects stop working). `op` and `iface` name the call in
/// error messages.
pub(crate) fn serialize_args(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    op: &str,
    iface: &str,
) -> Result<Payload, HostError> {
    need_args(cx, 1, op, iface)?;
    let fail = |m: &str| format!("Failed to execute '{op}' on '{iface}': {m}");
    let value = cx.arg_handle(0);
    // The transfer list: a sequence, or `{transfer}`.
    let mut list: Vec<Handle> = Vec::new();
    if cx.len() > 1 && !matches!(cx.arg_kind(1), ValueKind::Undefined | ValueKind::Null) {
        if !matches!(cx.arg_kind(1), ValueKind::Object | ValueKind::Function) {
            cx.release(value);
            return Err(HostError::type_error(fail(
                "The provided value cannot be converted to a sequence.",
            )));
        }
        let arg = cx.arg_handle(1);
        let seq = match cx.get_property(arg, "length")? {
            l if cx.handle_kind(l) == ValueKind::Number => {
                cx.release(l);
                arg
            }
            l => {
                cx.release(l);
                let t = cx.get_property(arg, "transfer")?;
                cx.release(arg);
                t
            }
        };
        if !matches!(cx.handle_kind(seq), ValueKind::Undefined) {
            let l = cx.get_property(seq, "length")?;
            let len = match cx.handle_value(l) {
                lb_js::JsValue::Number(n) => n as usize,
                _ => 0,
            };
            cx.release(l);
            for i in 0..len {
                let item = cx.get_property(seq, &i.to_string())?;
                list.push(item);
            }
        }
        cx.release(seq);
    }
    let mut buffers = Vec::new();
    let mut port_ids: Vec<u32> = Vec::new();
    for h in &list {
        if cx.is_array_buffer(*h) {
            buffers.push(*h);
            continue;
        }
        let port = cx
            .handle_host(*h)
            .filter(|r| r.kind == K_OBJ)
            .and_then(|r| port_core(w, r.id).map(|_| r.id));
        match port {
            Some(id) if port_ids.contains(&id) => {
                return Err(w.rt.err(
                    "DataCloneError",
                    fail(&format!(
                        "Message port at index {} is a duplicate of an earlier port.",
                        port_ids.len()
                    )),
                ));
            }
            Some(id) => port_ids.push(id),
            None if matches!(cx.handle_kind(*h), ValueKind::Object | ValueKind::Function) => {
                return Err(HostError::type_error(fail(
                    "Value at index 0 does not have a transferable type.",
                )));
            }
            None => {
                return Err(HostError::type_error(fail(
                    "Failed to convert value to 'object'.",
                )));
            }
        }
    }
    let ports: Vec<Arc<PortCore>> = port_ids.iter().filter_map(|id| port_core(w, *id)).collect();
    let mut entries: Vec<HostEntry> = Vec::new();
    let result = {
        let mut writer = |h: HostRef, class: &str| -> Result<u32, String> {
            if h.kind == K_OBJ {
                if let Some(core) = port_core(w, h.id) {
                    return match ports.iter().position(|p| Arc::ptr_eq(p, &core)) {
                        Some(i) => {
                            entries.push(HostEntry::Port(i));
                            Ok((entries.len() - 1) as u32)
                        }
                        None => Err(
                            "A MessagePort could not be cloned because it was not transferred."
                                .into(),
                        ),
                    };
                }
                let blob = w
                    .with_obj(h.id, |o| match o {
                        Obj::Blob(b) => Some((b.bytes.clone(), b.type_.clone())),
                        _ => None,
                    })
                    .flatten();
                if let Some((bytes, ty)) = blob {
                    entries.push(HostEntry::Blob(bytes, ty));
                    return Ok((entries.len() - 1) as u32);
                }
                if let Some(k) = super::crypto::clone_key(w, h) {
                    entries.push(HostEntry::Key(k));
                    return Ok((entries.len() - 1) as u32);
                }
            }
            Err(format!("{class} object could not be cloned."))
        };
        cx.serialize(value, &buffers, &mut writer)
    };
    cx.release(value);
    for h in list {
        cx.release(h);
    }
    let data = result.map_err(|m| match m {
        Some(m) => w.rt.err("DataCloneError", fail(&m)),
        None => HostError::Thrown,
    })?;
    // The sending side's port objects stop working.
    for id in &port_ids {
        if let Some(core) = port_core(w, *id) {
            core.set_owner(None);
        }
        let dead = PortCore::new();
        dead.lock().closed = true;
        w.with_obj(*id, |o| {
            if let Obj::Worker(b) = o
                && let WorkerHostObj::Port(p) = &mut **b
            {
                p.core = dead;
            }
        });
    }
    Ok(Payload {
        data,
        entries,
        ports,
    })
}

/// Rebuild a payload in `w`: the data (`Err` when it cannot be rebuilt
/// here) and the frozen `ports` array of the adopted ports.
pub(crate) fn unpack(
    js: &mut dyn Js,
    w: &Rc<Window>,
    p: Payload,
) -> (Result<Handle, ()>, Option<Handle>) {
    let port_ids: Vec<u32> = p.ports.into_iter().map(|c| adopt_port(w, c)).collect();
    let hosts: Vec<(InterfaceId, HostRef)> = p
        .entries
        .into_iter()
        .map(|e| match e {
            HostEntry::Port(i) => (
                w.rt.iface("MessagePort"),
                HostRef::new(K_OBJ, port_ids.get(i).copied().unwrap_or(0)),
            ),
            HostEntry::Blob(bytes, type_) => {
                let id = w.new_obj(Obj::Blob(Box::new(BlobData { bytes, type_ })));
                (w.rt.iface("Blob"), HostRef::new(K_OBJ, id))
            }
            HostEntry::Key(k) => {
                let id = w.new_obj(Obj::Crypto(Box::new(super::crypto::CryptoObj::Key(k))));
                (w.rt.iface("CryptoKey"), HostRef::new(K_OBJ, id))
            }
        })
        .collect();
    let ports = ports_array(js, w, &port_ids);
    let data = js.deserialize(&p.data, &hosts).map_err(|_| ());
    (data, ports)
}

/// Fire `message` (or `messageerror` when the data cannot be rebuilt here)
/// at `target` of `w` with a payload from another window or thread.
pub(crate) fn deliver(
    js: &mut dyn Js,
    w: &Rc<Window>,
    target: HostRef,
    p: Payload,
    origin: String,
    source: Option<Arg>,
) {
    let (data, ports) = unpack(js, w, p);
    let source = source.and_then(|s| js.make(s));
    let (type_, data) = match data {
        Ok(d) => ("message", Some(d)),
        Err(()) => ("messageerror", None),
    };
    fire_event_with(
        js,
        w,
        target,
        "MessageEvent",
        type_,
        false,
        false,
        EventExtra::Message {
            data,
            origin,
            source,
            ports,
        },
    );
}

/// `MessageEvent.ports`: a frozen array of the port objects.
fn ports_array(js: &mut dyn Js, w: &Window, ids: &[u32]) -> Option<Handle> {
    let arr = js.make(Arg::Array(ids.iter().map(|id| w.obj_arg(*id)).collect()))?;
    let freeze = w.helpers.borrow().freeze;
    if let Some(f) = freeze {
        let _ = js.call(f, Arg::Undefined, &[Arg::Handle(arr)]);
    }
    Some(arr)
}

/// A frozen empty `ports` array (events made by script).
pub(crate) fn empty_ports(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Option<Handle> {
    ports_array(cx, w, &[])
}

// ---------------------------------------------------------------------------
// Window side
// ---------------------------------------------------------------------------

impl Window {
    /// This window's inbox (made and pumped on first use).
    pub fn inbox(self: &Rc<Self>) -> Inbox {
        if let Some(i) = self.wk.inbox.borrow().clone() {
            return i;
        }
        let (tx, rx) = inbox();
        self.attach_inbox(tx.clone(), rx);
        tx
    }

    /// Make `tx`/`rx` this window's inbox: tasks sent to it are queued on
    /// the posted-message task source, in order.
    pub fn attach_inbox(self: &Rc<Self>, tx: Inbox, rx: InboxRx) {
        *self.wk.inbox.borrow_mut() = Some(tx);
        let weak = Rc::downgrade(self);
        let mut rx = rx.0;
        self.net.spawn(Box::pin(async move {
            while let Some(t) = rx.recv().await {
                let Some(w) = weak.upgrade() else { break };
                if w.is_closed() {
                    break;
                }
                w.queue_task(TaskSource::PostedMessage, Box::new(move |js, w| t(js, w)));
            }
        }));
    }

    /// Let this window start workers (set by the page layer).
    pub fn set_worker_host(&self, host: Option<Rc<dyn WorkerHost>>) {
        *self.wk.host.borrow_mut() = host;
    }

    pub fn worker_host(&self) -> Option<Rc<dyn WorkerHost>> {
        self.wk.host.borrow().clone()
    }

    /// Whether this window is a worker global scope.
    pub fn is_worker(&self) -> bool {
        self.wk.scope.borrow().is_some()
    }

    /// The worker's control, for a worker scope.
    pub fn worker_control(&self) -> Option<Arc<WorkerControl>> {
        self.wk.scope.borrow().as_ref().map(|s| s.control.clone())
    }

    /// The serialization of this window's origin (`self.origin`).
    pub(crate) fn origin_string(&self) -> String {
        if let Some(s) = self.wk.scope.borrow().as_ref() {
            return s.origin.clone();
        }
        super::frames::origin(self)
    }

    /// Make this window a worker scope. Call before creating its realm.
    pub fn set_worker_scope(&self, scope: ScopeInit) {
        *self.wk.scope.borrow_mut() = Some(Rc::new(scope));
    }

    /// The worker scope's realm: the scope's global with persona-driven
    /// navigator state, ICU defaults and the console.
    pub fn create_worker_realm(
        self: &Rc<Self>,
        engine: &mut dyn lb_js::RealmOps,
        bindings: &crate::Bindings,
    ) -> Result<lb_js::RealmId, JsError> {
        let map = self.module_map.clone();
        let base = self.url();
        let loader: lb_js::ModuleLoader = Rc::new(move |spec: &str, referrer: &str| {
            let from = Url::parse(referrer).unwrap_or_else(|_| base.clone());
            let url = from
                .join(spec)
                .map_err(|_| format!("Failed to resolve module specifier \"{spec}\""))?;
            if let Some(src) = map.borrow().get(url.as_str()) {
                return Ok(lb_js::ModuleSource {
                    url: url.to_string(),
                    source: src.clone(),
                });
            }
            match url.scheme() {
                "blob" => resolve_blob_url(url.as_str()).map(|(b, _)| lb_js::ModuleSource {
                    url: url.to_string(),
                    source: String::from_utf8_lossy(&b).into_owned(),
                }),
                "data" => decode_data_url(&url).map(|source| lb_js::ModuleSource {
                    url: url.to_string(),
                    source,
                }),
                _ => None,
            }
            .ok_or_else(|| format!("Failed to fetch dynamically imported module: {url}"))
        });
        let realm = bindings.new_worker_realm(engine, Some(WINDOW_REF), Some(loader))?;
        self.set_realm(realm);
        self.rt.attach(realm, self.clone());
        if let Some(p) = &self.profile {
            engine.set_default_locale(&p.locale.language);
            engine.set_time_zone(&p.locale.timezone);
        }
        self.setup_helpers(engine, realm)?;
        if self.sw.scope_set() {
            self.setup_sw_scope(engine, realm)?;
        }
        super::navigator::setup_realm(self, engine, realm)?;
        super::window::install_console(engine, realm, &self.rt)?;
        Ok(realm)
    }

    /// Workers and blob URLs of this window end with it.
    pub(crate) fn close_workers(&self) {
        for c in self.wk.children.borrow_mut().drain(..) {
            c.terminate();
        }
        for u in self.wk.blob_urls.borrow_mut().drain(..) {
            revoke_blob_url(&u);
        }
        if let Some(s) = self.wk.scope.borrow().as_ref() {
            s.control.close();
        }
    }
}

/// Report an uncaught exception of a worker scope to its `Worker` object
/// (after the scope's own `error` event was not canceled): an `ErrorEvent`
/// there, logged to the creator's console unless canceled.
pub(crate) fn forward_error(w: &Window, message: &str, e: &JsError) {
    let scope = w.wk.scope.borrow().clone();
    let Some(scope) = scope else { return };
    let Some((parent, id)) = scope.parent.clone() else {
        return;
    };
    let message = message.to_owned();
    let filename = e.url.clone().unwrap_or_default();
    let (lineno, colno) = (e.line.unwrap_or(0), e.column.map_or(0, |c| c + 1));
    let text = e
        .stack
        .as_ref()
        .map_or_else(|| message.clone(), |s| format!("Uncaught {s}"));
    parent.send(Box::new(move |js, pw| {
        let extra = EventExtra::Error {
            message,
            filename: filename.clone(),
            lineno,
            colno,
            error: None,
        };
        let ok = fire_event_with(
            js,
            pw,
            HostRef::new(K_OBJ, id),
            "ErrorEvent",
            "error",
            false,
            true,
            extra,
        );
        if ok {
            pw.log(crate::host::ConsoleMessage {
                level: "error".into(),
                text,
                url: Some(filename),
                line: Some(lineno),
            });
        }
    }));
}

/// The worker's script could not be loaded: a plain `error` event at the
/// `Worker` object (Chrome).
pub fn load_failed_task(id: u32) -> RemoteTask {
    Box::new(move |js, w| {
        fire_event_with(
            js,
            w,
            HostRef::new(K_OBJ, id),
            "Event",
            "error",
            false,
            true,
            EventExtra::None,
        );
    })
}

/// The `connect` event of a shared worker for a new `SharedWorker`
/// object: `port` becomes the scope's end of the connection.
pub fn connect_task(port: Arc<PortCore>) -> RemoteTask {
    Box::new(move |js, w| {
        let id = adopt_port(w, port);
        let ports = ports_array(js, w, &[id]);
        let source = js.make(w.obj_arg(id));
        fire_event_with(
            js,
            w,
            WINDOW_REF,
            "MessageEvent",
            "connect",
            false,
            false,
            EventExtra::Message {
                data: None,
                origin: String::new(),
                source,
                ports,
            },
        );
    })
}

/// Validate and resolve a worker script URL as Chrome does.
fn script_url(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    iface: &str,
) -> Result<(Url, Option<String>, String), HostError> {
    let s = cx.arg_string(0)?;
    let Some(url) = w.resolve_url(&s) else {
        return Err(w.rt.err(
            "SyntaxError",
            format!("Failed to construct '{iface}': '{s}' is not a valid URL."),
        ));
    };
    let mine = w.origin_string();
    match url.scheme() {
        "data" => {
            let src = decode_data_url(&url).ok_or_else(|| {
                w.rt.err(
                    "SyntaxError",
                    format!("Failed to construct '{iface}': '{s}' is not a valid URL."),
                )
            })?;
            Ok((url, Some(src), "null".into()))
        }
        "blob" => {
            let (bytes, _) = resolve_blob_url(url.as_str()).unwrap_or_default();
            Ok((
                url,
                Some(String::from_utf8_lossy(&bytes).into_owned()),
                mine,
            ))
        }
        _ => {
            if url.origin().ascii_serialization() != mine {
                return Err(w.rt.err(
                    "SecurityError",
                    format!(
                        "Failed to construct '{iface}': Script at '{url}' cannot be accessed from origin '{mine}'."
                    ),
                ));
            }
            Ok((url, None, mine))
        }
    }
}

/// The text of a `data:` URL.
/// A `data:` URL's MIME type (as Chrome's data URL loader reports it)
/// and body.
pub(crate) fn decode_data_url_bytes(url: &Url) -> Option<(String, Vec<u8>)> {
    let rest = url.as_str().strip_prefix("data:")?;
    let rest = rest.split('#').next().unwrap_or(rest);
    let (meta, body) = rest.split_once(',')?;
    let bytes: Vec<u8> = percent_encoding::percent_decode_str(body).collect();
    let meta = meta.trim();
    let (mime, b64) = match meta.strip_suffix(";base64") {
        Some(m) => (m.trim(), true),
        None => (meta, false),
    };
    let mime = if mime.is_empty() || mime.starts_with(';') {
        "text/plain;charset=US-ASCII".to_owned()
    } else {
        mime.to_owned()
    };
    let body = if b64 {
        let s: String = String::from_utf8_lossy(&bytes)
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        super::window::base64_decode(&s)?
    } else {
        bytes
    };
    Some((mime, body))
}

fn decode_data_url(url: &Url) -> Option<String> {
    let rest = url.as_str().strip_prefix("data:")?;
    let (meta, body) = rest.split_once(',')?;
    let bytes: Vec<u8> = percent_encoding::percent_decode_str(body).collect();
    if meta.ends_with(";base64") {
        let s: String = String::from_utf8_lossy(&bytes)
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        let b = super::window::base64_decode(&s)?;
        return Some(String::from_utf8_lossy(&b).into_owned());
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// `WorkerOptions` / `SharedWorker` options: (module, name).
fn worker_options(
    cx: &mut CallCtx<'_, '_, '_>,
    iface: &str,
    shared: bool,
) -> Result<(bool, String), HostError> {
    if cx.len() < 2 || matches!(cx.arg_kind(1), ValueKind::Undefined) {
        return Ok((false, String::new()));
    }
    if shared && !matches!(cx.arg_kind(1), ValueKind::Object | ValueKind::Function) {
        // `new SharedWorker(url, name)`.
        return Ok((false, cx.arg_string(1)?));
    }
    let d = super::Dict::arg(cx, 1);
    let name = d.string(cx, "name")?.unwrap_or_default();
    let ty = d.string(cx, "type")?;
    d.release(cx);
    let module = match ty.as_deref() {
        None | Some("classic") => false,
        Some("module") => true,
        Some(t) => {
            return Err(HostError::type_error(format!(
                "Failed to construct '{iface}': Failed to read the 'type' property from 'WorkerOptions': The provided value '{t}' is not a valid enum value of type WorkerType."
            )));
        }
    };
    Ok((module, name))
}

fn new_start(
    w: &Rc<Window>,
    kind: WorkerKind,
    (url, source, origin): (Url, Option<String>, String),
    (module, name): (bool, String),
) -> WorkerStart {
    let (tx, rx) = inbox();
    WorkerStart {
        kind,
        url,
        source,
        module,
        name,
        origin,
        creator: w.url(),
        persona: w.persona.clone(),
        profile: w.profile.clone(),
        virtual_time: w.clock.is_virtual(),
        control: WorkerControl::new(tx.clone()),
        inbox: tx,
        inbox_rx: rx,
        parent: None,
        service: None,
    }
}

fn scope_of(w: &Window) -> Option<Rc<ScopeInit>> {
    w.wk.scope.borrow().clone()
}

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // MessageChannel / MessagePort.
    ctor(reg, rt, "MessageChannel", |cx, w| {
        let (a, b) = PortCore::pair();
        let p1 = adopt_port(w, a);
        let p2 = adopt_port(w, b);
        let id = w.new_obj(Obj::Worker(Box::new(WorkerHostObj::Channel {
            port1: p1,
            port2: p2,
        })));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    for (name, first) in [("port1", true), ("port2", false)] {
        getter(reg, rt, "MessageChannel", name, move |cx, w| {
            let id = this_obj(cx)?;
            let port = w
                .with_obj(id, |o| match o {
                    Obj::Worker(b) => match &**b {
                        WorkerHostObj::Channel { port1, port2 } => {
                            Some(if first { *port1 } else { *port2 })
                        }
                        _ => None,
                    },
                    _ => None,
                })
                .flatten()
                .ok_or_else(illegal)?;
            cx.ret(w.obj_arg(port))
        });
    }
    method(reg, rt, "MessagePort", "postMessage", |cx, w| {
        let id = this_obj(cx)?;
        let core = port_core(w, id).ok_or_else(illegal)?;
        let p = serialize_args(cx, w, "postMessage", "MessagePort")?;
        core.post(p);
        Ok(())
    });
    method(reg, rt, "MessagePort", "start", |cx, w| {
        let id = this_obj(cx)?;
        port_core(w, id).ok_or_else(illegal)?.start();
        Ok(())
    });
    method(reg, rt, "MessagePort", "close", |cx, w| {
        let id = this_obj(cx)?;
        port_core(w, id).ok_or_else(illegal)?.close();
        Ok(())
    });
    // Setting `onmessage` starts the port.
    if let Some(set) = reg
        .get("MessagePort", "onmessage", ImplKind::Setter)
        .cloned()
    {
        let rt2 = rt.clone();
        reg.register(
            "MessagePort",
            "onmessage",
            ImplKind::Setter,
            host_fn(move |cx| {
                set(cx)?;
                if let Some(h) = cx.this_host()
                    && h.kind == K_OBJ
                    && let Ok(w) = rt2.win(cx.realm())
                    && let Some(core) = port_core(&w, h.id)
                {
                    core.start();
                }
                Ok(())
            }),
        );
    }

    // Worker.
    ctor(reg, rt, "Worker", |cx, w| {
        need_args(cx, 1, "construct", "Worker").map_err(|_| {
            HostError::type_error(format!(
                "Failed to construct 'Worker': 1 argument required, but only {} present.",
                cx.len()
            ))
        })?;
        let url = script_url(cx, w, "Worker")?;
        let opts = worker_options(cx, "Worker", false)?;
        let mut start = new_start(w, WorkerKind::Dedicated, url, opts);
        let control = start.control.clone();
        let id = w.new_obj(Obj::Worker(Box::new(WorkerHostObj::Worker(WorkerObj {
            control: control.clone(),
        }))));
        start.parent = Some((w.inbox(), id));
        w.wk.children.borrow_mut().push(control);
        if let Some(host) = w.worker_host() {
            host.start(start);
        }
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    method(reg, rt, "Worker", "postMessage", |cx, w| {
        let id = this_obj(cx)?;
        let control = worker_control_of(w, id).ok_or_else(illegal)?;
        let p = serialize_args(cx, w, "postMessage", "Worker")?;
        if !control.is_terminated() {
            control.inbox().send(Box::new(move |js, sw| {
                deliver(js, sw, WINDOW_REF, p, String::new(), None)
            }));
        }
        Ok(())
    });
    method(reg, rt, "Worker", "terminate", |cx, w| {
        let id = this_obj(cx)?;
        worker_control_of(w, id).ok_or_else(illegal)?.terminate();
        Ok(())
    });

    // SharedWorker.
    ctor(reg, rt, "SharedWorker", |cx, w| {
        if cx.is_empty() {
            return Err(HostError::type_error(
                "Failed to construct 'SharedWorker': 1 argument required, but only 0 present.",
            ));
        }
        let url = script_url(cx, w, "SharedWorker")?;
        let opts = worker_options(cx, "SharedWorker", true)?;
        let start = new_start(w, WorkerKind::Shared, url, opts);
        let (mine, theirs) = PortCore::pair();
        let port = adopt_port(w, mine);
        let id = w.new_obj(Obj::Worker(Box::new(WorkerHostObj::SharedWorker { port })));
        if let Some(host) = w.worker_host() {
            host.connect_shared(start, connect_task(theirs));
        }
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    getter(reg, rt, "SharedWorker", "port", |cx, w| {
        let id = this_obj(cx)?;
        let port = w
            .with_obj(id, |o| match o {
                Obj::Worker(b) => match &**b {
                    WorkerHostObj::SharedWorker { port } => Some(*port),
                    _ => None,
                },
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal)?;
        cx.ret(w.obj_arg(port))
    });

    register_scope(reg, rt);
}

fn worker_control_of(w: &Window, id: u32) -> Option<Arc<WorkerControl>> {
    w.with_obj(id, |o| match o {
        Obj::Worker(b) => match &**b {
            WorkerHostObj::Worker(x) => Some(x.control.clone()),
            _ => None,
        },
        _ => None,
    })
    .flatten()
}

/// Members of the worker global scopes.
fn register_scope(reg: &mut Registry, rt: &Runtime) {
    // Same implementations as the window's.
    const SAME: &[(&str, ImplKind)] = &[
        ("setTimeout", ImplKind::Method),
        ("setInterval", ImplKind::Method),
        ("clearTimeout", ImplKind::Method),
        ("clearInterval", ImplKind::Method),
        ("queueMicrotask", ImplKind::Method),
        ("reportError", ImplKind::Method),
        ("fetch", ImplKind::Method),
        ("performance", ImplKind::Getter),
        ("crossOriginIsolated", ImplKind::Getter),
        ("isSecureContext", ImplKind::Getter),
    ];
    for (name, kind) in SAME {
        if let Some(f) = reg.get("Window", name, *kind).cloned() {
            reg.register("WorkerGlobalScope", name, *kind, f);
        }
    }
    for (name, kind) in [
        ("requestAnimationFrame", ImplKind::Method),
        ("cancelAnimationFrame", ImplKind::Method),
    ] {
        if let Some(f) = reg.get("Window", name, kind).cloned() {
            reg.register("DedicatedWorkerGlobalScope", name, kind, f);
        }
    }
    reg.getter(
        "WorkerGlobalScope",
        "self",
        host_fn(|cx| {
            let this = cx.v8_args().this();
            cx.ret_v8(this.into());
            Ok(())
        }),
    );
    getter(reg, rt, "WorkerGlobalScope", "location", |cx, w| {
        let id = w.cached_obj((0, 0, "workerlocation", String::new()), || {
            Obj::Worker(Box::new(WorkerHostObj::Location))
        });
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "WorkerGlobalScope", "navigator", |cx, w| {
        let id = w.cached_obj((0, 0, "workernavigator", String::new()), || {
            Obj::Worker(Box::new(WorkerHostObj::Navigator))
        });
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "WorkerGlobalScope", "origin", |cx, w| {
        cx.ret(w.origin_string())
    });
    // WorkerLocation: the script URL.
    for prop in [
        "href", "origin", "protocol", "host", "hostname", "port", "pathname", "search", "hash",
    ] {
        getter(reg, rt, "WorkerLocation", prop, move |cx, w| {
            this_obj(cx)?;
            let u = w.url();
            cx.ret(match prop {
                "origin" => w.origin_string(),
                _ => super::misc::url_component(&u, prop),
            })
        });
    }
    method(reg, rt, "WorkerLocation", "toString", |cx, w| {
        this_obj(cx)?;
        cx.ret(w.url().to_string())
    });
    // WorkerNavigator: the window's getters (same persona, same values).
    const NAV: &[&str] = &[
        "hardwareConcurrency",
        "appCodeName",
        "appName",
        "appVersion",
        "platform",
        "product",
        "userAgent",
        "language",
        "languages",
        "onLine",
        "connection",
        "mediaCapabilities",
        "permissions",
        "deviceMemory",
        "userAgentData",
        "locks",
        "storage",
        "gpu",
        "storageBuckets",
        "hid",
        "serial",
        "usb",
    ];
    for name in NAV {
        if let Some(f) = reg.get("Navigator", name, ImplKind::Getter).cloned() {
            reg.register("WorkerNavigator", name, ImplKind::Getter, f);
        }
    }
    // Encodings and clones, with the scope's interface in messages.
    method(reg, rt, "WorkerGlobalScope", "btoa", |cx, w| {
        super::window::btoa_impl(cx, w, "WorkerGlobalScope")
    });
    method(reg, rt, "WorkerGlobalScope", "atob", |cx, w| {
        super::window::atob_impl(cx, w, "WorkerGlobalScope")
    });
    method(reg, rt, "WorkerGlobalScope", "structuredClone", |cx, w| {
        structured_clone(cx, w, "WorkerGlobalScope")
    });
    method(reg, rt, "Window", "structuredClone", |cx, w| {
        structured_clone(cx, w, "Window")
    });
    method(
        reg,
        rt,
        "WorkerGlobalScope",
        "importScripts",
        import_scripts,
    );
    method(
        reg,
        rt,
        "DedicatedWorkerGlobalScope",
        "postMessage",
        |cx, w| {
            let p = serialize_args(cx, w, "postMessage", "DedicatedWorkerGlobalScope")?;
            let Some(scope) = scope_of(w) else {
                return Err(illegal());
            };
            if scope.control.is_terminated() {
                return Ok(());
            }
            if let Some((parent, id)) = scope.parent.clone() {
                parent.send(Box::new(move |js, pw| {
                    deliver(js, pw, HostRef::new(K_OBJ, id), p, String::new(), None)
                }));
            }
            Ok(())
        },
    );
    for iface in ["DedicatedWorkerGlobalScope", "SharedWorkerGlobalScope"] {
        method(reg, rt, iface, "close", |_cx, w| {
            if let Some(s) = scope_of(w) {
                s.control.close();
            }
            Ok(())
        });
        getter(reg, rt, iface, "name", |cx, w| {
            cx.ret(scope_of(w).map(|s| s.name.clone()).unwrap_or_default())
        });
    }
}

/// `structuredClone(value, {transfer})` on V8's serializer.
fn structured_clone(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Rc<Window>,
    iface: &str,
) -> Result<(), HostError> {
    let p = serialize_args(cx, w, "structuredClone", iface)?;
    let port_ids: Vec<u32> = p.ports.into_iter().map(|c| adopt_port(w, c)).collect();
    let hosts: Vec<(InterfaceId, HostRef)> = p
        .entries
        .into_iter()
        .map(|e| match e {
            HostEntry::Port(i) => (
                w.rt.iface("MessagePort"),
                HostRef::new(K_OBJ, port_ids.get(i).copied().unwrap_or(0)),
            ),
            HostEntry::Blob(bytes, type_) => {
                let id = w.new_obj(Obj::Blob(Box::new(BlobData { bytes, type_ })));
                (w.rt.iface("Blob"), HostRef::new(K_OBJ, id))
            }
            HostEntry::Key(k) => {
                let id = w.new_obj(Obj::Crypto(Box::new(super::crypto::CryptoObj::Key(k))));
                (w.rt.iface("CryptoKey"), HostRef::new(K_OBJ, id))
            }
        })
        .collect();
    let h = cx
        .deserialize(&p.data, &hosts)
        .map_err(|m| w.rt.err("DataCloneError", m))?;
    cx.ret(Arg::Handle(h))
}

/// `importScripts(...urls)`: fetch each synchronously, then run them in
/// order in the global scope.
fn import_scripts(cx: &mut CallCtx<'_, '_, '_>, w: &Rc<Window>) -> Result<(), HostError> {
    let fail = |m: String| format!("Failed to execute 'importScripts' on 'WorkerGlobalScope': {m}");
    let scope = scope_of(w).ok_or_else(illegal)?;
    if scope.module {
        return Err(HostError::type_error(fail(
            "Module scripts don't support importScripts().".into(),
        )));
    }
    let mut urls = Vec::new();
    for i in 0..cx.len() {
        let s = cx.arg_string(i)?;
        let u = w.resolve_url(&s).ok_or_else(|| {
            w.rt.err("SyntaxError", fail(format!("The URL '{s}' is invalid.")))
        })?;
        urls.push(u);
    }
    let host = w.worker_host().ok_or_else(illegal)?;
    let mut sources = Vec::new();
    for u in urls {
        let failed = || {
            w.rt.err(
                "NetworkError",
                fail(format!("The script at '{u}' failed to load.")),
            )
        };
        let text = if u.scheme() == "blob" {
            let (bytes, _) = resolve_blob_url(u.as_str()).ok_or_else(failed)?;
            String::from_utf8_lossy(&bytes).into_owned()
        } else if u.scheme() == "data" {
            decode_data_url(&u).ok_or_else(failed)?
        } else {
            let mut req = NetRequest::get(u.clone(), Destination::Script, Some(w.url()));
            req.mode = "no-cors";
            let resp = host.fetch_sync(req).map_err(|_| failed())?;
            if !(200..300).contains(&resp.status) {
                return Err(failed());
            }
            crate::host::decode_text(&resp.body, resp.header("content-type"), false).0
        };
        sources.push((u, text));
    }
    for (u, text) in sources {
        match cx.run_classic(&text, lb_js::ScriptOrigin::new(u.as_str())) {
            Ok(()) => {}
            // A syntax error is thrown with the call's prefix; anything the
            // script throws propagates unchanged.
            Err(Some(msg)) => return Err(HostError::SyntaxError(fail(msg))),
            Err(None) => return Err(HostError::Thrown),
        }
    }
    Ok(())
}

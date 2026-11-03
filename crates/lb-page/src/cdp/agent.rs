//! The worker-side half of the CDP host: one [`Agent`] per page, living on
//! the page's worker thread. It tracks the page's realms as CDP execution
//! contexts, the remote objects handed out, new-document scripts,
//! bindings, interception settings, and reports page events to the server.
//!
//! Nothing here touches the main world for CDP's sake: isolated worlds get
//! the bindings, and new-document scripts run where the client asked.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use lb_cdp::remote::{
    self as cdp_remote, FunctionInfo, JsValue as CdpValue, ObjectInfo, RemoteOptions,
};
use lb_cdp::types::*;
use lb_cdp::{BindingScope, HostError, HostResult, PageEvent};
use lb_dom::{Document, NodeId};
use lb_js::{
    Arg, Caught, Engine, Handle, Inspected, JsErrorKind, PromiseOutcome, RealmId, RealmOps,
    ScriptOrigin,
};
use lb_webapi::host::ConsoleMessage;
use lb_webapi::rt::frames;
use lb_webapi::rt::{EngineJs, K_NODE, K_WINDOW, Window, decode_node, node_ref};

use super::Shared;
use crate::browser::WorkerState;
use crate::page::PageState;

/// Chrome-shaped loader id of navigation `seq` of page `page_id`.
pub(crate) fn loader_id(page_id: u64, seq: u64) -> String {
    format!("{page_id:016X}{seq:016X}")
}

/// What the host can read without asking the worker.
#[derive(Debug, Clone, Default)]
pub(crate) struct FrameInfo {
    pub url: String,
    pub loader: String,
    pub title: String,
}

/// A realm as a CDP execution context.
#[derive(Debug, Clone)]
pub(crate) struct WorldEntry {
    pub id: ExecutionContextId,
    pub realm: RealmId,
    /// `None` for the main world.
    pub name: Option<String>,
    /// Frame id in lb-webapi's tree (0: the page's main frame).
    pub frame: u64,
}

/// A remote object handed out to the client.
#[derive(Debug, Clone)]
pub(crate) struct ObjEntry {
    pub ctx: ExecutionContextId,
    pub realm: RealmId,
    pub handle: Handle,
    pub group: Option<String>,
}

#[derive(Default)]
pub(crate) struct AgentState {
    pub worlds: Vec<WorldEntry>,
    /// `(identifier, source, world)` from `addScriptToEvaluateOnNewDocument`.
    pub scripts: Vec<(String, String, Option<String>)>,
    pub bindings: Vec<(String, BindingScope)>,
    pub objects: HashMap<String, ObjEntry>,
    pub next_obj: u64,
    pub intercept: Option<Vec<RequestPattern>>,
    pub extra_headers: Vec<(String, String)>,
    /// Bodies of recent responses for `Network.getResponseBody`.
    pub bodies: VecDeque<(String, Vec<u8>)>,
    pub viewport: Option<(u32, u32)>,
    /// Navigation sequence of the current document (backend node ids).
    pub seq: u64,
    pub loader: String,
    /// Loader ids of child frames' current documents, by frame id.
    pub frame_loaders: HashMap<u64, String>,
    pub next_frame_loader: u64,
}

/// Responses kept for `Network.getResponseBody`.
const MAX_BODIES: usize = 64;

/// See the module docs.
pub(crate) struct Agent {
    pub target: String,
    pub page_id: u64,
    pub shared: Arc<Shared>,
    pub frame: Arc<Mutex<FrameInfo>>,
    pub st: RefCell<AgentState>,
}

/// Backend node ids: the navigation sequence in the high bits, so ids of
/// an old document never resolve in a new one.
const NODE_BITS: u32 = 24;

pub(crate) fn backend_id(seq: u64, n: NodeId) -> BackendNodeId {
    ((seq as i64) << NODE_BITS) | (i64::from(n.0) + 1)
}

fn node_of(seq: u64, id: BackendNodeId) -> Option<NodeId> {
    if id <= 0 || (id >> NODE_BITS) as u64 != seq {
        return None;
    }
    let n = (id & ((1 << NODE_BITS) - 1)) - 1;
    u32::try_from(n).ok().map(NodeId)
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

fn origin_of(url: &url::Url) -> String {
    match url.scheme() {
        "http" | "https" => url.origin().ascii_serialization(),
        _ => "://".into(),
    }
}

impl Agent {
    pub(crate) fn new(
        target: String,
        page_id: u64,
        shared: Arc<Shared>,
        frame: Arc<Mutex<FrameInfo>>,
    ) -> Agent {
        Agent {
            target,
            page_id,
            shared,
            frame,
            st: RefCell::new(AgentState::default()),
        }
    }

    pub(crate) fn emit(&self, ev: PageEvent) {
        self.shared.emit(&self.target, ev);
    }

    /// Monotonic seconds (CDP `timestamp`).
    pub(crate) fn mono(&self) -> f64 {
        self.shared.mono()
    }

    // ---- navigation ----

    /// A navigation started; returns its loader id.
    pub(crate) fn nav_started(&self, seq: u64) -> String {
        let loader = loader_id(self.page_id, seq);
        self.emit(PageEvent::FrameStartedLoading {
            frame_id: self.target.clone(),
        });
        loader
    }

    pub(crate) fn nav_failed(&self) {
        self.emit(PageEvent::FrameStoppedLoading {
            frame_id: self.target.clone(),
        });
    }

    /// Drop the isolated-world realms of the current document (before the
    /// main realm goes).
    pub(crate) fn drop_worlds(&self, ws: &WorkerState) {
        let worlds: Vec<WorldEntry> = self.st.borrow().worlds.clone();
        let rt = ws.runtime().map(|(rt, _)| rt);
        for wd in worlds.iter().filter(|w| w.name.is_some()) {
            if let Some(rt) = &rt {
                rt.detach(wd.realm);
            }
            ws.with_engine(|e| e.drop_realm(wd.realm));
        }
        let mut st = self.st.borrow_mut();
        st.objects.clear();
    }

    /// A new document committed: report it, create its isolated worlds and
    /// run the new-document scripts before any page script.
    pub(crate) fn committed(
        &self,
        ws: &Rc<WorkerState>,
        page: &Rc<PageState>,
        w: &Rc<Window>,
        seq: u64,
    ) {
        let loader = loader_id(self.page_id, seq);
        let url = w.url();
        let old: Vec<ExecutionContextId> = {
            let mut st = self.st.borrow_mut();
            st.seq = seq;
            st.loader = loader.clone();
            st.objects.clear();
            std::mem::take(&mut st.worlds)
                .into_iter()
                .map(|w| w.id)
                .collect()
        };
        {
            let mut f = self.frame.lock().unwrap_or_else(|e| e.into_inner());
            f.url = url.to_string();
            f.loader = loader.clone();
            f.title = String::new();
        }
        for id in old {
            self.emit(PageEvent::ExecutionContextDestroyed { context_id: id });
        }
        self.emit(PageEvent::ExecutionContextsCleared);
        self.emit(PageEvent::FrameNavigated {
            frame: Frame::new(self.target.clone(), None, loader.clone(), url.as_str()),
        });
        let viewport = self.st.borrow().viewport;
        if let Some((vw, vh)) = viewport {
            w.with_layout_engine(|l, _| l.set_viewport(vw as f32, vh as f32));
        }
        // Main world.
        let main = self.register_world(w.realm(), None, &url, 0);
        self.emit(PageEvent::ExecutionContextCreated { context: main });
        // Isolated worlds named by new-document scripts, in order.
        let mut names: Vec<String> = Vec::new();
        for (_, _, world) in &self.st.borrow().scripts {
            if let Some(n) = world
                && !names.contains(n)
            {
                names.push(n.clone());
            }
        }
        for n in names {
            let _ = self.create_world(ws, w, &n);
        }
        // New-document scripts.
        let scripts = self.st.borrow().scripts.clone();
        for (_, src, world) in scripts {
            if src.trim().is_empty() {
                continue;
            }
            let realm = match &world {
                None => Some(w.realm()),
                Some(n) => self
                    .st
                    .borrow()
                    .worlds
                    .iter()
                    .find(|x| x.name.as_deref() == Some(n))
                    .map(|x| x.realm),
            };
            if let Some(realm) = realm {
                ws.with_engine(|e| {
                    let r = e.evaluate_catching(realm, &src, ScriptOrigin::new(""));
                    if let Ok(h) = r {
                        e.release(h);
                    }
                    e.perform_microtask_checkpoint();
                });
            }
        }
        let _ = page;
    }

    fn register_world(
        &self,
        realm: RealmId,
        name: Option<String>,
        url: &url::Url,
        frame: u64,
    ) -> ExecutionContext {
        let id = self.shared.next_ctx.fetch_add(1, Ordering::Relaxed);
        self.st.borrow_mut().worlds.push(WorldEntry {
            id,
            realm,
            name: name.clone(),
            frame,
        });
        ExecutionContext {
            id,
            frame_id: self.frame_str(frame),
            world: match name {
                None => World::Main,
                Some(n) => World::Isolated(n),
            },
            origin: origin_of(url),
            unique_id: format!("{:016X}.{id}", self.shared.seed ^ self.page_id),
        }
    }

    /// Create an isolated world in the current document of `w` (the page's
    /// window or a frame's) and report it.
    pub(crate) fn create_world(
        &self,
        ws: &Rc<WorkerState>,
        w: &Rc<Window>,
        name: &str,
    ) -> HostResult<ExecutionContextId> {
        ws.with_engine(|e| self.create_world_with(e, w, name))
            .ok_or_else(|| HostError::new("Page is closed"))?
    }

    /// [`Agent::create_world`] with explicit realm operations (also from
    /// inside a callback, for frames created by script).
    pub(crate) fn create_world_with(
        &self,
        ops: &mut dyn RealmOps,
        w: &Rc<Window>,
        name: &str,
    ) -> HostResult<ExecutionContextId> {
        let bindings = w
            .runtime()
            .bindings()
            .ok_or_else(|| HostError::new("Page is closed"))?;
        let realm = w
            .create_isolated_world(ops, &bindings, name)
            .map_err(|e| HostError::new(e.message))?;
        let frame = frames::frame_id(w);
        let ctx = self.register_world(
            realm,
            Some(name.to_string()),
            &frames::effective_url(w),
            frame,
        );
        let id = ctx.id;
        let names: Vec<String> = self
            .st
            .borrow()
            .bindings
            .iter()
            .filter(|(_, s)| match s {
                BindingScope::AllIsolatedWorlds => true,
                BindingScope::World(n) => n == name,
                BindingScope::Context(_) => false,
            })
            .map(|(n, _)| n.clone())
            .collect();
        for b in names {
            self.install_binding_with(ops, realm, id, &b);
        }
        self.emit(PageEvent::ExecutionContextCreated { context: ctx });
        Ok(id)
    }

    /// Install binding `name` on an isolated world's global.
    pub(crate) fn install_binding(
        &self,
        ws: &WorkerState,
        realm: RealmId,
        ctx: ExecutionContextId,
        name: &str,
    ) {
        ws.with_engine(|e| self.install_binding_with(e, realm, ctx, name));
    }

    pub(crate) fn install_binding_with(
        &self,
        ops: &mut dyn RealmOps,
        realm: RealmId,
        ctx: ExecutionContextId,
        name: &str,
    ) {
        let shared = self.shared.clone();
        let target = self.target.clone();
        let bname = name.to_string();
        let f = ops.new_function(
            realm,
            name,
            1,
            lb_js::host_fn(move |cx| {
                let payload = cx.arg_string(0).unwrap_or_default();
                shared.emit(
                    &target,
                    PageEvent::BindingCalled {
                        name: bname.clone(),
                        payload,
                        context_id: ctx,
                    },
                );
                Ok(())
            }),
        );
        if let (Ok(f), Ok(g)) = (f, ops.global(realm)) {
            let _ = ops.set_property(realm, g, name, Arg::Handle(f));
            ops.release(f);
            ops.release(g);
        }
    }

    /// Lifecycle points of the current document.
    pub(crate) fn lifecycle(&self, page: &PageState, name: &str, seq: u64) {
        if name == "commit" {
            return;
        }
        let loader = loader_id(self.page_id, seq);
        self.emit(PageEvent::Lifecycle {
            frame_id: self.target.clone(),
            loader_id: loader,
            name: name.to_string(),
            timestamp: self.mono(),
        });
        if name == "DOMContentLoaded"
            && let Some(w) = page.window()
        {
            let title = w.with_doc(|d| d.title());
            self.frame.lock().unwrap_or_else(|e| e.into_inner()).title = title.clone();
            if !title.is_empty() {
                self.emit(PageEvent::TitleChanged { title });
            }
        }
        if name == "load" {
            self.emit(PageEvent::FrameStoppedLoading {
                frame_id: self.target.clone(),
            });
        }
    }

    // ---- child frames ----

    /// CDP frame id of lb-webapi frame `id` (the target id for the main
    /// frame; 32 hex digits like Chrome otherwise).
    pub(crate) fn frame_str(&self, id: u64) -> String {
        if id == 0 {
            self.target.clone()
        } else {
            format!("{:016X}{:016X}", self.page_id, id)
        }
    }

    /// lb-webapi frame id of a CDP frame id of this page.
    pub(crate) fn frame_of(&self, s: &str) -> Option<u64> {
        if s == self.target {
            return Some(0);
        }
        if s.len() != 32 || u64::from_str_radix(&s[..16], 16).ok()? != self.page_id {
            return None;
        }
        u64::from_str_radix(&s[16..], 16).ok().filter(|&id| id != 0)
    }

    /// A child frame's document committed (its initial `about:blank` or a
    /// navigation): frame events, its execution contexts (main world plus
    /// the worlds new-document scripts name) and those scripts.
    pub(crate) fn frame_committed(
        &self,
        ops: &mut dyn RealmOps,
        child: &Rc<Window>,
        initial: bool,
    ) {
        let id = frames::frame_id(child);
        let Some(parent) = frames::parent(child) else {
            return;
        };
        let fid = self.frame_str(id);
        let pid = self.frame_str(frames::frame_id(&parent));
        if initial {
            self.emit(PageEvent::FrameAttached {
                frame_id: fid.clone(),
                parent_frame_id: pid.clone(),
            });
        }
        let loader = {
            let mut st = self.st.borrow_mut();
            st.next_frame_loader += 1;
            let l = format!(
                "{:016X}{:016X}",
                id ^ (self.page_id << 32),
                st.next_frame_loader
            );
            st.frame_loaders.insert(id, l.clone());
            l
        };
        let url = child.url();
        let mut frame = Frame::new(fid.clone(), Some(pid), loader, url.as_str());
        let name = frames::window_name(child);
        if !name.is_empty() {
            frame.name = Some(name);
        }
        if url.scheme() == "about" {
            let origin = frames::effective_url(child).origin().ascii_serialization();
            if origin != "null" {
                frame.security_origin = origin;
            }
        }
        self.emit(PageEvent::FrameNavigated { frame });
        let main = self.register_world(child.realm(), None, &frames::effective_url(child), id);
        self.emit(PageEvent::ExecutionContextCreated { context: main });
        let mut names: Vec<String> = Vec::new();
        for (_, _, world) in &self.st.borrow().scripts {
            if let Some(n) = world
                && !names.contains(n)
            {
                names.push(n.clone());
            }
        }
        for n in names {
            let _ = self.create_world_with(ops, child, &n);
        }
        let scripts = self.st.borrow().scripts.clone();
        for (_, src, world) in scripts {
            if src.trim().is_empty() {
                continue;
            }
            let realm = match &world {
                None => Some(child.realm()),
                Some(n) => self
                    .st
                    .borrow()
                    .worlds
                    .iter()
                    .find(|x| x.frame == id && x.name.as_deref() == Some(n))
                    .map(|x| x.realm),
            };
            if let Some(realm) = realm {
                let _ = ops.evaluate(realm, &src, ScriptOrigin::new(""));
            }
        }
        if initial {
            for ev in ["init", "DOMContentLoaded", "load"] {
                self.frame_lifecycle(id, ev);
            }
        }
    }

    /// A child frame's window goes away: drop its isolated worlds and
    /// report its contexts destroyed (and the frame detached if `gone`).
    pub(crate) fn frame_detached(&self, ops: &mut dyn RealmOps, child: &Rc<Window>, gone: bool) {
        let id = frames::frame_id(child);
        let worlds: Vec<WorldEntry> = {
            let mut st = self.st.borrow_mut();
            let (mine, rest): (Vec<WorldEntry>, Vec<WorldEntry>) = std::mem::take(&mut st.worlds)
                .into_iter()
                .partition(|w| w.frame == id && id != 0);
            st.worlds = rest;
            let realms: Vec<RealmId> = mine.iter().map(|w| w.realm).collect();
            st.objects.retain(|_, o| !realms.contains(&o.realm));
            if gone {
                st.frame_loaders.remove(&id);
            }
            mine
        };
        for w in &worlds {
            if w.name.is_some() {
                child.runtime().detach(w.realm);
                ops.drop_realm(w.realm);
            }
            self.emit(PageEvent::ExecutionContextDestroyed { context_id: w.id });
        }
        if gone {
            self.emit(PageEvent::FrameDetached {
                frame_id: self.frame_str(id),
            });
        }
    }

    pub(crate) fn frame_started_loading(&self, id: u64) {
        self.emit(PageEvent::FrameStartedLoading {
            frame_id: self.frame_str(id),
        });
    }

    /// Lifecycle point `name` of child frame `id`'s current document.
    pub(crate) fn frame_lifecycle(&self, id: u64, name: &str) {
        if name == "commit" {
            return;
        }
        let Some(loader) = self.st.borrow().frame_loaders.get(&id).cloned() else {
            return;
        };
        let fid = self.frame_str(id);
        let names: &[&str] = if name == "load" {
            &["load", "networkAlmostIdle", "networkIdle"]
        } else {
            &[name]
        };
        for n in names {
            self.emit(PageEvent::Lifecycle {
                frame_id: fid.clone(),
                loader_id: loader.clone(),
                name: (*n).to_string(),
                timestamp: self.mono(),
            });
        }
        if name == "load" {
            self.emit(PageEvent::FrameStoppedLoading { frame_id: fid });
        }
    }

    /// Console messages and uncaught exceptions of the page.
    pub(crate) fn console(&self, _page: &PageState, m: &ConsoleMessage) {
        let ctx = self.main_ctx().unwrap_or(0);
        if m.level == "error"
            && let Some(rest) = m.text.strip_prefix("Uncaught ")
        {
            let class = rest.split(':').next().unwrap_or("Error").trim().to_string();
            let is_error = class.ends_with("Error") || class == "Error";
            let exception = if is_error {
                cdp_remote::remote_object(
                    &CdpValue::Object(Box::new(ObjectInfo {
                        class_name: class,
                        subtype: Some("error".into()),
                        description: Some(rest.to_string()),
                        ..Default::default()
                    })),
                    None,
                    RemoteOptions::default(),
                )
            } else {
                cdp_remote::remote_object(
                    &CdpValue::String(rest.to_string()),
                    None,
                    RemoteOptions::default(),
                )
            };
            self.emit(PageEvent::ExceptionThrown {
                timestamp: now_ms(),
                details: ExceptionDetails {
                    exception_id: 1,
                    text: "Uncaught".into(),
                    line_number: i64::from(m.line.unwrap_or(1).saturating_sub(1)),
                    column_number: 0,
                    script_id: None,
                    url: m.url.clone(),
                    stack_trace: None,
                    exception: Some(exception),
                    execution_context_id: Some(ctx),
                },
            });
            return;
        }
        let kind = match m.level.as_str() {
            "warn" => "warning",
            other => other,
        };
        self.emit(PageEvent::ConsoleApiCalled {
            kind: kind.to_string(),
            args: vec![cdp_remote::remote_object(
                &CdpValue::String(m.text.clone()),
                None,
                RemoteOptions::default(),
            )],
            context_id: ctx,
            timestamp: now_ms(),
            stack_trace: None,
        });
    }

    pub(crate) fn main_ctx(&self) -> Option<ExecutionContextId> {
        self.st
            .borrow()
            .worlds
            .iter()
            .find(|w| w.name.is_none() && w.frame == 0)
            .map(|w| w.id)
    }

    pub(crate) fn world(&self, id: ExecutionContextId) -> HostResult<WorldEntry> {
        self.st
            .borrow()
            .worlds
            .iter()
            .find(|w| w.id == id)
            .cloned()
            .ok_or_else(|| HostError::new("Cannot find context with specified id"))
    }

    pub(crate) fn object(&self, id: &str) -> HostResult<ObjEntry> {
        self.st
            .borrow()
            .objects
            .get(id)
            .cloned()
            .ok_or_else(|| HostError::new("Could not find object with given id"))
    }

    pub(crate) fn current_seq(&self) -> u64 {
        self.st.borrow().seq
    }

    pub(crate) fn store_body(&self, request_id: &str, body: Vec<u8>) {
        let mut st = self.st.borrow_mut();
        st.bodies.push_back((request_id.to_string(), body));
        while st.bodies.len() > MAX_BODIES {
            st.bodies.pop_front();
        }
    }

    // ---- remote objects ----

    fn register(
        &self,
        ctx: ExecutionContextId,
        realm: RealmId,
        handle: Handle,
        group: Option<&str>,
    ) -> String {
        let mut st = self.st.borrow_mut();
        st.next_obj += 1;
        let id = format!("{}.{}.{}", self.page_id, ctx, st.next_obj);
        st.objects.insert(
            id.clone(),
            ObjEntry {
                ctx,
                realm,
                handle,
                group: group.map(str::to_string),
            },
        );
        id
    }

    /// The `RemoteObject` for a held value. Keeps the handle (registered
    /// under a new object id) when the client gets an object id, releases
    /// it otherwise.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn remote(
        &self,
        engine: &mut Engine,
        w: &Window,
        realm: RealmId,
        ctx: ExecutionContextId,
        h: Handle,
        by_value: bool,
        preview: bool,
        group: Option<&str>,
    ) -> HostResult<RemoteObject> {
        let ins = engine
            .inspect(realm, h, preview && !by_value)
            .map_err(|e| HostError::new(e.message))?;
        let js = to_cdp(&ins, w);
        let opts = RemoteOptions {
            return_by_value: by_value,
            generate_preview: preview,
        };
        let keep = !by_value
            && matches!(
                js,
                CdpValue::Object(_) | CdpValue::Function(_) | CdpValue::Symbol(_)
            );
        let mut r = if keep {
            let id = self.register(ctx, realm, h, group);
            cdp_remote::remote_object(&js, Some(id), opts)
        } else {
            cdp_remote::remote_object(&js, None, opts)
        };
        if by_value && matches!(js, CdpValue::Object(_) | CdpValue::Function(_)) {
            let text = engine
                .json_by_value(realm, h)
                .map_err(|e| HostError::new(e.message))?;
            r.value = Some(serde_json::from_str(&text).map_err(|e| HostError::new(e.to_string()))?);
        }
        if !keep {
            engine.release(h);
        }
        Ok(r)
    }

    /// `ExceptionDetails` for a caught exception (`in_promise`: a rejected
    /// awaited promise).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn exception(
        &self,
        engine: &mut Engine,
        w: &Window,
        realm: RealmId,
        ctx: ExecutionContextId,
        c: Caught,
        in_promise: bool,
        group: Option<&str>,
    ) -> ExceptionDetails {
        let exception = match c.value {
            Some(h) => self
                .remote(engine, w, realm, ctx, h, false, false, group)
                .ok(),
            None => None,
        }
        .or_else(|| {
            let class = c
                .error
                .stack
                .as_deref()
                .and_then(|s| s.split(':').next())
                .unwrap_or("Error")
                .to_string();
            Some(cdp_remote::remote_object(
                &CdpValue::Object(Box::new(ObjectInfo {
                    class_name: class,
                    subtype: Some("error".into()),
                    description: Some(
                        c.error
                            .stack
                            .clone()
                            .unwrap_or_else(|| c.error.message.clone()),
                    ),
                    ..Default::default()
                })),
                None,
                RemoteOptions::default(),
            ))
        });
        let text = if in_promise {
            "Uncaught (in promise)"
        } else {
            "Uncaught"
        };
        let text = match c.error.kind {
            JsErrorKind::Terminated | JsErrorKind::OutOfMemory => c.error.message.clone(),
            _ => text.to_string(),
        };
        ExceptionDetails {
            exception_id: 1,
            text,
            line_number: i64::from(c.error.line.unwrap_or(1).saturating_sub(1)),
            column_number: i64::from(c.error.column.unwrap_or(0)),
            script_id: Some("0".into()),
            url: c.error.url.clone().filter(|u| !u.is_empty()),
            stack_trace: None,
            exception,
            execution_context_id: Some(ctx),
        }
    }

    // ---- DOM ----

    pub(crate) fn ids(&self) -> impl Fn(NodeId) -> BackendNodeId + use<> {
        let seq = self.current_seq();
        move |n| backend_id(seq, n)
    }

    /// The main-document node behind a `NodeRef`.
    pub(crate) fn node(&self, ws: &WorkerState, w: &Window, node: &NodeRef) -> HostResult<NodeId> {
        match node {
            NodeRef::Backend(id) => {
                let n = node_of(self.current_seq(), *id)
                    .ok_or_else(|| HostError::new("No node with given id found"))?;
                if n.0 as usize >= w.with_doc(|d| d.node_count()) {
                    return Err(HostError::new("No node with given id found"));
                }
                Ok(n)
            }
            NodeRef::Object(o) => {
                let obj = self.object(o)?;
                // Nodes of child frames are not addressable through the
                // DOM domain yet (their ids are relative to their window).
                if let Some(owner) = w.runtime().window(obj.realm)
                    && !std::ptr::eq(owner.as_ref(), w)
                {
                    return Err(HostError::new(
                        "Node belongs to a child frame; DOM methods on frame nodes are not supported yet",
                    ));
                }
                let host = ws
                    .with_engine(|e| e.unwrap_host(obj.realm, obj.handle))
                    .ok_or_else(|| HostError::new("Page is closed"))?
                    .map_err(|e| HostError::new(e.message))?;
                match host.and_then(decode_node) {
                    Some((0, n)) => Ok(n),
                    _ => Err(HostError::new(
                        "Node with given id does not belong to the document",
                    )),
                }
            }
        }
    }

    /// A remote object for main-document node `n` in `ctx`.
    pub(crate) fn node_object(
        &self,
        ws: &WorkerState,
        w: &Window,
        n: NodeId,
        ctx: ExecutionContextId,
        group: Option<&str>,
    ) -> HostResult<RemoteObject> {
        let world = self.world(ctx)?;
        let arg = w.node_arg(0, n);
        ws.with_engine(|e| {
            let h = e
                .make_handle(world.realm, arg)
                .map_err(|e| HostError::new(e.message))?;
            self.remote(e, w, world.realm, ctx, h, false, false, group)
        })
        .ok_or_else(|| HostError::new("Page is closed"))?
    }
}

/// [`Inspected`] as `lb_cdp`'s snapshot, with DOM nodes described.
pub(crate) fn to_cdp(v: &Inspected, w: &Window) -> CdpValue {
    match v {
        Inspected::Undefined => CdpValue::Undefined,
        Inspected::Null => CdpValue::Null,
        Inspected::Bool(b) => CdpValue::Bool(*b),
        Inspected::Number(n) => CdpValue::Number(*n),
        Inspected::BigInt(d) => CdpValue::BigInt(d.clone()),
        Inspected::String(s) => CdpValue::String(s.clone()),
        Inspected::Symbol(s) => CdpValue::Symbol(s.clone()),
        Inspected::Function { class_name, source } => CdpValue::Function(FunctionInfo {
            class_name: class_name.clone(),
            source: source.clone(),
        }),
        Inspected::Object(o) => {
            let mut info = ObjectInfo {
                class_name: o.class_name.clone(),
                subtype: o.subtype.clone(),
                description: o.description.clone(),
                length: o.length,
                properties: o
                    .properties
                    .iter()
                    .map(|(k, v)| (k.clone(), to_cdp(v, w)))
                    .collect(),
                entries: o
                    .entries
                    .iter()
                    .map(|(k, v)| (k.as_ref().map(|k| to_cdp(k, w)), to_cdp(v, w)))
                    .collect(),
            };
            if let Some(h) = o.host {
                if h.kind & 0xff == K_NODE {
                    info.subtype = Some("node".into());
                    if let Some((0, n)) = decode_node(h) {
                        info.description =
                            Some(w.with_doc(|d| lb_cdp::convert::node_description(d, n)));
                    }
                } else if h.kind == K_WINDOW {
                    info.class_name = "Window".into();
                    info.description = Some("Window".into());
                }
            }
            CdpValue::Object(Box::new(info))
        }
    }
}

/// Convert a `CallArgument` into an engine argument for `realm`.
pub(crate) fn call_arg(
    agent: &Agent,
    engine: &mut Engine,
    realm: RealmId,
    a: &CallArgument,
) -> HostResult<Arg> {
    if let Some(oid) = &a.object_id {
        let o = agent.object(oid)?;
        if o.realm != realm {
            return Err(HostError::new(
                "Argument should belong to the same JavaScript world as target object",
            ));
        }
        return Ok(Arg::Handle(o.handle));
    }
    if let Some(u) = &a.unserializable_value {
        return Ok(match u.as_str() {
            "NaN" => Arg::Number(f64::NAN),
            "Infinity" => Arg::Number(f64::INFINITY),
            "-Infinity" => Arg::Number(f64::NEG_INFINITY),
            "-0" => Arg::Number(-0.0),
            big if big.ends_with('n') => {
                let h = engine
                    .evaluate_catching(realm, big, ScriptOrigin::new(""))
                    .map_err(|_| HostError::new("Couldn't parse value object in call argument"))?;
                Arg::Handle(h)
            }
            _ => {
                return Err(HostError::new(
                    "Couldn't parse value object in call argument",
                ));
            }
        });
    }
    Ok(match &a.value {
        None => Arg::Undefined,
        Some(v) => Arg::Json(v.to_string()),
    })
}

/// After CDP ran JS in a page: microtasks, then DOM bookkeeping (mutation
/// observers, inserted scripts) in the main world.
pub(crate) fn settle(ws: &WorkerState, w: &Rc<Window>) {
    let realm = w.realm();
    ws.with_engine(|engine| {
        engine.perform_microtask_checkpoint();
        let mut js = EngineJs { engine, realm };
        w.after_mutation(&mut js);
    });
}

/// The main-world remote node for a node ref (for `DOM.*` helpers).
#[allow(dead_code)]
pub(crate) fn node_ref_of(n: NodeId) -> lb_js::HostRef {
    node_ref(0, n)
}

/// Evaluate a script or call a function in a world, await its promise if
/// asked, and build the CDP result. Runs on the page's worker.
pub(crate) async fn run_js(
    ws: Rc<WorkerState>,
    page: Rc<PageState>,
    ctx: ExecutionContextId,
    job: JsJob,
    opts: JsOpts,
) -> HostResult<EvalResult> {
    let agent = page
        .agent()
        .ok_or_else(|| HostError::new("Page is closed"))?;
    let world = agent.world(ctx)?;
    let realm = world.realm;
    // The window of the context's frame (main or child).
    let w = ws
        .runtime()
        .and_then(|(rt, _)| rt.window(realm))
        .or_else(|| page.window())
        .ok_or_else(|| HostError::new("Page is closed"))?;
    let first = ws
        .with_engine(|e| -> HostResult<Result<Handle, Caught>> {
            Ok(match job {
                JsJob::Eval(src) => e.evaluate_catching(realm, &src, ScriptOrigin::new("")),
                JsJob::Call { decl, this, args } => {
                    let f = match e.evaluate_catching(
                        realm,
                        &format!("({decl}\n)"),
                        ScriptOrigin::new(""),
                    ) {
                        Ok(f) => f,
                        Err(c) => return Ok(Err(c)),
                    };
                    if e.value_kind(realm, f).ok() != Some(lb_js::ValueKind::Function) {
                        e.release(f);
                        return Err(HostError::new(
                            "Given expression does not evaluate to a function",
                        ));
                    }
                    let mut argv = Vec::with_capacity(args.len());
                    for a in &args {
                        argv.push(call_arg(&agent, e, realm, a)?);
                    }
                    let this = match this {
                        Some(oid) => Arg::Handle(agent.object(&oid)?.handle),
                        None => Arg::Undefined,
                    };
                    let r = e.call_catching(realm, f, this, &argv);
                    e.release(f);
                    r
                }
            })
        })
        .ok_or_else(|| HostError::new("Page is closed"))??;
    settle(&ws, &w);
    let mut in_promise = false;
    let outcome: Result<Handle, Caught> = match first {
        Err(c) => Err(c),
        Ok(h) if !opts.await_promise => Ok(h),
        Ok(h) => loop {
            let notified = page.turn.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let alive = agent.world(ctx).is_ok_and(|x| x.realm == realm) && !page.closed.get();
            if !alive {
                return Err(HostError::new("Execution context was destroyed."));
            }
            let state = ws
                .with_engine(|e| e.promise_outcome(realm, h))
                .ok_or_else(|| HostError::new("Page is closed"))?
                .map_err(|e| HostError::new(e.message))?;
            match state {
                PromiseOutcome::NotAPromise => break Ok(h),
                PromiseOutcome::Fulfilled(v) => {
                    ws.with_engine(|e| e.release(h));
                    break Ok(v);
                }
                PromiseOutcome::Rejected(v) => {
                    ws.with_engine(|e| e.release(h));
                    in_promise = true;
                    break Err(Caught {
                        error: lb_js::JsError::new(JsErrorKind::Exception, "rejected"),
                        value: Some(v),
                    });
                }
                PromiseOutcome::Pending => notified.await,
            }
        },
    };
    // The document may have changed while waiting.
    let w = page
        .window()
        .ok_or_else(|| HostError::new("Page is closed"))?;
    if agent.world(ctx).map(|x| x.realm) != Ok(realm) {
        return Err(HostError::new("Execution context was destroyed."));
    }
    let group = opts.group.as_deref();
    ws.with_engine(|e| match outcome {
        Ok(h) => Ok(EvalResult {
            result: agent.remote(e, &w, realm, ctx, h, opts.by_value, opts.preview, group)?,
            exception_details: None,
        }),
        Err(mut c) => {
            if in_promise && let Some(v) = c.value {
                // Fill message/stack from the rejection value.
                if let Ok(Inspected::Object(o)) = e.inspect(realm, v, false) {
                    c.error.stack = o.description.clone();
                }
            }
            let details = agent.exception(e, &w, realm, ctx, c, in_promise, group);
            let result = details.exception.clone().unwrap_or_default();
            Ok(EvalResult {
                result,
                exception_details: Some(details),
            })
        }
    })
    .ok_or_else(|| HostError::new("Page is closed"))?
}

/// What [`run_js`] runs.
pub(crate) enum JsJob {
    Eval(String),
    Call {
        decl: String,
        /// Object id of `this`.
        this: Option<String>,
        args: Vec<CallArgument>,
    },
}

/// Result options of [`run_js`].
#[derive(Debug, Clone, Default)]
pub(crate) struct JsOpts {
    pub by_value: bool,
    pub preview: bool,
    pub await_promise: bool,
    pub group: Option<String>,
}

/// `Runtime.getProperties`.
pub(crate) fn get_properties(
    ws: &WorkerState,
    page: &PageState,
    req: &lb_cdp::GetPropertiesRequest,
) -> HostResult<Properties> {
    let agent = page
        .agent()
        .ok_or_else(|| HostError::new("Page is closed"))?;
    let w = page
        .window()
        .ok_or_else(|| HostError::new("Page is closed"))?;
    let obj = agent.object(&req.object_id)?;
    ws.with_engine(|e| {
        let props = e
            .own_properties(
                obj.realm,
                obj.handle,
                req.own_properties,
                req.accessor_properties_only,
            )
            .map_err(|x| HostError::new(x.message))?;
        let mut result = Vec::with_capacity(props.len());
        for p in props {
            let mut conv = |h: Option<Handle>| -> Option<RemoteObject> {
                h.and_then(|h| {
                    agent
                        .remote(
                            e,
                            &w,
                            obj.realm,
                            obj.ctx,
                            h,
                            false,
                            req.generate_preview,
                            obj.group.as_deref(),
                        )
                        .ok()
                })
            };
            let value = conv(p.value);
            let get = conv(p.get);
            let set = conv(p.set);
            let symbol = conv(p.symbol);
            let accessor = get.is_some() || set.is_some();
            result.push(PropertyDescriptor {
                name: p.name,
                value,
                writable: p.writable,
                get: if accessor {
                    get.or_else(|| Some(undefined_remote()))
                } else {
                    None
                },
                set: if accessor {
                    set.or_else(|| Some(undefined_remote()))
                } else {
                    None
                },
                configurable: p.configurable,
                enumerable: p.enumerable,
                was_thrown: None,
                is_own: Some(p.is_own),
                symbol,
            });
        }
        Ok(Properties {
            result,
            ..Default::default()
        })
    })
    .ok_or_else(|| HostError::new("Page is closed"))?
}

fn undefined_remote() -> RemoteObject {
    cdp_remote::remote_object(&CdpValue::Undefined, None, RemoteOptions::default())
}

/// The document of a window as a `DOM.Node` tree.
pub(crate) fn dom_node(agent: &Agent, w: &Window, n: NodeId, depth: i32) -> DomNode {
    let ids = agent.ids();
    let url = w.url().to_string();
    let info = lb_cdp::convert::DocInfo {
        url: &url,
        frame_id: &agent.target,
    };
    let mut node = w.with_doc(|d| lb_cdp::convert::dom_node(d, n, depth, &ids, &info));
    mark_frame_owners(agent, w, &mut node);
    node
}

/// Set `frameId` on `<iframe>` owners whose frame exists.
fn mark_frame_owners(agent: &Agent, w: &Window, node: &mut DomNode) {
    if node.local_name == "iframe"
        && let Some(n) = node_of(agent.current_seq(), node.backend_node_id)
        && let Some(c) = frames::child_for(w, n)
    {
        node.frame_id = Some(agent.frame_str(c.id));
    }
    if let Some(kids) = node.children.as_mut() {
        for k in kids {
            mark_frame_owners(agent, w, k);
        }
    }
}

/// The document node of a window.
pub(crate) const DOCUMENT: NodeId = Document::DOCUMENT;

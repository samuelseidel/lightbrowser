//! Server state and command dispatch.
//!
//! One [`Shared`] per server: browser contexts, targets, connections and
//! sessions, plus the execution contexts each page reported. Handlers never
//! hold a `RefCell` borrow across an `await` or a host call.
//!
//! Ordering: handlers queue everything they send (events and the reply) on
//! the server's single outgoing queue, behind any host events emitted during
//! the call. The pump ([`Shared::process`]) is the only place that writes to
//! connections, so the client sees host events before the reply of the
//! command that caused them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use lb_persona::PersonaProfile;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use crate::emulation;
use crate::host::*;
use crate::ids::IdGen;
use crate::protocol::{self, CdpError, Command, params};
use crate::server::Outgoing;
use crate::types::*;

pub(crate) type ConnId = u64;

/// Key of a connection's root session (messages without `sessionId`).
fn root_key(conn: ConnId) -> String {
    format!("#root{conn}")
}

#[derive(Debug)]
pub(crate) struct Session {
    /// `sessionId` on the wire; `None` for a connection's root session.
    pub wire: Option<String>,
    pub conn: ConnId,
    /// `None` for browser sessions.
    pub target: Option<TargetId>,
    /// Session that received `attachedToTarget` for this one.
    pub parent: Option<String>,
    pub domains: HashSet<String>,
    pub lifecycle: bool,
    /// `Target.setAutoAttach` on a browser session: `Some(waitForDebugger)`.
    pub auto_attach: Option<bool>,
    pub discover: bool,
}

impl Session {
    fn new(
        wire: Option<String>,
        conn: ConnId,
        target: Option<TargetId>,
        parent: Option<String>,
    ) -> Self {
        Session {
            wire,
            conn,
            target,
            parent,
            domains: HashSet::new(),
            lifecycle: false,
            auto_attach: None,
            discover: false,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ContextState {
    pub id: BrowserContextId,
    /// `disposeOnDetach`: dispose when this connection closes.
    pub owner: Option<ConnId>,
}

#[derive(Debug)]
pub(crate) struct TargetState {
    pub info: TargetInfo,
    pub viewport: Option<ViewportOverride>,
    /// Workers (ADR 0029): sessions attached with `waitingForDebugger`
    /// that have not sent `Runtime.runIfWaitingForDebugger` yet.
    pub waiters: HashSet<String>,
    /// Workers: the host was told to run the script. Pages: always true.
    pub resumed: bool,
}

impl TargetState {
    fn is_worker(&self) -> bool {
        matches!(
            self.info.kind.as_str(),
            "worker" | "shared_worker" | "service_worker"
        )
    }
}

#[derive(Debug)]
pub(crate) struct ConnState {
    pub tx: mpsc::UnboundedSender<String>,
}

#[derive(Debug, Default)]
pub(crate) struct State {
    pub browser_target_id: String,
    pub default_context: BrowserContextId,
    pub contexts: Vec<ContextState>,
    pub targets: Vec<TargetState>,
    pub conns: HashMap<ConnId, ConnState>,
    pub sessions: HashMap<String, Session>,
    pub exec: HashMap<TargetId, Vec<ExecutionContext>>,
    pub next_conn: ConnId,
}

impl State {
    pub fn target(&self, id: &str) -> Option<&TargetState> {
        self.targets.iter().find(|t| t.info.target_id == id)
    }

    fn target_mut(&mut self, id: &str) -> Option<&mut TargetState> {
        self.targets.iter_mut().find(|t| t.info.target_id == id)
    }

    fn context_of(&self, target: &str) -> Option<BrowserContextId> {
        self.target(target)?.info.browser_context_id.clone()
    }

    fn target_info(&self, id: &str) -> Option<TargetInfo> {
        let t = self.target(id)?;
        let mut info = t.info.clone();
        info.attached = self
            .sessions
            .values()
            .any(|s| s.target.as_deref() == Some(id));
        Some(info)
    }

    fn deliver(&self, key: &str, method: &str, params: Value) {
        if let Some(s) = self.sessions.get(key)
            && let Some(c) = self.conns.get(&s.conn)
        {
            let _ =
                c.tx.send(protocol::event(method, params, s.wire.as_deref()));
        }
    }

    fn main_world(&self, target: &str) -> Option<ExecutionContextId> {
        self.exec
            .get(target)?
            .iter()
            .find(|c| c.frame_id == target && c.world.is_main())
            .map(|c| c.id)
    }

    fn exec_context(&self, target: &str, id: ExecutionContextId) -> Option<&ExecutionContext> {
        self.exec.get(target)?.iter().find(|c| c.id == id)
    }
}

pub(crate) struct Shared<H> {
    pub host: Rc<H>,
    pub state: RefCell<State>,
    pub out: mpsc::UnboundedSender<Outgoing>,
    pub ids: IdGen,
    pub shutdown: watch::Sender<bool>,
    /// Path id of `ws://.../devtools/browser/<id>`.
    pub browser_ws_id: String,
}

impl<H: PageHost + 'static> Shared<H> {
    pub fn new(host: Rc<H>, out: mpsc::UnboundedSender<Outgoing>) -> Result<Rc<Self>, HostError> {
        let ids = IdGen::new();
        let default_context = ids.hex32();
        let browser_target_id = ids.hex32();
        let browser_ws_id = ids.uuid();
        host.set_event_sink(EventSink { tx: out.clone() });
        host.create_context(
            &default_context,
            ContextOptions {
                is_default: true,
                ..Default::default()
            },
        )?;
        let state = State {
            browser_target_id,
            default_context: default_context.clone(),
            contexts: vec![ContextState {
                id: default_context,
                owner: None,
            }],
            ..Default::default()
        };
        Ok(Rc::new(Shared {
            host,
            state: RefCell::new(state),
            out,
            ids,
            shutdown: watch::channel(false).0,
            browser_ws_id,
        }))
    }

    /// Queues an event for a session (ordered behind pending host events).
    fn queue_event(&self, key: &str, method: &str, params: Value) {
        let st = self.state.borrow();
        if let Some(s) = st.sessions.get(key) {
            let text = protocol::event(method, params, s.wire.as_deref());
            let _ = self.out.send(Outgoing::Send(s.conn, text));
        }
    }

    pub fn queue_text(&self, conn: ConnId, text: String) {
        let _ = self.out.send(Outgoing::Send(conn, text));
    }

    // ---- connections ----

    /// Registers a connection. `page` binds its root session to a target
    /// (`/devtools/page/<id>`); otherwise it is a browser connection.
    pub fn open_conn(&self, tx: mpsc::UnboundedSender<String>, page: Option<TargetId>) -> ConnId {
        let mut st = self.state.borrow_mut();
        st.next_conn += 1;
        let id = st.next_conn;
        st.conns.insert(id, ConnState { tx });
        st.sessions
            .insert(root_key(id), Session::new(None, id, page, None));
        id
    }

    /// Drops a connection's sessions and the contexts it asked to dispose
    /// on detach.
    pub fn close_conn(&self, conn: ConnId) {
        let (owned, mut workers): (Vec<BrowserContextId>, Vec<TargetId>) = {
            let mut st = self.state.borrow_mut();
            st.conns.remove(&conn);
            let gone: Vec<String> = st
                .sessions
                .iter()
                .filter(|(_, s)| s.conn == conn)
                .map(|(k, _)| k.clone())
                .collect();
            st.sessions.retain(|_, s| s.conn != conn);
            // Workers waiting for these sessions run now; every worker's
            // session count is reported again.
            let workers: Vec<TargetId> = st
                .targets
                .iter()
                .filter(|t| t.is_worker())
                .map(|t| t.info.target_id.clone())
                .collect();
            for t in st.targets.iter_mut() {
                t.waiters.retain(|w| !gone.contains(w));
            }
            let owned = st
                .contexts
                .iter()
                .filter(|c| c.owner == Some(conn))
                .map(|c| c.id.clone())
                .collect();
            (owned, workers)
        };
        for w in workers.drain(..) {
            self.count_worker_sessions(&w);
            self.maybe_resume(&w);
        }
        for ctx in owned {
            let _ = self.dispose_context(&ctx);
        }
    }

    // ---- pump ----

    /// Handles one outgoing item. Runs on the pump task only.
    pub fn process(&self, item: Outgoing) {
        match item {
            Outgoing::Send(conn, text) => {
                if let Some(c) = self.state.borrow().conns.get(&conn) {
                    let _ = c.tx.send(text);
                }
            }
            Outgoing::Host(ev) => self.on_host_event(*ev),
        }
    }

    fn on_host_event(&self, ev: HostEvent) {
        let t = ev.target_id.as_str();
        match ev.event {
            PageEvent::WorkerCreated {
                info,
                context,
                scope,
            } => {
                self.worker_created(t, info, context, scope);
                return;
            }
            PageEvent::WorkerDestroyed => {
                self.worker_destroyed(t);
                return;
            }
            _ => {}
        }
        let mut info_changed = false;
        {
            let mut st = self.state.borrow_mut();
            if st.target(t).is_none() {
                return;
            }
            match &ev.event {
                PageEvent::ExecutionContextCreated { context } => {
                    st.exec
                        .entry(t.to_string())
                        .or_default()
                        .push(context.clone());
                }
                PageEvent::ExecutionContextDestroyed { context_id } => {
                    if let Some(v) = st.exec.get_mut(t) {
                        v.retain(|c| c.id != *context_id);
                    }
                }
                PageEvent::ExecutionContextsCleared => {
                    st.exec.remove(t);
                }
                PageEvent::FrameNavigated { frame } if frame.id == t => {
                    let url = format!(
                        "{}{}",
                        frame.url,
                        frame.url_fragment.as_deref().unwrap_or("")
                    );
                    let ts = st.target_mut(t).expect("target");
                    if ts.info.title.is_empty() || ts.info.title == ts.info.url {
                        ts.info.title = url.clone();
                    }
                    ts.info.url = url;
                    info_changed = true;
                }
                PageEvent::NavigatedWithinDocument { frame_id, url } if frame_id == t => {
                    st.target_mut(t).expect("target").info.url = url.clone();
                    info_changed = true;
                }
                PageEvent::TitleChanged { title } => {
                    st.target_mut(t).expect("target").info.title = title.clone();
                    info_changed = true;
                }
                _ => {}
            }
        }
        let st = self.state.borrow();
        if info_changed && let Some(info) = st.target_info(t) {
            for (key, s) in &st.sessions {
                if s.discover {
                    st.deliver(
                        key,
                        "Target.targetInfoChanged",
                        json!({ "targetInfo": info }),
                    );
                }
            }
        }
        let msgs = translate(t, &ev.event);
        for (key, s) in &st.sessions {
            if s.target.as_deref() != Some(t) {
                continue;
            }
            for (gate, method, params) in &msgs {
                let open = match gate {
                    Gate::Domain(d) => s.domains.contains(*d),
                    Gate::Lifecycle => s.lifecycle && s.domains.contains("Page"),
                    Gate::Always => true,
                };
                if open {
                    st.deliver(key, method, params.clone());
                }
            }
        }
    }

    // ---- targets and contexts ----

    pub async fn create_target(
        self: &Rc<Self>,
        context: Option<String>,
        url: &str,
    ) -> Result<TargetId, CdpError> {
        let context = {
            let st = self.state.borrow();
            let ctx = context.unwrap_or_else(|| st.default_context.clone());
            if !st.contexts.iter().any(|c| c.id == ctx) {
                return Err(CdpError::server(
                    "Failed to find browser context with id ".to_string() + &ctx,
                ));
            }
            ctx
        };
        let id = self.ids.hex32();
        self.state.borrow_mut().targets.push(TargetState {
            info: TargetInfo {
                target_id: id.clone(),
                kind: "page".into(),
                title: "about:blank".into(),
                url: "about:blank".into(),
                attached: false,
                opener_id: None,
                can_access_opener: false,
                browser_context_id: Some(context.clone()),
                ..Default::default()
            },
            viewport: None,
            waiters: HashSet::new(),
            resumed: true,
        });
        if let Err(e) = self.host.create_page(&context, &id).await {
            self.state
                .borrow_mut()
                .targets
                .retain(|t| t.info.target_id != id);
            return Err(CdpError::server(e.0));
        }
        let (discover, attachers): (Vec<String>, Vec<(String, bool)>) = {
            let st = self.state.borrow();
            (
                st.sessions
                    .iter()
                    .filter(|(_, s)| s.discover)
                    .map(|(k, _)| k.clone())
                    .collect(),
                st.sessions
                    .iter()
                    .filter(|(_, s)| s.target.is_none())
                    .filter_map(|(k, s)| s.auto_attach.map(|w| (k.clone(), w)))
                    .collect(),
            )
        };
        let info = self.state.borrow().target_info(&id).expect("target");
        for key in &discover {
            self.queue_event(key, "Target.targetCreated", json!({ "targetInfo": info }));
        }
        for (key, wait) in attachers {
            self.attach(&key, &id, wait);
        }
        if !url.is_empty() && url != "about:blank" {
            let sh = self.clone();
            let (id2, url) = (id.clone(), url.to_string());
            tokio::task::spawn_local(async move {
                let req = NavigateRequest {
                    url,
                    ..Default::default()
                };
                let _ = sh.host.navigate(&id2, req).await;
            });
        }
        Ok(id)
    }

    /// Attaches a new flat session for `target` under `parent` and sends
    /// `Target.attachedToTarget` to the parent. Returns the session id.
    fn attach(&self, parent: &str, target: &str, waiting: bool) -> String {
        let sid = self.ids.hex32();
        let (info, waiting) = {
            let mut st = self.state.borrow_mut();
            let Some(conn) = st.sessions.get(parent).map(|s| s.conn) else {
                return sid;
            };
            // A worker that has not run yet waits for this session.
            let waiting = match st.target_mut(target) {
                Some(t) if t.is_worker() && !t.resumed && waiting => {
                    t.waiters.insert(sid.clone());
                    true
                }
                Some(t) if t.is_worker() => false,
                _ => waiting,
            };
            st.sessions.insert(
                sid.clone(),
                Session::new(
                    Some(sid.clone()),
                    conn,
                    Some(target.to_string()),
                    Some(parent.to_string()),
                ),
            );
            (st.target_info(target), waiting)
        };
        self.count_worker_sessions(target);
        if let Some(info) = info {
            self.queue_event(
                parent,
                "Target.attachedToTarget",
                json!({ "sessionId": sid, "targetInfo": info, "waitingForDebugger": waiting }),
            );
        }
        sid
    }

    fn detach_session(&self, sid: &str) {
        let removed = self.state.borrow_mut().sessions.remove(sid);
        if let Some(s) = removed
            && let (Some(parent), Some(target)) = (s.parent, s.target)
        {
            // Chrome lets a waiting worker run when its session detaches.
            self.stop_waiting(&target, sid);
            self.count_worker_sessions(&target);
            self.queue_event(
                &parent,
                "Target.detachedFromTarget",
                json!({ "sessionId": sid, "targetId": target }),
            );
        }
    }

    /// `sid` no longer holds worker `target` (it ran
    /// `Runtime.runIfWaitingForDebugger` or detached).
    fn stop_waiting(&self, target: &str, sid: &str) {
        let removed = self
            .state
            .borrow_mut()
            .target_mut(target)
            .is_some_and(|t| t.waiters.remove(sid));
        if removed {
            self.maybe_resume(target);
        }
    }

    /// Tell the host how many sessions worker `target` has.
    fn count_worker_sessions(&self, target: &str) {
        let n = {
            let st = self.state.borrow();
            if !st.target(target).is_some_and(|t| t.is_worker()) {
                return;
            }
            st.sessions
                .values()
                .filter(|s| s.target.as_deref() == Some(target))
                .count()
        };
        self.host.worker_sessions(target, n);
    }

    /// Run worker `target`'s script once no session waits for it.
    fn maybe_resume(&self, target: &str) {
        let go = match self.state.borrow_mut().target_mut(target) {
            Some(t) if t.is_worker() && !t.resumed && t.waiters.is_empty() => {
                t.resumed = true;
                true
            }
            _ => false,
        };
        if go {
            self.host.resume_worker(target);
        }
    }

    /// A worker target appeared (ADR 0029): announce it to discovering
    /// sessions, attach it where auto-attach is on (dedicated workers to
    /// sessions of their parent target, shared and service workers to
    /// browser sessions), and let it run unless a session waits.
    fn worker_created(
        &self,
        id: &str,
        mut info: TargetInfo,
        context: ExecutionContext,
        scope: Option<String>,
    ) {
        info.target_id = id.to_string();
        info.attached = false;
        let (discover, attachers) = {
            let mut st = self.state.borrow_mut();
            if st.target(id).is_some() {
                return;
            }
            let parent = info.parent_id.clone();
            let dedicated = info.kind == "worker";
            let ctx = info.browser_context_id.clone();
            // Pages of the context inside a service worker's scope.
            let in_scope: HashSet<TargetId> = match &scope {
                Some(sc) => st
                    .targets
                    .iter()
                    .filter(|t| {
                        t.info.kind == "page"
                            && t.info.browser_context_id == ctx
                            && t.info.url.starts_with(sc.as_str())
                    })
                    .map(|t| t.info.target_id.clone())
                    .collect(),
                None => HashSet::new(),
            };
            st.targets.push(TargetState {
                info,
                viewport: None,
                waiters: HashSet::new(),
                resumed: false,
            });
            st.exec.insert(id.to_string(), vec![context]);
            let discover: Vec<String> = st
                .sessions
                .iter()
                .filter(|(_, s)| s.discover)
                .map(|(k, _)| k.clone())
                .collect();
            let mut attachers: Vec<(String, bool)> = st
                .sessions
                .iter()
                .filter(|(_, s)| {
                    if dedicated {
                        parent.is_some() && s.target == parent
                    } else {
                        s.target.as_ref().is_none_or(|t| in_scope.contains(t))
                    }
                })
                .filter_map(|(k, s)| s.auto_attach.map(|w| (k.clone(), w)))
                .collect();
            attachers.sort();
            (discover, attachers)
        };
        let info = self.state.borrow().target_info(id);
        if let Some(info) = info {
            for key in &discover {
                self.queue_event(key, "Target.targetCreated", json!({ "targetInfo": info }));
            }
        }
        for (key, wait) in attachers {
            self.attach(&key, id, wait);
        }
        self.maybe_resume(id);
    }

    /// A worker target ended: detach its sessions, destroy the target.
    fn worker_destroyed(&self, id: &str) {
        let sessions: Vec<String> = {
            let st = self.state.borrow();
            if st.target(id).is_none() {
                return;
            }
            st.sessions
                .iter()
                .filter(|(_, s)| s.target.as_deref() == Some(id) && s.wire.is_some())
                .map(|(k, _)| k.clone())
                .collect()
        };
        for sid in sessions {
            self.detach_session(&sid);
        }
        let discover: Vec<String> = {
            let mut st = self.state.borrow_mut();
            st.targets.retain(|t| t.info.target_id != id);
            st.exec.remove(id);
            st.sessions
                .iter()
                .filter(|(_, s)| s.discover)
                .map(|(k, _)| k.clone())
                .collect()
        };
        for key in discover {
            self.queue_event(&key, "Target.targetDestroyed", json!({ "targetId": id }));
        }
    }

    /// Whether `id` is a worker target.
    pub(crate) fn is_worker(&self, id: &str) -> bool {
        self.state
            .borrow()
            .target(id)
            .is_some_and(|t| t.is_worker())
    }

    pub fn close_target(&self, id: &str) -> Result<(), CdpError> {
        if self.state.borrow().target(id).is_none() {
            return Err(CdpError::server("No target with given id found"));
        }
        // A page's dedicated workers (and theirs) go first, innermost
        // first, as Chrome detaches them before the page.
        let mut doomed: Vec<TargetId> = Vec::new();
        {
            let st = self.state.borrow();
            let mut frontier = vec![id.to_string()];
            while let Some(p) = frontier.pop() {
                for t in &st.targets {
                    if t.info.kind == "worker" && t.info.parent_id.as_deref() == Some(p.as_str()) {
                        doomed.push(t.info.target_id.clone());
                        frontier.push(t.info.target_id.clone());
                    }
                }
            }
        }
        for w in doomed.iter().rev() {
            self.worker_destroyed(w);
        }
        let host_result = self.host.close_page(id);
        let sessions: Vec<String> = self
            .state
            .borrow()
            .sessions
            .iter()
            .filter(|(_, s)| s.target.as_deref() == Some(id) && s.wire.is_some())
            .map(|(k, _)| k.clone())
            .collect();
        for sid in sessions {
            self.detach_session(&sid);
        }
        let discover: Vec<String> = {
            let mut st = self.state.borrow_mut();
            st.targets.retain(|t| t.info.target_id != id);
            st.exec.remove(id);
            st.sessions
                .iter()
                .filter(|(_, s)| s.discover)
                .map(|(k, _)| k.clone())
                .collect()
        };
        for key in discover {
            self.queue_event(&key, "Target.targetDestroyed", json!({ "targetId": id }));
        }
        host_result.map_err(|e| CdpError::server(e.0))
    }

    pub fn dispose_context(&self, ctx: &str) -> Result<(), CdpError> {
        let pages: Vec<TargetId> = {
            let st = self.state.borrow();
            if ctx == st.default_context {
                return Err(CdpError::server(
                    "Cannot dispose the default browser context",
                ));
            }
            if !st.contexts.iter().any(|c| c.id == ctx) {
                return Err(CdpError::server(
                    "Failed to find context with id ".to_string() + ctx,
                ));
            }
            st.targets
                .iter()
                .filter(|t| t.info.browser_context_id.as_deref() == Some(ctx))
                .map(|t| t.info.target_id.clone())
                .collect()
        };
        for p in pages {
            let _ = self.close_target(&p);
        }
        self.state.borrow_mut().contexts.retain(|c| c.id != ctx);
        self.host
            .dispose_context(ctx)
            .map_err(|e| CdpError::server(e.0))
    }

    /// Product string for `Browser.getVersion` and `/json/version`.
    pub fn version_info(&self) -> Value {
        let persona = self.default_persona();
        let major: u32 = persona
            .browser
            .version
            .split('.')
            .next()
            .and_then(|m| m.parse().ok())
            .unwrap_or(0);
        let name = if persona.browser.name.is_empty() {
            persona.browser.family.as_str().to_string()
        } else {
            persona.browser.name.replace(' ', "")
        };
        let js = match persona.browser.family {
            lb_persona::BrowserFamily::Chrome => format!("{}.{}.0.0", major / 10, major % 10),
            lb_persona::BrowserFamily::Firefox => String::new(),
        };
        json!({
            "protocolVersion": "1.3",
            "product": format!("{name}/{}", persona.browser.version),
            "revision": format!("@lightbrowser-{}", env!("CARGO_PKG_VERSION")),
            "userAgent": persona.browser.user_agent,
            "jsVersion": js,
        })
    }

    pub fn default_persona(&self) -> Arc<PersonaProfile> {
        let ctx = self.state.borrow().default_context.clone();
        self.host.persona(&ctx)
    }

    /// Page targets for `/json/list`.
    pub fn page_infos(&self) -> Vec<TargetInfo> {
        let st = self.state.borrow();
        st.targets
            .iter()
            .filter_map(|t| st.target_info(&t.info.target_id))
            .collect()
    }
}

/// Which session flag lets an event through.
enum Gate {
    Domain(&'static str),
    Lifecycle,
    /// Every session of the target (`Inspector.*`).
    Always,
}

/// Turns a page event into the CDP events it produces.
fn translate(target: &str, ev: &PageEvent) -> Vec<(Gate, String, Value)> {
    let one = |d: &'static str, m: &str, p: Value| vec![(Gate::Domain(d), m.to_string(), p)];
    match ev {
        PageEvent::FrameAttached {
            frame_id,
            parent_frame_id,
        } => one(
            "Page",
            "Page.frameAttached",
            json!({ "frameId": frame_id, "parentFrameId": parent_frame_id }),
        ),
        PageEvent::FrameDetached { frame_id } => one(
            "Page",
            "Page.frameDetached",
            json!({ "frameId": frame_id, "reason": "remove" }),
        ),
        PageEvent::FrameStartedLoading { frame_id } => one(
            "Page",
            "Page.frameStartedLoading",
            json!({ "frameId": frame_id }),
        ),
        PageEvent::FrameStoppedLoading { frame_id } => one(
            "Page",
            "Page.frameStoppedLoading",
            json!({ "frameId": frame_id }),
        ),
        PageEvent::FrameNavigated { frame } => one(
            "Page",
            "Page.frameNavigated",
            json!({ "frame": frame, "type": "Navigation" }),
        ),
        PageEvent::NavigatedWithinDocument { frame_id, url } => one(
            "Page",
            "Page.navigatedWithinDocument",
            json!({ "frameId": frame_id, "url": url, "navigationType": "fragment" }),
        ),
        PageEvent::Lifecycle {
            frame_id,
            loader_id,
            name,
            timestamp,
        } => {
            let mut v = Vec::new();
            if frame_id == target {
                match name.as_str() {
                    "DOMContentLoaded" => v.push((
                        Gate::Domain("Page"),
                        "Page.domContentEventFired".to_string(),
                        json!({ "timestamp": timestamp }),
                    )),
                    "load" => v.push((
                        Gate::Domain("Page"),
                        "Page.loadEventFired".to_string(),
                        json!({ "timestamp": timestamp }),
                    )),
                    _ => {}
                }
            }
            v.push((
                Gate::Lifecycle,
                "Page.lifecycleEvent".to_string(),
                json!({ "frameId": frame_id, "loaderId": loader_id, "name": name, "timestamp": timestamp }),
            ));
            v
        }
        PageEvent::TitleChanged { .. } => Vec::new(),
        PageEvent::ExecutionContextCreated { context } => one(
            "Runtime",
            "Runtime.executionContextCreated",
            json!({ "context": context.to_json() }),
        ),
        PageEvent::ExecutionContextDestroyed { context_id } => one(
            "Runtime",
            "Runtime.executionContextDestroyed",
            json!({ "executionContextId": context_id }),
        ),
        PageEvent::ExecutionContextsCleared => {
            one("Runtime", "Runtime.executionContextsCleared", json!({}))
        }
        PageEvent::ConsoleApiCalled {
            kind,
            args,
            context_id,
            timestamp,
            stack_trace,
        } => {
            let mut p = json!({ "type": kind, "args": args, "executionContextId": context_id, "timestamp": timestamp });
            if let Some(st) = stack_trace {
                p["stackTrace"] = serde_json::to_value(st).unwrap_or(Value::Null);
            }
            one("Runtime", "Runtime.consoleAPICalled", p)
        }
        PageEvent::ExceptionThrown { timestamp, details } => one(
            "Runtime",
            "Runtime.exceptionThrown",
            json!({ "timestamp": timestamp, "exceptionDetails": details }),
        ),
        PageEvent::BindingCalled {
            name,
            payload,
            context_id,
        } => one(
            "Runtime",
            "Runtime.bindingCalled",
            json!({ "name": name, "payload": payload, "executionContextId": context_id }),
        ),
        PageEvent::JavascriptDialogOpening {
            url,
            message,
            kind,
            default_prompt,
        } => {
            let mut p =
                json!({ "url": url, "message": message, "type": kind, "hasBrowserHandler": false });
            if let Some(d) = default_prompt {
                p["defaultPrompt"] = Value::String(d.clone());
            }
            one("Page", "Page.javascriptDialogOpening", p)
        }
        PageEvent::RequestWillBeSent(e) => one("Network", "Network.requestWillBeSent", to_value(e)),
        PageEvent::ResponseReceived(e) => one("Network", "Network.responseReceived", to_value(e)),
        PageEvent::LoadingFinished(e) => one("Network", "Network.loadingFinished", to_value(e)),
        PageEvent::LoadingFailed(e) => one("Network", "Network.loadingFailed", to_value(e)),
        PageEvent::RequestPaused(e) => one("Fetch", "Fetch.requestPaused", to_value(e)),
        PageEvent::AuthRequired(e) => one("Fetch", "Fetch.authRequired", to_value(e)),
        // Handled before translation.
        PageEvent::WorkerCreated { .. } | PageEvent::WorkerDestroyed => Vec::new(),
        PageEvent::Custom { method, params } => {
            let domain = method.split('.').next().unwrap_or("");
            // Only domains we track; others would never be enabled.
            match [
                "Page",
                "Runtime",
                "Network",
                "Fetch",
                "DOM",
                "Log",
                "Performance",
                "Security",
                "Accessibility",
            ]
            .iter()
            .find(|d| **d == domain)
            {
                Some(d) => vec![(Gate::Domain(d), method.clone(), params.clone())],
                None if domain == "Inspector" => {
                    vec![(Gate::Always, method.clone(), params.clone())]
                }
                None => Vec::new(),
            }
        }
    }
}

fn to_value<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn host_err(e: HostError) -> CdpError {
    CdpError::server(e.0)
}

/// One command being handled.
struct Call<'a, H> {
    sh: &'a Rc<Shared<H>>,
    method: &'a str,
    key: String,
    target: Option<TargetId>,
    p: &'a Value,
}

impl<H: PageHost + 'static> Call<'_, H> {
    /// The page this session is attached to. Page-only methods on a browser
    /// session get Chrome's "wasn't found" error.
    fn page(&self) -> Result<TargetId, CdpError> {
        self.target
            .clone()
            .ok_or_else(|| CdpError::method_not_found(self.method))
    }

    fn enable(&self, domain: &str, on: bool) -> Result<Value, CdpError> {
        let mut st = self.sh.state.borrow_mut();
        if let Some(s) = st.sessions.get_mut(&self.key) {
            if on {
                s.domains.insert(domain.to_string());
            } else {
                s.domains.remove(domain);
            }
        }
        Ok(json!({}))
    }

    fn context_of_page(&self) -> Result<BrowserContextId, CdpError> {
        let t = self.page()?;
        self.sh
            .state
            .borrow()
            .context_of(&t)
            .ok_or_else(|| CdpError::server("No target with given id found"))
    }

    /// The page's context, or the default context on a browser session.
    fn cookie_context(&self, explicit: Option<String>) -> BrowserContextId {
        if let Some(c) = explicit {
            return c;
        }
        let st = self.sh.state.borrow();
        self.target
            .as_deref()
            .and_then(|t| st.context_of(t))
            .unwrap_or_else(|| st.default_context.clone())
    }

    fn persona(&self) -> Arc<PersonaProfile> {
        match self.context_of_page() {
            Ok(ctx) => self.sh.host.persona(&ctx),
            Err(_) => self.sh.default_persona(),
        }
    }

    fn node_ref(&self) -> Result<NodeRef, CdpError> {
        node_ref(self.p)
    }
}

fn node_ref(p: &Value) -> Result<NodeRef, CdpError> {
    if let Some(id) = p.get("backendNodeId").and_then(Value::as_i64) {
        return Ok(NodeRef::Backend(id));
    }
    if let Some(id) = p.get("nodeId").and_then(Value::as_i64) {
        return Ok(NodeRef::Backend(id));
    }
    if let Some(id) = p.get("objectId").and_then(Value::as_str) {
        return Ok(NodeRef::Object(id.to_string()));
    }
    Err(CdpError::server(
        "Either nodeId, backendNodeId or objectId must be specified",
    ))
}

fn str_param<'a>(p: &'a Value, key: &str) -> Result<&'a str, CdpError> {
    p.get(key).and_then(Value::as_str).ok_or_else(|| {
        CdpError::invalid_params(format!(
            "Failed to deserialize params.{key} - BINDINGS: mandatory field missing"
        ))
    })
}

fn bool_param(p: &Value, key: &str) -> bool {
    p.get(key).and_then(Value::as_bool).unwrap_or(false)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvaluateParams {
    expression: String,
    #[serde(default)]
    context_id: Option<ExecutionContextId>,
    #[serde(default)]
    unique_context_id: Option<String>,
    #[serde(default)]
    object_group: Option<String>,
    #[serde(default)]
    return_by_value: bool,
    #[serde(default)]
    await_promise: bool,
    #[serde(default)]
    generate_preview: bool,
    #[serde(default)]
    user_gesture: bool,
    #[serde(default)]
    silent: bool,
    #[serde(default, rename = "includeCommandLineAPI")]
    include_command_line_api: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallFunctionParams {
    function_declaration: String,
    #[serde(default)]
    object_id: Option<String>,
    #[serde(default)]
    execution_context_id: Option<ExecutionContextId>,
    #[serde(default)]
    arguments: Vec<CallArgument>,
    #[serde(default)]
    object_group: Option<String>,
    #[serde(default)]
    return_by_value: bool,
    #[serde(default)]
    await_promise: bool,
    #[serde(default)]
    generate_preview: bool,
    #[serde(default)]
    user_gesture: bool,
    #[serde(default)]
    silent: bool,
}

/// Handles one command and returns its `result`.
pub(crate) async fn dispatch<H: PageHost + 'static>(
    sh: &Rc<Shared<H>>,
    conn: ConnId,
    cmd: &Command,
) -> Result<Value, CdpError> {
    let (key, target) = {
        let st = sh.state.borrow();
        let key = match &cmd.session_id {
            Some(s) => match st.sessions.get(s) {
                Some(sess) if sess.conn == conn => s.clone(),
                _ => return Err(CdpError::session_not_found()),
            },
            None => root_key(conn),
        };
        let target = st.sessions.get(&key).and_then(|s| s.target.clone());
        (key, target)
    };
    if !crate::methods::is_supported(&cmd.method) {
        return Err(CdpError::method_not_found(&cmd.method));
    }
    // Worker sessions (ADR 0029) answer the domains a worker has in
    // Chrome; Playwright uses Runtime, Network, Fetch, Target and
    // `Emulation.setUserAgentOverride` there.
    if let Some(t) = &target
        && sh.is_worker(t)
    {
        let ok = match cmd.domain() {
            "Runtime" | "Network" | "Fetch" | "Target" | "Log" | "Security" => true,
            "Emulation" => cmd.method == "Emulation.setUserAgentOverride",
            _ => false,
        };
        if !ok {
            return Err(CdpError::method_not_found(&cmd.method));
        }
    }
    let c = Call {
        sh,
        method: &cmd.method,
        key,
        target,
        p: &cmd.params,
    };
    match cmd.domain() {
        "Browser" => browser_domain(&c).await,
        "Target" => target_domain(&c).await,
        "Page" => page_domain(&c).await,
        "Runtime" => runtime_domain(&c).await,
        "DOM" => dom_domain(&c),
        "Input" => input_domain(&c).await,
        "Network" | "Storage" => network_domain(&c),
        "Fetch" => fetch_domain(&c),
        "Emulation" => emulation_domain(&c),
        "Accessibility" => accessibility_domain(&c),
        "Security" | "Log" | "Performance" => misc_domain(&c),
        "LightBrowser" => lightbrowser_domain(&c).await,
        _ => Err(CdpError::method_not_found(&cmd.method)),
    }
}

async fn browser_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    match c.method {
        "Browser.getVersion" => Ok(c.sh.version_info()),
        "Browser.close" => {
            let pages: Vec<TargetId> =
                c.sh.page_infos()
                    .into_iter()
                    .filter(|t| t.kind == "page")
                    .map(|t| t.target_id)
                    .collect();
            for p in pages {
                let _ = c.sh.close_target(&p);
            }
            // Stop after the reply has been written.
            let sh = c.sh.clone();
            tokio::task::spawn_local(async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let _ = sh.shutdown.send(true);
            });
            Ok(json!({}))
        }
        "Browser.setDownloadBehavior" => {
            // Downloads are never written to disk; every behavior is accepted
            // and nothing is downloaded.
            let b = str_param(c.p, "behavior")?;
            match b {
                "deny" | "allow" | "allowAndName" | "default" => Ok(json!({})),
                _ => Err(CdpError::invalid_params(format!("unknown behavior {b:?}"))),
            }
        }
        "Browser.getWindowForTarget" => Err(CdpError::server("Browser window not found")),
        _ => Err(CdpError::method_not_found(c.method)),
    }
}

async fn target_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    let sh = c.sh;
    match c.method {
        "Target.setAutoAttach" => {
            let auto = bool_param(c.p, "autoAttach");
            let wait = bool_param(c.p, "waitForDebuggerOnStart");
            let flatten = bool_param(c.p, "flatten");
            if let Some(t) = &c.target {
                // Page and worker sessions: their dedicated workers (no
                // out-of-process iframes).
                if auto && !flatten {
                    return Err(CdpError::server(
                        "Only flatten mode is supported (pass flatten: true)",
                    ));
                }
                let workers: Vec<TargetId> = {
                    let mut st = sh.state.borrow_mut();
                    let was = st.sessions.get(&c.key).and_then(|s| s.auto_attach);
                    if let Some(s) = st.sessions.get_mut(&c.key) {
                        s.auto_attach = auto.then_some(wait);
                    }
                    if !auto || was.is_some() {
                        Vec::new()
                    } else {
                        st.targets
                            .iter()
                            .filter(|x| {
                                x.info.kind == "worker"
                                    && x.info.parent_id.as_deref() == Some(t.as_str())
                            })
                            .map(|x| x.info.target_id.clone())
                            .collect()
                    }
                };
                for w in workers {
                    sh.attach(&c.key, &w, wait);
                }
                return Ok(json!({}));
            }
            if auto && !flatten {
                return Err(CdpError::server(
                    "Only flatten mode is supported (pass flatten: true)",
                ));
            }
            let pages: Vec<TargetId> = {
                let mut st = sh.state.borrow_mut();
                let was = st.sessions.get(&c.key).and_then(|s| s.auto_attach);
                if let Some(s) = st.sessions.get_mut(&c.key) {
                    s.auto_attach = auto.then_some(wait);
                }
                if !auto || was.is_some() {
                    Vec::new()
                } else {
                    // Attach to existing pages, shared and service workers
                    // not yet attached through this session.
                    st.targets
                        .iter()
                        .filter(|t| t.info.kind != "worker")
                        .map(|t| t.info.target_id.clone())
                        .filter(|id| {
                            !st.sessions.values().any(|s| {
                                s.parent.as_deref() == Some(&c.key)
                                    && s.target.as_deref() == Some(id)
                            })
                        })
                        .collect()
                }
            };
            for p in pages {
                sh.attach(&c.key, &p, wait);
            }
            Ok(json!({}))
        }
        "Target.setDiscoverTargets" => {
            let on = bool_param(c.p, "discover");
            let infos = {
                let mut st = sh.state.borrow_mut();
                let was = st.sessions.get(&c.key).is_some_and(|s| s.discover);
                if let Some(s) = st.sessions.get_mut(&c.key) {
                    s.discover = on;
                }
                if on && !was {
                    st.targets
                        .iter()
                        .filter_map(|t| st.target_info(&t.info.target_id))
                        .collect()
                } else {
                    Vec::new()
                }
            };
            for info in infos {
                sh.queue_event(
                    &c.key,
                    "Target.targetCreated",
                    json!({ "targetInfo": info }),
                );
            }
            Ok(json!({}))
        }
        "Target.createBrowserContext" => {
            let id = sh.ids.hex32();
            let opts = ContextOptions {
                is_default: false,
                proxy_server: c
                    .p
                    .get("proxyServer")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                proxy_bypass_list: c
                    .p
                    .get("proxyBypassList")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            };
            sh.host.create_context(&id, opts).map_err(host_err)?;
            let conn = sh.state.borrow().sessions.get(&c.key).map(|s| s.conn);
            sh.state.borrow_mut().contexts.push(ContextState {
                id: id.clone(),
                owner: if bool_param(c.p, "disposeOnDetach") {
                    conn
                } else {
                    None
                },
            });
            Ok(json!({ "browserContextId": id }))
        }
        "Target.disposeBrowserContext" => {
            sh.dispose_context(str_param(c.p, "browserContextId")?)?;
            Ok(json!({}))
        }
        "Target.getBrowserContexts" => {
            let st = sh.state.borrow();
            let ids: Vec<&str> = st
                .contexts
                .iter()
                .filter(|x| x.id != st.default_context)
                .map(|x| x.id.as_str())
                .collect();
            Ok(json!({ "browserContextIds": ids, "defaultBrowserContextId": st.default_context }))
        }
        "Target.createTarget" => {
            let url =
                c.p.get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("about:blank");
            let ctx =
                c.p.get("browserContextId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            let id = sh.create_target(ctx, url).await?;
            Ok(json!({ "targetId": id }))
        }
        "Target.attachToTarget" => {
            let id = str_param(c.p, "targetId")?;
            if sh.state.borrow().target(id).is_none() {
                return Err(CdpError::server("No target with given id found"));
            }
            if c.p.get("flatten").and_then(Value::as_bool) == Some(false) {
                return Err(CdpError::server(
                    "Only flatten mode is supported (pass flatten: true)",
                ));
            }
            let sid = sh.attach(&c.key, id, false);
            Ok(json!({ "sessionId": sid }))
        }
        "Target.attachToBrowserTarget" => {
            let sid = sh.ids.hex32();
            let (conn, browser_id) = {
                let st = sh.state.borrow();
                (
                    st.sessions.get(&c.key).map(|s| s.conn).unwrap_or(0),
                    st.browser_target_id.clone(),
                )
            };
            sh.state.borrow_mut().sessions.insert(
                sid.clone(),
                Session::new(Some(sid.clone()), conn, None, Some(c.key.clone())),
            );
            sh.queue_event(
                &c.key,
                "Target.attachedToTarget",
                json!({
                    "sessionId": sid,
                    "targetInfo": browser_target_info(&browser_id),
                    "waitingForDebugger": false
                }),
            );
            Ok(json!({ "sessionId": sid }))
        }
        "Target.detachFromTarget" => {
            let sid = match c.p.get("sessionId").and_then(Value::as_str) {
                Some(s) => s.to_string(),
                None => {
                    let tid = str_param(c.p, "targetId")?;
                    let st = sh.state.borrow();
                    st.sessions
                        .iter()
                        .find(|(_, s)| {
                            s.parent.as_deref() == Some(&c.key) && s.target.as_deref() == Some(tid)
                        })
                        .map(|(k, _)| k.clone())
                        .ok_or_else(|| CdpError::server("No session for given target id found"))?
                }
            };
            if !sh.state.borrow().sessions.contains_key(&sid) {
                return Err(CdpError::server("No session with given id found"));
            }
            sh.detach_session(&sid);
            Ok(json!({}))
        }
        "Target.closeTarget" => {
            sh.close_target(str_param(c.p, "targetId")?)?;
            Ok(json!({ "success": true }))
        }
        "Target.activateTarget" => {
            let id = str_param(c.p, "targetId")?;
            if sh.state.borrow().target(id).is_none() {
                return Err(CdpError::server("No target with given id found"));
            }
            Ok(json!({}))
        }
        "Target.getTargets" => Ok(json!({ "targetInfos": sh.page_infos() })),
        "Target.getTargetInfo" => {
            let st = sh.state.borrow();
            let id =
                c.p.get("targetId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(c.target.clone());
            match id {
                None => Ok(json!({ "targetInfo": browser_target_info(&st.browser_target_id) })),
                Some(id) if id == st.browser_target_id => {
                    Ok(json!({ "targetInfo": browser_target_info(&st.browser_target_id) }))
                }
                Some(id) => st
                    .target_info(&id)
                    .map(|i| json!({ "targetInfo": i }))
                    .ok_or_else(|| CdpError::server("No target with given id found")),
            }
        }
        _ => Err(CdpError::method_not_found(c.method)),
    }
}

fn browser_target_info(id: &str) -> Value {
    json!({
        "targetId": id,
        "type": "browser",
        "title": "",
        "url": "",
        "attached": true,
        "canAccessOpener": false,
    })
}

async fn page_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    let t = c.page()?;
    let host = &c.sh.host;
    match c.method {
        "Page.enable" => c.enable("Page", true),
        "Page.disable" => c.enable("Page", false),
        "Page.setLifecycleEventsEnabled" => {
            let on = bool_param(c.p, "enabled");
            if let Some(s) = c.sh.state.borrow_mut().sessions.get_mut(&c.key) {
                s.lifecycle = on;
            }
            Ok(json!({}))
        }
        "Page.getFrameTree" => {
            let tree = host.frame_tree(&t).map_err(host_err)?;
            Ok(json!({ "frameTree": tree }))
        }
        "Page.navigate" => {
            let req = NavigateRequest {
                url: str_param(c.p, "url")?.to_string(),
                referrer: c
                    .p
                    .get("referrer")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                referrer_policy: c
                    .p
                    .get("referrerPolicy")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                frame_id: c
                    .p
                    .get("frameId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                transition_type: c
                    .p
                    .get("transitionType")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            };
            if url::Url::parse(&req.url).is_err() {
                return Err(CdpError::invalid_params("Cannot navigate to invalid URL"));
            }
            let r = host.navigate(&t, req).await.map_err(host_err)?;
            Ok(to_value(&r))
        }
        "Page.reload" => {
            host.reload(&t, bool_param(c.p, "ignoreCache"))
                .await
                .map_err(host_err)?;
            Ok(json!({}))
        }
        "Page.stopLoading" | "Page.bringToFront" | "Page.setInterceptFileChooserDialog" => {
            Ok(json!({}))
        }
        "Page.getNavigationHistory" => {
            let info = c.sh.state.borrow().target_info(&t).unwrap_or_default();
            Ok(json!({
                "currentIndex": 0,
                "entries": [{ "id": 0, "url": info.url, "userTypedURL": info.url, "title": info.title, "transitionType": "typed" }]
            }))
        }
        "Page.getLayoutMetrics" => {
            let p = c.persona();
            let vp = c.sh.state.borrow().target(&t).and_then(|x| x.viewport);
            let (w, h) = vp.map_or((p.screen.inner_width, p.screen.inner_height), |v| {
                (v.width, v.height)
            });
            let dpr = p.screen.device_pixel_ratio;
            let m = host.page_metrics(&t).map_err(host_err)?;
            let (sx, sy) = m.map_or((0.0, 0.0), |m| (m.scroll_x, m.scroll_y));
            let (cw, ch) = m.map_or((f64::from(w), f64::from(h)), |m| {
                (m.content_width, m.content_height)
            });
            let layout = json!({ "pageX": sx, "pageY": sy, "clientWidth": w, "clientHeight": h });
            let visual = json!({ "offsetX": 0, "offsetY": 0, "pageX": sx, "pageY": sy, "clientWidth": w, "clientHeight": h, "scale": 1, "zoom": 1 });
            let content = json!({ "x": 0, "y": 0, "width": cw, "height": ch });
            let device = |v: u32| f64::from(v) * dpr;
            // Chrome reports device-pixel sizes but CSS-px page offsets here.
            Ok(json!({
                "layoutViewport": { "pageX": sx, "pageY": sy, "clientWidth": device(w), "clientHeight": device(h) },
                "visualViewport": { "offsetX": 0, "offsetY": 0, "pageX": sx, "pageY": sy, "clientWidth": device(w), "clientHeight": device(h), "scale": 1, "zoom": 1 },
                "contentSize": { "x": 0, "y": 0, "width": cw * dpr, "height": ch * dpr },
                "cssLayoutViewport": layout,
                "cssVisualViewport": visual,
                "cssContentSize": content,
            }))
        }
        "Page.createIsolatedWorld" => {
            let frame = str_param(c.p, "frameId")?;
            let name = c.p.get("worldName").and_then(Value::as_str).unwrap_or("");
            let universal = bool_param(c.p, "grantUniveralAccess");
            let id = host
                .create_isolated_world(&t, frame, name, universal)
                .map_err(host_err)?;
            Ok(json!({ "executionContextId": id }))
        }
        "Page.addScriptToEvaluateOnNewDocument" => {
            let source = str_param(c.p, "source")?;
            let world =
                c.p.get("worldName")
                    .and_then(Value::as_str)
                    .filter(|w| !w.is_empty());
            let id = host
                .add_script_to_evaluate_on_new_document(
                    &t,
                    source,
                    world,
                    bool_param(c.p, "runImmediately"),
                )
                .map_err(host_err)?;
            Ok(json!({ "identifier": id }))
        }
        "Page.removeScriptToEvaluateOnNewDocument" => {
            host.remove_script_to_evaluate_on_new_document(&t, str_param(c.p, "identifier")?)
                .map_err(host_err)?;
            Ok(json!({}))
        }
        "Page.handleJavaScriptDialog" => {
            host.handle_javascript_dialog(
                &t,
                bool_param(c.p, "accept"),
                c.p.get("promptText").and_then(Value::as_str),
            )
            .map_err(host_err)?;
            Ok(json!({}))
        }
        "Page.close" => {
            c.sh.close_target(&t)?;
            Ok(json!({}))
        }
        "Page.captureScreenshot" => {
            let data = host.capture_screenshot(&t, c.p).map_err(host_err)?;
            Ok(json!({ "data": data }))
        }
        _ => Err(CdpError::method_not_found(c.method)),
    }
}

async fn runtime_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    let t = c.page()?;
    let host = &c.sh.host;
    match c.method {
        "Runtime.enable" => {
            let was =
                c.sh.state
                    .borrow()
                    .sessions
                    .get(&c.key)
                    .is_some_and(|s| s.domains.contains("Runtime"));
            c.enable("Runtime", true)?;
            if !was {
                let ctxs =
                    c.sh.state
                        .borrow()
                        .exec
                        .get(&t)
                        .cloned()
                        .unwrap_or_default();
                for ctx in ctxs {
                    c.sh.queue_event(
                        &c.key,
                        "Runtime.executionContextCreated",
                        json!({ "context": ctx.to_json() }),
                    );
                }
            }
            Ok(json!({}))
        }
        "Runtime.disable" => c.enable("Runtime", false),
        "Runtime.runIfWaitingForDebugger" => {
            c.sh.stop_waiting(&t, &c.key);
            Ok(json!({}))
        }
        "Runtime.evaluate" => {
            let p: EvaluateParams = params(c.p)?;
            let context_id = {
                let st = c.sh.state.borrow();
                match (p.context_id, &p.unique_context_id) {
                    (Some(id), _) => {
                        st.exec_context(&t, id).ok_or_else(|| {
                            CdpError::server("Cannot find context with specified id")
                        })?;
                        id
                    }
                    (None, Some(u)) => st
                        .exec
                        .get(&t)
                        .and_then(|v| v.iter().find(|x| &x.unique_id == u))
                        .map(|x| x.id)
                        .ok_or_else(|| CdpError::server("Cannot find context with specified id"))?,
                    (None, None) => st
                        .main_world(&t)
                        .ok_or_else(|| CdpError::server("Cannot find default execution context"))?,
                }
            };
            let req = EvaluateRequest {
                context_id,
                expression: p.expression,
                object_group: p.object_group,
                return_by_value: p.return_by_value,
                await_promise: p.await_promise,
                generate_preview: p.generate_preview,
                user_gesture: p.user_gesture,
                silent: p.silent,
                include_command_line_api: p.include_command_line_api,
            };
            let r = host.evaluate(&t, req).await.map_err(host_err)?;
            Ok(to_value(&r))
        }
        "Runtime.callFunctionOn" => {
            let p: CallFunctionParams = params(c.p)?;
            let target = match (p.object_id, p.execution_context_id) {
                (Some(o), _) => CallTarget::Object(o),
                (None, Some(id)) => {
                    if c.sh.state.borrow().exec_context(&t, id).is_none() {
                        return Err(CdpError::server("Cannot find context with specified id"));
                    }
                    CallTarget::Context(id)
                }
                (None, None) => {
                    return Err(CdpError::server(
                        "Either ObjectId or executionContextId must be specified",
                    ));
                }
            };
            let req = CallFunctionRequest {
                function_declaration: p.function_declaration,
                target,
                arguments: p.arguments,
                object_group: p.object_group,
                return_by_value: p.return_by_value,
                await_promise: p.await_promise,
                generate_preview: p.generate_preview,
                user_gesture: p.user_gesture,
                silent: p.silent,
            };
            let r = host.call_function_on(&t, req).await.map_err(host_err)?;
            Ok(to_value(&r))
        }
        "Runtime.getProperties" => {
            let req = GetPropertiesRequest {
                object_id: str_param(c.p, "objectId")?.to_string(),
                own_properties: bool_param(c.p, "ownProperties"),
                accessor_properties_only: bool_param(c.p, "accessorPropertiesOnly"),
                generate_preview: bool_param(c.p, "generatePreview"),
                non_indexed_properties_only: bool_param(c.p, "nonIndexedPropertiesOnly"),
            };
            let r = host.get_properties(&t, req).map_err(host_err)?;
            Ok(to_value(&r))
        }
        "Runtime.releaseObject" => {
            host.release_object(&t, str_param(c.p, "objectId")?)
                .map_err(host_err)?;
            Ok(json!({}))
        }
        "Runtime.releaseObjectGroup" => {
            host.release_object_group(&t, str_param(c.p, "objectGroup")?)
                .map_err(host_err)?;
            Ok(json!({}))
        }
        "Runtime.addBinding" => {
            let name = str_param(c.p, "name")?;
            let scope = if let Some(id) = c.p.get("executionContextId").and_then(Value::as_i64) {
                let st = c.sh.state.borrow();
                match st.exec_context(&t, id) {
                    None => {
                        return Err(CdpError::server(
                            "Cannot find execution context with given executionContextId",
                        ));
                    }
                    Some(ctx) if ctx.world.is_main() => {
                        return Err(CdpError::server(
                            "Runtime.addBinding: bindings are only installed in isolated worlds, never in the page's main world",
                        ));
                    }
                    Some(_) => BindingScope::Context(id),
                }
            } else {
                match c.p.get("executionContextName").and_then(Value::as_str) {
                    Some(n) if !n.is_empty() => BindingScope::World(n.to_string()),
                    _ => BindingScope::AllIsolatedWorlds,
                }
            };
            host.add_binding(&t, name, scope).map_err(host_err)?;
            Ok(json!({}))
        }
        _ => Err(CdpError::method_not_found(c.method)),
    }
}

fn dom_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    let t = c.page()?;
    let host = &c.sh.host;
    let depth = c.p.get("depth").and_then(Value::as_i64).unwrap_or(1) as i32;
    match c.method {
        "DOM.enable" => c.enable("DOM", true),
        "DOM.disable" => c.enable("DOM", false),
        "DOM.getDocument" => {
            let pierce = c.p.get("pierce").and_then(Value::as_bool).unwrap_or(false);
            Ok(json!({ "root": host.get_document(&t, depth, pierce).map_err(host_err)? }))
        }
        "DOM.describeNode" => {
            let depth = c.p.get("depth").and_then(Value::as_i64).unwrap_or(0) as i32;
            let pierce = c.p.get("pierce").and_then(Value::as_bool).unwrap_or(false);
            Ok(
                json!({ "node": host.describe_node(&t, &c.node_ref()?, depth, pierce).map_err(host_err)? }),
            )
        }
        "DOM.resolveNode" => {
            let node = match c.node_ref()? {
                NodeRef::Backend(id) => id,
                NodeRef::Object(_) => {
                    return Err(CdpError::server(
                        "Either nodeId or backendNodeId must be specified",
                    ));
                }
            };
            let ctx = c.p.get("executionContextId").and_then(Value::as_i64);
            if let Some(id) = ctx
                && c.sh.state.borrow().exec_context(&t, id).is_none()
            {
                return Err(CdpError::server("Cannot find context with specified id"));
            }
            let group = c.p.get("objectGroup").and_then(Value::as_str);
            Ok(json!({ "object": host.resolve_node(&t, node, ctx, group).map_err(host_err)? }))
        }
        "DOM.requestNode" => {
            let id = host
                .request_node(&t, str_param(c.p, "objectId")?)
                .map_err(host_err)?;
            Ok(json!({ "nodeId": id }))
        }
        "DOM.getBoxModel" => {
            Ok(json!({ "model": host.box_model(&t, &c.node_ref()?).map_err(host_err)? }))
        }
        "DOM.getContentQuads" => {
            let quads = host.content_quads(&t, &c.node_ref()?).map_err(host_err)?;
            if quads.is_empty() {
                return Err(CdpError::server("Could not compute content quads."));
            }
            Ok(json!({ "quads": quads }))
        }
        "DOM.scrollIntoViewIfNeeded" => {
            let rect: Option<Rect> =
                c.p.get("rect")
                    .map(|r| serde_json::from_value(r.clone()))
                    .transpose()
                    .map_err(|e| CdpError::invalid_params(e.to_string()))?;
            host.scroll_into_view_if_needed(&t, &c.node_ref()?, rect)
                .map_err(host_err)?;
            Ok(json!({}))
        }
        "DOM.setFileInputFiles" => {
            let files: Vec<String> =
                c.p.get("files")
                    .map(|f| serde_json::from_value(f.clone()))
                    .transpose()
                    .map_err(|e| CdpError::invalid_params(e.to_string()))?
                    .ok_or_else(|| CdpError::invalid_params("files is required"))?;
            host.set_file_input_files(&t, &c.node_ref()?, files)
                .map_err(host_err)?;
            Ok(json!({}))
        }
        "DOM.querySelector" | "DOM.querySelectorAll" => {
            let node =
                c.p.get("nodeId")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| CdpError::invalid_params("nodeId is required"))?;
            let sel = str_param(c.p, "selector")?;
            if c.method == "DOM.querySelector" {
                let id = host.query_selector(&t, node, sel).map_err(host_err)?;
                Ok(json!({ "nodeId": id.unwrap_or(0) }))
            } else {
                Ok(json!({ "nodeIds": host.query_selector_all(&t, node, sel).map_err(host_err)? }))
            }
        }
        "DOM.getOuterHTML" => {
            Ok(json!({ "outerHTML": host.outer_html(&t, &c.node_ref()?).map_err(host_err)? }))
        }
        _ => Err(CdpError::method_not_found(c.method)),
    }
}

async fn input_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    let t = c.page()?;
    let host = &c.sh.host;
    match c.method {
        "Input.dispatchMouseEvent" => {
            host.dispatch_mouse_event(&t, params(c.p)?)
                .await
                .map_err(host_err)?;
        }
        "Input.dispatchKeyEvent" => {
            host.dispatch_key_event(&t, params(c.p)?)
                .await
                .map_err(host_err)?;
        }
        "Input.insertText" => {
            host.insert_text(&t, str_param(c.p, "text")?)
                .await
                .map_err(host_err)?;
        }
        "Input.dispatchTouchEvent" => {
            let touch = c.persona().hardware.max_touch_points > 0;
            return Err(CdpError::server(if touch {
                "Input.dispatchTouchEvent is not supported yet"
            } else {
                "Input.dispatchTouchEvent: the persona has no touch screen (maxTouchPoints is 0)"
            }));
        }
        _ => return Err(CdpError::method_not_found(c.method)),
    }
    Ok(json!({}))
}

fn network_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    let host = &c.sh.host;
    let urls = || -> Option<Vec<String>> {
        c.p.get("urls")
            .and_then(|u| serde_json::from_value(u.clone()).ok())
    };
    let explicit_ctx =
        c.p.get("browserContextId")
            .and_then(Value::as_str)
            .map(str::to_string);
    match c.method {
        "Network.enable" => {
            c.page()?;
            c.enable("Network", true)
        }
        "Network.disable" => c.enable("Network", false),
        "Network.getCookies" | "Network.getAllCookies" | "Storage.getCookies" => {
            let ctx = c.cookie_context(explicit_ctx);
            let urls = if c.method == "Network.getCookies" {
                urls()
            } else {
                None
            };
            let cookies = host.cookies(&ctx, urls.as_deref()).map_err(host_err)?;
            Ok(json!({ "cookies": cookies }))
        }
        "Network.setCookies" | "Storage.setCookies" => {
            let ctx = c.cookie_context(explicit_ctx);
            let cookies: Vec<CookieParam> =
                c.p.get("cookies")
                    .map(|v| serde_json::from_value(v.clone()))
                    .transpose()
                    .map_err(|e| CdpError::invalid_params(e.to_string()))?
                    .ok_or_else(|| CdpError::invalid_params("cookies is required"))?;
            host.set_cookies(&ctx, cookies).map_err(host_err)?;
            Ok(json!({}))
        }
        "Network.setCookie" => {
            let ctx = c.cookie_context(None);
            let cookie: CookieParam = params(c.p)?;
            let ok = host.set_cookies(&ctx, vec![cookie]).is_ok();
            Ok(json!({ "success": ok }))
        }
        "Network.deleteCookies" => {
            let ctx = c.cookie_context(None);
            host.delete_cookies(&ctx, params(c.p)?).map_err(host_err)?;
            Ok(json!({}))
        }
        "Network.clearBrowserCookies" | "Storage.clearCookies" => {
            let ctx = c.cookie_context(explicit_ctx);
            host.clear_cookies(&ctx).map_err(host_err)?;
            Ok(json!({}))
        }
        "Network.clearBrowserCache" => Ok(json!({})),
        "Network.setExtraHTTPHeaders" => {
            let t = c.page()?;
            let headers: Vec<(String, String)> =
                c.p.get("headers")
                    .and_then(Value::as_object)
                    .ok_or_else(|| CdpError::invalid_params("headers is required"))?
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                    .collect();
            host.set_extra_http_headers(&t, headers).map_err(host_err)?;
            Ok(json!({}))
        }
        "Network.setCacheDisabled" => {
            let t = c.page()?;
            host.set_cache_disabled(&t, bool_param(c.p, "cacheDisabled"))
                .map_err(host_err)?;
            Ok(json!({}))
        }
        "Network.getResponseBody" => {
            let t = c.page()?;
            let (body, b64) = host
                .response_body(&t, str_param(c.p, "requestId")?)
                .map_err(host_err)?;
            Ok(json!({ "body": body, "base64Encoded": b64 }))
        }
        "Network.setUserAgentOverride" => {
            emulation::check_user_agent(&c.persona(), &params(c.p)?)?;
            Ok(json!({}))
        }
        _ => Err(CdpError::method_not_found(c.method)),
    }
}

fn fetch_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    let t = c.page()?;
    let host = &c.sh.host;
    match c.method {
        "Fetch.enable" => {
            let patterns: Vec<RequestPattern> = match c.p.get("patterns") {
                Some(v) => serde_json::from_value(v.clone())
                    .map_err(|e| CdpError::invalid_params(e.to_string()))?,
                None => vec![RequestPattern {
                    url_pattern: Some("*".into()),
                    ..Default::default()
                }],
            };
            host.set_request_interception(&t, Some(patterns))
                .map_err(host_err)?;
            let auth =
                c.p.get("handleAuthRequests")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            host.set_auth_handling(&t, auth).map_err(host_err)?;
            c.enable("Fetch", true)
        }
        "Fetch.disable" => {
            host.set_request_interception(&t, None).map_err(host_err)?;
            host.set_auth_handling(&t, false).map_err(host_err)?;
            c.enable("Fetch", false)
        }
        "Fetch.continueWithAuth" => {
            let resp: AuthChallengeResponse =
                serde_json::from_value(c.p.get("authChallengeResponse").cloned().ok_or_else(
                    || CdpError::invalid_params("authChallengeResponse: object expected"),
                )?)
                .map_err(|e| CdpError::invalid_params(e.to_string()))?;
            host.continue_with_auth(&t, str_param(c.p, "requestId")?, resp)
                .map_err(host_err)?;
            Ok(json!({}))
        }
        "Fetch.continueRequest" => {
            host.continue_request(&t, params(c.p)?).map_err(host_err)?;
            Ok(json!({}))
        }
        "Fetch.fulfillRequest" => {
            host.fulfill_request(&t, params(c.p)?).map_err(host_err)?;
            Ok(json!({}))
        }
        "Fetch.failRequest" => {
            host.fail_request(
                &t,
                str_param(c.p, "requestId")?,
                str_param(c.p, "errorReason")?,
            )
            .map_err(host_err)?;
            Ok(json!({}))
        }
        _ => Err(CdpError::method_not_found(c.method)),
    }
}

fn emulation_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    let t = c.page()?;
    let host = &c.sh.host;
    let persona = c.persona();
    match c.method {
        "Emulation.setDeviceMetricsOverride" => {
            let vp = emulation::check_device_metrics(&persona, &params(c.p)?)?;
            host.set_viewport(&t, vp).map_err(host_err)?;
            if let Some(ts) = c.sh.state.borrow_mut().target_mut(&t) {
                ts.viewport = vp;
            }
        }
        "Emulation.clearDeviceMetricsOverride" => {
            host.set_viewport(&t, None).map_err(host_err)?;
            if let Some(ts) = c.sh.state.borrow_mut().target_mut(&t) {
                ts.viewport = None;
            }
        }
        "Emulation.setUserAgentOverride" => emulation::check_user_agent(&persona, &params(c.p)?)?,
        "Emulation.setTimezoneOverride" => emulation::check_timezone(
            &persona,
            c.p.get("timezoneId").and_then(Value::as_str).unwrap_or(""),
        )?,
        "Emulation.setLocaleOverride" => {
            emulation::check_locale(&persona, c.p.get("locale").and_then(Value::as_str))?
        }
        "Emulation.setFocusEmulationEnabled" => host
            .set_focus_emulation(&t, bool_param(c.p, "enabled"))
            .map_err(host_err)?,
        "Emulation.setEmulatedMedia" => emulation::check_emulated_media(&persona, &params(c.p)?)?,
        "Emulation.setTouchEmulationEnabled" => {
            let has_touch = persona.hardware.max_touch_points > 0;
            if bool_param(c.p, "enabled") != has_touch {
                return Err(CdpError::server(format!(
                    "Emulation.setTouchEmulationEnabled: the persona has maxTouchPoints {}; it contradicts the persona",
                    persona.hardware.max_touch_points
                )));
            }
        }
        "Emulation.setDefaultBackgroundColorOverride" => {
            let color = match c.p.get("color") {
                Some(v) if v.is_object() => {
                    let ch = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0).min(255) as u8;
                    Some(crate::host::Rgba {
                        r: ch("r"),
                        g: ch("g"),
                        b: ch("b"),
                        a: v.get("a")
                            .and_then(Value::as_f64)
                            .unwrap_or(1.0)
                            .clamp(0.0, 1.0),
                    })
                }
                _ => None,
            };
            host.set_default_background_color(&t, color)
                .map_err(host_err)?;
        }
        "Emulation.setGeolocationOverride" => {
            if c.p.get("latitude").is_some() || c.p.get("longitude").is_some() {
                return Err(CdpError::server(
                    "Emulation.setGeolocationOverride: the persona has no geolocation; only clearing is accepted",
                ));
            }
        }
        _ => return Err(CdpError::method_not_found(c.method)),
    }
    Ok(json!({}))
}

fn accessibility_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    let t = c.page()?;
    match c.method {
        "Accessibility.enable" => c.enable("Accessibility", true),
        "Accessibility.disable" => c.enable("Accessibility", false),
        "Accessibility.getFullAXTree" => {
            let nodes = c.sh.host.ax_tree(&t).map_err(host_err)?;
            Ok(json!({ "nodes": nodes }))
        }
        "Accessibility.queryAXTree" => {
            let nodes = c.sh.host.ax_tree(&t).map_err(host_err)?;
            let backend = match c.node_ref()? {
                NodeRef::Backend(id) => id,
                NodeRef::Object(o) => c.sh.host.request_node(&t, &o).map_err(host_err)?,
            };
            let name = c.p.get("accessibleName").and_then(Value::as_str);
            let role = c.p.get("role").and_then(Value::as_str);
            Ok(json!({ "nodes": query_ax(&nodes, backend, role, name) }))
        }
        _ => Err(CdpError::method_not_found(c.method)),
    }
}

/// `Accessibility.queryAXTree`: non-ignored nodes in the subtree of the node
/// for `backend` whose role and name match exactly.
pub fn query_ax(
    nodes: &[AxNode],
    backend: BackendNodeId,
    role: Option<&str>,
    name: Option<&str>,
) -> Vec<AxNode> {
    let by_id: HashMap<&str, &AxNode> = nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
    let Some(start) = nodes
        .iter()
        .find(|n| n.backend_dom_node_id == Some(backend))
    else {
        return Vec::new();
    };
    let text = |v: &Option<AxValue>| {
        v.as_ref()
            .and_then(|v| v.value.as_ref())
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let mut out = Vec::new();
    let mut stack = vec![start];
    while let Some(n) = stack.pop() {
        let ok = !n.ignored
            && role.is_none_or(|r| text(&n.role).as_deref() == Some(r))
            && name.is_none_or(|nm| text(&n.name).as_deref() == Some(nm));
        if ok {
            out.push(n.clone());
        }
        for child in n.child_ids.iter().rev() {
            if let Some(ch) = by_id.get(child.as_str()) {
                stack.push(ch);
            }
        }
    }
    out
}

fn misc_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    match c.method {
        "Security.setIgnoreCertificateErrors" => {
            c.sh.host
                .set_ignore_certificate_errors(c.target.as_deref(), bool_param(c.p, "ignore"))
                .map_err(host_err)?;
            Ok(json!({}))
        }
        "Security.enable" | "Log.enable" | "Performance.enable" => {
            c.enable(c.method.split('.').next().unwrap_or(""), true)
        }
        "Security.disable" | "Log.disable" | "Performance.disable" => {
            c.enable(c.method.split('.').next().unwrap_or(""), false)
        }
        "Log.clear" => Ok(json!({})),
        _ => Err(CdpError::method_not_found(c.method)),
    }
}

async fn lightbrowser_domain<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<Value, CdpError> {
    match c.method {
        "LightBrowser.setPersona" => {
            let raw =
                c.p.get("persona")
                    .ok_or_else(|| CdpError::invalid_params("persona is required"))?;
            let json = match raw {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let profile = PersonaProfile::from_json_checked(&json)
                .map_err(|e| CdpError::server(format!("LightBrowser.setPersona: {e}")))?;
            if profile.browser.family != c.sh.host.family() {
                return Err(CdpError::server(format!(
                    "LightBrowser.setPersona: persona family {} does not match this build ({})",
                    profile.browser.family,
                    c.sh.host.family()
                )));
            }
            let ctx = c.cookie_context(
                c.p.get("browserContextId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            );
            let busy =
                c.sh.state
                    .borrow()
                    .targets
                    .iter()
                    .any(|t| t.info.browser_context_id.as_deref() == Some(ctx.as_str()));
            if busy {
                return Err(CdpError::server(
                    "LightBrowser.setPersona: close the context's pages first; a persona cannot change under a live page",
                ));
            }
            let id = profile.profile_id.clone();
            c.sh.host
                .set_persona(&ctx, Arc::new(profile))
                .map_err(host_err)?;
            Ok(json!({ "browserContextId": ctx, "profileId": id }))
        }
        "LightBrowser.fingerprintReport" => {
            // The session's page, or `targetId` from a browser session.
            let target = match c.p.get("targetId").and_then(Value::as_str) {
                Some(t) => t.to_string(),
                None => c.target.clone().ok_or_else(|| {
                    CdpError::invalid_params("targetId is required on a browser session")
                })?,
            };
            c.sh.host
                .fingerprint_report(&target)
                .await
                .map_err(host_err)
        }
        "LightBrowser.humanMove"
        | "LightBrowser.humanClick"
        | "LightBrowser.humanType"
        | "LightBrowser.humanScroll" => {
            // Page sessions only: the input goes to the session's page.
            let t = c.page()?;
            let cmd = match c.method {
                "LightBrowser.humanMove" => HumanCommand::Move(params(c.p)?),
                "LightBrowser.humanClick" => {
                    let p: HumanClick = params(c.p)?;
                    if p.selector.is_none() && (p.x.is_none() || p.y.is_none()) {
                        return Err(CdpError::invalid_params(
                            "selector or both x and y are required",
                        ));
                    }
                    if let Some(b) = &p.button
                        && !matches!(b.as_str(), "left" | "middle" | "right")
                    {
                        return Err(CdpError::invalid_params(format!("unknown button {b:?}")));
                    }
                    HumanCommand::Click(p)
                }
                "LightBrowser.humanType" => HumanCommand::Type(params(c.p)?),
                _ => HumanCommand::Scroll(params(c.p)?),
            };
            c.sh.host.human_input(&t, cmd).await.map_err(host_err)
        }
        _ => Err(CdpError::method_not_found(c.method)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ax(
        id: &str,
        role: &str,
        name: &str,
        backend: i64,
        children: &[&str],
        ignored: bool,
    ) -> AxNode {
        AxNode {
            node_id: id.into(),
            ignored,
            role: Some(AxValue::new("role", json!(role))),
            name: Some(AxValue::new("computedString", json!(name))),
            description: None,
            value: None,
            properties: Vec::new(),
            parent_id: None,
            child_ids: children.iter().map(|s| s.to_string()).collect(),
            backend_dom_node_id: Some(backend),
            frame_id: None,
        }
    }

    #[test]
    fn query_ax_filters_subtree() {
        let nodes = vec![
            ax("1", "RootWebArea", "t", 1, &["2", "3"], false),
            ax("2", "button", "Log in", 5, &[], false),
            ax("3", "generic", "", 6, &["4"], true),
            ax("4", "button", "Other", 7, &[], false),
        ];
        let r = query_ax(&nodes, 1, Some("button"), None);
        assert_eq!(
            r.iter().map(|n| n.node_id.as_str()).collect::<Vec<_>>(),
            ["2", "4"]
        );
        let r = query_ax(&nodes, 1, Some("button"), Some("Log in"));
        assert_eq!(r.len(), 1);
        assert!(query_ax(&nodes, 6, None, None).iter().all(|n| !n.ignored));
        assert!(query_ax(&nodes, 99, None, None).is_empty());
    }
}

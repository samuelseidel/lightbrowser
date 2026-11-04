//! Workers as CDP targets (ADR 0029).
//!
//! A worker thread (dedicated, shared, service; `crate::worker`) whose
//! browser context belongs to a [`super::CdpHost`] announces itself once
//! its realm exists, before its script runs: [`announce`] gives it a
//! target id and reports [`PageEvent::WorkerCreated`] to the server, which
//! attaches it where auto-attach is on and calls
//! [`lb_cdp::PageHost::resume_worker`] once no session waits for the
//! debugger. Until then the thread answers CDP commands but runs no
//! script, as Chrome's paused worker does.
//!
//! Commands reach the thread as [`WorkerCmd`]s on a channel its event loop
//! polls; the thread's [`WorkerAgent`] keeps the remote objects it handed
//! out and the promises it still awaits. Events (console, exceptions,
//! network, `Inspector.workerScriptLoaded`) go straight to the server's
//! event sink with the worker's target id. Nothing is installed in the
//! worker's global scope: evaluation runs in the scope like Chrome's
//! `Runtime.evaluate` does, and no CDP helper is left behind.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lb_cdp::types::*;
use lb_cdp::{GetPropertiesRequest, HostError, HostResult, PageEvent};
use lb_js::{Arg, Caught, Engine, Handle, Inspected, JsErrorKind, PromiseOutcome, ScriptOrigin};
use lb_webapi::host::{ConsoleMessage, NetRequest, NetResponse};
use lb_webapi::rt::{Window, WorkerControl, WorkerKind};
use tokio::sync::{mpsc, oneshot};

use super::Shared;
use super::agent::{JsJob, JsOpts};

/// A CDP command for a worker thread.
pub(crate) enum WorkerCmd {
    Js {
        job: JsJob,
        opts: JsOpts,
        reply: oneshot::Sender<HostResult<EvalResult>>,
    },
    Props {
        req: GetPropertiesRequest,
        reply: oneshot::Sender<HostResult<Properties>>,
    },
    Release(String),
    ReleaseGroup(String),
}

/// What the host keeps of an inspected worker.
pub(crate) struct WorkerEntry {
    cmds: mpsc::UnboundedSender<WorkerCmd>,
    resume: Mutex<Option<oneshot::Sender<()>>>,
    control: Arc<WorkerControl>,
    /// A CDP session is attached (keeps a service worker running).
    inspected: Arc<AtomicBool>,
}

impl WorkerEntry {
    pub(crate) fn send(&self, cmd: WorkerCmd) -> HostResult<()> {
        self.cmds
            .send(cmd)
            .map_err(|_| HostError::new("Target closed"))
    }

    pub(crate) fn resume(&self) {
        if let Some(tx) = self.resume.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(());
        }
    }

    pub(crate) fn terminate(&self) {
        self.control.terminate();
    }

    pub(crate) fn set_inspected(&self, on: bool) {
        self.inspected.store(on, Ordering::SeqCst);
    }
}

/// The inspector of one browser context: set on the context by the CDP
/// host, read by worker threads.
pub(crate) struct Inspector {
    shared: Arc<Shared>,
    /// The CDP browser context id.
    context: String,
}

impl Inspector {
    pub(crate) fn new(shared: Arc<Shared>, context: String) -> Arc<Inspector> {
        Arc::new(Inspector { shared, context })
    }
}

/// A worker about to run its script.
pub(crate) struct Announce {
    pub kind: WorkerKind,
    pub url: url::Url,
    pub name: String,
    /// Dedicated workers: the target of the page or worker that made it.
    pub parent: Option<String>,
    /// Service workers: the registration's scope.
    pub scope: Option<String>,
    pub control: Arc<WorkerControl>,
}

/// The worker thread's end of its target.
pub(crate) struct Link {
    pub agent: Rc<WorkerAgent>,
    cmds: mpsc::UnboundedReceiver<WorkerCmd>,
    resume: Option<oneshot::Receiver<()>>,
}

impl Drop for Link {
    fn drop(&mut self) {
        let a = &self.agent;
        a.shared
            .workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&a.target);
        a.shared.emit(&a.target, PageEvent::WorkerDestroyed);
    }
}

/// Make worker `a` a target when its context is inspected.
pub(crate) fn announce(inspector: Option<&Arc<Inspector>>, a: Announce) -> Option<Link> {
    let ins = inspector?;
    let shared = ins.shared.clone();
    let n = shared.next_worker.fetch_add(1, Ordering::Relaxed);
    let target = target_id(shared.seed, n);
    let (tx, rx) = mpsc::unbounded_channel();
    let (rtx, rrx) = oneshot::channel();
    let inspected = Arc::new(AtomicBool::new(false));
    shared
        .workers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            target.clone(),
            Arc::new(WorkerEntry {
                cmds: tx,
                resume: Mutex::new(Some(rtx)),
                control: a.control.clone(),
                inspected: inspected.clone(),
            }),
        );
    let (kind, title) = match a.kind {
        WorkerKind::Dedicated => ("worker", a.name.clone()),
        WorkerKind::Shared => ("shared_worker", a.name.clone()),
        WorkerKind::Service => ("service_worker", format!("Service Worker {}", a.url)),
    };
    let title = if title.is_empty() {
        a.url.to_string()
    } else {
        title
    };
    let info = TargetInfo {
        target_id: target.clone(),
        kind: kind.into(),
        title,
        url: a.url.to_string(),
        attached: false,
        opener_id: None,
        parent_id: a.parent.clone(),
        can_access_opener: false,
        parent_frame_id: a.parent.clone(),
        browser_context_id: Some(ins.context.clone()),
    };
    let context = ExecutionContext {
        id: CONTEXT_ID,
        frame_id: target.clone(),
        world: World::Worker(match a.kind {
            WorkerKind::Service => String::new(),
            _ => a.name.clone(),
        }),
        // Chrome reports the script URL as a worker context's origin.
        origin: a.url.to_string(),
        unique_id: format!(
            "{}.{}",
            mix(shared.seed ^ n) as i64,
            mix(shared.seed.rotate_left(17) ^ n) as i64
        ),
    };
    shared.emit(
        &target,
        PageEvent::WorkerCreated {
            info,
            context,
            scope: a.scope,
        },
    );
    Some(Link {
        agent: Rc::new(WorkerAgent {
            target,
            parent: a.parent,
            shared,
            objects: RefCell::new(HashMap::new()),
            next_obj: Cell::new(0),
            next_request: Cell::new(0),
            pending: RefCell::new(Vec::new()),
            obj_prefix: mix(n ^ 0x5eed) as i64,
            inspected,
        }),
        cmds: rx,
        resume: Some(rrx),
    })
}

/// A worker's only execution context (Chrome numbers it 1 in every
/// worker).
const CONTEXT_ID: ExecutionContextId = 1;

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// A Chrome-shaped target id (32 upper-case hex digits).
fn target_id(seed: u64, n: u64) -> String {
    format!(
        "{:016X}{:016X}",
        mix(seed ^ n),
        mix(seed.rotate_left(29) ^ !n)
    )
}

impl Link {
    /// Wait until the server lets the script run, answering commands
    /// meanwhile. Returns false when the worker was terminated first.
    pub(crate) async fn wait_resume(
        &mut self,
        engine: &mut Engine,
        w: &Rc<Window>,
        control: &WorkerControl,
    ) -> bool {
        let Some(mut rx) = self.resume.take() else {
            return true;
        };
        loop {
            if control.is_terminated() {
                return false;
            }
            tokio::select! {
                r = &mut rx => return r.is_ok() || !control.is_terminated(),
                Some(cmd) = self.cmds.recv() => self.agent.run(engine, w, cmd),
                _ = tokio::time::sleep(std::time::Duration::from_millis(25)) => {}
            }
        }
    }

    /// Run every queued command and settle awaited promises.
    pub(crate) fn poll(&mut self, engine: &mut Engine, w: &Rc<Window>) {
        while let Ok(cmd) = self.cmds.try_recv() {
            self.agent.run(engine, w, cmd);
        }
        self.agent.settle_pending(engine, w);
    }

    /// The next command (for the event loop's wait).
    pub(crate) async fn next(&mut self) -> Option<WorkerCmd> {
        self.cmds.recv().await
    }

    /// Run one command (from [`Link::next`]) and settle awaited promises.
    pub(crate) fn handle(&mut self, engine: &mut Engine, w: &Rc<Window>, cmd: WorkerCmd) {
        self.agent.run(engine, w, cmd);
        self.agent.settle_pending(engine, w);
    }
}

struct Pending {
    handle: Handle,
    opts: JsOpts,
    reply: oneshot::Sender<HostResult<EvalResult>>,
}

/// The CDP state of one worker, on its thread.
pub(crate) struct WorkerAgent {
    pub target: String,
    parent: Option<String>,
    shared: Arc<Shared>,
    objects: RefCell<HashMap<String, (Handle, Option<String>)>>,
    next_obj: Cell<u64>,
    next_request: Cell<u64>,
    pending: RefCell<Vec<Pending>>,
    obj_prefix: i64,
    inspected: Arc<AtomicBool>,
}

impl WorkerAgent {
    /// Whether a CDP session is attached to the worker's target.
    pub(crate) fn inspected_flag(&self) -> Arc<AtomicBool> {
        self.inspected.clone()
    }

    fn emit(&self, ev: PageEvent) {
        self.shared.emit(&self.target, ev);
    }

    /// `Inspector.workerScriptLoaded` (Playwright waits for it before it
    /// evaluates in a worker).
    pub(crate) fn script_loaded(&self) {
        self.emit(PageEvent::Custom {
            method: "Inspector.workerScriptLoaded".into(),
            params: serde_json::json!({}),
        });
    }

    /// The worker's console messages and uncaught exceptions.
    pub(crate) fn console(&self, m: &ConsoleMessage) {
        self.emit(super::agent::console_event(m, CONTEXT_ID));
        // Chrome copies a dedicated worker's console calls to its page's
        // log (source `worker`; Playwright skips those).
        if let Some(p) = &self.parent
            && !(m.level == "error" && m.text.starts_with("Uncaught "))
        {
            let fwd = ConsoleMessage {
                from_worker: true,
                ..m.clone()
            };
            self.shared
                .emit(p, super::agent::console_event(&fwd, CONTEXT_ID));
        }
    }

    // ---- network ----

    /// The worker's script response, reported once the script may run
    /// (Chrome fetched it before the target existed, so there is no
    /// `requestWillBeSent` except for service workers).
    pub(crate) fn script_response(&self, kind: WorkerKind, resp: &NetResponse) {
        if kind == WorkerKind::Service {
            let mut req =
                NetRequest::get(resp.url.clone(), lb_webapi::host::Destination::Script, None);
            req.headers.push(("Service-Worker".into(), "script".into()));
            self.emit(PageEvent::RequestWillBeSent(Box::new(
                super::net::worker_request(&self.target, &self.target, &req, "Script"),
            )));
        }
        self.emit(PageEvent::ResponseReceived(Box::new(
            super::net::worker_response(&self.target, resp, "Script", self.shared.mono()),
        )));
        self.data_received(&self.target, resp.body.len());
    }

    /// `Network.dataReceived` (Chrome sends one before `loadingFinished`).
    fn data_received(&self, id: &str, len: usize) {
        self.emit(PageEvent::Custom {
            method: "Network.dataReceived".into(),
            params: serde_json::json!({
                "requestId": id,
                "timestamp": self.shared.mono(),
                "dataLength": len,
                "encodedDataLength": 0,
            }),
        });
    }

    pub(crate) fn script_finished(&self, len: usize) {
        self.emit(PageEvent::LoadingFinished(LoadingFinished {
            request_id: self.target.clone(),
            timestamp: self.shared.mono(),
            encoded_data_length: len as f64,
        }));
    }

    /// Network events around one request of the worker's own.
    pub(crate) fn request_started(&self, req: &NetRequest) -> String {
        let n = self.next_request.get() + 1;
        self.next_request.set(n);
        let id = format!("{}.{n}", self.obj_prefix.unsigned_abs() % 100_000);
        let rtype = if req.destination == lb_webapi::host::Destination::Empty {
            "Fetch"
        } else {
            "Script"
        };
        let mut ev = super::net::worker_request(&id, &self.target, req, rtype);
        ev.timestamp = self.shared.mono();
        self.emit(PageEvent::RequestWillBeSent(Box::new(ev)));
        id
    }

    pub(crate) fn request_done(
        &self,
        id: &str,
        req_dest_empty: bool,
        r: &Result<NetResponse, String>,
    ) {
        let rtype = if req_dest_empty { "Fetch" } else { "Script" };
        match r {
            Ok(resp) => {
                self.emit(PageEvent::ResponseReceived(Box::new(
                    super::net::worker_response(id, resp, rtype, self.shared.mono()),
                )));
                self.data_received(id, resp.body.len());
                self.emit(PageEvent::LoadingFinished(LoadingFinished {
                    request_id: id.to_string(),
                    timestamp: self.shared.mono(),
                    encoded_data_length: resp.body.len() as f64,
                }));
            }
            Err(e) => self.emit(PageEvent::LoadingFailed(LoadingFailed {
                request_id: id.to_string(),
                timestamp: self.shared.mono(),
                resource_type: rtype.into(),
                error_text: super::net::net_error_text(e),
                canceled: false,
            })),
        }
    }

    // ---- Runtime ----

    fn register(&self, h: Handle, group: Option<&str>) -> String {
        let n = self.next_obj.get() + 1;
        self.next_obj.set(n);
        let id = format!("{}.{CONTEXT_ID}.{n}", self.obj_prefix);
        self.objects
            .borrow_mut()
            .insert(id.clone(), (h, group.map(str::to_string)));
        id
    }

    fn object(&self, id: &str) -> HostResult<(Handle, Option<String>)> {
        self.objects
            .borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| HostError::new("Could not find object with given id"))
    }

    fn run(&self, engine: &mut Engine, w: &Rc<Window>, cmd: WorkerCmd) {
        match cmd {
            WorkerCmd::Js { job, opts, reply } => {
                // A call on an object keeps that object's group.
                let opts = match &job {
                    JsJob::Call { this: Some(o), .. } if opts.group.is_none() => JsOpts {
                        group: self.object(o).ok().and_then(|x| x.1),
                        ..opts
                    },
                    _ => opts,
                };
                match self.start_js(engine, w, job, &opts) {
                    Ok(Ok(h)) if opts.await_promise => {
                        self.pending.borrow_mut().push(Pending {
                            handle: h,
                            opts,
                            reply,
                        });
                        self.settle_pending(engine, w);
                    }
                    Ok(Ok(h)) => {
                        let _ = reply.send(self.finish(engine, w, Ok(h), false, &opts));
                    }
                    Ok(Err(c)) => {
                        let _ = reply.send(self.finish(engine, w, Err(c), false, &opts));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            WorkerCmd::Props { req, reply } => {
                let r = self.object(&req.object_id).and_then(|(h, group)| {
                    let realm = w.realm();
                    super::agent::properties_of(engine, w, realm, h, &req, &mut |x| {
                        self.register(x, group.as_deref())
                    })
                });
                let _ = reply.send(r);
            }
            WorkerCmd::Release(id) => {
                if let Some((h, _)) = self.objects.borrow_mut().remove(&id) {
                    engine.release(h);
                }
            }
            WorkerCmd::ReleaseGroup(g) => {
                let gone: Vec<Handle> = {
                    let mut objs = self.objects.borrow_mut();
                    let ids: Vec<String> = objs
                        .iter()
                        .filter(|(_, (_, x))| x.as_deref() == Some(g.as_str()))
                        .map(|(k, _)| k.clone())
                        .collect();
                    ids.iter()
                        .filter_map(|k| objs.remove(k))
                        .map(|x| x.0)
                        .collect()
                };
                for h in gone {
                    engine.release(h);
                }
            }
        }
    }

    /// Evaluate or call; the outer error is a protocol error.
    fn start_js(
        &self,
        e: &mut Engine,
        w: &Rc<Window>,
        job: JsJob,
        _opts: &JsOpts,
    ) -> HostResult<Result<Handle, Caught>> {
        let realm = w.realm();
        let r = match job {
            JsJob::Eval(src) => e.evaluate_catching(realm, &src, ScriptOrigin::new("")),
            JsJob::Call { decl, this, args } => {
                let f =
                    match e.evaluate_catching(realm, &format!("({decl}\n)"), ScriptOrigin::new(""))
                    {
                        Ok(f) => f,
                        Err(c) => return Ok(Err(c)),
                    };
                if e.value_kind(realm, f).ok() != Some(lb_js::ValueKind::Function) {
                    e.release(f);
                    return Err(HostError::new(
                        "Given expression does not evaluate to a function",
                    ));
                }
                let lookup = |oid: &str| self.object(oid).map(|(h, _)| (realm, h));
                let mut argv = Vec::with_capacity(args.len());
                for a in &args {
                    argv.push(super::agent::call_arg_with(&lookup, e, realm, a)?);
                }
                let this = match this {
                    Some(oid) => Arg::Handle(self.object(&oid)?.0),
                    None => Arg::Undefined,
                };
                let r = e.call_catching(realm, f, this, &argv);
                e.release(f);
                r
            }
        };
        e.perform_microtask_checkpoint();
        Ok(r)
    }

    /// Answer awaited promises that settled.
    pub(crate) fn settle_pending(&self, e: &mut Engine, w: &Rc<Window>) {
        if self.pending.borrow().is_empty() {
            return;
        }
        let realm = w.realm();
        let list = std::mem::take(&mut *self.pending.borrow_mut());
        let mut still = Vec::new();
        for p in list {
            let state = match e.promise_outcome(realm, p.handle) {
                Ok(s) => s,
                Err(x) => {
                    e.release(p.handle);
                    let _ = p.reply.send(Err(HostError::new(x.message)));
                    continue;
                }
            };
            let out = match state {
                PromiseOutcome::Pending => {
                    still.push(p);
                    continue;
                }
                PromiseOutcome::NotAPromise => (Ok(p.handle), false),
                PromiseOutcome::Fulfilled(v) => {
                    e.release(p.handle);
                    (Ok(v), false)
                }
                PromiseOutcome::Rejected(v) => {
                    e.release(p.handle);
                    (
                        Err(Caught {
                            error: lb_js::JsError::new(JsErrorKind::Exception, "rejected"),
                            value: Some(v),
                        }),
                        true,
                    )
                }
            };
            let r = self.finish(e, w, out.0, out.1, &p.opts);
            let _ = p.reply.send(r);
        }
        self.pending.borrow_mut().extend(still);
    }

    /// Drop awaited evaluations (the worker ends).
    pub(crate) fn abandon(&self) {
        for p in self.pending.borrow_mut().drain(..) {
            let _ = p
                .reply
                .send(Err(HostError::new("Execution context was destroyed.")));
        }
    }

    fn finish(
        &self,
        e: &mut Engine,
        w: &Rc<Window>,
        outcome: Result<Handle, Caught>,
        in_promise: bool,
        opts: &JsOpts,
    ) -> HostResult<EvalResult> {
        let realm = w.realm();
        let group = opts.group.as_deref();
        match outcome {
            Ok(h) => Ok(EvalResult {
                result: super::agent::remote_value(
                    e,
                    w,
                    realm,
                    h,
                    opts.by_value,
                    opts.preview,
                    &mut |x| self.register(x, group),
                )?,
                exception_details: None,
            }),
            Err(mut c) => {
                if in_promise && let Some(v) = c.value {
                    if let Ok(Inspected::Object(o)) = e.inspect(realm, v, false) {
                        c.error.stack = o.description.clone();
                    }
                }
                let details = super::agent::exception_details(
                    e,
                    w,
                    realm,
                    CONTEXT_ID,
                    c,
                    in_promise,
                    &mut |x| self.register(x, group),
                );
                let result = details.exception.clone().unwrap_or_default();
                Ok(EvalResult {
                    result,
                    exception_details: Some(details),
                })
            }
        }
    }
}

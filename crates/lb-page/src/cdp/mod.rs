//! [`CdpHost`]: `lb_cdp::PageHost` on real pages (chrome build).
//!
//! The CDP server runs on its own thread; pages live on the browser's
//! worker threads. Each page gets an [`agent::Agent`] on its worker that
//! reports events straight to the server's [`EventSink`] (which is
//! `Send`), so events emitted during a call reach the client before the
//! call's reply. Host methods reach a page in three ways:
//! - async methods (`evaluate`, `navigate`, input) run a local future on
//!   the worker and await its result;
//! - sync methods that answer (`DOM.*`, `getProperties`, AX tree) block the
//!   server thread on a worker job, with a timeout (the worker never waits
//!   for the server thread);
//! - sync methods that only change state (bindings, interception,
//!   viewport) post a job without waiting; jobs run in order, so later
//!   calls see the change.
//!
//! Paused requests (`Fetch.requestPaused`) wait on the worker for a
//! decision that `continue_request` / `fulfill_request` / `fail_request`
//! send through a shared table, without touching the worker thread.
//!
//! Browser contexts map to [`BrowserContext`]s (persona, cookie jar,
//! storage); `Target.createTarget` opens a [`Page`] in one. Cookies are
//! read and written through the context's jar.

pub(crate) mod agent;
mod human;
pub(crate) mod net;
mod screenshot;

pub use human::behavior_for;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lb_cdp::types::*;
use lb_cdp::{
    BindingScope, CallFunctionRequest, CallTarget, ContextOptions as CdpContextOptions,
    EvaluateRequest, EventSink, GetPropertiesRequest, HostError, HostResult, NavigateRequest,
    PageEvent, PageHost, ViewportOverride,
};
use lb_net::ClientHelloProfile;
use lb_net::cookies::CookieInfo;
use lb_persona::{BrowserFamily, PersonaProfile};
use tokio::sync::oneshot;

use crate::browser::{Browser, BrowserOptions};
use crate::context::{BrowserContext, ContextOptions, PageError};
use crate::page::Page;
use agent::{Agent, FrameInfo, JsJob, JsOpts};
use net::Decision;

/// How long a sync host method waits for a page's worker.
const BLOCK: Duration = Duration::from_secs(30);

/// State shared by the host and every page agent (any thread).
pub(crate) struct Shared {
    sink: Mutex<Option<EventSink>>,
    pub next_ctx: AtomicI64,
    pub next_request: AtomicU64,
    pub next_intercept: AtomicU64,
    /// Paused requests: interception id → (target, decision channel).
    pub paused: Mutex<HashMap<String, (String, oneshot::Sender<Decision>)>>,
    start: Instant,
    pub seed: u64,
}

impl Shared {
    pub(crate) fn emit(&self, target: &str, ev: PageEvent) {
        let sink = self.sink.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(s) = sink {
            s.emit(target, ev);
        }
    }

    /// Monotonic seconds for CDP timestamps.
    pub(crate) fn mono(&self) -> f64 {
        1000.0 + self.start.elapsed().as_secs_f64()
    }
}

/// How to build a [`CdpHost`].
#[derive(Clone, Debug)]
pub struct CdpHostOptions {
    /// The persona of the default context and of new contexts.
    pub profile: Arc<PersonaProfile>,
    /// Directory with the TLS/H2 profiles (`profiles/tls`).
    pub tls_dir: PathBuf,
    /// Skip certificate checks (our own local test servers only).
    pub accept_invalid_certs: bool,
    /// Run timers on a virtual clock.
    pub virtual_time: bool,
    /// Worker threads (default: available cores).
    pub workers: usize,
}

impl CdpHostOptions {
    pub fn new(profile: Arc<PersonaProfile>, tls_dir: PathBuf) -> CdpHostOptions {
        CdpHostOptions {
            profile,
            tls_dir,
            accept_invalid_certs: false,
            virtual_time: false,
            workers: BrowserOptions::default().workers,
        }
    }
}

struct CtxEntry {
    ctx: BrowserContext,
    profile: Arc<PersonaProfile>,
}

struct PageEntry {
    page: Rc<Page>,
    #[allow(dead_code)]
    context: String,
    frame: Arc<Mutex<FrameInfo>>,
}

/// See the module docs.
pub struct CdpHost {
    browser: Browser,
    opts: CdpHostOptions,
    shared: Arc<Shared>,
    contexts: RefCell<HashMap<String, CtxEntry>>,
    pages: RefCell<HashMap<String, PageEntry>>,
    next_script: Cell<u64>,
}

impl std::fmt::Debug for CdpHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CdpHost")
            .field("pages", &self.pages.borrow().len())
            .finish()
    }
}

/// Context options for `profile`: its network persona and the TLS/H2
/// profile it names. Fails when they do not belong together.
pub fn context_options_for(
    profile: &Arc<PersonaProfile>,
    tls_dir: &std::path::Path,
    accept_invalid_certs: bool,
    virtual_time: bool,
) -> Result<ContextOptions, String> {
    let net = profile.to_network();
    let path = tls_dir.join(format!("{}.json", net.tls_profile_id));
    let tls =
        ClientHelloProfile::load(&path).map_err(|e| format!("loading {}: {e}", path.display()))?;
    if let Some(f) = tls.family()
        && f != net.family
    {
        return Err(format!(
            "TLS profile {} is a {} profile but the persona is {}",
            tls.profile_id, f, net.family
        ));
    }
    Ok(ContextOptions {
        persona: Arc::new(net),
        profile: Some(profile.clone()),
        tls_profile: Arc::new(tls),
        accept_invalid_certs,
        virtual_time,
    })
}

fn page_err(e: PageError) -> HostError {
    match e {
        PageError::Closed => HostError::new("Target closed"),
        PageError::Timeout => HostError::new("Page did not respond in time"),
        other => HostError::new(other.to_string()),
    }
}

impl CdpHost {
    /// Start the worker threads; the default context is created by the
    /// server through [`PageHost::create_context`].
    pub fn new(opts: CdpHostOptions) -> Result<CdpHost, String> {
        // Validate the persona/TLS pair once, up front.
        context_options_for(
            &opts.profile,
            &opts.tls_dir,
            opts.accept_invalid_certs,
            opts.virtual_time,
        )?;
        let browser = Browser::new(BrowserOptions {
            workers: opts.workers.max(1),
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Ok(CdpHost {
            browser,
            opts,
            shared: Arc::new(Shared {
                sink: Mutex::new(None),
                next_ctx: AtomicI64::new(1),
                next_request: AtomicU64::new(1),
                next_intercept: AtomicU64::new(1),
                paused: Mutex::new(HashMap::new()),
                start: Instant::now(),
                seed,
            }),
            contexts: RefCell::new(HashMap::new()),
            pages: RefCell::new(HashMap::new()),
            next_script: Cell::new(0),
        })
    }

    fn page(&self, target: &str) -> HostResult<Rc<Page>> {
        self.pages
            .borrow()
            .get(target)
            .map(|p| p.page.clone())
            .ok_or_else(|| HostError::new("No target with given id found"))
    }

    fn context(&self, id: &str) -> HostResult<BrowserContext> {
        self.contexts
            .borrow()
            .get(id)
            .map(|c| c.ctx.clone())
            .ok_or_else(|| HostError::new("Failed to find browser context with id"))
    }

    fn build_context(&self, profile: &Arc<PersonaProfile>) -> HostResult<BrowserContext> {
        let opts = context_options_for(
            profile,
            &self.opts.tls_dir,
            self.opts.accept_invalid_certs,
            self.opts.virtual_time,
        )
        .map_err(HostError::new)?;
        BrowserContext::new(opts).map_err(page_err)
    }

    /// Run `f` with the page's agent and current window on its worker,
    /// blocking until it answers.
    fn blocking<R: Send + 'static>(
        &self,
        target: &str,
        f: impl FnOnce(
            &crate::browser::WorkerState,
            &Agent,
            &Rc<lb_webapi::rt::Window>,
        ) -> HostResult<R>
        + Send
        + 'static,
    ) -> HostResult<R> {
        let page = self.page(target)?;
        page.call_blocking(BLOCK, move |ws, p| {
            let agent = p.agent().ok_or_else(|| HostError::new("Target closed"))?;
            let w = p.window().ok_or_else(|| HostError::new("Target closed"))?;
            f(ws, &agent, &w)
        })
        .map_err(page_err)?
    }

    /// Queue `f` with the page's agent on its worker.
    fn post(
        &self,
        target: &str,
        f: impl FnOnce(&Rc<crate::browser::WorkerState>, &Rc<crate::page::PageState>, &Agent)
        + Send
        + 'static,
    ) -> HostResult<()> {
        let page = self.page(target)?;
        page.post(move |ws, p| {
            if let Some(a) = p.agent() {
                f(ws, p, &a);
            }
        })
        .map_err(page_err)
    }

    /// Run an async job on the page's worker.
    async fn on_worker<R, F, Fut>(&self, target: &str, f: F) -> HostResult<R>
    where
        R: Send + 'static,
        F: FnOnce(Rc<crate::browser::WorkerState>, Rc<crate::page::PageState>) -> Fut
            + Send
            + 'static,
        Fut: std::future::Future<Output = HostResult<R>> + 'static,
    {
        let page = self.page(target)?;
        page.call(move |ws, p| async move { Ok(f(ws, p).await) })
            .await
            .map_err(page_err)?
    }
}

fn cookie_to_cdp(c: CookieInfo) -> Cookie {
    let host = c.domain.trim_start_matches('.').to_string();
    Cookie {
        size: (c.name.len() + c.value.len()) as i64,
        session: c.expires.is_none(),
        expires: c.expires.unwrap_or(-1.0),
        same_site: c.same_site,
        priority: "Medium".into(),
        same_party: false,
        source_scheme: if c.secure { "Secure" } else { "NonSecure" }.into(),
        source_port: if c.secure { 443 } else { 80 },
        name: c.name,
        value: c.value,
        domain: if c.domain.is_empty() { host } else { c.domain },
        path: c.path,
        http_only: c.http_only,
        secure: c.secure,
    }
}

impl PageHost for CdpHost {
    fn family(&self) -> BrowserFamily {
        lb_js::COMPILED_FAMILY.unwrap_or(BrowserFamily::Chrome)
    }

    fn set_event_sink(&self, sink: EventSink) {
        *self.shared.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    fn persona(&self, context: &str) -> Arc<PersonaProfile> {
        self.contexts
            .borrow()
            .get(context)
            .map(|c| c.profile.clone())
            .unwrap_or_else(|| self.opts.profile.clone())
    }

    fn set_persona(&self, context: &str, persona: Arc<PersonaProfile>) -> HostResult<()> {
        let ctx = self.build_context(&persona)?;
        self.contexts.borrow_mut().insert(
            context.to_string(),
            CtxEntry {
                ctx,
                profile: persona,
            },
        );
        Ok(())
    }

    fn create_context(&self, id: &str, _opts: CdpContextOptions) -> HostResult<()> {
        let profile = self.opts.profile.clone();
        let ctx = self.build_context(&profile)?;
        self.contexts
            .borrow_mut()
            .insert(id.to_string(), CtxEntry { ctx, profile });
        Ok(())
    }

    fn dispose_context(&self, id: &str) -> HostResult<()> {
        self.contexts.borrow_mut().remove(id);
        Ok(())
    }

    async fn create_page(&self, context: &str, target_id: &str) -> HostResult<()> {
        let ctx = self.context(context)?;
        let frame = Arc::new(Mutex::new(FrameInfo {
            url: "about:blank".into(),
            ..Default::default()
        }));
        let (shared, f2, target) = (self.shared.clone(), frame.clone(), target_id.to_string());
        let setup: crate::page::PageSetup = Box::new(move |ps| {
            *ps.cdp.borrow_mut() = Some(Rc::new(Agent::new(target, ps.id, shared, f2)));
        });
        let page = self
            .browser
            .new_page_with(&ctx, Some(setup))
            .await
            .map_err(page_err)?;
        self.pages.borrow_mut().insert(
            target_id.to_string(),
            PageEntry {
                page: Rc::new(page),
                context: context.to_string(),
                frame,
            },
        );
        Ok(())
    }

    fn close_page(&self, target_id: &str) -> HostResult<()> {
        let entry = self.pages.borrow_mut().remove(target_id);
        if let Some(e) = entry {
            // Release requests still waiting for a decision.
            self.shared
                .paused
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|_, (t, _)| t != target_id);
            let _ = e.page.close_now();
        }
        Ok(())
    }

    async fn navigate(&self, target_id: &str, req: NavigateRequest) -> HostResult<NavigateResult> {
        if let Some(f) = &req.frame_id
            && f != target_id
        {
            return Err(HostError::new("No frame for given id found"));
        }
        let page = self.page(target_id)?;
        if url::Url::parse(&req.url).is_err() {
            return Err(HostError::new("Cannot navigate to invalid URL"));
        }
        let mut result = NavigateResult {
            frame_id: target_id.to_string(),
            loader_id: None,
            error_text: None,
        };
        match page.navigate(&req.url).await {
            Ok(nav) => result.loader_id = Some(agent::loader_id(page.id(), nav.loader_id)),
            Err(PageError::Net(e)) => result.error_text = Some(net::net_error_text(&e)),
            Err(PageError::Aborted) => result.error_text = Some("net::ERR_ABORTED".into()),
            Err(e) => return Err(page_err(e)),
        }
        Ok(result)
    }

    async fn reload(&self, target_id: &str, _ignore_cache: bool) -> HostResult<()> {
        let url = self
            .pages
            .borrow()
            .get(target_id)
            .map(|p| {
                p.frame
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .url
                    .clone()
            })
            .ok_or_else(|| HostError::new("No target with given id found"))?;
        self.navigate(
            target_id,
            NavigateRequest {
                url,
                ..Default::default()
            },
        )
        .await
        .map(|_| ())
    }

    fn frame_tree(&self, target_id: &str) -> HostResult<FrameTree> {
        let f = {
            let pages = self.pages.borrow();
            let p = pages
                .get(target_id)
                .ok_or_else(|| HostError::new("No target with given id found"))?;
            p.frame.lock().unwrap_or_else(|e| e.into_inner()).clone()
        };
        let children = self
            .blocking(target_id, |_, a, w| Ok(child_trees(a, w)))
            .unwrap_or_default();
        Ok(FrameTree {
            frame: Frame::new(target_id.into(), None, f.loader, &f.url),
            child_frames: children,
        })
    }

    fn create_isolated_world(
        &self,
        target_id: &str,
        frame_id: &str,
        world_name: &str,
        _grant_universal_access: bool,
    ) -> HostResult<ExecutionContextId> {
        let page = self.page(target_id)?;
        let name = world_name.to_string();
        let frame = frame_id.to_string();
        page.call_blocking(BLOCK, move |ws, p| {
            let agent = p.agent().ok_or_else(|| HostError::new("Target closed"))?;
            let top = p.window().ok_or_else(|| HostError::new("Target closed"))?;
            let w = agent
                .frame_of(&frame)
                .and_then(|id| lb_webapi::rt::frames::find(&top, id))
                .ok_or_else(|| HostError::new("No frame for given id found"))?;
            // Reported now, made on first use (nothing runs in it before).
            let _ = ws;
            Ok(agent.announce_world(&w, &name))
        })
        .map_err(page_err)?
    }

    async fn fingerprint_report(&self, target_id: &str) -> HostResult<serde_json::Value> {
        self.on_worker(target_id, |ws, p| async move {
            crate::fingerprint::report(&ws, &p)
                .await
                .map_err(HostError::new)
        })
        .await
    }

    fn add_script_to_evaluate_on_new_document(
        &self,
        target_id: &str,
        source: &str,
        world_name: Option<&str>,
        run_immediately: bool,
    ) -> HostResult<String> {
        let id = self.next_script.get() + 1;
        self.next_script.set(id);
        let id = id.to_string();
        let (sid, src, world) = (
            id.clone(),
            source.to_string(),
            world_name.map(str::to_string),
        );
        self.post(target_id, move |ws, p, a| {
            a.st.borrow_mut()
                .scripts
                .push((sid, src.clone(), world.clone()));
            if run_immediately && let Some(w) = p.window() {
                let realm = match &world {
                    None => Some(w.realm()),
                    Some(n) => {
                        let ctx =
                            a.st.borrow()
                                .worlds
                                .iter()
                                .find(|x| x.frame == 0 && x.name.as_deref() == Some(n))
                                .map(|x| x.id);
                        ctx.and_then(|c| ws.with_engine(|e| a.world_realm(e, &w, c).ok()))
                            .flatten()
                    }
                };
                if let Some(realm) = realm {
                    ws.with_engine(|e| {
                        if let Ok(h) =
                            e.evaluate_catching(realm, &src, lb_js::ScriptOrigin::new(""))
                        {
                            e.release(h);
                        }
                    });
                    agent::settle(ws, &w);
                }
            }
        })?;
        Ok(id)
    }

    fn remove_script_to_evaluate_on_new_document(
        &self,
        target_id: &str,
        identifier: &str,
    ) -> HostResult<()> {
        let id = identifier.to_string();
        self.post(target_id, move |_, _, a| {
            a.st.borrow_mut().scripts.retain(|(i, _, _)| *i != id);
        })
    }

    async fn evaluate(&self, target_id: &str, req: EvaluateRequest) -> HostResult<EvalResult> {
        let opts = JsOpts {
            by_value: req.return_by_value,
            preview: req.generate_preview,
            await_promise: req.await_promise,
            group: req.object_group.clone(),
        };
        let (ctx, expr) = (req.context_id, req.expression);
        self.on_worker(target_id, move |ws, p| {
            agent::run_js(ws, p, ctx, JsJob::Eval(expr), opts)
        })
        .await
    }

    async fn call_function_on(
        &self,
        target_id: &str,
        req: CallFunctionRequest,
    ) -> HostResult<EvalResult> {
        let opts = JsOpts {
            by_value: req.return_by_value,
            preview: req.generate_preview,
            await_promise: req.await_promise,
            group: req.object_group.clone(),
        };
        self.on_worker(target_id, move |ws, p| async move {
            let agent = p.agent().ok_or_else(|| HostError::new("Target closed"))?;
            let (ctx, this) = match req.target {
                CallTarget::Context(id) => (id, None),
                CallTarget::Object(o) => {
                    let obj = agent.object(&o)?;
                    let group = opts.group.clone().or(obj.group.clone());
                    let opts2 = JsOpts {
                        group,
                        ..opts.clone()
                    };
                    let job = JsJob::Call {
                        decl: req.function_declaration,
                        this: Some(o),
                        args: req.arguments,
                    };
                    return agent::run_js(ws, p, obj.ctx, job, opts2).await;
                }
            };
            let job = JsJob::Call {
                decl: req.function_declaration,
                this,
                args: req.arguments,
            };
            agent::run_js(ws, p, ctx, job, opts).await
        })
        .await
    }

    fn get_properties(&self, target_id: &str, req: GetPropertiesRequest) -> HostResult<Properties> {
        let page = self.page(target_id)?;
        page.call_blocking(BLOCK, move |ws, p| agent::get_properties(ws, p, &req))
            .map_err(page_err)?
    }

    fn release_object(&self, target_id: &str, object_id: &str) -> HostResult<()> {
        let id = object_id.to_string();
        self.post(target_id, move |ws, _, a| {
            let o = a.st.borrow_mut().objects.remove(&id);
            if let Some(o) = o {
                ws.with_engine(|e| e.release(o.handle));
            }
        })
    }

    fn release_object_group(&self, target_id: &str, group: &str) -> HostResult<()> {
        let group = group.to_string();
        self.post(target_id, move |ws, _, a| {
            let gone: Vec<agent::ObjEntry> = {
                let mut st = a.st.borrow_mut();
                let ids: Vec<String> = st
                    .objects
                    .iter()
                    .filter(|(_, o)| o.group.as_deref() == Some(group.as_str()))
                    .map(|(k, _)| k.clone())
                    .collect();
                ids.iter().filter_map(|k| st.objects.remove(k)).collect()
            };
            ws.with_engine(|e| {
                for o in gone {
                    e.release(o.handle);
                }
            });
        })
    }

    fn add_binding(&self, target_id: &str, name: &str, scope: BindingScope) -> HostResult<()> {
        let name = name.to_string();
        self.post(target_id, move |ws, _, a| {
            a.st.borrow_mut()
                .bindings
                .push((name.clone(), scope.clone()));
            let worlds = a.st.borrow().worlds.clone();
            for w in worlds {
                let hit = match (&scope, &w.name) {
                    (_, None) => false,
                    (BindingScope::AllIsolatedWorlds, Some(_)) => true,
                    (BindingScope::World(n), Some(x)) => n == x,
                    (BindingScope::Context(id), Some(_)) => *id == w.id,
                };
                // Worlds not made yet get their bindings when they are.
                if hit && let Some(realm) = w.realm {
                    a.install_binding(ws, realm, w.id, &name);
                }
            }
        })
    }

    // ---- DOM ----

    fn get_document(&self, target_id: &str, depth: i32) -> HostResult<DomNode> {
        self.blocking(target_id, move |_, a, w| {
            Ok(agent::dom_node(a, w, agent::DOCUMENT, depth))
        })
    }

    fn describe_node(&self, target_id: &str, node: &NodeRef, depth: i32) -> HostResult<DomNode> {
        let node = node.clone();
        self.blocking(target_id, move |ws, a, w| {
            let n = a.node(ws, w, &node)?;
            Ok(agent::dom_node(a, w, n, depth))
        })
    }

    fn resolve_node(
        &self,
        target_id: &str,
        node: BackendNodeId,
        context: Option<ExecutionContextId>,
        object_group: Option<&str>,
    ) -> HostResult<RemoteObject> {
        let group = object_group.map(str::to_string);
        self.blocking(target_id, move |ws, a, w| {
            let n = a.node(ws, w, &NodeRef::Backend(node))?;
            let ctx = match context {
                Some(c) => c,
                None => a
                    .main_ctx()
                    .ok_or_else(|| HostError::new("Cannot find default execution context"))?,
            };
            a.node_object(ws, w, n, ctx, group.as_deref())
        })
    }

    fn request_node(&self, target_id: &str, object_id: &str) -> HostResult<BackendNodeId> {
        let oid = object_id.to_string();
        self.blocking(target_id, move |ws, a, w| {
            let n = a.node(ws, w, &NodeRef::Object(oid))?;
            Ok(agent::backend_id(a.current_seq(), n))
        })
    }

    fn query_selector(
        &self,
        target_id: &str,
        node: BackendNodeId,
        selector: &str,
    ) -> HostResult<Option<BackendNodeId>> {
        let sel = selector.to_string();
        self.blocking(target_id, move |ws, a, w| {
            let n = a.node(ws, w, &NodeRef::Backend(node))?;
            let seq = a.current_seq();
            w.with_doc(|d| d.query_selector(n, &sel))
                .map(|r| r.map(|x| agent::backend_id(seq, x)))
                .map_err(|_| HostError::new("DOM Error while querying"))
        })
    }

    fn query_selector_all(
        &self,
        target_id: &str,
        node: BackendNodeId,
        selector: &str,
    ) -> HostResult<Vec<BackendNodeId>> {
        let sel = selector.to_string();
        self.blocking(target_id, move |ws, a, w| {
            let n = a.node(ws, w, &NodeRef::Backend(node))?;
            let seq = a.current_seq();
            w.with_doc(|d| d.query_selector_all(n, &sel))
                .map(|v| v.into_iter().map(|x| agent::backend_id(seq, x)).collect())
                .map_err(|_| HostError::new("DOM Error while querying"))
        })
    }

    fn outer_html(&self, target_id: &str, node: &NodeRef) -> HostResult<String> {
        let node = node.clone();
        self.blocking(target_id, move |ws, a, w| {
            let n = a.node(ws, w, &node)?;
            Ok(if n == agent::DOCUMENT {
                w.content()
            } else {
                w.with_doc(|d| d.outer_html(n))
            })
        })
    }

    fn box_model(&self, target_id: &str, node: &NodeRef) -> HostResult<BoxModel> {
        let node = node.clone();
        self.blocking(target_id, move |ws, a, w| {
            let n = a.node(ws, w, &node)?;
            w.with_layout_engine(|l, d| l.box_model(d, n))
                .map(|m| lb_cdp::convert::box_model(&m))
                .ok_or_else(|| HostError::new("Could not compute box model."))
        })
    }

    fn content_quads(&self, target_id: &str, node: &NodeRef) -> HostResult<Vec<Quad>> {
        let node = node.clone();
        self.blocking(target_id, move |ws, a, w| {
            let n = a.node(ws, w, &node)?;
            let rects = w.with_layout_engine(|l, d| l.client_rects(d, n));
            Ok(rects
                .iter()
                .filter(|r| r.width > 0.0 && r.height > 0.0)
                .map(|r| lb_cdp::convert::rect(r).quad())
                .collect())
        })
    }

    fn scroll_into_view_if_needed(
        &self,
        target_id: &str,
        node: &NodeRef,
        _rect: Option<Rect>,
    ) -> HostResult<()> {
        let node = node.clone();
        self.blocking(target_id, move |ws, a, w| {
            let n = a.node(ws, w, &node)?;
            if !w.with_doc(|d| d.is_connected(n)) {
                return Err(HostError::new("Node is detached from document"));
            }
            w.with_layout_engine(|l, d| {
                if l.box_model(d, n).is_none() {
                    return Err(HostError::new("Node does not have a layout object"));
                }
                l.scroll_into_view(d, n, lb_layout::ScrollAlign::Center, true);
                Ok(())
            })
        })
    }

    // ---- Input ----

    async fn dispatch_mouse_event(&self, target_id: &str, ev: MouseEvent) -> HostResult<()> {
        self.on_worker(
            target_id,
            move |ws, p| async move { input::mouse(&ws, &p, ev) },
        )
        .await
    }

    async fn dispatch_key_event(&self, target_id: &str, ev: KeyEvent) -> HostResult<()> {
        self.on_worker(
            target_id,
            move |ws, p| async move { input::key(&ws, &p, ev) },
        )
        .await
    }

    async fn human_input(
        &self,
        target_id: &str,
        cmd: HumanCommand,
    ) -> HostResult<serde_json::Value> {
        self.on_worker(target_id, move |ws, p| human::run(ws, p, cmd))
            .await
    }

    async fn insert_text(&self, target_id: &str, text: &str) -> HostResult<()> {
        let text = text.to_string();
        self.on_worker(target_id, move |ws, p| async move {
            input::text(&ws, &p, &text)
        })
        .await
    }

    // ---- Network / Fetch / Storage ----

    fn set_extra_http_headers(
        &self,
        target_id: &str,
        headers: Vec<(String, String)>,
    ) -> HostResult<()> {
        self.post(target_id, move |_, _, a| {
            a.st.borrow_mut().extra_headers = headers
        })
    }

    fn cookies(&self, context: &str, urls: Option<&[String]>) -> HostResult<Vec<Cookie>> {
        let jar = self.context(context)?.cookies();
        let list = match urls {
            None => jar.all(),
            Some(urls) => {
                let mut out: Vec<CookieInfo> = Vec::new();
                for u in urls {
                    if let Ok(u) = url::Url::parse(u) {
                        for c in jar.for_url(&u) {
                            if !out.contains(&c) {
                                out.push(c);
                            }
                        }
                    }
                }
                out
            }
        };
        Ok(list.into_iter().map(cookie_to_cdp).collect())
    }

    fn set_cookies(&self, context: &str, cookies: Vec<CookieParam>) -> HostResult<()> {
        let jar = self.context(context)?.cookies();
        for c in cookies {
            let path = c.path.clone().unwrap_or_else(|| "/".into());
            let url = match (&c.url, &c.domain) {
                (Some(u), _) => {
                    url::Url::parse(u).map_err(|_| HostError::new("Invalid cookie fields"))?
                }
                (None, Some(d)) => {
                    let scheme = if c.secure.unwrap_or(false) {
                        "https"
                    } else {
                        "http"
                    };
                    url::Url::parse(&format!("{scheme}://{}{path}", d.trim_start_matches('.')))
                        .map_err(|_| HostError::new("Invalid cookie fields"))?
                }
                (None, None) => return Err(HostError::new("Invalid cookie fields")),
            };
            // A cookie with an explicit domain is a domain cookie, as in
            // Chrome; one set through a URL is host-only.
            let domain = match &c.domain {
                Some(d) if !d.starts_with('.') && c.url.is_none() => format!(".{d}"),
                Some(d) => d.clone(),
                None => url.host_str().unwrap_or("").to_string(),
            };
            let info = CookieInfo {
                name: c.name,
                value: c.value,
                domain,
                path,
                expires: c.expires.filter(|e| *e > 0.0),
                http_only: c.http_only.unwrap_or(false),
                secure: c.secure.unwrap_or(false),
                same_site: c.same_site,
            };
            jar.set(&url, &info)
                .map_err(|e| HostError::new(format!("Invalid cookie fields: {e}")))?;
        }
        Ok(())
    }

    fn delete_cookies(&self, context: &str, f: DeleteCookies) -> HostResult<()> {
        let jar = self.context(context)?.cookies();
        let host = f
            .url
            .as_deref()
            .and_then(|u| url::Url::parse(u).ok())
            .and_then(|u| u.host_str().map(str::to_string));
        jar.remove(
            &f.name,
            f.domain.as_deref().or(host.as_deref()),
            f.path.as_deref(),
        );
        Ok(())
    }

    fn clear_cookies(&self, context: &str) -> HostResult<()> {
        self.context(context)?.cookies().clear();
        Ok(())
    }

    fn response_body(&self, target_id: &str, request_id: &str) -> HostResult<(String, bool)> {
        let id = request_id.to_string();
        self.blocking(target_id, move |_, a, _| {
            let st = a.st.borrow();
            let body = st
                .bodies
                .iter()
                .rev()
                .find(|(k, _)| *k == id)
                .map(|(_, b)| b.clone())
                .ok_or_else(|| HostError::new("No resource with given identifier found"))?;
            Ok(match String::from_utf8(body.to_vec()) {
                Ok(s) => (s, false),
                Err(e) => (net::base64_encode(e.as_bytes()), true),
            })
        })
    }

    fn set_request_interception(
        &self,
        target_id: &str,
        patterns: Option<Vec<RequestPattern>>,
    ) -> HostResult<()> {
        if patterns.is_none() {
            // Let requests that are still paused go on.
            let waiting: Vec<oneshot::Sender<Decision>> = {
                let mut m = self.shared.paused.lock().unwrap_or_else(|e| e.into_inner());
                let ids: Vec<String> = m
                    .iter()
                    .filter(|(_, (t, _))| t == target_id)
                    .map(|(k, _)| k.clone())
                    .collect();
                ids.iter()
                    .filter_map(|k| m.remove(k))
                    .map(|(_, tx)| tx)
                    .collect()
            };
            for tx in waiting {
                let _ = tx.send(Decision::Continue(ContinueRequest::default()));
            }
        }
        self.post(target_id, move |_, _, a| {
            a.st.borrow_mut().intercept = patterns
        })
    }

    fn continue_request(&self, target_id: &str, req: ContinueRequest) -> HostResult<()> {
        let tx = self.take_paused(target_id, &req.request_id)?;
        let _ = tx.send(Decision::Continue(req));
        Ok(())
    }

    fn fulfill_request(&self, target_id: &str, req: FulfillRequest) -> HostResult<()> {
        let tx = self.take_paused(target_id, &req.request_id)?;
        let _ = tx.send(Decision::Fulfill(req));
        Ok(())
    }

    fn fail_request(&self, target_id: &str, request_id: &str, reason: &str) -> HostResult<()> {
        let tx = self.take_paused(target_id, request_id)?;
        let _ = tx.send(Decision::Fail(reason.to_string()));
        Ok(())
    }

    fn set_ignore_certificate_errors(
        &self,
        _target_id: Option<&str>,
        _ignore: bool,
    ) -> HostResult<()> {
        Ok(())
    }

    // ---- Emulation ----

    fn set_viewport(&self, target_id: &str, viewport: Option<ViewportOverride>) -> HostResult<()> {
        let persona = {
            let pages = self.pages.borrow();
            let ctx = pages
                .get(target_id)
                .map(|p| p.context.clone())
                .unwrap_or_default();
            drop(pages);
            self.persona(&ctx)
        };
        let (vw, vh) = viewport.map_or(
            (persona.screen.inner_width, persona.screen.inner_height),
            |v| (v.width, v.height),
        );
        let keep = viewport.map(|_| (vw, vh));
        self.post(target_id, move |_, p, a| {
            a.st.borrow_mut().viewport = keep;
            if let Some(w) = p.window() {
                w.with_layout_engine(|l, _| l.set_viewport(vw as f32, vh as f32));
            }
        })
    }

    // ---- Screenshots ----

    fn capture_screenshot(
        &self,
        target_id: &str,
        params: &serde_json::Value,
    ) -> HostResult<String> {
        let req = screenshot::Request::parse(params)?;
        // Background images still loading: let the page's worker fetch
        // them (up to 5 s), then paint.
        for _ in 0..250 {
            if !self.blocking(target_id, |_, _, w| Ok(screenshot::images_pending(w)))? {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let bytes = self.blocking(target_id, move |_, a, w| {
            let bg = a.st.borrow().screenshot_bg;
            screenshot::capture(w, &req, bg)
        })?;
        let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
        lb_paint::encode::base64_into(&bytes, &mut out);
        Ok(out)
    }

    fn page_metrics(&self, target_id: &str) -> HostResult<Option<lb_cdp::PageMetrics>> {
        self.blocking(target_id, |_, _, w| Ok(Some(screenshot::metrics(w))))
    }

    fn set_default_background_color(
        &self,
        target_id: &str,
        color: Option<lb_cdp::Rgba>,
    ) -> HostResult<()> {
        self.post(target_id, move |_, _, a| {
            a.st.borrow_mut().screenshot_bg = color;
        })
    }

    // ---- Accessibility ----

    fn ax_tree(&self, target_id: &str) -> HostResult<Vec<AxNode>> {
        self.blocking(target_id, move |_, a, w| {
            let ids = a.ids();
            let tree = w.with_layout_engine(|l, d| l.accessibility_tree(d));
            Ok(lb_cdp::convert::ax_nodes(&tree, &ids, &a.target))
        })
    }
}

impl CdpHost {
    /// The main-world execution context of a page's current document.
    pub fn main_context(&self, target_id: &str) -> HostResult<ExecutionContextId> {
        self.blocking(target_id, |_, a, _| {
            a.main_ctx()
                .ok_or_else(|| HostError::new("Cannot find default execution context"))
        })
    }

    fn take_paused(&self, target_id: &str, id: &str) -> HostResult<oneshot::Sender<Decision>> {
        let mut m = self.shared.paused.lock().unwrap_or_else(|e| e.into_inner());
        match m.get(id) {
            Some((t, _)) if t == target_id => Ok(m.remove(id).map(|(_, tx)| tx).expect("present")),
            _ => Err(HostError::new("Invalid InterceptionId.")),
        }
    }
}

mod input;

/// Frame trees of the child frames of `w`.
fn child_trees(a: &agent::Agent, w: &lb_webapi::rt::Window) -> Vec<FrameTree> {
    use lb_webapi::rt::frames;
    frames::children(w)
        .into_iter()
        .map(|c| {
            let loader =
                a.st.borrow()
                    .frame_loaders
                    .get(&c.id)
                    .cloned()
                    .unwrap_or_default();
            let mut frame = Frame::new(
                a.frame_str(c.id),
                Some(a.frame_str(frames::frame_id(w))),
                loader,
                c.window.url().as_str(),
            );
            let name = frames::window_name(&c.window);
            if !name.is_empty() {
                frame.name = Some(name);
            }
            FrameTree {
                frame,
                child_frames: child_trees(a, &c.window),
            }
        })
        .collect()
}

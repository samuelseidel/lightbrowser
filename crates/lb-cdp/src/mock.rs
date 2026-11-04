//! [`MockHost`]: a [`PageHost`] without a JS engine, for tests and for the
//! firefox build's `lightbrowser serve` (the chrome build uses
//! `lb_page::CdpHost`).
//!
//! - Documents are real: `lb-dom` parses the HTML, `lb-layout` gives box
//!   models, hit tests and the accessibility tree.
//! - It never touches the network. Pages come from [`MockHost::add_page`];
//!   any other loopback URL gets a small built-in form page. Other hosts fail
//!   with `net::ERR_BLOCKED_BY_CLIENT` (loopback only).
//! - "JavaScript" is a tiny expression evaluator: literals, arithmetic,
//!   comparisons, `document.title`, `document.querySelector(...)`, node
//!   properties, `navigator.*` from the persona, `console.*`, binding calls,
//!   and arrow/function forms with parameters. It also recognises
//!   Playwright's utility-script trampoline and `page.content()`, which is
//!   enough for `connectOverCDP` + `goto` + `evaluate('1+1')` + `content()`.
//!   Anything else throws an `EvalError` that says so.
//! - Navigation emits Chrome's event sequence: `frameStartedLoading`,
//!   `requestWillBeSent`, `responseReceived`, contexts destroyed/cleared,
//!   `frameNavigated`, new main and isolated-world contexts, lifecycle
//!   `init`; then on the next tick `loadingFinished`, `DOMContentLoaded`,
//!   `load`, `frameStoppedLoading`, `networkAlmostIdle`, `networkIdle`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use lb_dom::{Document, NodeId};
use lb_layout::{LayoutEngine, ScrollAlign};
use lb_persona::{BrowserFamily, PersonaProfile};
use serde_json::{Value, json};

use crate::convert::{self, DocInfo};
use crate::host::*;
use crate::ids::IdGen;
use crate::remote::{JsValue, ObjectInfo, RemoteOptions, remote_object};
use crate::types::*;

/// What a mock object handle refers to.
#[derive(Clone, Debug)]
enum MockObj {
    Node(NodeId),
    /// An opaque script object (`UtilityScript`, `InjectedScript`).
    Named(String),
    Value(JsValue),
}

struct MockPage {
    context: String,
    url: String,
    loader: String,
    doc: Document,
    layout: LayoutEngine,
    contexts: Vec<ExecutionContext>,
    scripts: Vec<(String, Option<String>)>,
    bindings: Vec<(String, BindingScope)>,
    objects: HashMap<String, (ExecutionContextId, MockObj, Option<String>)>,
    focused: Option<NodeId>,
    intercept: Option<Vec<RequestPattern>>,
    paused: HashMap<String, (String, String)>,
    bodies: HashMap<String, String>,
    status: i64,
}

#[derive(Default)]
struct MockState {
    personas: HashMap<String, Arc<PersonaProfile>>,
    sites: HashMap<String, String>,
    pages: HashMap<TargetId, MockPage>,
    cookies: HashMap<String, Vec<Cookie>>,
    next_ctx: ExecutionContextId,
    next_obj: u64,
    next_script: u64,
    next_intercept: u64,
    input_log: Vec<String>,
}

/// See the module docs.
pub struct MockHost {
    default_persona: Arc<PersonaProfile>,
    family: BrowserFamily,
    state: RefCell<MockState>,
    sink: RefCell<Option<EventSink>>,
    ids: IdGen,
    start: Instant,
}

const BLANK: &str = "<html><head></head><body></body></html>";

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The page served for loopback URLs without a registered page.
fn default_page(url: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><title>LightBrowser mock</title></head><body>\
<h1>Mock page</h1><p id=\"url\">{}</p>\
<form id=\"login\"><label for=\"user\">User</label><input id=\"user\" name=\"user\">\
<input type=\"checkbox\" id=\"remember\" aria-label=\"Remember me\">\
<button id=\"go\" type=\"button\">Log in</button></form></body></html>",
        escape(url)
    )
}

fn is_loopback_url(url: &str) -> bool {
    match url::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => {
            matches!(
                u.host(),
                Some(url::Host::Ipv4(ip)) if ip.is_loopback()
            ) || matches!(u.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback())
                || u.host_str()
                    .is_some_and(|h| h == "localhost" || h.ends_with(".localhost"))
        }
        _ => false,
    }
}

fn wall_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// Glob match for `Fetch.RequestPattern.urlPattern` (`*` and `?`).
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Standard base64 (for `Fetch.fulfillRequest` bodies).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut buf, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        buf = (buf << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

impl MockHost {
    /// A mock with `persona` for the default context and every new one.
    pub fn new(persona: PersonaProfile) -> MockHost {
        MockHost {
            family: persona.browser.family,
            default_persona: Arc::new(persona),
            state: RefCell::new(MockState {
                next_ctx: 1,
                ..Default::default()
            }),
            sink: RefCell::new(None),
            ids: IdGen::new(),
            start: Instant::now(),
        }
    }

    /// Serves `html` for `url` (exact match, fragment ignored).
    pub fn add_page(&self, url: &str, html: &str) {
        self.state
            .borrow_mut()
            .sites
            .insert(url.to_string(), html.to_string());
    }

    /// Input events and file selections seen so far, one line each.
    pub fn input_log(&self) -> Vec<String> {
        self.state.borrow().input_log.clone()
    }

    /// Reports an uncaught page error (`Runtime.exceptionThrown`).
    pub fn page_error(&self, target_id: &str, message: &str) {
        let ctx = self.state.borrow().pages.get(target_id).and_then(main_ctx);
        self.emit(
            target_id,
            PageEvent::ExceptionThrown {
                timestamp: wall_ms(),
                details: exception(message, ctx),
            },
        );
    }

    /// The `value` attribute of the element with `id` (what typing produced).
    pub fn input_value(&self, target_id: &str, id: &str) -> Option<String> {
        let st = self.state.borrow();
        let p = st.pages.get(target_id)?;
        let n = p.doc.get_element_by_id(id)?;
        p.doc.get_attribute(n, "value").map(str::to_string)
    }

    fn emit(&self, target: &str, ev: PageEvent) {
        if let Some(s) = self.sink.borrow().as_ref() {
            s.emit(target, ev);
        }
    }

    fn now(&self) -> f64 {
        1000.0 + self.start.elapsed().as_secs_f64()
    }

    fn persona_for(&self, ctx: &str) -> Arc<PersonaProfile> {
        self.state
            .borrow()
            .personas
            .get(ctx)
            .cloned()
            .unwrap_or_else(|| self.default_persona.clone())
    }

    fn origin_of(url: &str) -> String {
        match url::Url::parse(url) {
            Ok(u) if matches!(u.scheme(), "http" | "https") => u.origin().ascii_serialization(),
            _ => "://".into(),
        }
    }

    fn new_context(
        &self,
        st: &mut MockState,
        target: &str,
        world: World,
        url: &str,
    ) -> ExecutionContext {
        let id = st.next_ctx;
        st.next_ctx += 1;
        let ctx = ExecutionContext {
            id,
            frame_id: target.to_string(),
            world,
            origin: Self::origin_of(url),
            unique_id: format!("{}.{}", self.ids.hex32().get(..16).unwrap_or("0"), id),
        };
        if let Some(p) = st.pages.get_mut(target) {
            p.contexts.push(ctx.clone());
        }
        ctx
    }

    /// Starts a navigation; commits now unless interception pauses it.
    fn start_navigation(&self, target: &str, url: &str) -> HostResult<NavigateResult> {
        let loader = self.ids.hex32();
        let mut result = NavigateResult {
            frame_id: target.to_string(),
            loader_id: Some(loader.clone()),
            error_text: None,
        };
        let blank = url == "about:blank" || url.starts_with("about:");
        if !blank && !is_loopback_url(url) {
            result.error_text = Some("net::ERR_BLOCKED_BY_CLIENT".into());
            return Ok(result);
        }
        self.emit(
            target,
            PageEvent::FrameStartedLoading {
                frame_id: target.into(),
            },
        );
        if !blank {
            self.emit(
                target,
                PageEvent::RequestWillBeSent(Box::new(self.request_event(target, &loader, url))),
            );
        }
        let paused = {
            let mut st = self.state.borrow_mut();
            let st = &mut *st;
            let page = st
                .pages
                .get_mut(target)
                .ok_or_else(|| HostError::new("No target with given id found"))?;
            let hit = !blank
                && page.intercept.as_ref().is_some_and(|pats| {
                    pats.iter()
                        .any(|p| glob(p.url_pattern.as_deref().unwrap_or("*"), url))
                });
            if hit {
                st.next_intercept += 1;
                let id = format!("interception-job-{}.0", st.next_intercept);
                page.paused
                    .insert(id.clone(), (url.to_string(), loader.clone()));
                Some(id)
            } else {
                None
            }
        };
        if let Some(id) = paused {
            self.emit(
                target,
                PageEvent::RequestPaused(Box::new(RequestPaused {
                    request_id: id,
                    request: self.request_event(target, &loader, url).request,
                    frame_id: target.into(),
                    resource_type: "Document".into(),
                    network_id: Some(loader.clone()),
                    response_status_code: None,
                })),
            );
            return Ok(result);
        }
        let html = if blank {
            BLANK.to_string()
        } else {
            let key = url.split('#').next().unwrap_or(url);
            self.state
                .borrow()
                .sites
                .get(key)
                .cloned()
                .unwrap_or_else(|| default_page(url))
        };
        self.commit(target, url, &loader, &html, 200)?;
        Ok(result)
    }

    fn request_event(&self, target: &str, loader: &str, url: &str) -> RequestWillBeSent {
        let persona = self.page_persona(target);
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("User-Agent".to_string(), persona.browser.user_agent.clone());
        headers.insert(
            "Accept-Language".to_string(),
            persona.browser.accept_language.clone(),
        );
        RequestWillBeSent {
            request_id: loader.into(),
            loader_id: loader.into(),
            document_url: url.into(),
            request: NetRequest {
                url: url.into(),
                method: "GET".into(),
                headers,
                post_data: None,
                has_post_data: None,
                initial_priority: "VeryHigh".into(),
                referrer_policy: "strict-origin-when-cross-origin".into(),
            },
            timestamp: self.now(),
            wall_time: wall_ms() / 1000.0,
            initiator: json!({ "type": "other" }),
            redirect_has_extra_info: false,
            redirect_response: None,
            resource_type: "Document".into(),
            frame_id: Some(target.into()),
            has_user_gesture: false,
        }
    }

    fn page_persona(&self, target: &str) -> Arc<PersonaProfile> {
        let ctx = self
            .state
            .borrow()
            .pages
            .get(target)
            .map(|p| p.context.clone());
        ctx.map_or_else(|| self.default_persona.clone(), |c| self.persona_for(&c))
    }

    /// Replaces the document and reports the commit; load events follow on
    /// the next tick.
    fn commit(
        &self,
        target: &str,
        url: &str,
        loader: &str,
        html: &str,
        status: i64,
    ) -> HostResult<()> {
        let blank = url.starts_with("about:");
        if !blank {
            self.emit(
                target,
                PageEvent::ResponseReceived(Box::new(ResponseReceived {
                    request_id: loader.into(),
                    loader_id: loader.into(),
                    timestamp: self.now(),
                    resource_type: "Document".into(),
                    response: NetResponse {
                        url: url.into(),
                        status,
                        status_text: if status == 200 {
                            "OK".into()
                        } else {
                            String::new()
                        },
                        headers: [(
                            "Content-Type".to_string(),
                            "text/html; charset=utf-8".to_string(),
                        )]
                        .into(),
                        mime_type: "text/html".into(),
                        charset: "utf-8".into(),
                        encoded_data_length: html.len() as i64,
                        remote_ip_address: Some("127.0.0.1".into()),
                        remote_port: url::Url::parse(url)
                            .ok()
                            .and_then(|u| u.port_or_known_default())
                            .map(i64::from),
                        protocol: Some("http/1.1".into()),
                        security_state: if url.starts_with("https:") {
                            "secure"
                        } else {
                            "insecure"
                        }
                        .into(),
                        ..Default::default()
                    },
                    has_extra_info: false,
                    frame_id: Some(target.into()),
                })),
            );
        }
        let (old, new_ctxs, title) = {
            let mut st = self.state.borrow_mut();
            let persona = {
                let ctx = st
                    .pages
                    .get(target)
                    .map(|p| p.context.clone())
                    .unwrap_or_default();
                st.personas
                    .get(&ctx)
                    .cloned()
                    .unwrap_or_else(|| self.default_persona.clone())
            };
            let page = st
                .pages
                .get_mut(target)
                .ok_or_else(|| HostError::new("No target with given id found"))?;
            let old: Vec<ExecutionContextId> = page.contexts.drain(..).map(|c| c.id).collect();
            page.objects.clear();
            page.focused = None;
            page.doc = Document::parse_html(html);
            page.layout = LayoutEngine::new(persona);
            page.url = url.to_string();
            page.loader = loader.to_string();
            page.status = status;
            page.bodies.insert(loader.to_string(), html.to_string());
            let mut worlds: Vec<String> = Vec::new();
            for (_, w) in &page.scripts {
                if let Some(w) = w
                    && !worlds.contains(w)
                {
                    worlds.push(w.clone());
                }
            }
            let title = page.doc.title();
            let mut new_ctxs = vec![self.new_context(&mut st, target, World::Main, url)];
            for w in worlds {
                new_ctxs.push(self.new_context(&mut st, target, World::Isolated(w), url));
            }
            (old, new_ctxs, title)
        };
        for id in old {
            self.emit(
                target,
                PageEvent::ExecutionContextDestroyed { context_id: id },
            );
        }
        self.emit(target, PageEvent::ExecutionContextsCleared);
        self.emit(
            target,
            PageEvent::FrameNavigated {
                frame: Frame::new(target.into(), None, loader.into(), url),
            },
        );
        for c in new_ctxs {
            self.emit(target, PageEvent::ExecutionContextCreated { context: c });
        }
        let lifecycle = |name: &str, ts: f64| PageEvent::Lifecycle {
            frame_id: target.into(),
            loader_id: loader.into(),
            name: name.into(),
            timestamp: ts,
        };
        self.emit(target, lifecycle("init", self.now()));
        if !title.is_empty() {
            self.emit(target, PageEvent::TitleChanged { title });
        }
        let Some(sink) = self.sink.borrow().clone() else {
            return Ok(());
        };
        let mut later = Vec::new();
        if !blank {
            later.push(PageEvent::LoadingFinished(LoadingFinished {
                request_id: loader.into(),
                timestamp: self.now(),
                encoded_data_length: html.len() as f64,
            }));
        }
        for name in ["DOMContentLoaded", "load"] {
            later.push(lifecycle(name, self.now()));
        }
        later.push(PageEvent::FrameStoppedLoading {
            frame_id: target.into(),
        });
        for name in ["networkAlmostIdle", "networkIdle"] {
            later.push(lifecycle(name, self.now()));
        }
        let target = target.to_string();
        tokio::task::spawn_local(async move {
            tokio::task::yield_now().await;
            for ev in later {
                sink.emit(&target, ev);
            }
        });
        Ok(())
    }

    fn register(
        &self,
        page: &mut MockPage,
        st_next: &mut u64,
        ctx: ExecutionContextId,
        obj: MockObj,
        group: Option<&str>,
    ) -> String {
        *st_next += 1;
        let id = format!("mock.{ctx}.{st_next}");
        page.objects
            .insert(id.clone(), (ctx, obj, group.map(str::to_string)));
        id
    }

    /// The remote object for an evaluation result, registering handles.
    fn to_remote(
        &self,
        page: &mut MockPage,
        next: &mut u64,
        ctx: ExecutionContextId,
        v: Val,
        opts: RemoteOptions,
        group: Option<&str>,
    ) -> RemoteObject {
        match v {
            Val::Node(n) => {
                let js = node_value(&page.doc, n);
                let id = (!opts.return_by_value)
                    .then(|| self.register(page, next, ctx, MockObj::Node(n), group));
                let mut r = remote_object(&js, id, opts);
                if opts.return_by_value {
                    r.value = Some(json!({}));
                }
                r
            }
            Val::Named(class) => {
                let js = JsValue::object(&class);
                let id = (!opts.return_by_value)
                    .then(|| self.register(page, next, ctx, MockObj::Named(class), group));
                remote_object(&js, id, opts)
            }
            other => {
                let js = other.into_js(&page.doc);
                let id = match &js {
                    JsValue::Object(_) | JsValue::Function(_) | JsValue::Symbol(_)
                        if !opts.return_by_value =>
                    {
                        Some(self.register(page, next, ctx, MockObj::Value(js.clone()), group))
                    }
                    _ => None,
                };
                remote_object(&js, id, opts)
            }
        }
    }

    fn page_node(&self, page: &MockPage, node: &NodeRef) -> HostResult<NodeId> {
        let n = match node {
            NodeRef::Backend(id) => {
                let idx = u32::try_from(*id - 1)
                    .map_err(|_| HostError::new("No node with given id found"))?;
                if idx as usize >= page.doc.node_count() {
                    return Err(HostError::new("No node with given id found"));
                }
                NodeId(idx)
            }
            NodeRef::Object(o) => match page.objects.get(o) {
                Some((_, MockObj::Node(n), _)) => *n,
                Some(_) => {
                    return Err(HostError::new(
                        "Node with given id does not belong to the document",
                    ));
                }
                None => return Err(HostError::new("Could not find object with given id")),
            },
        };
        Ok(n)
    }

    fn with_page<R>(
        &self,
        target: &str,
        f: impl FnOnce(&mut MockPage) -> HostResult<R>,
    ) -> HostResult<R> {
        let mut st = self.state.borrow_mut();
        let page = st
            .pages
            .get_mut(target)
            .ok_or_else(|| HostError::new("No target with given id found"))?;
        f(page)
    }

    /// Runs `f` with a page and the object-id counter.
    fn with_page_ids<R>(
        &self,
        target: &str,
        f: impl FnOnce(&mut MockPage, &mut u64) -> HostResult<R>,
    ) -> HostResult<R> {
        let mut st = self.state.borrow_mut();
        let st = &mut *st;
        let page = st
            .pages
            .get_mut(target)
            .ok_or_else(|| HostError::new("No target with given id found"))?;
        f(page, &mut st.next_obj)
    }

    /// Evaluates in a realm and collects console calls and binding calls.
    fn run_eval(
        &self,
        target: &str,
        ctx: ExecutionContextId,
        run: impl FnOnce(&mut Interp<'_>) -> Result<Val, String>,
        opts: RemoteOptions,
        group: Option<&str>,
    ) -> HostResult<EvalResult> {
        let persona = self.page_persona(target);
        let (result, console, calls) = self.with_page_ids(target, |page, next| {
            let context = page
                .contexts
                .iter()
                .find(|c| c.id == ctx)
                .cloned()
                .ok_or_else(|| HostError::new("Cannot find context with specified id"))?;
            let bindings: Vec<String> = page
                .bindings
                .iter()
                .filter(|(_, scope)| match (scope, &context.world) {
                    (_, World::Main) => false,
                    (BindingScope::AllIsolatedWorlds, _) => true,
                    (BindingScope::World(n), World::Isolated(w)) => n == w,
                    (BindingScope::Context(id), _) => *id == ctx,
                })
                .map(|(n, _)| n.clone())
                .collect();
            let objects: HashMap<String, MockObj> = page
                .objects
                .iter()
                .filter(|(_, (c, _, _))| *c == ctx)
                .map(|(k, (_, o, _))| (k.clone(), o.clone()))
                .collect();
            let mut interp = Interp {
                doc: &page.doc,
                url: &page.url,
                persona: &persona,
                bindings: &bindings,
                objects: &objects,
                env: HashMap::new(),
                console: Vec::new(),
                binding_calls: Vec::new(),
            };
            let out = run(&mut interp);
            let (console, calls) = (
                std::mem::take(&mut interp.console),
                std::mem::take(&mut interp.binding_calls),
            );
            let result = match out {
                Ok(v) => EvalResult {
                    result: self.to_remote(page, next, ctx, v, opts, group),
                    exception_details: None,
                },
                Err(msg) => EvalResult {
                    result: error_object(&msg),
                    exception_details: Some(exception(&msg, Some(ctx))),
                },
            };
            Ok((result, console, calls))
        })?;
        for (kind, args) in console {
            let args = args
                .iter()
                .map(|a| remote_object(a, None, RemoteOptions::default()))
                .collect();
            self.emit(
                target,
                PageEvent::ConsoleApiCalled {
                    kind,
                    args,
                    context_id: ctx,
                    timestamp: wall_ms(),
                    stack_trace: None,
                },
            );
        }
        for (name, payload) in calls {
            self.emit(
                target,
                PageEvent::BindingCalled {
                    name,
                    payload,
                    context_id: ctx,
                },
            );
        }
        Ok(result)
    }
}

fn main_ctx(p: &MockPage) -> Option<ExecutionContextId> {
    p.contexts.iter().find(|c| c.world.is_main()).map(|c| c.id)
}

fn error_object(msg: &str) -> RemoteObject {
    let name = msg.split(':').next().unwrap_or("Error");
    let js = JsValue::Object(Box::new(ObjectInfo {
        class_name: name.to_string(),
        subtype: Some("error".into()),
        description: Some(format!("{msg}\n    at <anonymous>:1:1")),
        ..Default::default()
    }));
    remote_object(&js, None, RemoteOptions::default())
}

fn exception(msg: &str, ctx: Option<ExecutionContextId>) -> ExceptionDetails {
    ExceptionDetails {
        exception_id: 1,
        text: "Uncaught".into(),
        line_number: 0,
        column_number: 0,
        script_id: None,
        url: None,
        stack_trace: Some(StackTrace::default()),
        exception: Some(error_object(msg)),
        execution_context_id: ctx,
    }
}

fn node_value(doc: &Document, n: NodeId) -> JsValue {
    JsValue::Object(Box::new(ObjectInfo {
        class_name: convert::node_class(doc, n),
        subtype: Some("node".into()),
        description: Some(convert::node_description(doc, n)),
        ..Default::default()
    }))
}

fn backend(n: NodeId) -> BackendNodeId {
    i64::from(n.0) + 1
}

impl PageHost for MockHost {
    fn family(&self) -> BrowserFamily {
        self.family
    }

    fn set_event_sink(&self, sink: EventSink) {
        *self.sink.borrow_mut() = Some(sink);
    }

    fn persona(&self, context: &str) -> Arc<PersonaProfile> {
        self.persona_for(context)
    }

    fn set_persona(&self, context: &str, persona: Arc<PersonaProfile>) -> HostResult<()> {
        self.state
            .borrow_mut()
            .personas
            .insert(context.to_string(), persona);
        Ok(())
    }

    fn create_context(&self, id: &str, _opts: ContextOptions) -> HostResult<()> {
        let mut st = self.state.borrow_mut();
        st.personas
            .insert(id.to_string(), self.default_persona.clone());
        st.cookies.insert(id.to_string(), Vec::new());
        Ok(())
    }

    fn dispose_context(&self, id: &str) -> HostResult<()> {
        let mut st = self.state.borrow_mut();
        st.personas.remove(id);
        st.cookies.remove(id);
        Ok(())
    }

    async fn create_page(&self, context: &str, target_id: &str) -> HostResult<()> {
        let persona = self.persona_for(context);
        self.state.borrow_mut().pages.insert(
            target_id.to_string(),
            MockPage {
                context: context.to_string(),
                url: "about:blank".into(),
                loader: String::new(),
                doc: Document::parse_html(BLANK),
                layout: LayoutEngine::new(persona),
                contexts: Vec::new(),
                scripts: Vec::new(),
                bindings: Vec::new(),
                objects: HashMap::new(),
                focused: None,
                intercept: None,
                paused: HashMap::new(),
                bodies: HashMap::new(),
                status: 200,
            },
        );
        let loader = self.ids.hex32();
        self.commit(target_id, "about:blank", &loader, BLANK, 200)
    }

    fn close_page(&self, target_id: &str) -> HostResult<()> {
        self.state.borrow_mut().pages.remove(target_id);
        Ok(())
    }

    async fn navigate(&self, target_id: &str, req: NavigateRequest) -> HostResult<NavigateResult> {
        if let Some(f) = &req.frame_id
            && f != target_id
        {
            return Err(HostError::new("No frame for given id found"));
        }
        self.start_navigation(target_id, &req.url)
    }

    async fn reload(&self, target_id: &str, _ignore_cache: bool) -> HostResult<()> {
        let url = self.with_page(target_id, |p| Ok(p.url.clone()))?;
        self.start_navigation(target_id, &url).map(|_| ())
    }

    fn frame_tree(&self, target_id: &str) -> HostResult<FrameTree> {
        self.with_page(target_id, |p| {
            Ok(FrameTree {
                frame: Frame::new(target_id.into(), None, p.loader.clone(), &p.url),
                child_frames: Vec::new(),
            })
        })
    }

    fn create_isolated_world(
        &self,
        target_id: &str,
        frame_id: &str,
        world_name: &str,
        _grant_universal_access: bool,
    ) -> HostResult<ExecutionContextId> {
        if frame_id != target_id {
            return Err(HostError::new("No frame for given id found"));
        }
        let ctx = {
            let mut st = self.state.borrow_mut();
            let url = st
                .pages
                .get(target_id)
                .map(|p| p.url.clone())
                .ok_or_else(|| HostError::new("No target with given id found"))?;
            self.new_context(&mut st, target_id, World::Isolated(world_name.into()), &url)
        };
        let id = ctx.id;
        self.emit(
            target_id,
            PageEvent::ExecutionContextCreated { context: ctx },
        );
        Ok(id)
    }

    fn add_script_to_evaluate_on_new_document(
        &self,
        target_id: &str,
        source: &str,
        world_name: Option<&str>,
        _run_immediately: bool,
    ) -> HostResult<String> {
        let mut st = self.state.borrow_mut();
        st.next_script += 1;
        let id = st.next_script.to_string();
        let page = st
            .pages
            .get_mut(target_id)
            .ok_or_else(|| HostError::new("No target with given id found"))?;
        page.scripts
            .push((source.to_string(), world_name.map(str::to_string)));
        Ok(id)
    }

    async fn evaluate(&self, target_id: &str, req: EvaluateRequest) -> HostResult<EvalResult> {
        let opts = RemoteOptions {
            return_by_value: req.return_by_value,
            generate_preview: req.generate_preview,
        };
        let expr = req.expression.clone();
        self.run_eval(
            target_id,
            req.context_id,
            |i| i.eval_source(&expr),
            opts,
            req.object_group.as_deref(),
        )
    }

    async fn call_function_on(
        &self,
        target_id: &str,
        req: CallFunctionRequest,
    ) -> HostResult<EvalResult> {
        let ctx = match &req.target {
            CallTarget::Context(id) => *id,
            CallTarget::Object(o) => self.with_page(target_id, |p| {
                p.objects
                    .get(o)
                    .map(|(c, _, _)| *c)
                    .ok_or_else(|| HostError::new("Could not find object with given id"))
            })?,
        };
        let opts = RemoteOptions {
            return_by_value: req.return_by_value,
            generate_preview: req.generate_preview,
        };
        let decl = req.function_declaration.clone();
        let args = req.arguments.clone();
        self.run_eval(
            target_id,
            ctx,
            |i| i.call(&decl, &args, req.return_by_value),
            opts,
            req.object_group.as_deref(),
        )
    }

    fn get_properties(&self, target_id: &str, req: GetPropertiesRequest) -> HostResult<Properties> {
        self.with_page_ids(target_id, |page, next| {
            let (ctx, obj, _) = page
                .objects
                .get(&req.object_id)
                .cloned()
                .ok_or_else(|| HostError::new("Could not find object with given id"))?;
            let props = match obj {
                MockObj::Value(JsValue::Object(o)) if !req.accessor_properties_only => {
                    o.properties.clone()
                }
                _ => Vec::new(),
            };
            let opts = RemoteOptions {
                generate_preview: req.generate_preview,
                return_by_value: false,
            };
            let result = props
                .into_iter()
                .map(|(name, v)| PropertyDescriptor {
                    name,
                    value: Some(self.to_remote(page, next, ctx, Val::Js(v), opts, None)),
                    writable: Some(true),
                    configurable: true,
                    enumerable: true,
                    is_own: Some(true),
                    ..Default::default()
                })
                .collect();
            Ok(Properties {
                result,
                ..Default::default()
            })
        })
    }

    fn release_object(&self, target_id: &str, object_id: &str) -> HostResult<()> {
        self.with_page(target_id, |p| {
            p.objects.remove(object_id);
            Ok(())
        })
    }

    fn release_object_group(&self, target_id: &str, group: &str) -> HostResult<()> {
        self.with_page(target_id, |p| {
            p.objects.retain(|_, (_, _, g)| g.as_deref() != Some(group));
            Ok(())
        })
    }

    fn add_binding(&self, target_id: &str, name: &str, scope: BindingScope) -> HostResult<()> {
        self.with_page(target_id, |p| {
            p.bindings.push((name.to_string(), scope));
            Ok(())
        })
    }

    fn get_document(&self, target_id: &str, depth: i32, pierce: bool) -> HostResult<DomNode> {
        self.with_page(target_id, |p| {
            let info = DocInfo {
                url: &p.url,
                frame_id: target_id,
                pierce,
            };
            Ok(convert::dom_node(
                &p.doc,
                Document::DOCUMENT,
                depth,
                &backend,
                &info,
            ))
        })
    }

    fn describe_node(
        &self,
        target_id: &str,
        node: &NodeRef,
        depth: i32,
        pierce: bool,
    ) -> HostResult<DomNode> {
        self.with_page(target_id, |p| {
            let n = self.page_node(p, node)?;
            let info = DocInfo {
                url: &p.url,
                frame_id: target_id,
                pierce,
            };
            Ok(convert::dom_node(&p.doc, n, depth, &backend, &info))
        })
    }

    fn resolve_node(
        &self,
        target_id: &str,
        node: BackendNodeId,
        context: Option<ExecutionContextId>,
        object_group: Option<&str>,
    ) -> HostResult<RemoteObject> {
        self.with_page_ids(target_id, |page, next| {
            let n = self.page_node(page, &NodeRef::Backend(node))?;
            let ctx = match context {
                Some(c) => c,
                None => main_ctx(page)
                    .ok_or_else(|| HostError::new("Cannot find default execution context"))?,
            };
            Ok(self.to_remote(
                page,
                next,
                ctx,
                Val::Node(n),
                RemoteOptions::default(),
                object_group,
            ))
        })
    }

    fn request_node(&self, target_id: &str, object_id: &str) -> HostResult<BackendNodeId> {
        self.with_page(target_id, |p| {
            Ok(backend(
                self.page_node(p, &NodeRef::Object(object_id.into()))?,
            ))
        })
    }

    fn query_selector(
        &self,
        target_id: &str,
        node: BackendNodeId,
        selector: &str,
    ) -> HostResult<Option<BackendNodeId>> {
        self.with_page(target_id, |p| {
            let n = self.page_node(p, &NodeRef::Backend(node))?;
            p.doc
                .query_selector(n, selector)
                .map(|r| r.map(backend))
                .map_err(|_| HostError::new("DOM Error while querying"))
        })
    }

    fn query_selector_all(
        &self,
        target_id: &str,
        node: BackendNodeId,
        selector: &str,
    ) -> HostResult<Vec<BackendNodeId>> {
        self.with_page(target_id, |p| {
            let n = self.page_node(p, &NodeRef::Backend(node))?;
            p.doc
                .query_selector_all(n, selector)
                .map(|v| v.into_iter().map(backend).collect())
                .map_err(|_| HostError::new("DOM Error while querying"))
        })
    }

    fn outer_html(&self, target_id: &str, node: &NodeRef) -> HostResult<String> {
        self.with_page(target_id, |p| {
            let n = self.page_node(p, node)?;
            Ok(if n == Document::DOCUMENT {
                p.doc.to_html()
            } else {
                p.doc.outer_html(n)
            })
        })
    }

    fn box_model(&self, target_id: &str, node: &NodeRef) -> HostResult<BoxModel> {
        self.with_page(target_id, |p| {
            let n = self.page_node(p, node)?;
            p.layout
                .box_model(&p.doc, n)
                .map(|m| convert::box_model(&m))
                .ok_or_else(|| HostError::new("Could not compute box model."))
        })
    }

    fn content_quads(&self, target_id: &str, node: &NodeRef) -> HostResult<Vec<Quad>> {
        self.with_page(target_id, |p| {
            let n = self.page_node(p, node)?;
            Ok(p.layout
                .client_rects(&p.doc, n)
                .iter()
                .filter(|r| r.width > 0.0 && r.height > 0.0)
                .map(|r| convert::rect(r).quad())
                .collect())
        })
    }

    fn scroll_into_view_if_needed(
        &self,
        target_id: &str,
        node: &NodeRef,
        _rect: Option<Rect>,
    ) -> HostResult<()> {
        self.with_page(target_id, |p| {
            let n = self.page_node(p, node)?;
            if !p.doc.is_connected(n) {
                return Err(HostError::new("Node is detached from document"));
            }
            if p.layout.box_model(&p.doc, n).is_none() {
                return Err(HostError::new("Node does not have a layout object"));
            }
            p.layout
                .scroll_into_view(&p.doc, n, ScrollAlign::Center, true);
            Ok(())
        })
    }

    fn set_file_input_files(
        &self,
        target_id: &str,
        node: &NodeRef,
        files: Vec<String>,
    ) -> HostResult<()> {
        let line = self.with_page(target_id, |p| {
            let n = self.page_node(p, node)?;
            let is_file = p.doc.local_name(n).is_some_and(|l| &**l == "input")
                && p.doc
                    .get_attribute(n, "type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("file"));
            if !is_file {
                return Err(HostError::new("Node is not a file input element"));
            }
            Ok(format!("files {} {}", backend(n), files.join(",")))
        })?;
        self.state.borrow_mut().input_log.push(line);
        Ok(())
    }

    async fn dispatch_mouse_event(&self, target_id: &str, ev: MouseEvent) -> HostResult<()> {
        let line = self.with_page(target_id, |p| {
            let hit = p
                .layout
                .element_from_point(&p.doc, ev.x as f32, ev.y as f32);
            if ev.kind == "mousePressed"
                && let Some(h) = hit
            {
                let focusable = p.doc.ancestors(h).chain(std::iter::once(h)).find(|n| {
                    p.doc.local_name(*n).is_some_and(|l| {
                        matches!(&**l, "input" | "textarea" | "button" | "select" | "a")
                    })
                });
                p.focused = focusable;
            }
            Ok(format!(
                "{} {} {} {}",
                ev.kind,
                ev.x,
                ev.y,
                hit.map_or("none".to_string(), |h| convert::node_description(&p.doc, h))
            ))
        })?;
        self.state.borrow_mut().input_log.push(line);
        Ok(())
    }

    async fn dispatch_key_event(&self, target_id: &str, ev: KeyEvent) -> HostResult<()> {
        let text = ev
            .text
            .clone()
            .filter(|_| ev.kind == "keyDown" || ev.kind == "char");
        if let Some(t) = text {
            self.insert_text(target_id, &t).await?;
        }
        self.state.borrow_mut().input_log.push(format!(
            "{} {}",
            ev.kind,
            ev.key.unwrap_or_default()
        ));
        Ok(())
    }

    async fn insert_text(&self, target_id: &str, text: &str) -> HostResult<()> {
        self.with_page(target_id, |p| {
            if let Some(f) = p.focused
                && p.doc
                    .local_name(f)
                    .is_some_and(|l| matches!(&**l, "input" | "textarea"))
            {
                let v = format!("{}{text}", p.doc.get_attribute(f, "value").unwrap_or(""));
                let _ = p.doc.set_attribute(f, "value", &v);
            }
            Ok(())
        })?;
        self.state
            .borrow_mut()
            .input_log
            .push(format!("insertText {text}"));
        Ok(())
    }

    fn cookies(&self, context: &str, urls: Option<&[String]>) -> HostResult<Vec<Cookie>> {
        let st = self.state.borrow();
        let all = st.cookies.get(context).cloned().unwrap_or_default();
        Ok(match urls {
            None => all,
            Some(urls) => all
                .into_iter()
                .filter(|c| {
                    urls.iter().any(|u| {
                        url::Url::parse(u).is_ok_and(|u| {
                            let host = u.host_str().unwrap_or("");
                            let d = c.domain.trim_start_matches('.');
                            (host == d || host.ends_with(&format!(".{d}")))
                                && u.path().starts_with(&c.path)
                                && (!c.secure
                                    || u.scheme() == "https"
                                    || host == "localhost"
                                    || host == "127.0.0.1")
                        })
                    })
                })
                .collect(),
        })
    }

    fn set_cookies(&self, context: &str, cookies: Vec<CookieParam>) -> HostResult<()> {
        let mut st = self.state.borrow_mut();
        let jar = st.cookies.entry(context.to_string()).or_default();
        for c in cookies {
            let parsed = c.url.as_deref().and_then(|u| url::Url::parse(u).ok());
            let domain = c
                .domain
                .clone()
                .or_else(|| {
                    parsed
                        .as_ref()
                        .and_then(|u| u.host_str().map(str::to_string))
                })
                .ok_or_else(|| HostError::new("Invalid cookie fields"))?;
            let path = c.path.clone().unwrap_or_else(|| "/".into());
            let expires = c.expires.unwrap_or(-1.0);
            jar.retain(|x| !(x.name == c.name && x.domain == domain && x.path == path));
            jar.push(Cookie {
                size: (c.name.len() + c.value.len()) as i64,
                name: c.name,
                value: c.value,
                domain,
                path,
                expires,
                http_only: c.http_only.unwrap_or(false),
                secure: c.secure.unwrap_or(false),
                session: expires < 0.0,
                same_site: c.same_site,
                priority: "Medium".into(),
                same_party: false,
                source_scheme: if parsed.as_ref().is_some_and(|u| u.scheme() == "https") {
                    "Secure"
                } else {
                    "NonSecure"
                }
                .into(),
                source_port: parsed
                    .as_ref()
                    .and_then(|u| u.port_or_known_default())
                    .map_or(-1, i64::from),
            });
        }
        Ok(())
    }

    fn delete_cookies(&self, context: &str, f: DeleteCookies) -> HostResult<()> {
        let mut st = self.state.borrow_mut();
        let host = f
            .url
            .as_deref()
            .and_then(|u| url::Url::parse(u).ok())
            .and_then(|u| u.host_str().map(str::to_string));
        if let Some(jar) = st.cookies.get_mut(context) {
            jar.retain(|c| {
                let hit = c.name == f.name
                    && f.domain.as_deref().is_none_or(|d| d == c.domain)
                    && f.path.as_deref().is_none_or(|p| p == c.path)
                    && host
                        .as_deref()
                        .is_none_or(|h| h == c.domain.trim_start_matches('.'));
                !hit
            });
        }
        Ok(())
    }

    fn clear_cookies(&self, context: &str) -> HostResult<()> {
        self.state
            .borrow_mut()
            .cookies
            .insert(context.to_string(), Vec::new());
        Ok(())
    }

    fn response_body(&self, target_id: &str, request_id: &str) -> HostResult<(String, bool)> {
        self.with_page(target_id, |p| {
            p.bodies
                .get(request_id)
                .map(|b| (b.clone(), false))
                .ok_or_else(|| HostError::new("No resource with given identifier found"))
        })
    }

    fn set_request_interception(
        &self,
        target_id: &str,
        patterns: Option<Vec<RequestPattern>>,
    ) -> HostResult<()> {
        self.with_page(target_id, |p| {
            p.intercept = patterns;
            Ok(())
        })
    }

    fn continue_request(&self, target_id: &str, req: ContinueRequest) -> HostResult<()> {
        let (url, loader) = self.with_page(target_id, |p| {
            p.paused
                .remove(&req.request_id)
                .ok_or_else(|| HostError::new("Invalid InterceptionId."))
        })?;
        let url = req.url.unwrap_or(url);
        if !is_loopback_url(&url) {
            return Err(HostError::new(
                "Invalid URL: the mock serves loopback URLs only",
            ));
        }
        let key = url.split('#').next().unwrap_or(&url).to_string();
        let html = self
            .state
            .borrow()
            .sites
            .get(&key)
            .cloned()
            .unwrap_or_else(|| default_page(&url));
        self.commit(target_id, &url, &loader, &html, 200)
    }

    fn fulfill_request(&self, target_id: &str, req: FulfillRequest) -> HostResult<()> {
        let (url, loader) = self.with_page(target_id, |p| {
            p.paused
                .remove(&req.request_id)
                .ok_or_else(|| HostError::new("Invalid InterceptionId."))
        })?;
        let body = req
            .body
            .as_deref()
            .map(|b| {
                base64_decode(b)
                    .ok_or_else(|| HostError::new("Invalid parameters: body is not base64"))
            })
            .transpose()?
            .unwrap_or_default();
        self.commit(
            target_id,
            &url,
            &loader,
            &String::from_utf8_lossy(&body),
            req.response_code,
        )
    }

    fn fail_request(&self, target_id: &str, request_id: &str, reason: &str) -> HostResult<()> {
        let (_, loader) = self.with_page(target_id, |p| {
            p.paused
                .remove(request_id)
                .ok_or_else(|| HostError::new("Invalid InterceptionId."))
        })?;
        self.emit(
            target_id,
            PageEvent::LoadingFailed(LoadingFailed {
                request_id: loader,
                timestamp: self.now(),
                resource_type: "Document".into(),
                error_text: format!("net::ERR_{}", reason.to_ascii_uppercase()),
                canceled: false,
            }),
        );
        self.emit(
            target_id,
            PageEvent::FrameStoppedLoading {
                frame_id: target_id.into(),
            },
        );
        Ok(())
    }

    fn set_viewport(&self, target_id: &str, viewport: Option<ViewportOverride>) -> HostResult<()> {
        let persona = self.page_persona(target_id);
        self.with_page(target_id, |p| {
            let (w, h) = viewport.map_or(
                (persona.screen.inner_width, persona.screen.inner_height),
                |v| (v.width, v.height),
            );
            p.layout.set_viewport(w as f32, h as f32);
            Ok(())
        })
    }

    fn ax_tree(&self, target_id: &str) -> HostResult<Vec<AxNode>> {
        self.with_page(target_id, |p| {
            let tree = p.layout.accessibility_tree(&p.doc);
            Ok(convert::ax_nodes(&tree, &backend, target_id))
        })
    }
}

// ---------------------------------------------------------------------------
// The tiny evaluator.

/// A value during evaluation.
#[derive(Clone, Debug)]
enum Val {
    Js(JsValue),
    Node(NodeId),
    Nodes(Vec<NodeId>),
    /// A host object path not yet resolved (`document`, `navigator.x`).
    Path(String),
    Named(String),
}

impl Val {
    fn into_js(self, doc: &Document) -> JsValue {
        match self {
            Val::Js(v) => v,
            Val::Node(n) => node_value(doc, n),
            Val::Nodes(v) => JsValue::Object(Box::new(ObjectInfo {
                class_name: "NodeList".into(),
                length: Some(v.len()),
                properties: v
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (i.to_string(), node_value(doc, *n)))
                    .collect(),
                ..Default::default()
            })),
            Val::Path(_) | Val::Named(_) => JsValue::object("Object"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    P(&'static str),
}

const PUNCT: &[&str] = &[
    "===", "!==", "=>", "==", "!=", "<=", ">=", "&&", "||", "...", "(", ")", "[", "]", "{", "}",
    ",", ".", "+", "-", "*", "/", "%", "<", ">", "!", ";", ":", "?", "=",
];

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit()
            || (c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()))
        {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '.') {
                i += 1;
            }
            let t: String = b[s..i].iter().collect();
            out.push(Tok::Num(
                t.parse()
                    .map_err(|_| format!("SyntaxError: Invalid number {t}"))?,
            ));
        } else if c == '"' || c == '\'' || c == '`' {
            i += 1;
            let mut s = String::new();
            while i < b.len() && b[i] != c {
                if b[i] == '\\' && i + 1 < b.len() {
                    i += 1;
                    s.push(match b[i] {
                        'n' => '\n',
                        't' => '\t',
                        o => o,
                    });
                } else {
                    s.push(b[i]);
                }
                i += 1;
            }
            if i >= b.len() {
                return Err("SyntaxError: Invalid or unexpected token".into());
            }
            i += 1;
            out.push(Tok::Str(s));
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let s = i;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_' || b[i] == '$') {
                i += 1;
            }
            out.push(Tok::Ident(b[s..i].iter().collect()));
        } else {
            let rest: String = b[i..b.len().min(i + 3)].iter().collect();
            match PUNCT.iter().find(|p| rest.starts_with(**p)) {
                Some(p) => {
                    out.push(Tok::P(p));
                    i += p.len();
                }
                None => return Err(format!("SyntaxError: Unexpected token '{c}'")),
            }
        }
    }
    Ok(out)
}

struct Interp<'a> {
    doc: &'a Document,
    url: &'a str,
    persona: &'a PersonaProfile,
    bindings: &'a [String],
    objects: &'a HashMap<String, MockObj>,
    env: HashMap<String, Val>,
    console: Vec<(String, Vec<JsValue>)>,
    binding_calls: Vec<(String, String)>,
}

/// A parsed function: parameter names and a body expression (or statements).
struct Func {
    params: Vec<String>,
    body: String,
}

fn parse_function(src: &str) -> Option<Func> {
    let mut s = src.trim();
    while let Some(r) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        // `(function () {...})` or `(() => x)`, but not `(a, b) => x`.
        if r.contains("=>")
            || r.trim_start().starts_with("function")
            || r.trim_start().starts_with("async")
        {
            s = r.trim();
        } else {
            break;
        }
    }
    let s = s.strip_prefix("async").map_or(s, str::trim_start);
    if let Some(rest) = s.strip_prefix("function") {
        let open = rest.find('(')?;
        let close = rest[open..].find(')')? + open;
        let params = split_params(&rest[open + 1..close]);
        let body = rest[close + 1..].trim();
        let body = body.strip_prefix('{')?.strip_suffix('}')?;
        return Some(Func {
            params,
            body: body.to_string(),
        });
    }
    let arrow = s.find("=>")?;
    let head = s[..arrow].trim();
    let head = head
        .strip_prefix('(')
        .and_then(|h| h.strip_suffix(')'))
        .unwrap_or(head);
    let body = s[arrow + 2..].trim();
    let body = match body.strip_prefix('{').and_then(|b| b.strip_suffix('}')) {
        Some(b) => b.to_string(),
        None => format!("return {body}"),
    };
    Some(Func {
        params: split_params(head),
        body,
    })
}

fn split_params(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().split('=').next().unwrap_or("").trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Splits statements at top-level `;` (outside strings and brackets).
fn statements(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut depth, mut quote, mut cur) = (0i32, None::<char>, String::new());
    let mut esc = false;
    for c in body.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ';' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Interp<'_> {
    fn unsupported(&self) -> String {
        "EvalError: the mock host evaluates only simple expressions (lb-page runs real JavaScript)"
            .into()
    }

    /// `Runtime.evaluate` source.
    fn eval_source(&mut self, src: &str) -> Result<Val, String> {
        if src.contains("module.exports") {
            if src.contains("module.exports.InjectedScript(") {
                return Ok(Val::Named("InjectedScript".into()));
            }
            if src.contains("module.exports.UtilityScript(") {
                return Ok(Val::Named("UtilityScript".into()));
            }
            return Err(self.unsupported());
        }
        self.run_body(src)
    }

    /// Runs statements; the value is the `return` value or the last
    /// expression statement.
    fn run_body(&mut self, body: &str) -> Result<Val, String> {
        let mut last = Val::Js(JsValue::Undefined);
        for st in statements(body) {
            if let Some(e) = st.strip_prefix("return") {
                return self.eval_expr(e);
            }
            if let Some(e) = st.strip_prefix("throw ") {
                let v = self.eval_expr(e)?;
                return Err(match v {
                    Val::Js(JsValue::String(s)) => format!("Uncaught {s}"),
                    Val::Js(JsValue::Object(o)) if o.subtype.as_deref() == Some("error") => {
                        o.description.unwrap_or_else(|| "Error".into())
                    }
                    _ => "Error".into(),
                });
            }
            last = self.eval_expr(&st)?;
        }
        Ok(last)
    }

    fn eval_expr(&mut self, src: &str) -> Result<Val, String> {
        let toks = lex(src)?;
        if toks.is_empty() {
            return Ok(Val::Js(JsValue::Undefined));
        }
        let mut p = 0;
        let v = self.expr(&toks, &mut p)?;
        if p != toks.len() {
            return Err(self.unsupported());
        }
        self.resolve(v)
    }

    /// `Runtime.callFunctionOn`.
    fn call(
        &mut self,
        decl: &str,
        args: &[CallArgument],
        return_by_value: bool,
    ) -> Result<Val, String> {
        if decl.contains("utilityScript.evaluate(") {
            return self.playwright_evaluate(args, return_by_value);
        }
        let f = parse_function(decl).ok_or_else(|| self.unsupported())?;
        let vals: Vec<Val> = args.iter().map(|a| self.arg(a)).collect::<Result<_, _>>()?;
        self.invoke(&f, vals)
    }

    fn invoke(&mut self, f: &Func, args: Vec<Val>) -> Result<Val, String> {
        let saved = self.env.clone();
        for (i, p) in f.params.iter().enumerate() {
            let v = if let Some(rest) = p.strip_prefix("...") {
                let _ = rest;
                Val::Js(JsValue::Undefined)
            } else {
                args.get(i).cloned().unwrap_or(Val::Js(JsValue::Undefined))
            };
            self.env.insert(p.trim_start_matches("...").to_string(), v);
        }
        let r = self.run_body(&f.body);
        self.env = saved;
        r
    }

    fn arg(&self, a: &CallArgument) -> Result<Val, String> {
        if let Some(o) = &a.object_id {
            return match self.objects.get(o) {
                Some(MockObj::Node(n)) => Ok(Val::Node(*n)),
                Some(MockObj::Named(c)) => Ok(Val::Named(c.clone())),
                Some(MockObj::Value(v)) => Ok(Val::Js(v.clone())),
                None => Err("Error: Could not find object with given id".into()),
            };
        }
        if let Some(u) = &a.unserializable_value {
            let n = match u.as_str() {
                "NaN" => f64::NAN,
                "Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                "-0" => -0.0,
                b if b.ends_with('n') => {
                    return Ok(Val::Js(JsValue::BigInt(b.trim_end_matches('n').into())));
                }
                _ => return Err("Error: Invalid unserializable value".into()),
            };
            return Ok(Val::Js(JsValue::Number(n)));
        }
        Ok(Val::Js(
            a.value.as_ref().map_or(JsValue::Undefined, json_to_js),
        ))
    }

    /// Playwright's `(utilityScript, ...args) => utilityScript.evaluate(...args)`:
    /// args are `[utilityScript, isFunction, returnByValue, expression,
    /// argCount, ...serializedArgs, ...handles]`.
    fn playwright_evaluate(
        &mut self,
        args: &[CallArgument],
        return_by_value: bool,
    ) -> Result<Val, String> {
        let get = |i: usize| {
            args.get(i)
                .and_then(|a| a.value.clone())
                .unwrap_or(Value::Null)
        };
        let is_function = get(1).as_bool().unwrap_or(false);
        let expr = get(3).as_str().unwrap_or("").to_string();
        let argc = get(4).as_u64().unwrap_or(0) as usize;
        let handles = &args[(5 + argc).min(args.len())..];
        let mut vals = Vec::new();
        for i in 0..argc {
            let v = get(5 + i);
            vals.push(match v.get("h").and_then(Value::as_u64) {
                Some(h) => match handles.get(h as usize) {
                    Some(a) => self.arg(a)?,
                    None => Val::Js(JsValue::Undefined),
                },
                None => Val::Js(pw_deserialize(&v)),
            });
        }
        let result = if is_function {
            if expr.contains("documentElement.outerHTML") && expr.contains("doctype") {
                Val::Js(JsValue::String(self.content()))
            } else {
                let f = parse_function(&expr).ok_or_else(|| self.unsupported())?;
                self.invoke(&f, vals)?
            }
        } else {
            self.run_body(&expr)?
        };
        if return_by_value {
            let js = result.into_js(self.doc);
            return Ok(Val::Js(json_to_js(&pw_serialize(&js))));
        }
        Ok(result)
    }

    /// What Playwright's `page.content()` computes.
    fn content(&self) -> String {
        let mut s = String::new();
        if let Some(d) = self.doc.doctype_node().and_then(|n| self.doc.doctype(n)) {
            s.push_str(&format!("<!DOCTYPE {}>", d.name));
        }
        if let Some(root) = self.doc.document_element() {
            s.push_str(&self.doc.outer_html(root));
        }
        s
    }

    fn expr(&mut self, t: &[Tok], p: &mut usize) -> Result<Val, String> {
        let cond = self.binary(t, p, 0)?;
        if t.get(*p) == Some(&Tok::P("?")) {
            *p += 1;
            let a = self.expr(t, p)?;
            if t.get(*p) != Some(&Tok::P(":")) {
                return Err(self.unsupported());
            }
            *p += 1;
            let b = self.expr(t, p)?;
            let c = self.resolve(cond)?;
            return Ok(if truthy(&c) { a } else { b });
        }
        Ok(cond)
    }

    fn binary(&mut self, t: &[Tok], p: &mut usize, level: usize) -> Result<Val, String> {
        const LEVELS: &[&[&str]] = &[
            &["||"],
            &["&&"],
            &["===", "!==", "==", "!="],
            &["<", ">", "<=", ">="],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if level == LEVELS.len() {
            return self.unary(t, p);
        }
        let mut lhs = self.binary(t, p, level + 1)?;
        while let Some(Tok::P(op)) = t.get(*p) {
            if !LEVELS[level].contains(op) {
                break;
            }
            *p += 1;
            let rhs = self.binary(t, p, level + 1)?;
            let (a, b) = (self.resolve(lhs)?, self.resolve(rhs)?);
            lhs = Val::Js(apply(op, &js(self.doc, a), &js(self.doc, b)));
        }
        Ok(lhs)
    }

    fn unary(&mut self, t: &[Tok], p: &mut usize) -> Result<Val, String> {
        match t.get(*p) {
            Some(Tok::P("-")) => {
                *p += 1;
                let v = self.unary(t, p)?;
                let v = js(self.doc, self.resolve(v)?);
                Ok(Val::Js(JsValue::Number(-to_number(&v))))
            }
            Some(Tok::P("!")) => {
                *p += 1;
                let v = self.unary(t, p)?;
                let v = self.resolve(v)?;
                Ok(Val::Js(JsValue::Bool(!truthy(&v))))
            }
            Some(Tok::Ident(k)) if k == "typeof" => {
                *p += 1;
                let v = self.unary(t, p);
                let ty = match v.and_then(|v| self.resolve(v)) {
                    Ok(Val::Js(v)) => v.type_name(),
                    Ok(_) => "object",
                    Err(_) => "undefined",
                };
                Ok(Val::Js(JsValue::String(ty.into())))
            }
            Some(Tok::Ident(k)) if k == "new" => {
                *p += 1;
                let Some(Tok::Ident(class)) = t.get(*p).cloned() else {
                    return Err(self.unsupported());
                };
                *p += 1;
                let args = if t.get(*p) == Some(&Tok::P("(")) {
                    self.args(t, p)?
                } else {
                    Vec::new()
                };
                match class.as_str() {
                    "Error" | "TypeError" | "RangeError" => {
                        let msg = args
                            .first()
                            .map(|a| to_string(&js(self.doc, a.clone())))
                            .unwrap_or_default();
                        Ok(Val::Js(JsValue::Object(Box::new(ObjectInfo {
                            class_name: class.clone(),
                            subtype: Some("error".into()),
                            description: Some(if msg.is_empty() {
                                class
                            } else {
                                format!("{class}: {msg}")
                            }),
                            ..Default::default()
                        }))))
                    }
                    _ => Err(self.unsupported()),
                }
            }
            _ => self.postfix(t, p),
        }
    }

    fn args(&mut self, t: &[Tok], p: &mut usize) -> Result<Vec<Val>, String> {
        *p += 1; // (
        let mut out = Vec::new();
        while t.get(*p) != Some(&Tok::P(")")) {
            let v = self.expr(t, p)?;
            out.push(self.resolve(v)?);
            match t.get(*p) {
                Some(Tok::P(",")) => *p += 1,
                Some(Tok::P(")")) => {}
                _ => return Err(self.unsupported()),
            }
        }
        *p += 1;
        Ok(out)
    }

    fn postfix(&mut self, t: &[Tok], p: &mut usize) -> Result<Val, String> {
        let mut v = self.primary(t, p)?;
        loop {
            match t.get(*p) {
                Some(Tok::P(".")) => {
                    *p += 1;
                    let Some(Tok::Ident(name)) = t.get(*p).cloned() else {
                        return Err(self.unsupported());
                    };
                    *p += 1;
                    v = self.member(v, &name)?;
                }
                Some(Tok::P("[")) => {
                    *p += 1;
                    let i = self.expr(t, p)?;
                    let i = js(self.doc, self.resolve(i)?);
                    if t.get(*p) != Some(&Tok::P("]")) {
                        return Err(self.unsupported());
                    }
                    *p += 1;
                    v = self.member(v, &to_string(&i))?;
                }
                Some(Tok::P("(")) => {
                    let args = self.args(t, p)?;
                    v = self.apply_call(v, args)?;
                }
                _ => return Ok(v),
            }
        }
    }

    fn primary(&mut self, t: &[Tok], p: &mut usize) -> Result<Val, String> {
        let tok = t
            .get(*p)
            .cloned()
            .ok_or_else(|| "SyntaxError: Unexpected end of input".to_string())?;
        *p += 1;
        Ok(match tok {
            Tok::Num(n) => Val::Js(JsValue::Number(n)),
            Tok::Str(s) => Val::Js(JsValue::String(s)),
            Tok::P("(") => {
                let v = self.expr(t, p)?;
                if t.get(*p) != Some(&Tok::P(")")) {
                    return Err(self.unsupported());
                }
                *p += 1;
                v
            }
            Tok::P("[") => {
                let mut items = Vec::new();
                while t.get(*p) != Some(&Tok::P("]")) {
                    let v = self.expr(t, p)?;
                    items.push(js(self.doc, self.resolve(v)?));
                    if t.get(*p) == Some(&Tok::P(",")) {
                        *p += 1;
                    } else if t.get(*p) != Some(&Tok::P("]")) {
                        return Err(self.unsupported());
                    }
                }
                *p += 1;
                Val::Js(array(items))
            }
            Tok::Ident(id) => match id.as_str() {
                "true" => Val::Js(JsValue::Bool(true)),
                "false" => Val::Js(JsValue::Bool(false)),
                "null" => Val::Js(JsValue::Null),
                "undefined" => Val::Js(JsValue::Undefined),
                "NaN" => Val::Js(JsValue::Number(f64::NAN)),
                "Infinity" => Val::Js(JsValue::Number(f64::INFINITY)),
                "window" | "self" | "globalThis" => Val::Path(String::new()),
                _ => match self.env.get(&id) {
                    Some(v) => v.clone(),
                    None => Val::Path(id),
                },
            },
            Tok::P(_) => return Err(self.unsupported()),
        })
    }

    fn member(&mut self, v: Val, name: &str) -> Result<Val, String> {
        Ok(match v {
            Val::Path(base) => Val::Path(if base.is_empty() {
                name.to_string()
            } else {
                format!("{base}.{name}")
            }),
            Val::Node(n) => self.node_prop(n, name),
            Val::Nodes(list) => match name {
                "length" => Val::Js(JsValue::Number(list.len() as f64)),
                i => match i.parse::<usize>().ok().and_then(|i| list.get(i)) {
                    Some(n) => Val::Node(*n),
                    None => Val::Js(JsValue::Undefined),
                },
            },
            Val::Js(JsValue::String(s)) if name == "length" => {
                Val::Js(JsValue::Number(s.chars().count() as f64))
            }
            Val::Js(JsValue::Object(o)) => {
                if name == "length"
                    && let Some(l) = o.length
                {
                    return Ok(Val::Js(JsValue::Number(l as f64)));
                }
                Val::Js(
                    o.properties
                        .iter()
                        .find(|(k, _)| k == name)
                        .map_or(JsValue::Undefined, |(_, v)| v.clone()),
                )
            }
            Val::Js(JsValue::Undefined | JsValue::Null) => {
                return Err(format!(
                    "TypeError: Cannot read properties of undefined (reading '{name}')"
                ));
            }
            _ => Val::Js(JsValue::Undefined),
        })
    }

    fn node_prop(&self, n: NodeId, name: &str) -> Val {
        let d = self.doc;
        let s = |v: String| Val::Js(JsValue::String(v));
        match name {
            "outerHTML" => s(d.outer_html(n)),
            "innerHTML" => s(d.inner_html(n)),
            "textContent" | "innerText" => s(d.text_content(n).unwrap_or_default()),
            "tagName" | "nodeName" => s(d.node_name(n)),
            "nodeType" => Val::Js(JsValue::Number(d.node_type(n) as u32 as f64)),
            "id" => s(d.element_id(n).unwrap_or("").to_string()),
            "className" => s(d.class_name(n).to_string()),
            "value" => s(d.get_attribute(n, "value").unwrap_or("").to_string()),
            "checked" => Val::Js(JsValue::Bool(d.has_attribute(n, "checked"))),
            "isConnected" => Val::Js(JsValue::Bool(d.is_connected(n))),
            "parentElement" | "parentNode" => d.parent(n).map_or(Val::Js(JsValue::Null), Val::Node),
            "children" => Val::Nodes(d.element_children(n).collect()),
            "getAttribute" | "querySelector" | "querySelectorAll" => {
                Val::Path(format!("#node{}.{name}", n.0))
            }
            _ => Val::Js(JsValue::Undefined),
        }
    }

    fn apply_call(&mut self, f: Val, args: Vec<Val>) -> Result<Val, String> {
        let Val::Path(path) = f else {
            return Err("TypeError: value is not a function".into());
        };
        let arg_str = |i: usize| {
            args.get(i)
                .map(|a| to_string(&js(self.doc, a.clone())))
                .unwrap_or_default()
        };
        if let Some(rest) = path.strip_prefix("#node") {
            let (id, method) = rest.split_once('.').unwrap_or((rest, ""));
            let n = NodeId(id.parse().unwrap_or(0));
            return self.node_call(n, method, &arg_str(0));
        }
        match path.as_str() {
            "document.querySelector" => {
                self.node_call(Document::DOCUMENT, "querySelector", &arg_str(0))
            }
            "document.querySelectorAll" => {
                self.node_call(Document::DOCUMENT, "querySelectorAll", &arg_str(0))
            }
            "document.getElementById" => Ok(self
                .doc
                .get_element_by_id(&arg_str(0))
                .map_or(Val::Js(JsValue::Null), Val::Node)),
            "JSON.stringify" => {
                let v = args
                    .first()
                    .cloned()
                    .map(|a| js(self.doc, a))
                    .unwrap_or(JsValue::Undefined);
                Ok(Val::Js(JsValue::String(v.to_json().to_string())))
            }
            p if p.starts_with("console.") => {
                let kind = match &p["console.".len()..] {
                    "warn" => "warning".to_string(),
                    k => k.to_string(),
                };
                let vals = args.into_iter().map(|a| js(self.doc, a)).collect();
                self.console.push((kind, vals));
                Ok(Val::Js(JsValue::Undefined))
            }
            p if self.bindings.iter().any(|b| b == p) => {
                self.binding_calls.push((p.to_string(), arg_str(0)));
                Ok(Val::Js(JsValue::Undefined))
            }
            p => Err(match p.split('.').next() {
                Some(root) if !self.is_known_root(root) => {
                    format!("ReferenceError: {root} is not defined")
                }
                _ => format!("TypeError: {p} is not a function"),
            }),
        }
    }

    fn node_call(&self, n: NodeId, method: &str, arg: &str) -> Result<Val, String> {
        let bad = |_| {
            format!("SyntaxError: Failed to execute '{method}': '{arg}' is not a valid selector.")
        };
        match method {
            "querySelector" => Ok(self
                .doc
                .query_selector(n, arg)
                .map_err(bad)?
                .map_or(Val::Js(JsValue::Null), Val::Node)),
            "querySelectorAll" => Ok(Val::Nodes(
                self.doc.query_selector_all(n, arg).map_err(bad)?,
            )),
            "getAttribute" => Ok(self
                .doc
                .get_attribute(n, arg)
                .map_or(Val::Js(JsValue::Null), |v| {
                    Val::Js(JsValue::String(v.into()))
                })),
            _ => Err(format!("TypeError: {method} is not a function")),
        }
    }

    fn is_known_root(&self, root: &str) -> bool {
        matches!(
            root,
            "document" | "navigator" | "location" | "console" | "JSON" | "screen"
        ) || self.bindings.iter().any(|b| b == root)
    }

    /// Turns a host path into a value.
    fn resolve(&self, v: Val) -> Result<Val, String> {
        let Val::Path(path) = v else { return Ok(v) };
        let p = self.persona;
        let s = |v: &str| Ok(Val::Js(JsValue::String(v.to_string())));
        let n = |v: f64| Ok(Val::Js(JsValue::Number(v)));
        let url = url::Url::parse(self.url).ok();
        match path.as_str() {
            "" | "document" | "navigator" | "location" | "console" | "screen" | "JSON" => {
                Ok(Val::Path(path))
            }
            "document.title" => s(&self.doc.title()),
            "document.URL"
            | "document.documentURI"
            | "location.href"
            | "document.location.href" => s(self.url),
            "location.origin" => s(&url
                .map(|u| u.origin().ascii_serialization())
                .unwrap_or_else(|| "null".into())),
            "location.host" => s(&url
                .map(|u| {
                    u.host_str()
                        .map(|h| match u.port() {
                            Some(p) => format!("{h}:{p}"),
                            None => h.to_string(),
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default()),
            "location.hostname" => s(url.as_ref().and_then(|u| u.host_str()).unwrap_or("")),
            "location.pathname" => s(url.as_ref().map(|u| u.path()).unwrap_or("blank")),
            "document.readyState" => s("complete"),
            "document.documentElement" => Ok(self
                .doc
                .document_element()
                .map_or(Val::Js(JsValue::Null), Val::Node)),
            "document.body" => Ok(self.doc.body().map_or(Val::Js(JsValue::Null), Val::Node)),
            "document.head" => Ok(self.doc.head().map_or(Val::Js(JsValue::Null), Val::Node)),
            "document.doctype" => Ok(self
                .doc
                .doctype_node()
                .map_or(Val::Js(JsValue::Null), Val::Node)),
            "navigator.userAgent" => s(&p.browser.user_agent),
            "navigator.platform" => s(&p.os.platform),
            "navigator.language" => s(&p.locale.language),
            "navigator.languages" => Ok(Val::Js(array(
                p.locale
                    .languages
                    .iter()
                    .map(|l| JsValue::String(l.clone()))
                    .collect(),
            ))),
            "navigator.vendor" => s(&p.browser.vendor),
            "navigator.hardwareConcurrency" => n(f64::from(p.hardware.hardware_concurrency)),
            "navigator.deviceMemory" => n(p.hardware.device_memory),
            "navigator.webdriver" => Ok(Val::Js(JsValue::Bool(false))),
            "screen.width" => n(f64::from(p.screen.width)),
            "screen.height" => n(f64::from(p.screen.height)),
            "innerWidth" => n(f64::from(p.screen.inner_width)),
            "innerHeight" => n(f64::from(p.screen.inner_height)),
            "devicePixelRatio" => n(p.screen.device_pixel_ratio),
            other => {
                let root = other.split('.').next().unwrap_or("");
                if self.bindings.iter().any(|b| b == other) {
                    Ok(Val::Path(path))
                } else if other.contains('.') && self.is_known_root(root) {
                    Ok(Val::Js(JsValue::Undefined))
                } else if !other.contains('.') {
                    Err(format!("ReferenceError: {other} is not defined"))
                } else {
                    Err(format!("ReferenceError: {root} is not defined"))
                }
            }
        }
    }
}

fn js(doc: &Document, v: Val) -> JsValue {
    v.into_js(doc)
}

fn array(items: Vec<JsValue>) -> JsValue {
    JsValue::Object(Box::new(ObjectInfo {
        class_name: "Array".into(),
        subtype: Some("array".into()),
        length: Some(items.len()),
        properties: items
            .into_iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v))
            .collect(),
        ..Default::default()
    }))
}

fn truthy(v: &Val) -> bool {
    match v {
        Val::Js(JsValue::Undefined | JsValue::Null) => false,
        Val::Js(JsValue::Bool(b)) => *b,
        Val::Js(JsValue::Number(n)) => *n != 0.0 && !n.is_nan(),
        Val::Js(JsValue::String(s)) => !s.is_empty(),
        _ => true,
    }
}

fn to_number(v: &JsValue) -> f64 {
    match v {
        JsValue::Number(n) => *n,
        JsValue::Bool(b) => f64::from(u8::from(*b)),
        JsValue::Null => 0.0,
        JsValue::String(s) if s.trim().is_empty() => 0.0,
        JsValue::String(s) => s.trim().parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

fn to_string(v: &JsValue) -> String {
    match v {
        JsValue::String(s) => s.clone(),
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Bool(b) => b.to_string(),
        other => other.description().unwrap_or_default(),
    }
}

fn apply(op: &str, a: &JsValue, b: &JsValue) -> JsValue {
    use JsValue::*;
    let num = |f: fn(f64, f64) -> f64| Number(f(to_number(a), to_number(b)));
    match op {
        "+" => match (a, b) {
            (String(_), _) | (_, String(_)) => String(to_string(a) + &to_string(b)),
            _ => num(|x, y| x + y),
        },
        "-" => num(|x, y| x - y),
        "*" => num(|x, y| x * y),
        "/" => num(|x, y| x / y),
        "%" => num(|x, y| x % y),
        "===" | "==" => Bool(loose_eq(a, b)),
        "!==" | "!=" => Bool(!loose_eq(a, b)),
        "<" => Bool(to_number(a) < to_number(b)),
        ">" => Bool(to_number(a) > to_number(b)),
        "<=" => Bool(to_number(a) <= to_number(b)),
        ">=" => Bool(to_number(a) >= to_number(b)),
        "&&" => {
            if truthy(&Val::Js(a.clone())) {
                b.clone()
            } else {
                a.clone()
            }
        }
        "||" => {
            if truthy(&Val::Js(a.clone())) {
                a.clone()
            } else {
                b.clone()
            }
        }
        _ => Undefined,
    }
}

fn loose_eq(a: &JsValue, b: &JsValue) -> bool {
    match (a, b) {
        (JsValue::Number(x), JsValue::Number(y)) => x == y,
        (JsValue::String(x), JsValue::String(y)) => x == y,
        (JsValue::Bool(x), JsValue::Bool(y)) => x == y,
        (JsValue::Undefined | JsValue::Null, JsValue::Undefined | JsValue::Null) => true,
        _ => false,
    }
}

fn json_to_js(v: &Value) -> JsValue {
    match v {
        Value::Null => JsValue::Null,
        Value::Bool(b) => JsValue::Bool(*b),
        Value::Number(n) => JsValue::Number(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => JsValue::String(s.clone()),
        Value::Array(a) => array(a.iter().map(json_to_js).collect()),
        Value::Object(m) => JsValue::Object(Box::new(ObjectInfo {
            class_name: "Object".into(),
            properties: m.iter().map(|(k, v)| (k.clone(), json_to_js(v))).collect(),
            ..Default::default()
        })),
    }
}

/// Playwright's `serializeAsCallArgument` wire format (results of
/// `utilityScript.evaluate` with `returnByValue`).
fn pw_serialize(v: &JsValue) -> Value {
    match v {
        JsValue::Undefined => json!({ "v": "undefined" }),
        JsValue::Null => json!({ "v": "null" }),
        JsValue::Number(n) if n.is_nan() => json!({ "v": "NaN" }),
        JsValue::Number(n) if n.is_infinite() => {
            json!({ "v": if *n > 0.0 { "Infinity" } else { "-Infinity" } })
        }
        JsValue::Number(n) if *n == 0.0 && n.is_sign_negative() => json!({ "v": "-0" }),
        JsValue::BigInt(d) => json!({ "bi": d }),
        JsValue::Symbol(_) | JsValue::Function(_) => json!({ "v": "undefined" }),
        JsValue::Object(o) if o.subtype.as_deref() == Some("array") => {
            json!({ "a": o.properties.iter().map(|(_, v)| pw_serialize(v)).collect::<Vec<_>>(), "id": 1 })
        }
        JsValue::Object(o) if o.subtype.as_deref() == Some("error") => {
            json!({ "e": { "n": o.class_name, "m": o.description.clone().unwrap_or_default(), "s": "" } })
        }
        JsValue::Object(o) if o.subtype.as_deref() == Some("node") => json!({ "o": [], "id": 1 }),
        JsValue::Object(o) => json!({
            "o": o.properties.iter().map(|(k, v)| json!({ "k": k, "v": pw_serialize(v) })).collect::<Vec<_>>(),
            "id": 1
        }),
        other => other.to_json(),
    }
}

/// The inverse of [`pw_serialize`] for the shapes the mock understands.
fn pw_deserialize(v: &Value) -> JsValue {
    if let Some(s) = v.get("v").and_then(Value::as_str) {
        return match s {
            "null" => JsValue::Null,
            "NaN" => JsValue::Number(f64::NAN),
            "Infinity" => JsValue::Number(f64::INFINITY),
            "-Infinity" => JsValue::Number(f64::NEG_INFINITY),
            "-0" => JsValue::Number(-0.0),
            _ => JsValue::Undefined,
        };
    }
    if let Some(a) = v.get("a").and_then(Value::as_array) {
        return array(a.iter().map(pw_deserialize).collect());
    }
    if let Some(o) = v.get("o").and_then(Value::as_array) {
        return JsValue::Object(Box::new(ObjectInfo {
            class_name: "Object".into(),
            properties: o
                .iter()
                .map(|e| {
                    (
                        e.get("k").and_then(Value::as_str).unwrap_or("").to_string(),
                        pw_deserialize(e.get("v").unwrap_or(&Value::Null)),
                    )
                })
                .collect(),
            ..Default::default()
        }));
    }
    json_to_js(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_and_base64() {
        assert!(glob("*", "http://127.0.0.1/x"));
        assert!(glob("http://127.0.0.1/*.html", "http://127.0.0.1/a.html"));
        assert!(!glob("*.css", "http://127.0.0.1/a.html"));
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert_eq!(base64_decode("PGI+eDwvYj4=").unwrap(), b"<b>x</b>");
    }

    #[test]
    fn parses_functions() {
        let f = parse_function("(a, b) => a + b").unwrap();
        assert_eq!(f.params, ["a", "b"]);
        let f = parse_function("async function foo(x) { return x * 2; }").unwrap();
        assert_eq!(f.params, ["x"]);
        assert_eq!(f.body.trim(), "return x * 2;");
        let f = parse_function("(() => 1 + 1)").unwrap();
        assert!(f.params.is_empty());
        assert_eq!(
            statements("let a = ';'; return a"),
            ["let a = ';'", "return a"]
        );
    }

    #[test]
    fn loopback_only() {
        assert!(is_loopback_url("http://127.0.0.1:8765/shop"));
        assert!(is_loopback_url("https://localhost/"));
        assert!(is_loopback_url("http://[::1]:80/"));
        assert!(!is_loopback_url("https://example.com/"));
        assert!(!is_loopback_url("file:///etc/passwd"));
    }
}

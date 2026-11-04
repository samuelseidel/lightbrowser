//! Navigation: fetch, commit (new window + realm), streaming parse with
//! script execution, `DOMContentLoaded`, subresources, `load`.

use std::rc::Rc;

use lb_dom::{Document, NodeFlags, NodeId, ParseStep, local_name};
use lb_js::{JsErrorKind, ScriptOrigin};
use lb_webapi::host::{Destination, NetRequest, NetResponse, decode_text};
use lb_webapi::rt::script::{self, ScriptKind};
use lb_webapi::rt::{EngineJs, ReadyState, TaskSource, WINDOW_REF, Window, WindowInit, fire_event};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use url::Url;

use crate::browser::WorkerState;
use crate::context::PageError;
use crate::page::{Life, PageState};
use crate::{NavResult, is_html_type, text_document};

/// Parser input is fed in chunks of this size, yielding to the event loop
/// in between (like a network stream).
const CHUNK: usize = 32 * 1024;

/// The [`lb_webapi::host::NetHost`] of a page: its context's network
/// stack on the page's worker.
struct PageNet {
    ctx: crate::BrowserContext,
    page: std::rc::Weak<PageState>,
}

impl lb_webapi::host::NetHost for PageNet {
    fn fetch(&self, req: NetRequest) -> lb_webapi::host::LocalFuture<Result<NetResponse, String>> {
        let ctx = self.ctx.clone();
        let page = self.page.upgrade();
        Box::pin(async move {
            match page {
                Some(p) => crate::cdp::net::page_fetch(&p, req, None).await,
                None => ctx.fetch(req).await,
            }
        })
    }
    fn spawn(&self, fut: lb_webapi::host::LocalFuture<()>) {
        tokio::task::spawn_local(fut);
    }
    fn cookie_string(&self, url: &Url) -> String {
        self.ctx.cookies().document_cookie(url)
    }
    fn set_cookie_string(&self, url: &Url, cookie: &str) {
        self.ctx.cookies().set_document_cookie(url, cookie);
    }
}

/// A navigation request through the controlling service worker's `fetch`
/// event when there is one with a fetch handler, else the network.
pub(crate) async fn sw_or_network(
    page: &Rc<PageState>,
    req: NetRequest,
    sw: Option<&(lb_webapi::rt::sw::VersionInfo, bool)>,
    replaced: String,
    reserved: &str,
    reload: bool,
) -> Result<NetResponse, String> {
    sw_or_network_loader(page, req, sw, replaced, reserved, reload, None).await
}

async fn sw_or_network_loader(
    page: &Rc<PageState>,
    req: NetRequest,
    sw: Option<&(lb_webapi::rt::sw::VersionInfo, bool)>,
    replaced: String,
    reserved: &str,
    reload: bool,
    loader: Option<String>,
) -> Result<NetResponse, String> {
    use lb_webapi::rt::sw::{FetchOutcome, SwRequest};
    if let Some((v, true)) = sw {
        let sreq = SwRequest::from_net(&req, replaced, reserved.to_owned(), reload);
        match crate::sw::navigation_fetch(&page.ctx, v.id, sreq).await {
            FetchOutcome::Fallback => {}
            FetchOutcome::Respond(r) => return Ok(r),
            FetchOutcome::Error => return Err("net::ERR_FAILED".into()),
        }
    }
    crate::cdp::net::page_fetch(page, req, loader).await
}

/// Navigate (script-initiated or from [`crate::Page::navigate`]).
pub(crate) async fn navigate(
    ws: Rc<WorkerState>,
    page: Rc<PageState>,
    url: Url,
    initiator: Option<Url>,
    replace: bool,
) -> Result<NavResult, PageError> {
    let (tx, _rx) = oneshot::channel();
    navigate_committed(ws, page, url, initiator, replace, tx).await
}

/// Navigate; `committed` gets the result at commit (or the failure). The
/// returned future runs until `load`.
pub(crate) async fn navigate_committed(
    ws: Rc<WorkerState>,
    page: Rc<PageState>,
    url: Url,
    initiator: Option<Url>,
    replace: bool,
    committed: oneshot::Sender<Result<NavResult, PageError>>,
) -> Result<NavResult, PageError> {
    let seq = page.nav_seq.get() + 1;
    page.nav_seq.set(seq);
    page.loading.set(true);
    let agent = page.agent();
    let loader = agent.as_ref().map(|a| a.nav_started(seq));
    let reload = page.reloading.replace(false);
    // The service worker controlling the URL (ADR 0022) and the new
    // document's client id (`resultingClientId`).
    let sw = crate::sw::navigation_controller(&page.ctx, &url);
    let reserved = lb_webapi::rt::sw::uuid4();

    // Fetch (about:blank needs none).
    let resp = if url.scheme() == "about" {
        Ok(NetResponse {
            status: 200,
            status_text: "OK".into(),
            headers: vec![("content-type".into(), "text/html".into())],
            body: bytes::Bytes::new(),
            url: url.clone(),
            redirected: false,
        })
    } else {
        let mut req = NetRequest::get(url.clone(), Destination::Document, initiator.clone());
        req.mode = "navigate";
        // The document being replaced, as the request's client (Chrome
        // reports it for same-origin navigations such as reloads).
        let replaced = page
            .window()
            .filter(|w| w.url().origin() == url.origin())
            .map(|w| w.client_id())
            .unwrap_or_default();
        sw_or_network_loader(
            &page,
            req,
            sw.as_ref(),
            replaced,
            &reserved,
            reload,
            loader.clone(),
        )
        .await
    };
    if page.nav_seq.get() != seq || page.closed.get() {
        let _ = committed.send(Err(PageError::Aborted));
        return Err(PageError::Aborted);
    }
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            page.loading.set(false);
            if let Some(a) = &agent {
                a.nav_failed();
            }
            page.update_life(|l| {
                *l = Life {
                    loader: seq,
                    error: Some(e.clone()),
                    url: url.to_string(),
                    ..Life::default()
                }
            });
            let _ = committed.send(Err(PageError::Net(e.clone())));
            return Err(PageError::Net(e));
        }
    };

    // Commit: the old document and realm go away in one step.
    let history = page.window().map_or(0, |w| w.history_entry_count());
    page.drop_window(&ws);
    if !replace {
        page.add_history(history);
    }
    let Some((rt, bindings)) = ws.runtime() else {
        let _ = committed.send(Err(PageError::Closed));
        return Err(PageError::Closed);
    };
    let ct = resp
        .header("content-type")
        .unwrap_or("text/html")
        .to_owned();
    let html = is_html_type(&ct);
    let (text, charset) = decode_text(&resp.body, Some(&ct), html);
    let text = if html { text } else { text_document(&text) };
    let mime = ct
        .split(';')
        .next()
        .unwrap_or("text/html")
        .trim()
        .to_ascii_lowercase();
    let w = Window::new(
        &rt,
        WindowInit {
            url: resp.url.clone(),
            persona: page.ctx.persona().clone(),
            profile: page.ctx.profile().cloned(),
            net: Rc::new(PageNet {
                ctx: page.ctx.clone(),
                page: Rc::downgrade(&page),
            }),
            local_storage: page
                .ctx
                .local_storage()
                .area(&resp.url.origin().ascii_serialization()),
            session_storage: page.session_area(&resp.url),
            virtual_time: page.ctx.options().virtual_time,
            wake: page.waker(),
            referrer: initiator
                .as_ref()
                .map(|u| u.to_string())
                .unwrap_or_default(),
            content_type: if html { mime } else { "text/html".into() },
            charset,
            history_len: page.history_len(),
        },
    );
    w.set_console_sink(Some(page.console_sink()));
    w.set_worker_host(Some(crate::worker::Workers::host(&page.ctx)));
    w.set_sw_host(Some(crate::sw::host(&page.ctx)));
    let controller = sw.map(|(v, _)| v);
    w.init_sw_client(Some(reserved), false, controller.clone());
    if let (Some(v), Some(h)) = (&controller, w.sw_host()) {
        h.schedule_update(v.id);
    }
    let realm = match ws.with_engine(|e| w.create_realm(e, &bindings)) {
        Some(Ok(r)) => r,
        Some(Err(e)) => {
            let err = PageError::Setup(e.to_string());
            let _ = committed.send(Err(err.clone()));
            return Err(err);
        }
        None => {
            let _ = committed.send(Err(PageError::Closed));
            return Err(PageError::Closed);
        }
    };
    let _ = realm;
    crate::frames::install(&ws, &page, &w);
    w.start_parser();
    *page.window.borrow_mut() = Some(w.clone());
    page.update_life(|l| {
        *l = Life {
            loader: seq,
            committed: true,
            status: resp.status,
            url: resp.url.to_string(),
            ..Life::default()
        }
    });
    if let Some(a) = &agent {
        a.committed(&ws, &page, &w, seq);
    }
    page.notify.notify_one();
    let result = NavResult {
        status: resp.status,
        url: resp.url.to_string(),
        loader_id: seq,
        headers: resp.headers.clone(),
    };
    let _ = committed.send(Ok(result.clone()));
    page.emit("init");
    page.emit("commit");

    let r = load(&ws, &page, &w, Doc::Main(seq), &text).await;
    page.loading.set(false);
    page.notify.notify_one();
    r.map(|()| result)
}

/// A script that runs after parsing (`defer`, modules).
enum Deferred {
    Classic(NodeId, JoinHandle<Result<NetResponse, String>>),
    Module(Option<NodeId>, Url, Source),
}

enum Source {
    Inline(String),
    Fetch(JoinHandle<Result<NetResponse, String>>),
}

fn spawn_fetch(
    page: &Rc<PageState>,
    w: &Rc<Window>,
    url: Url,
    dest: Destination,
) -> JoinHandle<Result<NetResponse, String>> {
    let page = page.clone();
    let req = NetRequest::get(url, dest, Some(w.url()));
    w.in_flight.set(w.in_flight.get() + 1);
    let w2 = Rc::downgrade(w);
    let fut = w.net_fetch(req);
    let _ = page;
    tokio::task::spawn_local(async move {
        let r = fut.await;
        if let Some(w) = w2.upgrade() {
            w.in_flight.set(w.in_flight.get().saturating_sub(1));
            w.wake();
        }
        r
    })
}

async fn join(h: JoinHandle<Result<NetResponse, String>>) -> Result<NetResponse, String> {
    h.await.unwrap_or_else(|e| Err(e.to_string()))
}

fn ok_body(r: &Result<NetResponse, String>) -> Option<&NetResponse> {
    r.as_ref().ok().filter(|r| (200..300).contains(&r.status))
}

/// Run `f` with the engine as a [`lb_webapi::rt::Js`].
fn with_js<R>(
    ws: &WorkerState,
    w: &Rc<Window>,
    f: impl FnOnce(&mut EngineJs<'_>) -> R,
) -> Option<R> {
    let realm = w.realm();
    ws.with_engine(|engine| {
        let mut js = EngineJs { engine, realm };
        let r = f(&mut js);
        js.engine.perform_microtask_checkpoint();
        r
    })
}

/// Which document [`load`] runs: the page's (navigation sequence) or a
/// child frame's (frame id).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Doc {
    Main(u64),
    Frame(u64),
}

fn current(page: &PageState, w: &Rc<Window>, doc: Doc) -> bool {
    let seq_ok = match doc {
        Doc::Main(seq) => page.nav_seq.get() == seq,
        Doc::Frame(_) => true,
    };
    seq_ok && !w.is_closed() && !page.closed.get()
}

/// A lifecycle point of `doc`.
fn doc_event(page: &PageState, doc: Doc, name: &str) {
    match doc {
        Doc::Main(_) => {
            match name {
                "DOMContentLoaded" => page.update_life(|l| l.dcl = true),
                "load" => page.update_life(|l| l.load = true),
                _ => {}
            }
            page.emit(name);
        }
        Doc::Frame(id) => crate::frames::lifecycle(page, id, name),
    }
}

/// Wait until `cond` holds (re-checked whenever the page's loads change).
async fn wait_for(
    page: &PageState,
    w: &Rc<Window>,
    doc: Doc,
    cond: impl Fn(&Window) -> bool,
) -> Result<(), PageError> {
    loop {
        if !current(page, w, doc) {
            return Err(PageError::Aborted);
        }
        let notified = page.loads.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if cond(w) {
            return Ok(());
        }
        notified.await;
    }
}

/// A parser script waits for script-blocking style sheets. Afterwards the
/// gate stays closed (sheet events wait) until the caller ran its script
/// and called `set_script_waiting_for_sheets(false)`.
async fn wait_script_sheets(page: &PageState, w: &Rc<Window>, doc: Doc) -> Result<(), PageError> {
    if w.script_blocking_sheets() == 0 {
        return Ok(());
    }
    w.set_script_waiting_for_sheets(true);
    let r = wait_for(page, w, doc, |w| w.script_blocking_sheets() == 0).await;
    if r.is_err() {
        w.set_script_waiting_for_sheets(false);
    }
    r
}

/// After a parser script that waited for sheets: their `load` events fire
/// before parsing resumes.
async fn resume_after_sheets(
    page: &PageState,
    w: &Rc<Window>,
    doc: Doc,
    waited: bool,
) -> Result<(), PageError> {
    if !waited {
        return Ok(());
    }
    w.set_script_waiting_for_sheets(false);
    wait_for(page, w, doc, |w| w.sheet_events_pending() == 0).await
}

/// Parse the document, run its scripts and fire the load events.
pub(crate) async fn load(
    ws: &Rc<WorkerState>,
    page: &Rc<PageState>,
    w: &Rc<Window>,
    doc: Doc,
    text: &str,
) -> Result<(), PageError> {
    let chunks = split_chunks(text);
    let mut next = 0;
    let mut deferred: Vec<Deferred> = Vec::new();
    if chunks.is_empty() {
        w.parser_end();
    }
    loop {
        if !current(page, w, doc) {
            return Err(PageError::Aborted);
        }
        let Some(step) = w.parser_run() else { break };
        with_js(ws, w, |js| w.after_mutation(js));
        match step {
            ParseStep::NeedInput => {
                if next < chunks.len() {
                    w.parser_push(chunks[next]);
                    next += 1;
                    if next > 1 {
                        tokio::task::yield_now().await;
                    }
                } else {
                    w.parser_end();
                }
            }
            ParseStep::Done => break,
            ParseStep::Script(n) => {
                parser_script(ws, page, w, n, doc, &mut deferred).await?;
            }
            ParseStep::StyleSheet(_) => {
                // A sheet in <body> stops the parser until it loaded and
                // its `load` event fired (Chrome).
                if w.parser_blocking_sheets() > 0 {
                    wait_for(page, w, doc, |w| w.parser_blocking_sheets() == 0).await?;
                    wait_for(page, w, doc, |w| w.sheet_events_pending() == 0).await?;
                }
            }
        }
    }
    // Chrome ends parsing in a later task than the last script ran in:
    // tasks queued by then (element `load` events, ...) run first.
    for _ in 0..w.queued_task_count() {
        let Some(task) = w.take_task() else { break };
        with_js(ws, w, |js| task(js, w));
    }
    w.finish_parser();
    if !current(page, w, doc) {
        return Err(PageError::Aborted);
    }

    // End of parsing: interactive, deferred scripts, DOMContentLoaded.
    w.set_ready_state(ReadyState::Interactive);
    with_js(ws, w, |js| {
        fire_event(
            js,
            w,
            w.node_ref(0, Document::DOCUMENT),
            "readystatechange",
            false,
            false,
        )
    });
    // Deferred scripts wait for script-blocking sheets too; once those
    // loaded, the scripts and DOMContentLoaded run before the sheets'
    // `load` events (Chrome).
    for d in deferred {
        if !current(page, w, doc) {
            w.set_script_waiting_for_sheets(false);
            return Err(PageError::Aborted);
        }
        match d {
            Deferred::Classic(n, h) => {
                if !h.is_finished() {
                    w.set_script_waiting_for_sheets(false);
                }
                let r = join(h).await;
                wait_script_sheets(page, w, doc).await?;
                run_fetched_classic(ws, w, n, &r);
            }
            Deferred::Module(n, url, src) => {
                let source = match src {
                    Source::Inline(s) => Some(s),
                    Source::Fetch(h) => {
                        if !h.is_finished() {
                            w.set_script_waiting_for_sheets(false);
                        }
                        ok_body(&join(h).await)
                            .map(|r| script::script_source(&r.body, r.header("content-type")))
                    }
                };
                wait_script_sheets(page, w, doc).await?;
                match source {
                    Some(s) => run_module(ws, page, w, n, &url, s).await,
                    None => {
                        if let Some(n) = n {
                            with_js(ws, w, |js| script::fire_error(js, w, n));
                        }
                    }
                }
            }
        }
    }
    with_js(ws, w, |js| {
        fire_event(
            js,
            w,
            w.node_ref(0, Document::DOCUMENT),
            "DOMContentLoaded",
            true,
            false,
        )
    });
    doc_event(page, doc, "DOMContentLoaded");
    w.sw_enable_messages();
    w.set_script_waiting_for_sheets(false);
    w.load_background_images();

    // Wait for async scripts, stylesheets and script-inserted scripts.
    loop {
        if !current(page, w, doc) {
            return Err(PageError::Aborted);
        }
        let notified = page.loads.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if w.pending_loads.get() == 0 {
            break;
        }
        notified.await;
    }

    w.set_ready_state(ReadyState::Complete);
    with_js(ws, w, |js| {
        fire_event(
            js,
            w,
            w.node_ref(0, Document::DOCUMENT),
            "readystatechange",
            false,
            false,
        );
        fire_event(js, w, WINDOW_REF, "load", false, false);
        lb_webapi::rt::fire_event_with(
            js,
            w,
            WINDOW_REF,
            "PageTransitionEvent",
            "pageshow",
            false,
            false,
            lb_webapi::rt::EventExtra::PageTransition { persisted: false },
        );
    });
    doc_event(page, doc, "load");
    if let Doc::Main(_) = doc {
        request_favicon(page, w);
    }
    page.notify.notify_one();
    Ok(())
}

/// Chrome asks for a top-level document's icon after `load`: the last
/// `<link rel=icon>` href, or `/favicon.ico` on the page's origin. The
/// browser process makes that request, so it is not reported to CDP and
/// nothing waits for it.
fn request_favicon(page: &Rc<PageState>, w: &Rc<Window>) {
    let href = w.with_doc(|d| {
        d.descendants(Document::DOCUMENT)
            .filter(|&n| {
                d.is_html_element_named(n, &local_name!("link"))
                    && d.get_attribute(n, "rel").is_some_and(|r| {
                        r.split_ascii_whitespace()
                            .any(|t| t.eq_ignore_ascii_case("icon"))
                    })
            })
            .filter_map(|n| d.get_attribute(n, "href").map(str::to_owned))
            .last()
    });
    let icon = href.and_then(|h| w.resolve_url(&h));
    let page_url = w.url();
    let ctx = page.ctx.clone();
    tokio::task::spawn_local(async move {
        let _ = ctx.request_favicon(&page_url, icon).await;
    });
}

fn split_chunks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut end = rest.len().min(CHUNK);
        while !rest.is_char_boundary(end) {
            end += 1;
        }
        out.push(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

fn run_fetched_classic(
    ws: &Rc<WorkerState>,
    w: &Rc<Window>,
    n: NodeId,
    r: &Result<NetResponse, String>,
) {
    with_js(ws, w, |js| exec_fetched(js, w, n, r));
}

fn exec_fetched(
    js: &mut dyn lb_webapi::rt::Js,
    w: &Rc<Window>,
    n: NodeId,
    r: &Result<NetResponse, String>,
) {
    match ok_body(r) {
        Some(resp) => {
            let src = script::script_source(&resp.body, resp.header("content-type"));
            script::execute_classic(js, w, Some(n), &src, resp.url.as_str(), 0);
            script::fire_load(js, w, n);
        }
        None => script::fire_error(js, w, n),
    }
}

/// A `</script>` the parser stopped at.
async fn parser_script(
    ws: &Rc<WorkerState>,
    page: &Rc<PageState>,
    w: &Rc<Window>,
    n: NodeId,
    doc: Doc,
    deferred: &mut Vec<Deferred>,
) -> Result<(), PageError> {
    let kind = w.with_doc(|d| {
        if !d.is_html_element_named(n, &local_name!("script"))
            || d.flags(n).contains(NodeFlags::ALREADY_STARTED)
        {
            return None;
        }
        // Scripts inside <template> are inert.
        if !d.is_connected(n) {
            return None;
        }
        let kind = script::script_kind(d, n);
        if kind == ScriptKind::Classic && d.has_attribute(n, "nomodule") {
            return None;
        }
        Some(kind)
    });
    let Some(kind) = kind else { return Ok(()) };
    if kind == ScriptKind::Data {
        return Ok(());
    }
    w.set_node_flags(n, NodeFlags::ALREADY_STARTED, true);
    let src = script::script_src(w, n);
    let (is_async, is_defer) = script::script_flags(w, n);
    let line = w.parser_script_line(n).unwrap_or(1).saturating_sub(1) as u32;
    match (kind, src) {
        (ScriptKind::Classic, None) => {
            let waited = w.script_blocking_sheets() > 0;
            wait_script_sheets(page, w, doc).await?;
            let text = script::inline_text(w, n);
            let url = w.url().to_string();
            with_js(ws, w, |js| {
                script::execute_classic(js, w, Some(n), &text, &url, line)
            });
            resume_after_sheets(page, w, doc, waited).await?;
        }
        (_, Some(None)) => {
            let w2 = w.clone();
            w.queue_task(
                TaskSource::DomManipulation,
                Box::new(move |js, _| script::fire_error(js, &w2, n)),
            );
        }
        (ScriptKind::Classic, Some(Some(url))) => {
            let h = spawn_fetch(page, w, url, Destination::Script);
            if is_async {
                w.pending_loads.set(w.pending_loads.get() + 1);
                let w2 = w.clone();
                tokio::task::spawn_local(async move {
                    let r = join(h).await;
                    w2.queue_task(
                        TaskSource::Networking,
                        Box::new(move |js, w| {
                            exec_fetched(js, w, n, &r);
                            w.pending_loads.set(w.pending_loads.get().saturating_sub(1));
                            w.wake();
                        }),
                    );
                });
            } else if is_defer {
                deferred.push(Deferred::Classic(n, h));
            } else {
                // Parser-blocking: waits for the script and for
                // script-blocking style sheets.
                let r = join(h).await;
                let waited = w.script_blocking_sheets() > 0;
                wait_script_sheets(page, w, doc).await?;
                run_fetched_classic(ws, w, n, &r);
                resume_after_sheets(page, w, doc, waited).await?;
            }
        }
        (ScriptKind::Module, src) => {
            let doc_url = w.url();
            let (url, source) = match src {
                None => (doc_url, Source::Inline(script::inline_text(w, n))),
                Some(Some(u)) => (
                    u.clone(),
                    Source::Fetch(spawn_fetch(page, w, u, Destination::Script)),
                ),
                Some(None) => unreachable!(),
            };
            let node = src_node(n);
            if is_async {
                w.pending_loads.set(w.pending_loads.get() + 1);
                let (ws2, p2, w2) = (ws.clone(), page.clone(), w.clone());
                tokio::task::spawn_local(async move {
                    let s = match source {
                        Source::Inline(s) => Some(s),
                        Source::Fetch(h) => ok_body(&join(h).await)
                            .map(|r| script::script_source(&r.body, r.header("content-type"))),
                    };
                    if let Some(s) = s {
                        run_module(&ws2, &p2, &w2, node, &url, s).await;
                    }
                    w2.pending_loads
                        .set(w2.pending_loads.get().saturating_sub(1));
                    w2.wake();
                });
            } else {
                deferred.push(Deferred::Module(node, url, source));
            }
        }
        (ScriptKind::Data, _) => {}
    }
    Ok(())
}

fn src_node(n: NodeId) -> Option<NodeId> {
    Some(n)
}

/// Static import specifiers of a module (`import ... from "x"`,
/// `import "x"`, `export ... from "x"`). A scan, not a parser.
pub(crate) fn import_specifiers(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = src.as_bytes();
    let mut i = 0;
    let quoted_after = |mut j: usize| -> Option<(String, usize)> {
        while j < b.len() && (b[j] as char).is_whitespace() {
            j += 1;
        }
        let q = *b.get(j)?;
        if q != b'"' && q != b'\'' {
            return None;
        }
        let end = src[j + 1..].find(q as char)? + j + 1;
        Some((src[j + 1..end].to_owned(), end + 1))
    };
    while i < b.len() {
        let rest = &src[i..];
        let word_start =
            i == 0 || !(b[i - 1] as char).is_alphanumeric() && b[i - 1] != b'_' && b[i - 1] != b'.';
        let kw = if !word_start {
            0
        } else if rest.starts_with("from") {
            4
        } else if rest.starts_with("import") {
            6
        } else {
            0
        };
        if kw > 0
            && let Some((s, end)) = quoted_after(i + kw)
        {
            out.push(s);
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// Prefetch a module graph's static imports, then evaluate it.
async fn run_module(
    ws: &Rc<WorkerState>,
    page: &Rc<PageState>,
    w: &Rc<Window>,
    n: Option<NodeId>,
    url: &Url,
    source: String,
) {
    let mut queue: Vec<(Url, String)> = vec![(url.clone(), source.clone())];
    let mut fetched = 0;
    while let Some((base, src)) = queue.pop() {
        for spec in import_specifiers(&src) {
            let Ok(dep) = base.join(&spec) else { continue };
            if w.module_map.borrow().contains_key(dep.as_str()) || fetched > 256 {
                continue;
            }
            fetched += 1;
            let r = join(spawn_fetch(page, w, dep.clone(), Destination::Script)).await;
            if let Some(resp) = ok_body(&r) {
                let s = script::script_source(&resp.body, resp.header("content-type"));
                w.module_map.borrow_mut().insert(dep.to_string(), s.clone());
                queue.push((dep, s));
            }
        }
    }
    let realm = w.realm();
    let result = ws.with_engine(|e| {
        let r = e.evaluate_module(realm, &source, ScriptOrigin::new(url.as_str()));
        e.perform_microtask_checkpoint();
        r
    });
    with_js(ws, w, |js| {
        if let Some(Err(e)) = &result
            && e.kind != JsErrorKind::Host
        {
            w.report_exception(js, e);
        }
        w.after_mutation(js);
        if let Some(n) = n.filter(|&n| w.with_doc(|d| d.has_attribute(n, "src"))) {
            match &result {
                Some(Ok(_)) => script::fire_load(js, w, n),
                _ => script::fire_error(js, w, n),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports() {
        let s = "import a from './a.js';\nimport {b} from \"../b.js\"; import './side.js';\nexport * from './c.js'; const from = 1; x.import('no');";
        assert_eq!(
            import_specifiers(s),
            ["./a.js", "../b.js", "./side.js", "./c.js"]
        );
    }

    #[test]
    fn chunks_on_char_boundaries() {
        let s = "é".repeat(CHUNK);
        let c = split_chunks(&s);
        assert_eq!(c.concat(), s);
        assert!(c.len() >= 2);
    }
}

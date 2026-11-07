//! Child frames on the page layer (chrome).
//!
//! lb-webapi owns the frame tree (`lb_webapi::rt::frames`): it creates an
//! iframe's initial `about:blank` window synchronously and asks for
//! navigations through [`FrameHooks`]. This module implements those hooks
//! for a page:
//!
//! - `navigate`: load the frame's document (network, `srcdoc`), build its
//!   window with the frame's origin and storage (by its storage key: the
//!   page's top-level site and the ancestor chain bit, ADR 0037), swap it
//!   in, then parse it with the same loader as top-level documents
//!   (`nav::load`). The parent's `load` waits for the frame; the
//!   `<iframe>` gets `load` (also after a network error, as in Chrome).
//! - `committed` / `detached`: report frames and their execution contexts
//!   to the CDP agent (frameAttached, frameNavigated, contexts per world,
//!   lifecycle, frameDetached).
//!
//! The page's event loop runs tasks, timers and animation frames of every
//! window in the tree ([`tree`]).

use std::rc::{Rc, Weak};

use lb_js::RealmOps;
use lb_webapi::host::StorageKey;
use lb_webapi::host::{Destination, NetResponse, decode_text};
use lb_webapi::rt::frames::{self as wf, FrameHooks, FrameNavigation};
use lb_webapi::rt::{TaskSource, Window, fire_event};

use crate::browser::WorkerState;
use crate::page::PageState;
use crate::{is_html_type, text_document};

/// The page's hooks, installed on each top-level window.
struct Hooks {
    ws: Weak<WorkerState>,
    page: Weak<PageState>,
}

impl FrameHooks for Hooks {
    fn committed(&self, ops: &mut dyn RealmOps, child: &Rc<Window>, initial: bool) {
        if let Some(p) = self.page.upgrade()
            && let Some(a) = p.agent()
        {
            a.frame_committed(ops, child, initial);
        }
    }

    fn detached(&self, ops: &mut dyn RealmOps, child: &Rc<Window>, gone: bool) {
        if let Some(p) = self.page.upgrade()
            && let Some(a) = p.agent()
        {
            a.frame_detached(ops, child, gone);
        }
    }

    fn navigate(&self, child: &Rc<Window>, nav: FrameNavigation) {
        let (Some(ws), Some(page)) = (self.ws.upgrade(), self.page.upgrade()) else {
            return;
        };
        start(&ws, &page, child, nav);
    }
}

/// Count the navigation against the parent's `load` right away (the
/// parser may be about to check), then run it.
fn start(ws: &Rc<WorkerState>, page: &Rc<PageState>, child: &Rc<Window>, nav: FrameNavigation) {
    let Some(parent) = wf::parent(child) else {
        return;
    };
    parent.pending_loads.set(parent.pending_loads.get() + 1);
    let (ws, page, child) = (ws.clone(), page.clone(), child.clone());
    tokio::task::spawn_local(async move {
        navigate_frame(ws, page, parent, child, nav).await;
    });
}

/// Install the frame hooks on a page's new top-level window.
pub(crate) fn install(ws: &Rc<WorkerState>, page: &Rc<PageState>, w: &Window) {
    wf::set_hooks(
        w,
        Some(Rc::new(Hooks {
            ws: Rc::downgrade(ws),
            page: Rc::downgrade(page),
        })),
    );
}

/// The page's windows: the top-level one first, then every frame.
pub(crate) fn tree(top: &Rc<Window>) -> Vec<Rc<Window>> {
    let mut v = vec![top.clone()];
    v.extend(wf::descendants(top));
    v
}

/// Load `nav` into the frame whose current window is `old`.
async fn navigate_frame(
    ws: Rc<WorkerState>,
    page: Rc<PageState>,
    parent: Rc<Window>,
    old: Rc<Window>,
    nav: FrameNavigation,
) {
    // `start` counted this navigation in the parent's `pending_loads`. The
    // parent comes from `start`: a frame removed before this task runs has
    // no parent link any more, and its count must still be settled.
    let id = wf::frame_id(&old);
    let done = FrameLoad {
        parent: parent.clone(),
        id,
    };
    if let Some(a) = page.agent() {
        a.frame_started_loading(id);
    }
    let parent_url = wf::effective_url(&parent);
    // The frame's storage key (ADR 0037): the parent's top-level site and
    // ancestor chain bit, and the new document's origin.
    let parent_key = parent.storage_key();
    // A service worker controlling the frame's URL sees the navigation
    // first (ADR 0022), the one registered in the frame's partition.
    let sw = if nav.srcdoc.is_none() {
        let key = StorageKey::for_frame(&parent_key, &nav.url);
        crate::sw::navigation_controller(&page.ctx, &nav.url, &key)
    } else {
        None
    };
    let reserved = lb_webapi::rt::sw::uuid4();
    let (resp, creator) = match &nav.srcdoc {
        Some(doc) => (
            Ok(NetResponse {
                status: 200,
                status_text: "OK".into(),
                headers: vec![("content-type".into(), "text/html".into())],
                body: doc.clone().into_bytes().into(),
                url: nav.url.clone(),
                redirected: false,
            }),
            Some(parent_url.clone()),
        ),
        // `frame-src` blocked it (ADR 0036): Chrome's error page.
        None if nav.url.as_str() == lb_webapi::rt::frames::CSP_BLOCKED_FRAME => (
            Ok(NetResponse {
                status: 200,
                status_text: "OK".into(),
                headers: vec![("content-type".into(), "text/html".into())],
                body: bytes::Bytes::new(),
                url: nav.url.clone(),
                redirected: false,
            }),
            None,
        ),
        None if nav.url.scheme() == "about" => (
            Ok(NetResponse {
                status: 200,
                status_text: "OK".into(),
                headers: vec![("content-type".into(), "text/html".into())],
                body: bytes::Bytes::new(),
                url: nav.url.clone(),
                redirected: false,
            }),
            Some(parent_url.clone()),
        ),
        None => {
            let mut req = crate::nav::document_request(
                nav.url.clone(),
                Destination::Iframe,
                nav.initiator.clone(),
                nav.post.clone(),
                nav.user_activated,
            );
            // Cookies of a frame navigation: the parent's site for cookies.
            req.storage_key = Some(parent_key.clone());
            parent.in_flight.set(parent.in_flight.get() + 1);
            let r =
                crate::nav::sw_or_network(&page, req, sw.as_ref(), String::new(), &reserved, false)
                    .await;
            parent
                .in_flight
                .set(parent.in_flight.get().saturating_sub(1));
            (r, None)
        }
    };
    // Still this frame's current window?
    let current = wf::children(&parent)
        .iter()
        .any(|c| c.id == id && Rc::ptr_eq(&c.window, &old));
    if !current || parent.is_closed() || page.closed.get() {
        done.cancel();
        return;
    }
    let mut resp = match resp {
        Ok(r) => r,
        Err(e) => {
            old.warn(format!("Failed to load frame {}: {e}", nav.url));
            if let Some(a) = page.agent() {
                a.frame_lifecycle(id, "load");
            }
            done.finish();
            return;
        }
    };
    // The document's `frame-ancestors` (ADR 0036): refused, Chrome shows
    // its error page.
    if creator.is_none()
        && !lb_webapi::rt::csp::frame_ancestors_allow(&parent, &resp.headers, &resp.url)
        && let Ok(u) = url::Url::parse(lb_webapi::rt::frames::CSP_BLOCKED_FRAME)
    {
        resp = NetResponse {
            status: 200,
            status_text: "OK".into(),
            headers: vec![("content-type".into(), "text/html".into())],
            body: bytes::Bytes::new(),
            url: u,
            redirected: false,
        };
    }
    let Some((rt, _)) = ws.runtime() else {
        done.cancel();
        return;
    };
    let ct = resp
        .header("content-type")
        .unwrap_or("text/html")
        .to_owned();
    let html = is_html_type(&ct);
    let (text, charset) = decode_text(&resp.body, Some(&ct), html);
    let text = if html { text } else { text_document(&text) };
    let mut init = wf::child_window_init(&parent, resp.url.clone());
    let key = StorageKey::for_frame(&parent_key, creator.as_ref().unwrap_or(&resp.url));
    if creator.is_none() {
        init.local_storage = page.ctx.local_storage().area(&key.serialize());
        init.session_storage = page.session_area(&key);
    }
    init.charset = charset;
    init.referrer = nav
        .initiator
        .as_ref()
        .map(|u| u.to_string())
        .unwrap_or_default();
    let w = Window::new(&rt, init);
    // Content Security Policy (ADR 0036): `srcdoc` and `about:` documents
    // inherit the parent's policies, others take their response's.
    if creator.is_some() {
        w.csp_inherit(&parent);
    } else {
        w.set_csp_bypass(parent.csp_bypass());
        if resp.url.as_str() != lb_webapi::rt::frames::CSP_BLOCKED_FRAME {
            w.csp_deliver(&resp.headers, resp.status);
        }
    }
    w.set_storage_key(key.clone());
    w.set_worker_host(parent.worker_host());
    w.set_sw_host(Some(crate::sw::host(&page.ctx, &key)));
    w.set_idb_backend(parent.idb_backend());
    let controller = sw.map(|(v, _)| v);
    w.init_sw_client(Some(reserved), true, controller.clone());
    if let (Some(v), Some(h)) = (&controller, w.sw_host()) {
        h.schedule_update(v.id);
    }
    let r = ws.with_engine(|e| wf::commit_child(e, &parent, id, &w, creator));
    match r {
        Some(Ok(())) => {}
        Some(Err(e)) => {
            parent.warn(format!("Failed to create frame realm: {}", e.message));
            done.finish();
            return;
        }
        None => {
            done.cancel();
            return;
        }
    }
    ws.with_engine(|e| w.csp_apply_codegen(e));
    w.start_parser();
    if let Some(a) = page.agent() {
        a.frame_lifecycle(id, "init");
        a.frame_lifecycle(id, "commit");
    }
    let _ = crate::nav::load(&ws, &page, &w, crate::nav::Doc::Frame(id), &text).await;
    page.notify.notify_one();
    if !w.is_closed() {
        done.finish();
    } else {
        done.cancel();
    }
}

/// Settles the parent's `pending_loads` for one frame navigation, firing
/// `load` at the `<iframe>` first when the frame finished.
struct FrameLoad {
    parent: Rc<Window>,
    id: u64,
}

impl FrameLoad {
    fn finish(self) {
        let id = self.id;
        self.parent.queue_task(
            TaskSource::DomManipulation,
            Box::new(move |js, p| {
                let el = wf::children(p)
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.element);
                if let Some(el) = el {
                    fire_event(js, p, p.node_ref(0, el), "load", false, false);
                }
                p.pending_loads.set(p.pending_loads.get().saturating_sub(1));
                p.wake();
            }),
        );
    }

    fn cancel(self) {
        let p = &self.parent;
        p.pending_loads.set(p.pending_loads.get().saturating_sub(1));
        p.wake();
    }
}

/// Lifecycle points of a frame document loaded by `nav::load`.
pub(crate) fn lifecycle(page: &PageState, id: u64, name: &str) {
    if let Some(a) = page.agent() {
        a.frame_lifecycle(id, name);
    }
}

/// Frame window `w` asked for a navigation (`location`, form submission).
pub(crate) fn navigate_from_script(
    ws: &Rc<WorkerState>,
    page: &Rc<PageState>,
    w: &Rc<Window>,
    req: lb_webapi::rt::NavRequest,
) {
    let nav = FrameNavigation {
        url: req.url,
        srcdoc: None,
        initiator: Some(w.url()),
        post: req.post,
        user_activated: req.user_activated,
    };
    start(ws, page, w, nav);
}

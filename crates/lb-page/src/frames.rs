//! Child frames on the page layer (chrome).
//!
//! lb-webapi owns the frame tree (`lb_webapi::rt::frames`): it creates an
//! iframe's initial `about:blank` window synchronously and asks for
//! navigations through [`FrameHooks`]. This module implements those hooks
//! for a page:
//!
//! - `navigate`: load the frame's document (network, `srcdoc`), build its
//!   window with the frame's origin and storage, swap it in, then parse it
//!   with the same loader as top-level documents (`nav::load`). The
//!   parent's `load` waits for the frame; the `<iframe>` gets `load` (also
//!   after a network error, as in Chrome).
//! - `committed` / `detached`: report frames and their execution contexts
//!   to the CDP agent (frameAttached, frameNavigated, contexts per world,
//!   lifecycle, frameDetached).
//!
//! The page's event loop runs tasks, timers and animation frames of every
//! window in the tree ([`tree`]).

use std::rc::{Rc, Weak};

use lb_js::RealmOps;
use lb_webapi::host::{Destination, NetRequest, NetResponse, decode_text};
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
        navigate_frame(ws, page, child, nav).await;
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
    old: Rc<Window>,
    nav: FrameNavigation,
) {
    let Some(parent) = wf::parent(&old) else {
        return;
    };
    let id = wf::frame_id(&old);
    // `start` counted this navigation in the parent's `pending_loads`.
    let done = FrameLoad {
        parent: parent.clone(),
        id,
    };
    if let Some(a) = page.agent() {
        a.frame_started_loading(id);
    }
    let parent_url = wf::effective_url(&parent);
    // A service worker controlling the frame's URL sees the navigation
    // first (ADR 0022).
    let sw = if nav.srcdoc.is_none() {
        crate::sw::navigation_controller(&page.ctx, &nav.url)
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
            let mut req =
                NetRequest::get(nav.url.clone(), Destination::Iframe, nav.initiator.clone());
            req.mode = "navigate";
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
    let resp = match resp {
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
    if creator.is_none() {
        let origin = resp.url.origin().ascii_serialization();
        init.local_storage = page.ctx.local_storage().area(&origin);
        init.session_storage = page.session_area(&resp.url);
    }
    init.charset = charset;
    init.referrer = nav
        .initiator
        .as_ref()
        .map(|u| u.to_string())
        .unwrap_or_default();
    let w = Window::new(&rt, init);
    w.set_worker_host(parent.worker_host());
    w.set_sw_host(parent.sw_host());
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

/// `location` changed inside frame window `w` (script navigation).
pub(crate) fn navigate_from_script(
    ws: &Rc<WorkerState>,
    page: &Rc<PageState>,
    w: &Rc<Window>,
    url: url::Url,
) {
    let nav = FrameNavigation {
        url,
        srcdoc: None,
        initiator: Some(w.url()),
    };
    start(ws, page, w, nav);
}

//! Pages: the `Send` handle, the per-page state on its worker, and the
//! HTML event loop.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use lb_js::{Engine, JsValue, ScriptOrigin};
use lb_webapi::host::{ConsoleMessage, StorageArea};
use lb_webapi::rt::{EngineJs, Window};
use tokio::sync::{Notify, oneshot, watch};
use url::Url;

use crate::browser::{WorkerHandle, WorkerState};
use crate::context::{BrowserContext, PageError};
use crate::{NavResult, PageObserver, WaitUntil};

/// One frame of animation at 60 Hz.
const FRAME_MS: f64 = 1000.0 / 60.0;
/// Quiet time before `networkIdle`.
const IDLE_MS: f64 = 500.0;
/// A page that has been quiet this long (no task, no request in flight, no
/// timer due within a second) gets one full GC (V8 low-memory
/// notification). Like V8's memory reducer, which waits for a long idle
/// period: right after load the GC only costs CPU, the page is usually
/// still being worked with, and a page that is closed soon is collected
/// with the worker's GC after close.
const IDLE_GC_MS: f64 = 2000.0;

/// Lifecycle of the current document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Life {
    pub loader: u64,
    pub committed: bool,
    pub dcl: bool,
    pub load: bool,
    pub network_idle: bool,
    pub error: Option<String>,
    pub status: u16,
    pub url: String,
}

/// Page state on its worker thread.
pub(crate) struct PageState {
    #[allow(dead_code)]
    pub id: u64,
    pub ctx: BrowserContext,
    pub window: RefCell<Option<Rc<Window>>>,
    /// Wakes the event loop.
    pub notify: Rc<Notify>,
    /// Wakes code waiting for subresource loads.
    pub loads: Rc<Notify>,
    pub life: watch::Sender<Life>,
    pub nav_seq: Cell<u64>,
    pub observer: RefCell<Option<Arc<dyn PageObserver>>>,
    /// `sessionStorage` areas of this page, by origin.
    pub session: lb_webapi::host::StoragePartition,
    pub closed: Cell<bool>,
    /// Navigation in progress (parser or network): the virtual clock
    /// does not jump while this is set.
    pub loading: Cell<bool>,
    idle_since: Cell<Option<f64>>,
    history_len: Cell<u32>,
    /// Notified after each event loop turn that ran something (CDP waits
    /// for promises on it).
    pub turn: Rc<Notify>,
    /// The CDP agent of this page, when it is driven over CDP.
    pub cdp: RefCell<Option<Rc<crate::cdp::agent::Agent>>>,
    /// The next navigation is a reload (service worker `FetchEvent`
    /// request cache mode).
    pub reloading: Cell<bool>,
}

impl PageState {
    pub(crate) fn window(&self) -> Option<Rc<Window>> {
        self.window.borrow().clone()
    }

    pub(crate) fn emit(&self, name: &str) {
        let (loader, url) = {
            let l = self.life.borrow();
            (l.loader, l.url.clone())
        };
        let obs = self.observer.borrow().clone();
        if let Some(o) = obs {
            o.lifecycle(name, loader, &url);
        }
        if let Some(a) = self.agent() {
            a.lifecycle(self, name, loader);
        }
    }

    /// The CDP agent, if the page is driven over CDP.
    pub(crate) fn agent(&self) -> Option<Rc<crate::cdp::agent::Agent>> {
        self.cdp.borrow().clone()
    }

    pub(crate) fn update_life(&self, f: impl FnOnce(&mut Life)) {
        self.life.send_modify(f);
    }

    pub(crate) fn session_area(&self, url: &Url) -> StorageArea {
        self.session.area(&url.origin().ascii_serialization())
    }

    pub(crate) fn history_len(&self) -> u32 {
        self.history_len.get()
    }

    pub(crate) fn add_history(&self, n: u32) {
        self.history_len.set(self.history_len.get() + n);
    }

    /// Tear down the current document: realm and arena go in one step.
    pub(crate) fn drop_window(&self, ws: &WorkerState) {
        // Unload events run while the window is still current, so their
        // handlers see a live document (and beacons from them go out).
        if let Some(w) = self.window() {
            let realm = w.realm();
            ws.with_engine(|engine| {
                let mut js = EngineJs { engine, realm };
                w.unload(&mut js);
                js.engine.perform_microtask_checkpoint();
            });
        }
        let old = self.window.borrow_mut().take();
        if let Some(w) = old {
            ws.with_engine(|e| lb_webapi::rt::frames::drop_children(e, &w));
            w.close();
            let realm = w.realm();
            if let Some(a) = self.agent() {
                a.drop_worlds(ws);
            }
            if let Some((rt, _)) = ws.runtime() {
                rt.detach(realm);
            }
            ws.with_engine(|e| e.drop_realm(realm));
            self.turn.notify_waiters();
        }
    }

    pub(crate) fn shutdown(&self, ws: &WorkerState) {
        self.closed.set(true);
        self.drop_window(ws);
        self.notify.notify_one();
        ws.request_gc();
    }

    /// A wake function for windows of this page.
    pub(crate) fn waker(&self) -> Rc<dyn Fn()> {
        let n = self.notify.clone();
        let l = self.loads.clone();
        Rc::new(move || {
            n.notify_one();
            l.notify_waiters();
        })
    }

    /// Console sink forwarding to the observer.
    pub(crate) fn console_sink(self: &Rc<Self>) -> Rc<dyn Fn(&ConsoleMessage)> {
        let me = Rc::downgrade(self);
        Rc::new(move |m: &ConsoleMessage| {
            if let Some(p) = me.upgrade() {
                let obs = p.observer.borrow().clone();
                if let Some(o) = obs {
                    o.console(m);
                }
                if let Some(a) = p.agent() {
                    a.console(&p, m);
                }
            }
        })
    }
}

/// The page's event loop. Runs until the page closes.
pub(crate) async fn event_loop(ws: Rc<WorkerState>, page: Rc<PageState>) {
    let mut next_frame = 0.0f64;
    let mut gc_done = false;
    let mut quiet_since: Option<f64> = None;
    // Tasks run before timers, but a page that keeps queueing tasks (an
    // IndexedDB request loop, a postMessage ping-pong) must not starve
    // its due timers: after this many tasks in a row a due timer goes
    // first (real clock only).
    const TASK_STREAK: u32 = 50;
    let mut streak = 0u32;
    loop {
        if page.closed.get() {
            return;
        }
        let Some(w) = page.window() else {
            page.notify.notified().await;
            continue;
        };
        // Every window of the page: the top-level one, then its frames.
        let wins = crate::frames::tree(&w);
        let mut did = false;
        if streak >= TASK_STREAK && !w.clock.is_virtual() {
            streak = 0;
            for x in &wins {
                let realm = x.realm();
                let ran = ws
                    .with_engine(|engine| {
                        let mut js = EngineJs { engine, realm };
                        let ran = x.run_due_timer(&mut js);
                        if ran {
                            js.engine.perform_microtask_checkpoint();
                        }
                        ran
                    })
                    .unwrap_or(false);
                if ran {
                    did = true;
                    break;
                }
            }
        }
        for x in wins.iter().filter(|_| !did) {
            if let Some(task) = x.take_task() {
                let realm = x.realm();
                ws.with_engine(|engine| {
                    let mut js = EngineJs { engine, realm };
                    task(&mut js, x);
                    js.engine.perform_microtask_checkpoint();
                });
                did = true;
                streak += 1;
                break;
            }
        }
        if !did {
            streak = 0;
            for x in &wins {
                let realm = x.realm();
                let ran = ws
                    .with_engine(|engine| {
                        let mut js = EngineJs { engine, realm };
                        let ran = x.run_due_timer(&mut js);
                        if ran {
                            js.engine.perform_microtask_checkpoint();
                        }
                        ran
                    })
                    .unwrap_or(false);
                if ran {
                    did = true;
                    break;
                }
            }
        }
        if !did && wins.iter().any(|x| x.has_animation_frames()) && w.clock.now() >= next_frame {
            let now = w.clock.now();
            next_frame = (now / FRAME_MS).floor() * FRAME_MS + FRAME_MS;
            for x in wins.iter().filter(|x| x.has_animation_frames()) {
                let realm = x.realm();
                let t = x.clock.now();
                ws.with_engine(|engine| {
                    let mut js = EngineJs { engine, realm };
                    x.run_animation_frames(&mut js, lb_webapi::rt::coarse_time(x, t));
                    js.engine.perform_microtask_checkpoint();
                });
            }
            did = true;
        }
        if let Some(nav) = w.take_nav_request() {
            let (ws2, p2) = (ws.clone(), page.clone());
            let initiator = w.url();
            tokio::task::spawn_local(async move {
                let _ = crate::nav::navigate(ws2, p2, nav.url, Some(initiator), nav.replace).await;
            });
            did = true;
        }
        for x in wins.iter().skip(1) {
            if let Some(nav) = x.take_nav_request() {
                crate::frames::navigate_from_script(&ws, &page, x, nav.url);
                did = true;
            }
        }
        let idle_deadline = track_network_idle(&page, &w);
        if did {
            gc_done = false;
            quiet_since = None;
            page.turn.notify_waiters();
            tokio::task::yield_now().await;
            continue;
        }

        // Idle: work out when something is due next (on the top-level
        // clock; frames have their own clocks, compared by delay).
        let now = w.clock.now();
        let mut deadline = w.next_timer_due();
        for x in wins.iter().skip(1) {
            if let Some(d) = x.next_timer_due() {
                let at = now + (d - x.clock.now()).max(0.0);
                deadline = Some(deadline.map_or(at, |y: f64| y.min(at)));
            }
        }
        if wins.iter().any(|x| x.has_animation_frames()) {
            deadline = Some(deadline.map_or(next_frame, |d| d.min(next_frame)));
        }
        if let Some(d) = idle_deadline {
            deadline = Some(deadline.map_or(d, |x| x.min(d)));
        }
        let quiet = !page.loading.get()
            && wins
                .iter()
                .all(|x| x.in_flight.get() == 0 && !x.has_tasks());
        let idle = quiet && deadline.is_none_or(|d| d - now > 1000.0);
        if !idle {
            quiet_since = None;
        } else if !gc_done {
            let since = *quiet_since.get_or_insert(now);
            if now - since >= IDLE_GC_MS {
                ws.with_engine(|e| e.low_memory_notification());
                gc_done = true;
            } else if !w.clock.is_virtual() {
                let at = since + IDLE_GC_MS;
                deadline = Some(deadline.map_or(at, |d| d.min(at)));
            }
        }
        if w.clock.is_virtual()
            && quiet
            && let Some(d) = deadline
        {
            let step = d - now;
            for x in &wins {
                x.clock.advance_to(x.clock.now() + step);
            }
            tokio::task::yield_now().await;
            continue;
        }
        match deadline {
            Some(d) => {
                let ms = (d - now).max(0.0);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs_f64(ms / 1000.0)) => {}
                    _ = page.notify.notified() => {}
                }
            }
            None => page.notify.notified().await,
        }
    }
}

/// Update the `networkIdle` lifecycle; returns when to check again.
fn track_network_idle(page: &PageState, w: &Window) -> Option<f64> {
    let life = page.life.borrow().clone();
    if !life.load || life.network_idle {
        page.idle_since.set(None);
        return None;
    }
    let busy = w.in_flight.get() > 0 || w.pending_loads.get() > 0 || page.loading.get();
    let now = w.clock.now();
    if busy {
        page.idle_since.set(None);
        return None;
    }
    let since = match page.idle_since.get() {
        Some(s) => s,
        None => {
            page.idle_since.set(Some(now));
            now
        }
    };
    if now - since >= IDLE_MS {
        page.update_life(|l| l.network_idle = true);
        page.emit("networkAlmostIdle");
        page.emit("networkIdle");
        None
    } else {
        Some(since + IDLE_MS)
    }
}

// ---------------------------------------------------------------------------
// Page handle
// ---------------------------------------------------------------------------

/// Runs once on the worker when a page is created, before its first
/// navigation (installs the CDP agent).
pub(crate) type PageSetup = Box<dyn FnOnce(&Rc<PageState>) + Send>;

/// What [`Page::run`] closures get: the engine and the current window.
pub struct PageHandle<'a> {
    pub engine: &'a mut Engine,
    pub window: Rc<Window>,
}

/// A page (tab). A `Send` handle; the page itself lives on its worker.
pub struct Page {
    worker: WorkerHandle,
    id: u64,
    life: watch::Receiver<Life>,
    ctx: BrowserContext,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page").field("id", &self.id).finish()
    }
}

impl Page {
    pub(crate) async fn open(
        worker: WorkerHandle,
        id: u64,
        ctx: BrowserContext,
        setup: Option<PageSetup>,
    ) -> Result<Page, PageError> {
        let page = Self::start(worker, id, ctx, setup, false).await?;
        // Start on about:blank so scripts can be evaluated right away.
        page.navigate("about:blank").await?;
        Ok(page)
    }

    /// A page with no document yet (see `Browser::new_page_to_load`): its
    /// history holds the entry of the about:blank document it skips (an
    /// about:blank window has one), which the first navigation keeps.
    pub(crate) async fn open_unstarted(
        worker: WorkerHandle,
        id: u64,
        ctx: BrowserContext,
    ) -> Result<Page, PageError> {
        Self::start(worker, id, ctx, None, true).await
    }

    async fn start(
        worker: WorkerHandle,
        id: u64,
        ctx: BrowserContext,
        setup: Option<PageSetup>,
        skip_blank: bool,
    ) -> Result<Page, PageError> {
        let (tx, rx) = oneshot::channel();
        let c2 = ctx.clone();
        worker.send(Box::new(move |ws| {
            let r = (|| {
                ws.ensure_engine(c2.persona())?;
                let (life_tx, life_rx) = watch::channel(Life::default());
                let page = Rc::new(PageState {
                    id,
                    ctx: c2,
                    window: RefCell::new(None),
                    notify: Rc::new(Notify::new()),
                    loads: Rc::new(Notify::new()),
                    life: life_tx,
                    nav_seq: Cell::new(0),
                    observer: RefCell::new(None),
                    session: lb_webapi::host::StoragePartition::new(),
                    closed: Cell::new(false),
                    loading: Cell::new(false),
                    idle_since: Cell::new(None),
                    history_len: Cell::new(u32::from(skip_blank)),
                    turn: Rc::new(Notify::new()),
                    cdp: RefCell::new(None),
                    reloading: Cell::new(false),
                });
                if let Some(setup) = setup {
                    setup(&page);
                }
                ws.pages.borrow_mut().insert(id, page.clone());
                tokio::task::spawn_local(event_loop(ws.clone(), page));
                Ok(life_rx)
            })();
            let _ = tx.send(r);
        }))?;
        let life = rx.await.map_err(|_| PageError::Closed)??;
        Ok(Page {
            worker,
            id,
            life,
            ctx,
        })
    }

    /// The page id (unique in its browser).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Run `f` on the page's worker with its state; `f` returns a local
    /// future.
    pub(crate) async fn call<R, F, Fut>(&self, f: F) -> Result<R, PageError>
    where
        R: Send + 'static,
        F: FnOnce(Rc<WorkerState>, Rc<PageState>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<R, PageError>> + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let id = self.id;
        self.worker.send(Box::new(move |ws| {
            let page = ws.pages.borrow().get(&id).cloned();
            let Some(page) = page else {
                let _ = tx.send(Err(PageError::Closed));
                return;
            };
            let fut = f(ws.clone(), page);
            tokio::task::spawn_local(async move {
                let _ = tx.send(fut.await);
            });
        }))?;
        rx.await.map_err(|_| PageError::Closed)?
    }

    /// Run `f` on the page's worker and block this thread until it
    /// returns (at most `timeout`). For callers that cannot await (sync
    /// CDP methods). `f` must not wait for the calling thread.
    pub(crate) fn call_blocking<R: Send + 'static>(
        &self,
        timeout: Duration,
        f: impl FnOnce(&Rc<WorkerState>, &Rc<PageState>) -> R + Send + 'static,
    ) -> Result<R, PageError> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let id = self.id;
        self.worker.send(Box::new(move |ws| {
            let page = ws.pages.borrow().get(&id).cloned();
            let r = match page {
                Some(p) => Ok(f(ws, &p)),
                None => Err(PageError::Closed),
            };
            let _ = tx.send(r);
        }))?;
        rx.recv_timeout(timeout).map_err(|e| match e {
            std::sync::mpsc::RecvTimeoutError::Timeout => PageError::Timeout,
            std::sync::mpsc::RecvTimeoutError::Disconnected => PageError::Closed,
        })?
    }

    /// Queue `f` on the page's worker without waiting. Jobs run in the
    /// order they were posted.
    pub(crate) fn post(
        &self,
        f: impl FnOnce(&Rc<WorkerState>, &Rc<PageState>) + Send + 'static,
    ) -> Result<(), PageError> {
        let id = self.id;
        self.worker.send(Box::new(move |ws| {
            let page = ws.pages.borrow().get(&id).cloned();
            if let Some(p) = page {
                f(ws, &p);
            }
        }))
    }

    /// The context this page belongs to.
    pub fn context(&self) -> &BrowserContext {
        &self.ctx
    }

    /// Navigate; resolves once the response is committed (the new
    /// document exists). Use [`Page::wait_for`] or [`Page::goto`] to wait
    /// for later lifecycle points.
    pub async fn navigate(&self, url: &str) -> Result<NavResult, PageError> {
        let url =
            Url::parse(url).map_err(|e| PageError::Net(format!("invalid url {url:?}: {e}")))?;
        let (tx, rx) = oneshot::channel();
        self.call(move |ws, page| async move {
            tokio::task::spawn_local(async move {
                let r = crate::nav::navigate_committed(ws, page, url, None, false, tx).await;
                let _ = r;
            });
            Ok(())
        })
        .await?;
        rx.await.map_err(|_| PageError::Aborted)?
    }

    /// Reload the current document (a navigation to its URL marked as a
    /// reload); resolves at commit like [`Page::navigate`].
    pub async fn reload(&self) -> Result<NavResult, PageError> {
        let (tx, rx) = oneshot::channel();
        self.call(move |ws, page| async move {
            let url = page.window().map(|w| w.url()).ok_or(PageError::Closed)?;
            page.reloading.set(true);
            tokio::task::spawn_local(async move {
                let r = crate::nav::navigate_committed(ws, page, url, None, false, tx).await;
                let _ = r;
            });
            Ok(())
        })
        .await?;
        rx.await.map_err(|_| PageError::Aborted)?
    }

    /// Navigate and wait for `until` (with a timeout).
    pub async fn goto(
        &self,
        url: &str,
        until: WaitUntil,
        timeout: Duration,
    ) -> Result<NavResult, PageError> {
        let r = self.navigate(url).await?;
        self.wait_for(until, timeout).await?;
        Ok(r)
    }

    /// Wait for a lifecycle point of the current document.
    pub async fn wait_for(&self, until: WaitUntil, timeout: Duration) -> Result<(), PageError> {
        let mut rx = self.life.clone();
        let fut = async move {
            let need = |l: &Life| {
                l.error.is_some()
                    || match until {
                        WaitUntil::Commit => l.committed,
                        WaitUntil::DomContentLoaded => l.dcl,
                        WaitUntil::Load | WaitUntil::Time(_) => l.load,
                        WaitUntil::NetworkIdle => l.network_idle,
                    }
            };
            let l = rx
                .wait_for(need)
                .await
                .map_err(|_| PageError::Closed)?
                .clone();
            if let Some(e) = l.error {
                return Err(PageError::Net(e));
            }
            if let WaitUntil::Time(d) = until {
                tokio::time::sleep(d).await;
            }
            Ok(())
        };
        tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| PageError::Timeout)?
    }

    /// Run a classic script in the main world; the completion value is
    /// copied out (objects as JSON).
    pub async fn evaluate(&self, source: &str) -> Result<JsValue, PageError> {
        let source = source.to_owned();
        self.run(move |h| {
            let realm = h.window.realm();
            let url = h.window.url().to_string();
            let r = h.engine.evaluate(realm, &source, ScriptOrigin::new(&url));
            h.engine.perform_microtask_checkpoint();
            let mut js = EngineJs {
                engine: h.engine,
                realm,
            };
            h.window.after_mutation(&mut js);
            r.map_err(|e| PageError::Js(e.to_string()))
        })
        .await?
    }

    /// Run `f` on the worker with the engine and the current window
    /// (CDP layer, tests).
    pub async fn run<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut PageHandle<'_>) -> R + Send + 'static,
    ) -> Result<R, PageError> {
        self.call(move |ws, page| async move {
            let w = page.window().ok_or(PageError::Closed)?;
            ws.with_engine(|engine| {
                let mut h = PageHandle { engine, window: w };
                f(&mut h)
            })
            .ok_or(PageError::Closed)
        })
        .await
    }

    /// The serialized DOM (`document.documentElement.outerHTML` plus the
    /// doctype).
    pub async fn content(&self) -> Result<String, PageError> {
        self.run(|h| h.window.content()).await
    }

    /// Text of the page body (`innerText`-like).
    pub async fn text(&self) -> Result<String, PageError> {
        self.run(|h| h.window.text()).await
    }

    /// Current URL.
    pub async fn url(&self) -> Result<String, PageError> {
        self.run(|h| h.window.url().to_string()).await
    }

    /// Console messages of the current document.
    pub async fn console_messages(&self) -> Result<Vec<ConsoleMessage>, PageError> {
        self.run(|h| h.window.console_messages()).await
    }

    /// Receive lifecycle and console events.
    pub async fn set_observer(&self, obs: Option<Arc<dyn PageObserver>>) -> Result<(), PageError> {
        self.call(move |_ws, page| async move {
            *page.observer.borrow_mut() = obs;
            Ok(())
        })
        .await
    }

    /// Close the page without waiting (CDP `Target.closeTarget`).
    pub(crate) fn close_now(&self) -> Result<(), PageError> {
        let id = self.id;
        self.post(move |ws, page| {
            page.shutdown(ws);
            if ws.pages.borrow_mut().remove(&id).is_some() {
                ws.page_count.fetch_sub(1, Ordering::Relaxed);
            }
        })
    }

    /// Close the page: its document and realm are dropped.
    pub async fn close(self) -> Result<(), PageError> {
        let id = self.id;
        self.call(move |ws, page| async move {
            page.shutdown(&ws);
            ws.pages.borrow_mut().remove(&id);
            ws.page_count.fetch_sub(1, Ordering::Relaxed);
            Ok(())
        })
        .await
    }
}

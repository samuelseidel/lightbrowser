//! Worker threads (ADR 0021): each dedicated or shared worker runs on its
//! own OS thread with a `current_thread` tokio runtime, its own V8 engine
//! (isolate), bindings for its global scope (`Bindings::define_worker`)
//! and one worker-scope window ([`lb_webapi::rt::Window`] without a
//! document). The thread exists only while the worker does: pages that
//! make no worker start nothing.
//!
//! Requests go through the creating context's network stack and cookie
//! jar ([`BrowserContext::fetch`]); `importScripts` blocks the worker
//! thread on a helper thread's runtime. Shared workers are kept per
//! context by (script URL, name) and end with the context.

use std::rc::Rc;
use std::time::Duration;

use lb_js::{Engine, EngineOptions, JsErrorKind, ScriptOrigin};
use lb_webapi::host::{Destination, NetRequest, NetResponse, StorageArea};
use lb_webapi::rt::{
    EngineJs, RemoteTask, Runtime, ScopeInit, Window, WindowInit, WorkerHost, WorkerKind,
    WorkerStart, worker,
};
use lb_webapi::{Bindings, Registry};
use tokio::sync::Notify;
use url::Url;

use crate::context::BrowserContext;

/// JS heap cap of a worker's engine.
const WORKER_HEAP: usize = 512 << 20;
/// One frame at 60 Hz (`requestAnimationFrame` in dedicated workers).
const FRAME_MS: f64 = 1000.0 / 60.0;

/// The [`WorkerHost`] of a context's windows and worker scopes.
pub(crate) struct Workers {
    ctx: BrowserContext,
}

impl Workers {
    pub(crate) fn host(ctx: BrowserContext) -> Rc<dyn WorkerHost> {
        Rc::new(Workers { ctx })
    }
}

impl WorkerHost for Workers {
    fn start(&self, start: WorkerStart) {
        spawn(self.ctx.clone(), start);
    }

    fn connect_shared(&self, start: WorkerStart, connect: RemoteTask) {
        let key = (start.url.to_string(), start.name.clone());
        let mut reg = self
            .ctx
            .shared_workers()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        reg.retain(|_, (inbox, c)| !inbox.is_closed() && !c.is_done());
        if let Some((inbox, _)) = reg.get(&key) {
            inbox.send(connect);
            return;
        }
        start.inbox.send(connect);
        reg.insert(key, (start.inbox.clone(), start.control.clone()));
        drop(reg);
        spawn(self.ctx.clone(), start);
    }

    fn fetch_sync(&self, req: NetRequest) -> Result<NetResponse, String> {
        let ctx = self.ctx.clone();
        // The worker thread already runs a runtime; block on another
        // thread's.
        std::thread::scope(|s| {
            s.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                rt.block_on(ctx.fetch(req))
            })
            .join()
            .unwrap_or_else(|_| Err("fetch thread failed".into()))
        })
    }
}

/// The network host of a worker scope: the context's stack on the worker
/// thread.
struct WorkerNet {
    ctx: BrowserContext,
}

impl lb_webapi::host::NetHost for WorkerNet {
    fn fetch(&self, req: NetRequest) -> lb_webapi::host::LocalFuture<Result<NetResponse, String>> {
        let ctx = self.ctx.clone();
        Box::pin(async move { ctx.fetch(req).await })
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

/// Start the thread of one worker.
fn spawn(ctx: BrowserContext, start: WorkerStart) {
    let name = match start.kind {
        WorkerKind::Dedicated => "lb-dedicated-worker",
        WorkerKind::Shared => "lb-shared-worker",
    };
    let r = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || thread_main(ctx, start));
    if let Err(e) = r {
        tracing::warn!("worker thread: {e}");
    }
}

fn thread_main(ctx: BrowserContext, start: WorkerStart) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::warn!("worker runtime: {e}");
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        if let Err(e) = run(ctx, start).await {
            tracing::debug!("worker: {e}");
        }
    });
}

/// Load the script, build the scope, run the script, then the event loop
/// until the worker closes or is terminated.
async fn run(ctx: BrowserContext, start: WorkerStart) -> Result<(), String> {
    let WorkerStart {
        kind,
        url,
        source,
        module,
        name,
        origin,
        creator,
        persona,
        profile,
        virtual_time,
        inbox,
        inbox_rx,
        control,
        parent,
    } = start;
    if control.is_terminated() {
        return Ok(());
    }
    // The script (Chrome fires a plain `error` event at the `Worker` when
    // it cannot be loaded).
    let (script_url, text) = match source {
        Some(s) => (url.clone(), s),
        None => {
            let mut req = NetRequest::get(url.clone(), Destination::Script, Some(creator.clone()));
            req.mode = if module { "cors" } else { "same-origin" };
            match ctx.fetch(req).await {
                Ok(r) if (200..300).contains(&r.status) => {
                    let t = lb_webapi::rt::script::script_source(&r.body, r.header("content-type"));
                    (r.url, t)
                }
                _ => {
                    if let Some((p, id)) = &parent {
                        p.send(worker::load_failed_task(*id));
                    }
                    return Ok(());
                }
            }
        }
    };
    if control.is_terminated() {
        return Ok(());
    }

    let mut engine = Engine::new(
        persona.clone(),
        EngineOptions {
            max_heap_bytes: Some(WORKER_HEAP),
        },
    )
    .map_err(|e| e.to_string())?;
    let mut reg = Registry::new();
    let rt = Runtime::new(&mut reg);
    let bindings =
        Bindings::define_worker(&mut engine, &reg, kind.global()).map_err(|e| e.to_string())?;
    rt.set_bindings(bindings.clone());
    let notify = Rc::new(Notify::new());
    let wake: Rc<dyn Fn()> = {
        let n = notify.clone();
        Rc::new(move || n.notify_one())
    };
    let w = Window::new(
        &rt,
        WindowInit {
            url: script_url.clone(),
            persona,
            profile,
            net: Rc::new(WorkerNet { ctx: ctx.clone() }),
            local_storage: StorageArea::new(),
            session_storage: StorageArea::new(),
            virtual_time,
            wake,
            referrer: creator.to_string(),
            content_type: "text/javascript".into(),
            charset: "UTF-8",
            history_len: 0,
        },
    );
    w.set_worker_scope(ScopeInit {
        kind,
        name,
        module,
        origin,
        control: control.clone(),
        parent,
    });
    w.set_worker_host(Some(Workers::host(ctx.clone())));
    let realm = w
        .create_worker_realm(&mut engine, &bindings)
        .map_err(|e| e.to_string())?;
    control.set_isolate(engine.terminate_handle());
    w.attach_inbox(inbox, inbox_rx);

    // Run the script (module graph: static imports first).
    let result = if module {
        prefetch_modules(&ctx, &w, &script_url, &text).await;
        engine
            .evaluate_module(realm, &text, ScriptOrigin::new(script_url.as_str()))
            .map(|h| engine.release(h))
    } else {
        engine
            .evaluate(realm, &text, ScriptOrigin::new(script_url.as_str()))
            .map(|_| ())
    };
    if let Err(e) = result
        && e.kind != JsErrorKind::Host
        && e.kind != JsErrorKind::Terminated
    {
        let mut js = EngineJs {
            engine: &mut engine,
            realm,
        };
        w.report_exception(&mut js, &e);
    }
    engine.perform_microtask_checkpoint();

    event_loop(&mut engine, &w, &notify).await;

    w.close();
    rt.detach(realm);
    engine.drop_realm(realm);
    Ok(())
}

/// Fetch a module graph's static imports into the scope's module map.
async fn prefetch_modules(ctx: &BrowserContext, w: &Rc<Window>, url: &Url, source: &str) {
    let mut queue: Vec<(Url, String)> = vec![(url.clone(), source.to_owned())];
    let mut fetched = 0;
    while let Some((base, src)) = queue.pop() {
        for spec in crate::nav::import_specifiers(&src) {
            let Ok(dep) = base.join(&spec) else { continue };
            if w.module_map.borrow().contains_key(dep.as_str()) || fetched > 256 {
                continue;
            }
            fetched += 1;
            if dep.scheme() == "blob" || dep.scheme() == "data" {
                continue;
            }
            let mut req = NetRequest::get(dep.clone(), Destination::Script, Some(w.url()));
            req.mode = "cors";
            if let Ok(r) = ctx.fetch(req).await
                && (200..300).contains(&r.status)
            {
                let s = lb_webapi::rt::script::script_source(&r.body, r.header("content-type"));
                w.module_map.borrow_mut().insert(dep.to_string(), s.clone());
                queue.push((dep, s));
            }
        }
    }
}

/// The worker's event loop: tasks, due timers, animation frames (60 Hz),
/// a microtask checkpoint after each. Ends when the worker is closed or
/// terminated.
async fn event_loop(engine: &mut Engine, w: &Rc<Window>, notify: &Notify) {
    let realm = w.realm();
    let Some(control) = w.worker_control() else {
        return;
    };
    let mut next_frame = 0.0f64;
    loop {
        if control.is_done() {
            return;
        }
        if let Some(task) = w.take_task() {
            let mut js = EngineJs {
                engine: &mut *engine,
                realm,
            };
            task(&mut js, w);
            js.engine.perform_microtask_checkpoint();
            tokio::task::yield_now().await;
            continue;
        }
        let ran = {
            let mut js = EngineJs {
                engine: &mut *engine,
                realm,
            };
            let ran = w.run_due_timer(&mut js);
            if ran {
                js.engine.perform_microtask_checkpoint();
            }
            ran
        };
        if ran {
            tokio::task::yield_now().await;
            continue;
        }
        let now = w.clock.now();
        if w.has_animation_frames() && now >= next_frame {
            next_frame = (now / FRAME_MS).floor() * FRAME_MS + FRAME_MS;
            let mut js = EngineJs {
                engine: &mut *engine,
                realm,
            };
            w.run_animation_frames(&mut js, lb_webapi::rt::coarse_time(w, now));
            js.engine.perform_microtask_checkpoint();
            tokio::task::yield_now().await;
            continue;
        }
        let mut deadline = w.next_timer_due();
        if w.has_animation_frames() {
            deadline = Some(deadline.map_or(next_frame, |d| d.min(next_frame)));
        }
        if w.clock.is_virtual()
            && w.in_flight.get() == 0
            && !w.has_tasks()
            && let Some(d) = deadline
        {
            w.clock.advance_to(d);
            tokio::task::yield_now().await;
            continue;
        }
        match deadline {
            Some(d) => {
                let ms = (d - now).max(0.0);
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs_f64(ms / 1000.0)) => {}
                    _ = notify.notified() => {}
                }
            }
            None => notify.notified().await,
        }
    }
}

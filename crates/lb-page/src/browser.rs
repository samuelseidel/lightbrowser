//! The browser process: worker threads, each with a `current_thread`
//! tokio runtime, a `LocalSet` and one JS engine. Threads start on demand,
//! up to [`BrowserOptions::workers`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use lb_js::{Engine, EngineOptions};
use lb_persona::Persona;
use lb_webapi::rt::Runtime;
use lb_webapi::{Bindings, Registry};
use tokio::sync::mpsc;

use crate::context::{BrowserContext, PageError};
use crate::page::{Page, PageState};

/// Browser-wide options.
#[derive(Debug, Clone)]
pub struct BrowserOptions {
    /// Most worker threads (default: available cores). A thread starts when
    /// a page needs one: the first page, then a page opened while every
    /// running worker has a page.
    pub workers: usize,
    /// JS heap cap per engine (worker).
    pub max_heap_bytes: Option<usize>,
    /// Restore engines from the process-wide V8 startup snapshot
    /// (`lb_webapi::snapshot`, ADR 0013), made in memory when the first
    /// engine starts (about 90 ms once). Off by default: with interface
    /// objects made lazily (ADR 0016) a window realm built from templates
    /// costs about what a deserialized one does, and the snapshot's blob,
    /// its creation and the template set it holds cost ~3 MB of resident
    /// memory in `serve`.
    pub startup_snapshot: bool,
}

impl Default for BrowserOptions {
    fn default() -> Self {
        BrowserOptions {
            workers: std::thread::available_parallelism().map_or(1, |n| n.get()),
            max_heap_bytes: Some(512 << 20),
            startup_snapshot: false,
        }
    }
}

pub(crate) type Job = Box<dyn FnOnce(&Rc<WorkerState>) + Send>;

/// Sends jobs to one worker thread.
#[derive(Clone)]
pub(crate) struct WorkerHandle {
    tx: mpsc::UnboundedSender<Job>,
    pages: Arc<AtomicUsize>,
}

impl WorkerHandle {
    pub(crate) fn send(&self, job: Job) -> Result<(), PageError> {
        self.tx.send(job).map_err(|_| PageError::Closed)
    }
}

/// Per-thread state: the engine, its bindings and the pages on it.
pub(crate) struct WorkerState {
    engine: RefCell<Option<Engine>>,
    runtime: RefCell<Option<(Runtime, Bindings)>>,
    pub(crate) pages: RefCell<HashMap<u64, Rc<PageState>>>,
    max_heap: Option<usize>,
    startup_snapshot: bool,
    /// Page count of this worker (load balancing).
    pub(crate) page_count: Arc<AtomicUsize>,
    stop: std::cell::Cell<bool>,
    /// A page closed: collect garbage once the job queue is empty.
    gc_pending: std::cell::Cell<bool>,
}

impl WorkerState {
    /// Create the engine and define the bindings on first use.
    pub(crate) fn ensure_engine(&self, persona: &Arc<Persona>) -> Result<(), PageError> {
        if self.engine.borrow().is_some() {
            return Ok(());
        }
        let options = EngineOptions {
            max_heap_bytes: self.max_heap,
        };
        let snapshot = if self.startup_snapshot {
            lb_webapi::snapshot::startup_snapshot(persona.family)
                .inspect_err(|e| tracing::warn!("no startup snapshot: {e}"))
                .ok()
        } else {
            None
        };
        let mut engine = match snapshot {
            Some(blob) => Engine::with_snapshot(persona.clone(), options, blob),
            None => Engine::new(persona.clone(), options),
        }
        .map_err(|e| PageError::Setup(e.to_string()))?;
        let mut reg = Registry::new();
        let rt = Runtime::new(&mut reg);
        let bindings =
            Bindings::define(&mut engine, &reg).map_err(|e| PageError::Setup(e.to_string()))?;
        rt.set_bindings(bindings.clone());
        rt.install_dynamic_import(&mut engine);
        if snapshot.is_some() {
            tracing::debug!("engine snapshot: {:?}", engine.snapshot_status());
        }
        *self.engine.borrow_mut() = Some(engine);
        *self.runtime.borrow_mut() = Some((rt, bindings));
        Ok(())
    }

    /// Run `f` with the engine. Never call JS-running host code that
    /// re-enters this (host functions get a `CallCtx`, not the engine).
    pub(crate) fn with_engine<R>(&self, f: impl FnOnce(&mut Engine) -> R) -> Option<R> {
        let mut e = self.engine.borrow_mut();
        e.as_mut().map(f)
    }

    pub(crate) fn runtime(&self) -> Option<(Runtime, Bindings)> {
        self.runtime.borrow().clone()
    }

    /// Ask for one full GC (V8 low-memory notification) when this worker
    /// next has nothing queued, e.g. after a page closed: dropped realms
    /// are otherwise kept until the next allocation pressure.
    pub(crate) fn request_gc(&self) {
        self.gc_pending.set(true);
    }
}

/// The browser: worker threads that host pages.
pub struct Browser {
    pool: Mutex<Pool>,
    opts: BrowserOptions,
    next_page: Arc<AtomicU64>,
}

/// The worker threads started so far.
#[derive(Default)]
struct Pool {
    workers: Vec<WorkerHandle>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("workers", &self.started_workers())
            .field("max_workers", &self.opts.workers)
            .finish()
    }
}

impl Browser {
    /// A browser with no worker threads yet: they start as pages need
    /// them (see [`BrowserOptions::workers`]). An idle browser, such as
    /// `serve` before its first page, then holds no runtimes, engines or
    /// thread stacks of its own.
    pub fn new(mut opts: BrowserOptions) -> Result<Browser, PageError> {
        opts.workers = opts.workers.max(1);
        Ok(Browser {
            pool: Mutex::new(Pool::default()),
            opts,
            next_page: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Worker threads started so far.
    pub fn started_workers(&self) -> usize {
        self.pool.lock().map_or(0, |p| p.workers.len())
    }

    /// Start worker thread `i`.
    fn spawn_worker(&self, i: usize) -> Result<(WorkerHandle, JoinHandle<()>), PageError> {
        let (tx, rx) = mpsc::unbounded_channel::<Job>();
        let handle = WorkerHandle {
            tx,
            pages: Arc::new(AtomicUsize::new(0)),
        };
        let count = handle.pages.clone();
        let max_heap = self.opts.max_heap_bytes;
        let snapshot = self.opts.startup_snapshot;
        let t = std::thread::Builder::new()
            .name(format!("lb-worker-{i}"))
            .spawn(move || worker_main(rx, count, max_heap, snapshot))
            .map_err(|e| PageError::Setup(e.to_string()))?;
        Ok((handle, t))
    }

    /// Open a page in `ctx`, pinned to the least loaded worker.
    pub async fn new_page(&self, ctx: &BrowserContext) -> Result<Page, PageError> {
        self.new_page_with(ctx, None).await
    }

    /// A page to navigate at once, for one-shot loads (`lightbrowser
    /// fetch`): it skips the initial about:blank document, whose realm
    /// nothing could use, but keeps its history entry, so the loaded
    /// document sees the same `history.length` as after
    /// [`Browser::new_page`]. Navigate it before anything else.
    pub async fn new_page_to_load(&self, ctx: &BrowserContext) -> Result<Page, PageError> {
        let worker = self.pick_worker()?;
        let id = self.next_page.fetch_add(1, Ordering::Relaxed);
        Page::open_unstarted(worker, id, ctx.clone()).await
    }

    /// The least loaded worker (the first of equals). A new thread starts
    /// only when every running worker has a page and fewer than the
    /// maximum run, so pages land where they would if all threads had
    /// started at once.
    fn pick_worker(&self) -> Result<WorkerHandle, PageError> {
        let mut pool = self.pool.lock().map_err(|_| PageError::Closed)?;
        let least = pool
            .workers
            .iter()
            .min_by_key(|w| w.pages.load(Ordering::Relaxed))
            .filter(|w| {
                w.pages.load(Ordering::Relaxed) == 0 || pool.workers.len() >= self.opts.workers
            })
            .cloned();
        let worker = match least {
            Some(w) => w,
            None => {
                let (w, t) = self.spawn_worker(pool.workers.len())?;
                pool.workers.push(w.clone());
                pool.threads.push(t);
                w
            }
        };
        worker.pages.fetch_add(1, Ordering::Relaxed);
        Ok(worker)
    }

    /// Like [`Browser::new_page`], running `setup` on the worker before the
    /// first navigation.
    pub(crate) async fn new_page_with(
        &self,
        ctx: &BrowserContext,
        setup: Option<crate::page::PageSetup>,
    ) -> Result<Page, PageError> {
        let worker = self.pick_worker()?;
        let id = self.next_page.fetch_add(1, Ordering::Relaxed);
        Page::open(worker, id, ctx.clone(), setup).await
    }

    /// Stop the workers; their pages are closed.
    pub fn shutdown(self) {
        drop(self);
    }

    fn stop(&mut self) {
        let pool = std::mem::take(self.pool.get_mut().unwrap_or_else(|e| e.into_inner()));
        for w in pool.workers {
            let _ = w.send(Box::new(|ws| ws.stop.set(true)));
        }
        for t in pool.threads {
            let _ = t.join();
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker_main(
    mut rx: mpsc::UnboundedReceiver<Job>,
    page_count: Arc<AtomicUsize>,
    max_heap: Option<usize>,
    startup_snapshot: bool,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("worker runtime: {e}");
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let state = Rc::new(WorkerState {
            engine: RefCell::new(None),
            runtime: RefCell::new(None),
            pages: RefCell::new(HashMap::new()),
            max_heap,
            startup_snapshot,
            page_count,
            stop: std::cell::Cell::new(false),
            gc_pending: std::cell::Cell::new(false),
        });
        while let Some(job) = rx.recv().await {
            job(&state);
            if state.stop.get() {
                break;
            }
            if state.gc_pending.get() && rx.is_empty() {
                // Let the closed pages' tasks finish unwinding first.
                tokio::task::yield_now().await;
                state.gc_pending.set(false);
                state.with_engine(|e| e.reduce_memory());
            }
        }
        // Close every page before the engine goes.
        let pages: Vec<Rc<PageState>> = state.pages.borrow_mut().drain().map(|(_, p)| p).collect();
        for p in pages {
            p.shutdown(&state);
        }
        state.runtime.borrow_mut().take();
        state.engine.borrow_mut().take();
    });
}

//! The browser process: worker threads, each with a `current_thread`
//! tokio runtime, a `LocalSet` and one JS engine.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    /// Worker threads (default: available cores).
    pub workers: usize,
    /// JS heap cap per engine (worker).
    pub max_heap_bytes: Option<usize>,
}

impl Default for BrowserOptions {
    fn default() -> Self {
        BrowserOptions {
            workers: std::thread::available_parallelism().map_or(1, |n| n.get()),
            max_heap_bytes: Some(512 << 20),
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
    /// Page count of this worker (load balancing).
    pub(crate) page_count: Arc<AtomicUsize>,
    stop: std::cell::Cell<bool>,
}

impl WorkerState {
    /// Create the engine and define the bindings on first use.
    pub(crate) fn ensure_engine(&self, persona: &Arc<Persona>) -> Result<(), PageError> {
        if self.engine.borrow().is_some() {
            return Ok(());
        }
        let mut engine = Engine::new(
            persona.clone(),
            EngineOptions {
                max_heap_bytes: self.max_heap,
            },
        )
        .map_err(|e| PageError::Setup(e.to_string()))?;
        let mut reg = Registry::new();
        let rt = Runtime::new(&mut reg);
        let bindings =
            Bindings::define(&mut engine, &reg).map_err(|e| PageError::Setup(e.to_string()))?;
        rt.set_bindings(bindings.clone());
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
}

/// The browser: worker threads that host pages.
pub struct Browser {
    workers: Vec<WorkerHandle>,
    threads: Vec<JoinHandle<()>>,
    next_page: Arc<AtomicU64>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("workers", &self.workers.len())
            .finish()
    }
}

impl Browser {
    /// Start the worker threads.
    pub fn new(opts: BrowserOptions) -> Result<Browser, PageError> {
        let n = opts.workers.max(1);
        let mut workers = Vec::with_capacity(n);
        let mut threads = Vec::with_capacity(n);
        for i in 0..n {
            let (tx, rx) = mpsc::unbounded_channel::<Job>();
            let handle = WorkerHandle {
                tx,
                pages: Arc::new(AtomicUsize::new(0)),
            };
            let count = handle.pages.clone();
            let max_heap = opts.max_heap_bytes;
            let t = std::thread::Builder::new()
                .name(format!("lb-worker-{i}"))
                .spawn(move || worker_main(rx, count, max_heap))
                .map_err(|e| PageError::Setup(e.to_string()))?;
            workers.push(handle);
            threads.push(t);
        }
        Ok(Browser {
            workers,
            threads,
            next_page: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Open a page in `ctx`, pinned to the least loaded worker.
    pub async fn new_page(&self, ctx: &BrowserContext) -> Result<Page, PageError> {
        self.new_page_with(ctx, None).await
    }

    /// Like [`Browser::new_page`], running `setup` on the worker before the
    /// first navigation.
    pub(crate) async fn new_page_with(
        &self,
        ctx: &BrowserContext,
        setup: Option<crate::page::PageSetup>,
    ) -> Result<Page, PageError> {
        let worker = self
            .workers
            .iter()
            .min_by_key(|w| w.pages.load(Ordering::Relaxed))
            .ok_or(PageError::Closed)?
            .clone();
        let id = self.next_page.fetch_add(1, Ordering::Relaxed);
        worker.pages.fetch_add(1, Ordering::Relaxed);
        Page::open(worker, id, ctx.clone(), setup).await
    }

    /// Stop the workers; their pages are closed.
    pub fn shutdown(self) {
        drop(self);
    }

    fn stop(&mut self) {
        for w in self.workers.drain(..) {
            let _ = w.send(Box::new(|ws| ws.stop.set(true)));
        }
        for t in self.threads.drain(..) {
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
            page_count,
            stop: std::cell::Cell::new(false),
        });
        while let Some(job) = rx.recv().await {
            job(&state);
            if state.stop.get() {
                break;
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

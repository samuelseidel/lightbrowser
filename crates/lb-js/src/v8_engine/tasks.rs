//! V8's foreground tasks, run by the embedder (ADR 0038).
//!
//! V8 posts work for an isolate's own thread to the platform: the steps
//! and finalization of its GCs, `FinalizationRegistry` cleanups, the
//! memory reducer, compile finalization. V8's default platform queues them
//! until the embedder pumps its message loop, which nothing did, so none
//! ran: a page's `FinalizationRegistry` callbacks never fired (Chrome runs
//! them after the GC that collected the targets), and V8 never finished a
//! GC it started on its own.
//!
//! The platform is a custom one ([`Platform`]): every task is filed under
//! its isolate and the isolate's waker is called (from whatever thread
//! posted it). The thread that owns the isolate then runs the due tasks
//! between its own tasks ([`super::Engine::run_foreground_tasks`]).

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// Called, from any thread, when V8 posted a task for an isolate: its
/// owner should then call [`super::Engine::run_foreground_tasks`].
pub type TaskWaker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Queue {
    ready: VecDeque<v8::Task>,
    /// Delayed tasks with their due time, in posting order (few at a time).
    delayed: Vec<(Instant, v8::Task)>,
    waker: Option<TaskWaker>,
}

/// Queues per isolate (keyed by the isolate's address). Not fingerprint
/// data: only work V8 handed over, routed to the thread that owns it.
fn queues() -> MutexGuard<'static, HashMap<usize, Queue>> {
    static Q: OnceLock<Mutex<HashMap<usize, Queue>>> = OnceLock::new();
    Q.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The platform's foreground side (`v8::new_custom_platform`); background
/// tasks still go to V8's own worker pool.
pub(crate) struct Platform;

impl v8::PlatformImpl for Platform {
    fn post_task(&self, isolate: *mut c_void, task: v8::Task) {
        post(isolate, task, None);
    }
    fn post_non_nestable_task(&self, isolate: *mut c_void, task: v8::Task) {
        // Tasks only run from the owner's loop, never nested in another.
        post(isolate, task, None);
    }
    fn post_delayed_task(&self, isolate: *mut c_void, task: v8::Task, delay: f64) {
        post(isolate, task, Some(delay));
    }
    fn post_non_nestable_delayed_task(&self, isolate: *mut c_void, task: v8::Task, delay: f64) {
        post(isolate, task, Some(delay));
    }
    /// Idle tasks are off (`idle_task_support` is false), so V8 posts none.
    fn post_idle_task(&self, _isolate: *mut c_void, task: v8::IdleTask) {
        drop(task);
    }
}

fn post(isolate: *mut c_void, task: v8::Task, delay: Option<f64>) {
    let waker = {
        let mut q = queues();
        let e = q.entry(isolate as usize).or_default();
        match delay {
            None => e.ready.push_back(task),
            Some(d) => {
                let at = Instant::now() + Duration::from_secs_f64(d.max(0.0));
                e.delayed.push((at, task));
            }
        }
        e.waker.clone()
    };
    // Outside the lock: a waker may take other locks.
    if let Some(w) = waker {
        w();
    }
}

/// Set an isolate's waker; wakes at once if tasks are already waiting.
pub(crate) fn set_waker(key: usize, waker: TaskWaker) {
    let pending = {
        let mut q = queues();
        let e = q.entry(key).or_default();
        e.waker = Some(waker.clone());
        !e.ready.is_empty() || !e.delayed.is_empty()
    };
    if pending {
        waker();
    }
}

/// The next ready task, else the earliest delayed task due by `now`.
pub(crate) fn take_due(key: usize, now: Instant) -> Option<v8::Task> {
    let mut q = queues();
    let e = q.get_mut(&key)?;
    if let Some(t) = e.ready.pop_front() {
        return Some(t);
    }
    let i = e
        .delayed
        .iter()
        .enumerate()
        .filter(|(_, (at, _))| *at <= now)
        .min_by_key(|(_, (at, _))| *at)
        .map(|(i, _)| i)?;
    Some(e.delayed.remove(i).1)
}

/// When the isolate's next task is due: now if one is ready.
pub(crate) fn next_due(key: usize) -> Option<Instant> {
    let q = queues();
    let e = q.get(&key)?;
    if !e.ready.is_empty() {
        return Some(Instant::now());
    }
    e.delayed.iter().map(|(at, _)| *at).min()
}

/// Forget an isolate's tasks. Called before the isolate is disposed: a
/// pending cancelable task tells its isolate's task manager it is gone
/// when it is deleted.
pub(crate) fn remove(key: usize) {
    let q = queues().remove(&key);
    // Dropped outside the lock.
    drop(q);
}

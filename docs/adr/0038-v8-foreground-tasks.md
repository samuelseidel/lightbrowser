# ADR 0038: V8's foreground tasks run on the isolate's own thread

Status: Accepted (2026-11-07).

## Context

V8 hands work for an isolate's own thread to the platform as foreground
tasks: the steps and finalization of GCs it starts itself (incremental
marking, sweeping, the memory reducer), `FinalizationRegistry` cleanups,
compile finalization. V8's default platform keeps them in a queue until
the embedder pumps the isolate's message loop. Nothing in LightBrowser
did, so none ran:

- A page's `FinalizationRegistry` callbacks never fired, even after full
  GCs (0 of 2000 registered objects on rtx-dev). Chrome runs them in the
  task after the GC that collected the targets (2000 of 2000 within a
  second after `gc()` in Chrome for Testing 1243).
- V8 could never finish a GC it scheduled itself, so a worker that skipped
  the GC after a closed page grew to 69 MB of private memory in a crawl.

## Decision

- `lb-js` creates V8's platform with `v8::new_custom_platform`. Its
  foreground side (`v8_engine/tasks.rs`) files every posted task, delayed
  or not, under the posting isolate and calls that isolate's waker
  (`Engine::set_task_waker`) from whatever thread posted it. Background
  tasks stay on V8's own pool (2 threads).
- `Engine::run_foreground_tasks` runs the due tasks (at most 32 a call)
  with the isolate entered, then a microtask checkpoint, as Blink runs them
  as tasks of their own. `Engine::next_task_due` gives the next delayed
  task's time.
- Page workers run them from a task of their local set (`lb-page`
  `browser.rs` `run_v8_tasks`), woken by the waker or the next due time, so
  they run between page tasks and CDP jobs, never inside one. Web workers
  (dedicated, shared, service) run them at the top of their loop and wait
  on the waker too.
- An engine's queue is dropped before its isolate is disposed (deleting a
  cancelable task tells the isolate's task manager) and once more after,
  under the isolate setup lock, so an isolate later made at the same
  address never sees a dead one's tasks. A snapshot creator does the same
  around `CreateBlob`.
  Disposal itself also holds the setup lock until cleanup finishes. Taking
  it only after disposal permits address reuse and can delete the new
  isolate's queue (fixed 2026-11-19).

## Consequences

- `FinalizationRegistry` callbacks run (lb-js
  `finalization_registry_callbacks_run_as_foreground_tasks`).
- V8's own GC scheduling works. The GC after a closed page is still
  forced (ADR 0039 explains what changed around it): with V8 alone the
  worker settles at about 45 MB of private memory in a crawl, against
  about 11 MB with one memory-reducing GC per closed page.
- A page that keeps its worker busy delays V8's tasks as a long task
  delays them in Chrome.

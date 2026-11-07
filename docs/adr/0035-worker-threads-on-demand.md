# ADR 0035: Worker threads start on demand

Status: Accepted (2026-11-07).

## Context

`lb_page::Browser` started all of its page worker threads at once: one per
core by default (`BrowserOptions::workers`, `serve --workers`). Each thread
builds a `current_thread` tokio runtime with an I/O driver (mio's event
buffer alone is 48 KB), a `LocalSet`, its thread stack and thread-local
state, before any page exists. A live-allocation trace of `serve` on a
10-core Mac after a 50-page sequential crawl found ten runtimes, of which
one had ever run a page: a sequential crawl stays on the first worker,
because the least loaded worker wins and ties go to the first. On a
64-core server the unused threads cost dozens of runtimes for nothing,
and every `serve` paid their start-up before printing its DevTools line.

## Decision

`Browser::new` starts no threads. `pick_worker` (every new page) takes the
least loaded running worker, first of equals, as before; it starts a new
thread only when every running worker has at least one page and fewer
than `workers` run. Threads are never stopped early; they end with the
browser, as before.

Pages land on the same worker as when all threads started at once: the old
picker took the first worker with the fewest pages, and an unstarted
worker would have had none, so it was chosen exactly when every earlier
worker had a page. Concurrent pages still spread over up to `workers`
threads.

`Browser::started_workers()` reports how many run. The pool sits behind a
`Mutex`, taken once per new page.

## Consequences

- An idle `serve` holds the CDP thread and nothing per worker. Idle and
  crawl numbers: `docs/ROADMAP.md`, "macOS: lazy ICU data, workers on
  demand, bench and Playwright".
- The first page of a browser also starts its thread (tens of
  microseconds), the same cost a later concurrent page pays.
- A thread spawn failure now surfaces on the page that needed the thread
  (`PageError::Setup`), not from `Browser::new`.

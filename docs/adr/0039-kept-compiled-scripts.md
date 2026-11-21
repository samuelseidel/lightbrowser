# ADR 0039: Compiled classic scripts are kept per engine and partition

Status: Accepted (2026-11-07).

## Context

A page worker forces one memory-reducing GC after each closed page
(`Engine::reduce_memory`, V8's critical memory-pressure path). In V8 15.2
every memory-pressure level also clears the isolate's compilation cache
(`FreeCachesOnMemoryPressure`), so every page compiled again what the
previous page had compiled: Playwright's 12 KB utility script (evaluated in
each new main world), the CDP `callFunctionOn` wrapper, a site's shared
scripts and LightBrowser's own helpers. In a 300-page CDP crawl on rtx-dev,
parsing and compiling took 199 ms with the per-close GC against 24 ms
without it. The old code cache did not help: it only took scripts of
16 KB or more, and its hash-only key could, on a collision of equal-length
sources, have run another script's code.

## Decision

- `lb-js` `run_script` keeps compiled classic scripts
  (`v8::Global<v8::UnboundScript>`) and binds a kept script to the current
  context when the same source runs again from the same URL, line and
  column in a realm of the same partition. That is what a hit in V8's own
  compilation cache does; each context still gets its own top-level
  function, globals and intrinsics.
- A script is kept the second time it is compiled (a page's own inline
  scripts would otherwise push out the shared ones), up to 32 scripts and
  512 KB of source per engine, least recently used out first. Hits compare
  the whole source and URL, not a hash. Syntax errors are never kept.
- The partition (`RealmOps::set_script_partition`) is set when a window
  realm takes its document: its browser context and the origin of the
  top-level document. Chrome's renderers are split by site and its code
  cache by top-level site, so a script compiled for one site is not reused
  for another there either.
- Only CDP and the engine's own setup compile with an empty URL (page
  scripts, timer strings and handlers carry their document's URL), so a
  page can never hit, or time, a script a client sent.
- Kept scripts survive `reduce_memory` and the 2 s idle low-memory GC (V8's
  memory reducer keeps Chrome's compilation cache too); they go when the
  heap cap is hit and with the engine. The code cache is removed.

## Consequences

- In the crawl, `page.evaluate` fell from 1.2-1.3 ms to 0.7-0.9 ms and the
  page thread no longer compiles the shared scripts (ROADMAP "Timer order
  repeated, lock file, Lightpanda on Linux").
- A few hundred KB of V8 heap stay live per engine at most (bounded), and
  are marked by every full GC.
- Modules are not covered (a `v8::Module` cannot be bound to another
  context).

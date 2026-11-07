# ADR 0023: Cheaper window realms, made ahead while the worker waits

Status: Accepted (2026-11-04). Closes the M6 open item "CDP crawl wall time
vs Lightpanda".

## Context

The bench's CDP crawl (`bench/run.py`, 5 x 50 pages, Playwright `new_page`,
`goto(load)`, `evaluate`, `close`) took 0.80-0.83 s against Lightpanda's
0.74 s. A server-side trace of the CDP messages and callgrind over the crawl
showed where a page's time went: two window realms per page (the new
target's `about:blank`, which Playwright evaluates in, and the document)
cost ~15M instructions each, 43 % of the worker's work; style matching
tested every rule against every element; layout from `app.js`'s
`getBoundingClientRect()`; the product pages asked 19 image requests where
Chrome asks 5 (ADR 0024). The rest was Playwright's client and the Python
corpus server, which we do not control.

## Decision

- **Realm setup does per-interface work once.** The global's overflow plan
  (keys as isolate-wide V8 names, which members go last, which engine
  built-ins move) is built with the interface and shared (`Rc`), so a
  realm only defines properties; namespaces are defined from their own
  templates instead of a 32-member chunk instance; the engine globals
  Chrome does not expose are listed by the first realm and later realms
  delete them by name (`RealmOps::delete_globals`). 1.6 → 0.9 ms per realm
  (`realm_loop` microbenchmark), same surface.
- **Spare realms.** `lb_webapi::rt::Runtime::prepare_spare_realm` makes a
  main-world window realm before its document exists, keyed by what a
  realm depends on then (insecure context, the persona's OS gates). It runs
  while the worker would otherwise wait: after a navigation's request is
  sent (the commit takes the realm), and after a CDP-driven page's `load`
  (the next target's `about:blank` takes it). `Window::create_realm` takes
  a matching spare, gives it its module loader
  (`RealmOps::set_module_loader`) and finishes it exactly as a new one:
  attach, security token, console, helpers, navigator, `chrome`. At most
  one spare per key; `lightbrowser fetch` makes none after load.
- **Cascade.** One matching context per element instead of per rule, and
  a pre-check per selector from its rightmost compound (ID, class or type,
  compared case-insensitively and in both spellings, so it never rejects a
  match).

## Alternatives considered

- **Skip or defer the GC after a closed page.** 1 ms less per page, peak
  RSS 77 MB; every other page or a growth threshold made no measurable
  wall-time difference (the GC mostly overlaps the client's own time).
- **Alternate pages between two workers**, so the GC never delays the
  next target: a second isolate brings its own heap and bindings (several
  MB, not measured) against a CDP peak RSS margin of ~2 MB.
- **Assume the root scrollbar in the first layout pass.** Not exact (a
  narrower page can be taller or shorter); kept the two passes.
- **Move `Temporal` without reading it.** V8 installs it as a lazy
  accessor; moving it to Chrome's position materialises it (0.2 ms per
  realm). No public API moves an accessor; kept.

## Consequences

- CDP crawl 0.67 s against Lightpanda's 0.76-0.77 s in two 5 x 50 runs;
  CPU 0.57 vs 0.77-0.78 s; CDP peak RSS 29.0 vs 31.3 MB (a spare realm is
  ~0.4 MB of V8 heap).
- `tools/surface_diff` is identical for three personas, `fetch` and CDP
  (the second CDP page commits into a spare realm).
- A worker that loaded a CDP page keeps one unused realm until the next
  page.

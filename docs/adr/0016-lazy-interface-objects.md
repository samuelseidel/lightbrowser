# ADR 0016: Lazy interfaces and interface objects; hot code packed at link time

Status: Accepted (2026-11-03)

## Context

The Lightpanda comparison (`docs/ROADMAP.md`, "M6 status: Lightpanda
comparison") failed two checks: `fetch` CPU (2.51 vs 1.37 s for 50
one-page processes) and CDP peak RSS (42.2 vs 31.4 MB). Every engine
defined all ~980 interfaces as V8 templates, and every window realm
instantiated all ~900 interface objects with their prototypes and member
functions, then ran a JS fixup over them: in a `fetch` process that was
60 % of all instructions, and 1.6 MB of V8 heap per realm in `serve`. A
page touches a few dozen interfaces.

## Decision

- **Interface objects are lazy data properties of the global**, as in
  Blink (`v8::Object::SetLazyDataProperty`): same key, same place in the
  property order, same descriptor (writable, configurable, not
  enumerable); a script cannot tell them from plain data properties. The
  object is made the first time anything in the realm reaches the
  interface: the global property (the getter runs in the global's own
  realm, also when read from another realm or after the realm was
  dropped), a wrapper, a subclass, `Arg::Construct`. Ancestors are made
  first, then `[[Prototype]]` is linked to the parent interface object.
- **Fixups run as each interface object is made** (`InterfaceFixup`,
  native, in `lb-js`): late members moved after `constructor`,
  `Symbol.unscopables`, `DOMException.prototype` on `Error.prototype`, and
  the realm's removals (`[SecureContext]` members, OS-gated members). The
  window's late globals are put last while the global is set up
  (`InterfaceSpec::reorder_last`); the engine globals Chrome lacks are
  deleted natively without reading any value. `window.chrome` gets a slot
  in its place so it is not moved afterwards.
- **Specs and templates are built on demand** (`define_interface_lazy`)
  in plain engines; snapshot creators and restoring engines still build
  all in order. The window's own members are defined from their function
  templates directly instead of being copied from chunk instances.
- **No startup snapshot by default.** A lazily built realm costs about
  what a deserialized one does, and the snapshot (blob, creation, its full
  template set) cost ~3 MB of RSS in `serve`. The snapshot code stays
  (`BrowserOptions::startup_snapshot`), tested against built realms.
- **`fetch` exits fast**: no initial about:blank realm (its history entry
  is kept, so `history.length` is unchanged), no teardown of V8 or the
  DOM after the output, a current-thread runtime on the main thread, the
  ICU default zone set without first detecting the host's, the parsed
  header profile shared per context, and libstdc++ no longer loaded
  (BoringSSL's one call into it is defined locally).
- **Less memory per process**: the generated binding table holds no
  pointers (strings and lists are offsets into pooled arrays, `desc::S`
  and `desc::L`), so it stays in shared read-only pages instead of being
  relocated into private ones at start-up (PIE relocations dirtied ~0.6 MB
  per process); the registry's function maps are sorted vectors after
  install; response bodies are `bytes::Bytes` shared between the page,
  the CDP agent and the decoders instead of copied; a layout box tree is
  shrunk to fit after it is built, and its boxes share equal taffy styles
  (552 of a box's 1,224 bytes; every text run has the same one); the ICU
  data is paged in 4 KiB chunks (ADR 0013, revision 4).
- **Hot code first**: `bin/lightbrowser/hot-symbols.txt` orders the
  functions a page load runs at the start of `.text`
  (`--symbol-ordering-file`). Made by `tools/hot_symbols/gen.py` under
  callgrind from a training workload (detect-bench pages, the WPT subset,
  one generic page), never the bench corpus.

## Alternatives considered

- **Build-time snapshot embedded in the binary** for `fetch`: saved
  ~10 ms of 47 per process in a test with the blob read from a file. It
  needed ~0.4 MB the binary did not have under the old < 40 MB budget;
  with < 50 MB (ADR 0013, revision 4) size is no longer the obstacle, but
  the build cost is (a build script that links V8 compiles `lb-webapi`
  and V8 a second time for the host, ADR 0013), and the lazily built realm
  already meets the < 5 ms budget and wins the CPU checks. Not done.
- **Lazy accessors** (rejected in ADR 0013): change descriptors. Lazy
  data properties do not.
- **Smaller V8 young generation** (`--max-semi-space-size=1`): no RSS
  change. **`malloc_trim` after a page closed**: no change.
  **`MADV_RANDOM` on the binary**: does not stop the kernel's fault-around.
- **Order list trained on the bench corpus**: CDP peak ~26 MB, but it
  would tune the layout to the benchmark; not used.

## Consequences

- Fidelity (`tools/surface_diff`, against the M7 build `525e94d`, for a
  Windows, a macOS and a Linux persona, in `fetch` and over CDP): the
  global's keys in order, every global and interface object, prototype and
  their descriptors, function names, lengths, `toString`, errors of calls
  with a wrong receiver, cross-realm and detached-frame behaviour are
  identical. One difference, a fix: a page's global function declaration
  that replaces an interface object (`function Attr() {}`) now moves the
  key to the end of `Object.getOwnPropertyNames(window)`, as in Chrome 153
  (checked against the local Chrome for Testing 153.0.8010.12, where Blink
  also installs interface objects as lazy data properties); M7 kept the
  old position.
- The bench's `fetch` peak RSS was wrong for every target: a child's
  `ru_maxrss` includes its parent's RSS at `exec`, so each `fetch`
  process "peaked" at the size of the Python bench process (Playwright
  loaded). `bench/run.py` now spawns the `fetch` processes from a small
  helper process; this lowered both LightBrowser's and Lightpanda's
  `fetch` peak (Lightpanda 44.9 → 24.3 MB).
- See "M6 status: Lightpanda comparison" ("Second pass") in
  `docs/ROADMAP.md` for the measured effect.
- A stale order list only costs locality; regenerate it after large
  changes or a V8 upgrade.

# ADR 0013: V8 startup snapshot made in memory; binary budget follows the V8 floor

Status: Accepted (2026-11-03)

## Context

Without a snapshot every window realm (page, frame, isolated world) built
~1,000 interface objects and their prototypes from templates and ran the
installer fixup: about 30 ms per realm and 34 ms for define plus the first
realm (release), against a budget of < 5 ms. Built realms also left a lot of
garbage: one local page cost 152 MB RSS in `serve`.

The binary budget is < 40 MB stripped per family. The Chrome binary was
62.1 MB. The prebuilt `v8` crate (152.2.0) is fixed: we cannot drop its
JIT tiers, Wasm or ICU data without building V8 from source.

## Decision

- **Snapshot of templates plus one window context** (`lb_js::Engine::
  snapshot_creator` / `create_snapshot` / `with_snapshot`). Host callbacks
  stay Rust closures: templates hold one of 12 trampolines (the external
  reference list) and an index into the engine's callback table. A
  restoring engine replays the same `define_interface` calls; each spec is
  checked against a digest recorded at creation and only its callbacks are
  pushed. The first differing spec makes the rest build normally and turns
  the snapshot context off (`SnapshotStatus`). Realms of the window
  interface are then `Context::from_snapshot` copies with the fixup already
  run; only the global-object part of the fixup runs again, because V8
  re-adds its flag-gated globals (`Temporal`, `DisposableStack`,
  `Float16Array`, `SharedArrayBuffer`, `WebAssembly`) to every new context.
  A test compares the full surface of snapshot and built realms.
- **No persona data in the blob.** It is made with the family's default
  persona; values that reach templates are part of the digest, persona
  values are read by callbacks at call time. One blob serves all personas.
- **Made in memory, once per process** (`lb_webapi::snapshot::
  startup_snapshot`), the first time an engine starts: 110 ms, 2.57 MB, no
  disk writes. `BrowserOptions::startup_snapshot` (default on) switches it.
- **One snapshot per process, made alone.** V8 shares one read-only heap
  between all isolates: `CreateBlob` writes into it, and isolates from
  different snapshots cannot live side by side (crashes in V8 otherwise).
  lb-js enforces it: one creator per process, which must be the only
  isolate and blocks new ones while it lives; afterwards every isolate is
  made from that blob (plain engines without replay). A restore that would
  mix snapshots builds normally instead.
- **Realm ids live in engine state** (keyed by the global's identity hash),
  not in rusty_v8 context slots: their per-context weak handle double-frees
  at isolate teardown for deserialized contexts.
- **Size: link options, not code removal.** `bin/lightbrowser/build.rs`
  adds `--icf=safe` and `-z pack-relative-relocs` to release links
  (rust-lld); non-hot crates build with `opt-level = "s"`. The budget
  becomes **< 56 MB** for the Chrome binary with the prebuilt V8, i.e. the
  V8 floor plus 14 MB for everything we write and link.

## Alternatives considered

- **Blob made at build time and embedded.** Preferred at first. A build
  script needs `lb-webapi` as a build dependency, which Cargo compiled a
  second time for the host with V8 (+255 MB build script, +209 MB libv8,
  +89 MB lb-webapi in debug; release cannot share either, because build
  scripts never use `panic = "abort"`), and it adds 2.6 MB to the binary.
  The in-memory blob's one-time peak (95 MB) is below a non-snapshot first
  page (102 MB). Revisit if CI disk allows.
- **Lazy interface objects** (accessor on first touch). Changes property
  descriptors that fingerprinting scripts read; rejected.
- **Template reuse only** (no context). Templates are already per engine;
  the cost is instantiation.
- **mimalloc.** Measured worse: idle 20.5 MB vs 10.1, 10 pages 244 MB vs
  196.
- **`-no-pie`**: another ~1 MB, but drops ASLR for the main image.

## Consequences

- Release, full registry: realm 34 ms → 2.0 ms; define 9.1 → 5.2 ms; engine
  0.8 → 2.7 ms. `serve`, one local page: 152 → 67 MB RSS.
- Floor (`lb-js` example `v8_floor`, same profile and link flags): 42.0 MB,
  of which 10.9 MB is ICU data. < 40 MB needs a from-source V8 with
  filtered ICU data (Chrome-visible features must stay).
- Engines must define the same interfaces in the same order as the
  creator; anything else is detected and only costs speed.

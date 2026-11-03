# ADR 0013: V8 startup snapshot made in memory; binary budget follows the V8 floor

Status: Accepted (2026-11-03); budget back to < 40 MB in revision 2

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
  `lightbrowser fetch` loads one page per process and turns it off: on the
  50 `bench/` corpus pages (2026-11-03, release, paired runs) the median
  page went from 167 to 128 ms and CPU from 9.9 to 8.8 s, for 97 → 112 MB
  peak RSS.
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
  of which 10.9 MB is ICU data (shipped compressed since the Revision
  below). < 40 MB needs a from-source V8 with filtered ICU data
  (Chrome-visible features must stay).
- Engines must define the same interfaces in the same order as the
  creator; anything else is detected and only costs speed.

## Revision: ICU data compressed, no unwind tables (2026-11-03)

The < 56 MB budget stays; the binary is now 44.37 MB (54.96 before).
The original < 40 MB is still missed, by 4.4 MB.

- **ICU data shipped compressed.** The prebuilt archive's ICU package
  (`icudt78_dat` in `icudtl_dat.o`, 10.88 MB) is only linked because
  ICU's `udata.o` references the symbol. `lb-js` defines `icudt78_dat`
  itself as a valid, empty ICU package, so the linker never extracts the
  archive's copy (a second copy would be a duplicate-symbol error, not a
  silent fallback). The same bytes are checked in zstd-compressed
  (`crates/lb-js/icu/icudt78.dat.zst`, level 22, 3.83 MB; the
  `icu_pack` example rewrites it from the archive) and installed with
  `udata_setCommonData` in `init_v8`, before V8 starts. A test checks the
  file against the archive byte for byte. Intl output is identical: a
  47.8 KB probe (supported locales of every Intl constructor over all
  two-letter codes, `supportedValuesOf`, 40 locales x 18 formatters and
  collators, Thai/Khmer/Lao/Burmese/CJK segmentation, 16 calendars, 7
  time zones, normalization) gave the same bytes with the linked and the
  installed data. -7.05 MB.
- **No unwind tables.** `bin/lightbrowser/build.rs` links release
  binaries with a linker script that discards `.eh_frame` (so lld writes
  no `.eh_frame_hdr`): -3.6 MB. Nothing unwinds in the release binary
  (`panic = "abort"`; V8, ICU, libc++ and BoringSSL have no C++
  exceptions). Crash backtraces (Rust, V8's fatal-error trace, gdb) lose
  their frames; `LB_KEEP_UNWIND=1` keeps the tables for debugging.

Cost of the compressed ICU data, paid when the first engine starts (not at
process start, so idle RSS is unchanged): ~17 ms (decompression and page
faults; a one-script process goes from 4.3 to 22 ms) and one 10.9 MB heap
allocation, all resident. The linked-in data was paged in only as far as
ICU read it, so peak RSS of a one-script process grows by 10.8 MB (15.9 →
26.7 MB), of one that formats a date and a number by 7.6 MB, and of the
Intl probe above by 2.4 MB. The compressed pages are dropped from RSS
again (`madvise`) after decompressing. Realm creation is unchanged
(`realm_bench`: 2.41 ms first, 2.08 ms next).

What is left (link map of the release binary, unwind tables excluded):

| Part | MB |
|---|---|
| V8 C++ from the prebuilt archive (incl. ICU code, libc++) | 26.59 |
| ICU data, zstd | 3.83 |
| Rust deps of the `v8` crate (Temporal, ICU4X) | 0.26 |
| Rust std/core/alloc | 1.29 |
| LightBrowser crates and their Rust deps: code | 7.00 |
| Rust constant data (binding table, CSS/font/codec tables, strings) | 1.96 |
| BoringSSL | 1.67 |
| zstd (C) | 0.38 |
| merged strings, CRT, PLT/GOT, padding | 1.39 |

V8 plus its ICU data is 30.7 MB, which leaves 9.3 MB under 40 MB for
everything we add (13.2 MB now). Measured but not applied:

- brotli (quality 11, 16 MiB window) instead of zstd for the ICU data:
  3.51 MB (-0.33 MB), but 51 ms to decompress instead of 14 ms.
- `opt-level = "s"` for the root crate, which sets the LTO pipeline for
  all Rust code: -0.77 MB, and slower DOM, style, layout and bindings.
- `--icf=all`: -0.22 MB; breaks function-pointer identity.
- The generated binding table as offsets into one string blob instead of
  `&'static str` (4,410 attributes, 2,424 operations, 909 interfaces):
  estimated -0.5 MB of the 1.1 MB of Rust `.data.rel.ro`.

Together these give about -1.8 MB (42.5 MB). Below that, Rust code we
use would have to go (e.g. the hickory DNS resolver, image codecs), or V8
itself: only a from-source V8 can drop code, and its ICU data is already
Chromium's filtered desktop set.

## Revision 2: unused V8 code not linked, original < 40 MB met (2026-11-03)

The budget goes back to **< 40 MB**: the binary is 39,174,008 bytes
(39.17 MB, 44.37 before), with nothing a page, a fingerprint or a CLI
user can observe removed. The < 56 MB step above is history.

- **V8 code behind off-by-default flags is not linked**
  (`lb-js` `v8_engine/unused_v8.rs`), 3.37 MB. The prebuilt archive
  links whole objects that only flag-gated paths reach: Turbolev
  (`--turbolev`) and its Wasm-in-JS inlining, the Turboshaft
  type-assertion, typed-optimisation, loop-optimisation and
  random-rescheduling phases, block instrumentation (`--turbo-profiling`,
  mksnapshot only), the Wasm SIMD revectorizer, the fuzzer's random Wasm
  module generator, LiveEdit and code coverage (inspector and test
  natives only), three compiler verifiers, Turboshaft JSON graph tracing
  and the experimental regexp engine. For each, `lb-js` defines the
  object's strong global symbols (mangled C++ names) as functions that
  abort; the linker then never extracts the object. As with `icudt78_dat`
  this cannot fail silently: if anything else needs such an object, the
  link fails with a duplicate symbol. LightBrowser passes V8 no flags
  from the CLI or pages, `init_v8` panics on any flag that would reach a
  stub (`REFUSED_FLAGS`), and V8 freezes its flags at initialisation. A
  test checks every stub against the archive's symbol index, so a `v8`
  upgrade that renames one shows up as a test failure (the object would
  otherwise come back and only cost size).
- **Rust code**: the root crate at `opt-level = "s"`, which is the level
  of the fat-LTO pipeline for all Rust code (-0.68 MB), and `lb-webapi`
  (binding glue) at `"s"` before LTO (about -0.5 MB). `lb_net::fetch`
  returns a boxed future, so the network stack's state machines are
  compiled once instead of once per crate that awaits them (-0.36 MB);
  `rt::Runtime::win` is no longer inlined into 1,084 binding closures
  (-0.06 MB).
- **Embedded data compressed** with the zstd already linked: font metric
  tables and glyph outline packs (`lb-layout`/`lb-paint` `build.rs`,
  -0.21 MB), unpacked per use (< 0.2 ms), byte-identical.

| step | bytes |
|---|---|
| before | 44,367,736 |
| V8 stubs (Turbolev, phases, revectorizer, fuzzer) | 41,189,240 |
| boxed `lb_net::fetch` | 40,824,696 |
| root crate `opt-level = "s"` | 40,144,760 |
| `Runtime::win` not inlined | 40,083,320 |
| compressed font tables and glyph packs | 39,874,424 |
| V8 stubs (LiveEdit, coverage, verifiers, JSON tracing) | 39,718,776 |
| V8 stubs (experimental regexp) + `lb-webapi` at `"s"` | 39,174,008 |

Cost: `realm_bench` first/next realm 2.39 / 2.06 ms (unchanged),
registry + define 5.35 → 6.09 ms. `bench/run.py`: `fetch` page median
140.0 → 141.6 ms, `cdp` 98.5 → 99.9 ms, CPU +3 %; idle RSS 10.0 MB.
Unchanged: fingerprint-report 113 ok / 0 mismatch for both Chrome
personas, TLS/H2 fingerprint test, WPT 2552/2788, Playwright 20/20,
T-010 fp 0, ICU data. Still not applied: `opt-level = "z"` (-0.23 MB,
slower), `--icf=all`, brotli ICU data, a DNS resolver swap (behaviour
differs), the binding table as string offsets (about -0.15 MB).


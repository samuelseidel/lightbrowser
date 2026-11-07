# ADR 0013: V8 startup snapshot made in memory; binary budget follows the V8 floor

Status: Accepted (2026-11-03); budget back to < 40 MB in revision 2; ICU data paged lazily in revision 3; budget raised to < 50 MB by user decision in revision 4

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
  (`crates/lb-js/icu/icudt78.dat.zst`, level 22, 3.83 MB, replaced by
  `icudt78.dat.zch` in Revision 3; the
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


## Revision 3: ICU data paged in lazily, realms built in linear time (2026-11-03)

Found while comparing with Lightpanda (`docs/ROADMAP.md`, "M6 status:
Lightpanda comparison"). The budget stays < 40 MB: 39,628,664 bytes.

- **ICU data decompressed on first touch.** A page load reads about
  120 KB of the 10.9 MB package (its table of contents, the root locale,
  time zone data), but the first engine of every process decompressed
  all of it: ~15 ms and 10.9 MB of resident heap. The data now ships as
  independent zstd frames of 128 KiB (`icu/icudt78.dat.zch`, 4.26 MB,
  +0.43 MB of binary). On Linux the package's size is reserved
  `PROT_NONE`; a `SIGSEGV` handler decompresses the chunk a read falls in
  into a scratch mapping and `mremap`s it into place (atomic for other
  threads), so about 5 chunks (0.6 MB, < 1 ms) become resident. Faults
  outside the package go to the previous handler (crashes stay crashes);
  without the reservation or the handler, or on other systems, the whole
  package is decompressed at once as before. Byte-identical data (test
  against the archive, plus 8 threads reading scattered chunks). Costs:
  the handler runs on the faulting thread's stack (Rust's alternate
  signal stacks are too small for zstd), so a stack overflow ends with a
  plain `SIGSEGV` instead of Rust's message; debuggers stop on these
  faults (`handle SIGSEGV nostop noprint pass` in gdb).
  Chunk sizes measured: 64 KiB +0.58 MB, 128 KiB +0.43 MB, 256 KiB
  +0.35 MB of binary; a 64-256 KB raw dictionary saved < 50 KB.
- **The window global's members are defined after the context is made.**
  V8 instantiates a template into a fresh fast-mode object, copying the
  map's descriptor array for every member: O(n²) for the ~1,200 members
  of the window global (60 % of a `fetch` process's instructions, 60 MB of
  garbage per realm). An interface with more than 64 instance members now
  keeps none on its instance template: interface objects (and their
  `[LegacyWindowAlias]` names) are defined directly from their function
  templates, the remaining members are copied in order from chunk
  templates of 32 (`lb-js` `apply_overflow`). Snapshot creation 130 →
  92 ms; a realm built without the snapshot 34.7 / 43.5 → 7.0 / 5.7 ms
  (first / next, `realm_bench`, same machine and session). Order, keys,
  descriptors and prototype chains of the global and every interface are
  unchanged, except that the six alias interface objects now have their
  interface's `name` (`webkitURL.name === "URL"`), as in Chrome 153;
  before, the alias name leaked into the shared function
  (`URL.name === "webkitURL"`).
- Cost: a snapshot realm takes 2.5 ms instead of 2.3 ms (`realm_bench`
  next realm, same session) and keeps 1.63 MB of V8 heap instead of
  1.39 MB; where the extra 0.24 MB comes from is not pinned down (not the
  chunk templates' cached instances, not the code cache, not the template
  split threshold). Still under the < 5 ms budget.

## Revision 4: budget < 50 MB (user decision), ICU data in 4 KiB chunks (2026-11-03)

**The binary budget is < 50 MB** (was < 40 MB), a user decision "to have
all features possible": the extra room may hold anything that helps, as
long as each use pays for itself in the bench and loses no fidelity.
`bench/run.py` checks `BUDGET_BINARY_MB = 50.0`. Nothing cut for size
earlier has come back yet: the root crate and the bindings stay at
`opt-level = "s"` (`3` was not re-measured against the new code), the V8
code behind off-by-default flags stays unlinked (no page can reach it),
and there is no embedded build-time snapshot (ADR 0016: the startup
snapshot is off by default and the lazily built realm meets the budget;
the obstacle is the build cost, not size). Stripped release binary:
40,386,424 bytes (40.39 MB; 39,718,776 before this revision, of which
+0.66 MB is the ICU chunk size below).

First use of the room: **ICU data in 4 KiB chunks** instead of 128 KiB
(`icu/icudt78.dat.zch` 4.26 → 4.92 MB, +0.66 MB of binary). A page load
reads ~27 of the package's pages, scattered over its table of contents,
the root locale and time zone data; with 128 KiB frames ~0.6 MB of
decompressed data stayed resident per process, with 4 KiB frames
~0.1 MB. Sizes measured for the whole package: 4 KiB 4.92 MB, 8 KiB
4.75 MB, 16 KiB 4.60 MB, 32 KiB 4.49 MB, 64 KiB 4.41 MB, 128 KiB 4.26 MB
(zstd level 22; level 19 is within 0.1 %). The lazy path checks that the
system page size divides the chunk size (else it decompresses eagerly).
After each chunk, the pages of the compressed pack that reading its frame
mapped (the kernel maps up to 64 KB of the binary around a fault) are
unmapped again with `MADV_DONTNEED`; they are clean pages of the
executable, so this changes RSS (-0.37 MB at the end of a `fetch`), not what the
page cache holds.

Tools: callgrind does not retry a load that faulted into the lazily paged
data correctly (the retried load uses a stale register, so ICU follows a
garbage pointer); with 4 KiB chunks that broke every
`tools/hot_symbols` run. `LB_ICU_EAGER=1` decompresses the package at once
and `gen.py` sets it. Plain valgrind needs
`--vex-iropt-register-updates=allregs-at-mem-access` for the same reason.


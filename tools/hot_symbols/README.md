# hot_symbols

Makes `bin/lightbrowser/hot-symbols.txt`, the list of functions the release
link puts first in `.text` (lld `--symbol-ordering-file`, see
`bin/lightbrowser/build.rs` and ADR 0016). Packing the code a page load runs
onto few pages cuts resident memory: the kernel maps the 64 KB around every
code page fault, so a hot set spread over the binary's ~29 MB of code costs
memory and faults.

```bash
cargo build --release -p lightbrowser \
    --config 'profile.release.package.lightbrowser.strip=false'
python3 tools/hot_symbols/gen.py          # ~10 min under valgrind, loopback only
cargo build --release -p lightbrowser     # relink with the new order
```

Needs valgrind and Python Playwright. The training workload is the
detect-bench pages (`tests/detect-bench/server.py`) and the WPT subset
(`tests/wpt`), never the bench corpus the result is measured on. Every run
goes under callgrind: one `fetch` per page, one `serve` crawling every page
twice through Playwright, one idle `serve`. Order: start-up code, then code
by the number of runs that used it (code only the crawl used counts as used
by every run), then by instructions.

Regenerate after large code changes or a V8 upgrade. A stale list is safe:
symbols a build lacks are ignored and new code just lands after the list,
which only costs locality. `LB_NO_ORDER=1` links without the list.

**After a Rust toolchain change, remap the list.** Rust symbol names carry a
hash per crate that changes with the rustc build, so a list made with
another rustc orders no Rust code at all (lld drops unknown names without a
word; C++ code stays ordered). The list's first line names the rustc that
made it, and the release build warns when it differs. `remap.py` carries the
list over in seconds, keeping its order: it matches each Rust name to the
functions of the same demangled name in an unstripped build made with the
new toolchain (`c++filt`, crate hashes removed).

```bash
cargo build --release -p lightbrowser \
    --config 'profile.release.package.lightbrowser.strip=false'
python3 tools/hot_symbols/remap.py --binary target/release/lightbrowser
cargo build --release -p lightbrowser     # relink with the remapped order
```

On rtx-dev (rustc 1.99.1) the list made on 2026-11-04 with another rustc
matched none of its 4,312 Rust names; remapped, 4,164 match, and `fetch`
peak RSS fell about 0.55 MB, `serve`'s CDP crawl peak about 0.4 MB (2 x 2
runs of `bench/run.py`, binaries evicted).

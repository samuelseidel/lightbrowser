# bench

Crawl benchmark for the budgets in `docs/ARCHITECTURE.md` (PLAN M6 item 2).
Local only: the corpus is served on 127.0.0.1 and nothing leaves the machine.

```bash
cargo build --release -p lightbrowser          # chrome family, stripped binary
python3 bench/run.py                           # 5 runs, all targets -> bench/results/
python3 bench/run.py --runs 1 --pages 10 --no-write   # quick check
python3 -m unittest discover -s bench          # tooling tests (no browser)
python3 bench/gen_corpus.py [--check]          # regenerate / verify bench/corpus
```

Needs Python 3.10+, `playwright` (Python, any 1.5x or newer; only the client
is used) and the Chromium at `/opt/pw-browsers/chromium-1243` (override with
`--chromium`). `--binary` or `$LIGHTBROWSER_BIN` picks the LightBrowser
binary (default `target/release/lightbrowser`). Exit code 1 if a budget
fails or a browser did not render every page.

## Loopback only

- `serve_corpus.py` binds 127.0.0.1 only. There is no flag to point the runner
  at another host or an existing server.
- `run.py` refuses (exit 2) any URL or DevTools endpoint whose host is not
  `127.0.0.1`, `::1` or `localhost`; this is applied to the corpus base URL
  and every WebSocket endpoint before use.
- Proxy variables are removed from the runner's environment and `NO_PROXY` is
  set; Chromium also gets `--no-proxy-server`.

## What it runs

| target | how |
|---|---|
| `fetch:lightbrowser` | `lightbrowser fetch URL --dump html`, one process per page |
| `cdp:lightbrowser` | `lightbrowser serve --port 0`, Playwright `connect_over_cdp` |
| `cdp:chromium` | headless Chromium (`--headless=new --no-sandbox`), the same Playwright code |
| `fetch:lightpanda`, `cdp:lightpanda` | only if a `lightpanda` binary is on PATH; skipped with a note otherwise. Untested here (not available offline): `lightpanda fetch --dump URL` and `lightpanda serve --host 127.0.0.1 --port P` are assumed. |

Each repetition runs every target once, in that order, on a fresh process.
Default is 5 repetitions; the report gives the median and the min-max over
repetitions.

**Corpus.** `bench/corpus/` has 50 generated pages (articles, product grids,
tables, forms, API docs, comment threads), 1.7 MB in all, plus a manifest.
Each page links a shared stylesheet and scripts (sync in head, `defer`,
`async`), its own stylesheet, and images, and builds part of its DOM from
inline script. When the scripts are done the page sets
`<html data-bench-rendered="N">`; the runner checks N for every page, so a
browser that skips scripts cannot look fast. "pages verified" in the results
must be 50/50 for LightBrowser and Chromium. `gen_corpus.py` is deterministic
(own PRNG, no clock); the corpus hash is recorded in every result.

**Metrics.**

- *Page time*: CDP: `new_page` + `goto(wait_until=load)` + one `evaluate` +
  `close`, per page, sequential. Fetch: process spawn to exit, so it includes
  startup. Median and nearest-rank p90 over the 50 pages, then the median over
  repetitions. Total crawl time is the whole loop.
- *Peak RSS*: fetch: the largest `ru_maxrss` of any single process (exact).
  CDP: the sum of the live process tree's own high-water marks (`VmHWM`,
  read every 50 ms from spawn, so it includes startup and the one-time V8
  snapshot peak). That is exact for LightBrowser (one process) and an upper
  bound for multi-process Chromium, whose shared pages are counted once per
  process. Recorded next to it: `peak_rss_sampled_mb` (plain simultaneous
  sum of `VmRSS`) and `peak_pss_mb` (summed PSS, which counts shared pages
  once, sampled every 200 ms).
- *CPU time*: user+sys. Fetch: summed `wait4` rusage. CDP: the tree's
  `/proc/PID/stat` times (including reaped children) at the end minus at the
  start of the crawl.
- *Idle RSS*: `serve`, no client connected, 1 s after the DevTools line. Also
  recorded: after connecting, and after the crawl once all pages are closed.
- *Startup*: spawn to the DevTools line (`listen_ms`), and to
  `connect_over_cdp` returning (`connect_ms`); `first_page_ms` is the first
  `new_page`, `new_page_ms` the median of 15 more.
- *Binary size*: file size of the (stripped) release binary; the Chromium
  figure is the `chrome` executable alone, for scale.
- *Realm creation*: the `realm_bench` example of `lb-webapi`
  (`cargo build --release -p lb-webapi --example realm_bench`), snapshot line,
  median of its 7 x 20 samples, then median over repetitions. Skipped with a
  note if `target/release/examples/realm_bench` does not exist. It is not
  rebuilt by the runner.
- *Disk writes*: browsers run with an empty temp dir as cwd, `HOME` and
  `TMPDIR`; the budget passes if nothing is created there and
  `/proc/PID/io` `write_bytes` stays 0 during the crawl.

**Budgets** checked (from ARCHITECTURE): idle RSS < 15 MB, binary < 40 MB,
realm creation < 5 ms, no disk writes, and wall time / peak RSS / CPU no worse
than Lightpanda (status `skip` while Lightpanda is absent).

## Results

`bench/results/<date>-<sha>.json` holds everything, including the raw runs
and the per-page times; `bench/results/README.md` is the summary table,
rewritten on each run. The sha is `git rev-parse --short HEAD`; the JSON also
says whether the tree was dirty and records the binary's sha256, so a result
can be tied to the exact binary. Pinned: LightBrowser sha and version,
Chromium version, Playwright (Python) version, rustc, Python, kernel, CPU,
cores, RAM, corpus hash.

Notes: the machine runs other work at times; compare runs through the
min-max, not one number. The runner is one process driving one browser at a
time and does not run the browsers concurrently.

# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-03-a060281.json` -- 2026-11-03, LightBrowser `a060281` (dirty tree), binary sha256 `e801e8cd2f8ea13d`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v230. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07).

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium |
|---|---|---|---|
| crawl total (s) | 18.4 (17.7-19.0) | 14.2 (13.8-15.0) | 10.3 (9.8-10.9) |
| page median (ms) | 165.5 (162.8-179.0) | 100.0 (100.0-103.7) | 205.0 (194.9-212.6) |
| page p90 (ms) | 1195.9 (1178.5-1198.8) | 1084.0 (1076.0-1096.1) | 221.4 (203.2-251.2) |
| peak RSS (MB) | 95.8 (95.5-96.9) | 95.3 (95.0-95.9) | 2876.4 (2822.2-3029.3) |
| peak PSS (MB, sampled) | - | 44.9 (44.7-45.5) | 689.5 (685.2-861.1) |
| CPU (s) | 10.0 (10.0-10.4) | 1.41 (1.35-1.43) | 23.7 (23.6-24.1) |
| idle RSS (MB) | - | 9.95 (9.87-10.44) | 1083.9 (1080.4-1086.6) |
| idle PSS (MB) | - | 7.25 (7.19-7.75) | 376.8 (375.8-378.3) |
| start to listening (ms) | - | 4.08 (3.63-4.25) | 103.1 (100.2-114.4) |
| start to CDP connect (ms) | - | 8.56 (8.48-18.57) | 119.0 (113.4-137.6) |
| first new page (ms) | - | 151.0 (148.2-151.7) | 89.0 (73.5-89.6) |
| new page, median (ms) | - | 5.74 (5.41-5.97) | 71.2 (60.7-77.5) |
| pages verified | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| chromium (chrome binary only) | 293.3 |
| lightbrowser | 54.9 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 2.83 (2.71-2.91) | 5.31 (5.23-5.39) | 2.41 (2.35-2.48) | 2.08 (2.01-2.14) |

## Skipped

- fetch/cdp:lightpanda: no `lightpanda` binary on PATH (not available offline); budgets vs Lightpanda are not checked

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 9.945 | **pass** |
| binary size (stripped) | < 56 MB | 54.88 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 2.41 / 2.08 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 18.45 | **skip** - lightpanda not available on this machine |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 95.758 | **skip** - lightpanda not available on this machine |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 10.048 | **skip** - lightpanda not available on this machine |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 14.15 | **skip** - lightpanda not available on this machine |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 95.285 | **skip** - lightpanda not available on this machine |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 1.41 | **skip** - lightpanda not available on this machine |

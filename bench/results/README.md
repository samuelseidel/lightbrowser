# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-03-f2d6632.json` -- 2026-11-03, LightBrowser `f2d6632` (dirty tree), binary sha256 `0e2df17d93846b2b`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v230. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07).

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium |
|---|---|---|---|
| crawl total (s) | 7.18 (7.05-7.25) | 4.96 (4.89-5.05) | 9.71 (9.67-9.93) |
| page median (ms) | 141.6 (140.1-143.8) | 99.9 (96.0-100.0) | 193.9 (192.2-197.9) |
| page p90 (ms) | 151.6 (149.5-152.6) | 103.8 (100.0-104.0) | 206.3 (203.4-214.2) |
| peak RSS (MB) | 113.9 (113.8-114.2) | 105.3 (104.4-105.6) | 2821.8 (2811.3-2828.5) |
| peak PSS (MB, sampled) | - | 54.0 (53.1-54.3) | 687.9 (683.2-694.1) |
| CPU (s) | 10.4 (10.1-10.6) | 1.35 (1.33-1.38) | 23.4 (23.1-23.9) |
| idle RSS (MB) | - | 10.0 (9.8-10.2) | 1079.7 (1075.8-1086.7) |
| idle PSS (MB) | - | 7.33 (7.07-7.44) | 377.0 (376.5-379.9) |
| start to listening (ms) | - | 3.92 (3.88-3.96) | 104.3 (96.8-112.9) |
| start to CDP connect (ms) | - | 8.86 (8.18-19.41) | 116.8 (108.5-125.7) |
| first new page (ms) | - | 167.6 (164.3-174.4) | 75.9 (74.0-84.2) |
| new page, median (ms) | - | 5.56 (5.43-6.32) | 64.0 (59.7-67.1) |
| pages verified | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| lightbrowser | 39.2 |
| chromium (chrome binary only) | 293.3 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 2.78 (2.76-2.81) | 6.09 (6.08-6.15) | 2.39 (2.36-2.46) | 2.06 (2.03-2.14) |

## Skipped

- fetch/cdp:lightpanda: no `lightpanda` binary on PATH (not available offline); budgets vs Lightpanda are not checked

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 10.035 | **pass** |
| binary size (stripped) | < 40 MB | 39.17 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 2.39 / 2.06 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 7.184 | **skip** - lightpanda not available on this machine |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 113.938 | **skip** - lightpanda not available on this machine |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 10.363 | **skip** - lightpanda not available on this machine |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 4.957 | **skip** - lightpanda not available on this machine |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 105.285 | **skip** - lightpanda not available on this machine |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 1.35 | **skip** - lightpanda not available on this machine |

## History

| file | sha | crawl cdp (s) | page median (ms) | peak RSS (MB) | idle RSS (MB) | binary (MB) |
|---|---|---|---|---|---|---|
| 2026-11-03-a060281.json | a060281 | 14.2 (13.8-15.0) | 100.0 (100.0-103.7) | 95.3 (95.0-95.9) | 9.95 (9.87-10.44) | 54.9 |
| 2026-11-03-ebe5f36.json | ebe5f36 | 4.94 (4.80-5.01) | 98.5 (96.0-100.0) | 105.9 (105.6-106.3) | 10.3 (10.2-10.4) | 44.4 |
| 2026-11-03-f2d6632.json | f2d6632 | 4.96 (4.89-5.05) | 99.9 (96.0-100.0) | 105.3 (104.4-105.6) | 10.0 (9.8-10.2) | 39.2 |

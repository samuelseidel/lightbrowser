# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-03-ebe5f36.json` -- 2026-11-03, LightBrowser `ebe5f36` (dirty tree), binary sha256 `6aa2ebe44ac8b5a2`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v230. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07).

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium |
|---|---|---|---|
| crawl total (s) | 7.01 (6.92-7.12) | 4.94 (4.80-5.01) | 9.72 (9.62-10.12) |
| page median (ms) | 140.0 (137.5-142.6) | 98.5 (96.0-100.0) | 193.1 (192.8-202.5) |
| page p90 (ms) | 146.8 (146.1-151.0) | 103.9 (100.0-104.0) | 206.0 (202.3-219.0) |
| peak RSS (MB) | 115.2 (115.0-120.7) | 105.9 (105.6-106.3) | 2820.4 (2811.4-2850.8) |
| peak PSS (MB, sampled) | - | 55.3 (55.2-55.3) | 687.0 (678.7-691.8) |
| CPU (s) | 10.0 (9.8-10.1) | 1.33 (1.28-1.37) | 23.4 (23.0-23.8) |
| idle RSS (MB) | - | 10.3 (10.2-10.4) | 1083.2 (1079.0-1083.6) |
| idle PSS (MB) | - | 7.54 (7.50-7.62) | 376.6 (375.9-377.2) |
| start to listening (ms) | - | 3.99 (3.74-5.71) | 106.5 (100.6-116.3) |
| start to CDP connect (ms) | - | 9.52 (8.20-19.00) | 119.5 (118.1-128.6) |
| first new page (ms) | - | 165.9 (165.5-171.2) | 79.5 (75.9-91.5) |
| new page, median (ms) | - | 5.73 (5.50-6.90) | 62.9 (60.8-64.8) |
| pages verified | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| lightbrowser | 44.4 |
| chromium (chrome binary only) | 293.3 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 2.81 (2.77-2.84) | 5.35 (5.28-5.37) | 2.39 (2.34-2.42) | 2.07 (2.05-2.18) |

## Skipped

- fetch/cdp:lightpanda: no `lightpanda` binary on PATH (not available offline); budgets vs Lightpanda are not checked

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 10.258 | **pass** |
| binary size (stripped) | < 56 MB | 44.37 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 2.39 / 2.07 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 7.014 | **skip** - lightpanda not available on this machine |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 115.227 | **skip** - lightpanda not available on this machine |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 10.004 | **skip** - lightpanda not available on this machine |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 4.937 | **skip** - lightpanda not available on this machine |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 105.941 | **skip** - lightpanda not available on this machine |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 1.33 | **skip** - lightpanda not available on this machine |

## History

| file | sha | crawl cdp (s) | page median (ms) | peak RSS (MB) | idle RSS (MB) | binary (MB) |
|---|---|---|---|---|---|---|
| 2026-11-03-a060281.json | a060281 | 14.2 (13.8-15.0) | 100.0 (100.0-103.7) | 95.3 (95.0-95.9) | 9.95 (9.87-10.44) | 54.9 |
| 2026-11-03-ebe5f36.json | ebe5f36 | 4.94 (4.80-5.01) | 98.5 (96.0-100.0) | 105.9 (105.6-106.3) | 10.3 (10.2-10.4) | 44.4 |

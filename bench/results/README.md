# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-03-ca3f1b4.json` -- 2026-11-03, LightBrowser `ca3f1b4` (dirty tree), binary sha256 `573c54b604e5669d`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v230. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 2.63 (2.57-2.78) | 1.32 (1.22-1.40) | 11.4 (10.9-12.0) | 3.50 (3.44-3.61) | 5.15 (5.02-5.24) |
| page median (ms) | 50.0 (49.3-52.2) | 24.0 (22.8-25.4) | 228.3 (217.6-239.5) | 69.0 (68.6-71.9) | 103.0 (100.2-104.0) |
| page p90 (ms) | 61.3 (59.2-63.5) | 36.0 (31.4-38.5) | 248.2 (236.8-264.3) | 74.3 (72.6-77.6) | 107.7 (104.1-112.7) |
| peak RSS (MB) | 43.8 (40.5-46.7) | 42.2 (42.0-42.6) | 2840.7 (2825.5-2873.8) | 44.9 (38.5-46.7) | 31.4 (31.3-31.4) |
| peak PSS (MB, sampled) | - | 39.2 (39.0-39.8) | 687.4 (676.5-721.9) | - | 29.3 (29.1-29.4) |
| CPU (s) | 2.51 (2.45-2.64) | 0.99 (0.96-1.04) | 28.8 (28.0-30.5) | 1.37 (1.32-1.44) | 1.06 (0.93-1.12) |
| idle RSS (MB) | - | 10.2 (10.1-10.3) | 1081.9 (1047.8-1086.8) | - | 12.4 (12.4-12.4) |
| idle PSS (MB) | - | 7.48 (7.25-7.53) | 375.6 (368.9-377.2) | - | 10.5 (10.5-10.6) |
| start to listening (ms) | - | 4.23 (3.93-4.84) | 115.6 (108.4-228.4) | - | 17.0 (16.3-18.9) |
| start to CDP connect (ms) | - | 8.95 (8.75-20.82) | 128.5 (126.9-251.5) | - | 26.6 (24.9-29.1) |
| first new page (ms) | - | 121.2 (113.5-122.5) | 100.2 (86.7-113.2) | - | 4.41 (3.90-8.71) |
| new page, median (ms) | - | 7.12 (6.36-7.58) | 79.9 (77.5-85.9) | - | 4.55 (4.03-4.59) |
| pages verified | 50/50 | 50/50 | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| chromium (chrome binary only) | 293.3 |
| lightbrowser | 39.6 |
| lightpanda | 77.3 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 3.21 (3.16-3.40) | 5.70 (5.58-6.06) | 3.13 (3.10-3.19) | 2.47 (2.41-2.65) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 10.242 | **pass** |
| binary size (stripped) | < 40 MB | 39.63 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 3.13 / 2.47 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 2.635 vs 3.499 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 43.801 vs 44.926 | **pass** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 2.514 vs 1.367 | **fail** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 1.321 vs 5.149 | **pass** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 42.188 vs 31.434 | **fail** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.99 vs 1.06 | **pass** |

## History

| file | sha | crawl cdp (s) | page median (ms) | peak RSS (MB) | idle RSS (MB) | binary (MB) |
|---|---|---|---|---|---|---|
| 2026-11-03-a060281.json | a060281 | 14.2 (13.8-15.0) | 100.0 (100.0-103.7) | 95.3 (95.0-95.9) | 9.95 (9.87-10.44) | 54.9 |
| 2026-11-03-ca3f1b4.json | ca3f1b4 | 1.32 (1.22-1.40) | 24.0 (22.8-25.4) | 42.2 (42.0-42.6) | 10.2 (10.1-10.3) | 39.6 |
| 2026-11-03-ebe5f36.json | ebe5f36 | 4.94 (4.80-5.01) | 98.5 (96.0-100.0) | 105.9 (105.6-106.3) | 10.3 (10.2-10.4) | 44.4 |
| 2026-11-03-f2d6632.json | f2d6632 | 4.96 (4.89-5.05) | 99.9 (96.0-100.0) | 105.3 (104.4-105.6) | 10.0 (9.8-10.2) | 39.2 |

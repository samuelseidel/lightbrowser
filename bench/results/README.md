# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-03-7c755a8.json` -- 2026-11-03, LightBrowser `7c755a8` (dirty tree), binary sha256 `5b50c95d7beb399a`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v230. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 1.14 (1.10-1.24) | 1.04 (0.98-1.10) | 10.9 (10.7-11.0) | 3.36 (3.29-3.38) | 5.03 (5.00-5.18) |
| page median (ms) | 21.2 (21.0-23.1) | 19.4 (18.3-20.2) | 217.6 (212.7-220.1) | 66.6 (64.6-67.7) | 100.6 (100.2-102.5) |
| page p90 (ms) | 30.5 (29.6-33.5) | 28.7 (27.6-30.6) | 234.1 (226.2-239.1) | 69.2 (68.8-70.8) | 103.6 (102.7-111.3) |
| peak RSS (MB) | 23.3 (23.2-23.3) | 28.0 (26.8-28.4) | 2832.8 (2776.9-2836.7) | 23.2 (23.0-23.2) | 29.0 (28.9-29.1) |
| peak PSS (MB, sampled) | - | 25.2 (24.8-25.4) | 681.2 (678.2-687.4) | - | 27.1 (27.0-27.1) |
| CPU (s) | 0.92 (0.89-1.00) | 0.71 (0.67-0.73) | 27.6 (27.1-28.1) | 1.25 (1.19-1.25) | 0.97 (0.95-1.10) |
| idle RSS (MB) | - | 6.05 (5.90-6.20) | 1083.2 (1044.5-1084.5) | - | 12.1 (11.9-12.1) |
| idle PSS (MB) | - | 4.17 (4.02-4.32) | 376.8 (368.3-378.4) | - | 10.2 (10.1-10.2) |
| start to listening (ms) | - | 2.82 (2.72-2.93) | 115.9 (109.4-234.1) | - | 21.9 (16.6-26.9) |
| start to CDP connect (ms) | - | 7.79 (7.04-20.32) | 131.0 (121.5-266.7) | - | 31.5 (25.8-36.1) |
| first new page (ms) | - | 12.3 (11.7-18.1) | 92.3 (90.6-94.4) | - | 4.58 (4.05-7.01) |
| new page, median (ms) | - | 4.27 (4.12-4.85) | 76.1 (73.2-77.8) | - | 4.51 (4.39-4.71) |
| pages verified | 50/50 | 50/50 | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| lightbrowser | 39.7 |
| chromium (chrome binary only) | 293.3 |
| lightpanda | 77.3 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 2.78 (2.72-2.86) | 4.28 (4.21-4.33) | 1.42 (1.41-1.48) | 1.17 (1.15-1.20) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 6.055 | **pass** |
| binary size (stripped) | < 40 MB | 39.72 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.42 / 1.17 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 1.14 vs 3.358 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 23.273 vs 23.168 | **fail** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 0.922 vs 1.246 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 1.036 vs 5.028 | **pass** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 27.977 vs 29.047 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.71 vs 0.97 | **pass** |

## History

| file | sha | crawl cdp (s) | page median (ms) | peak RSS (MB) | idle RSS (MB) | binary (MB) |
|---|---|---|---|---|---|---|
| 2026-11-03-7c755a8.json | 7c755a8 | 1.04 (0.98-1.10) | 19.4 (18.3-20.2) | 28.0 (26.8-28.4) | 6.05 (5.90-6.20) | 39.7 |
| 2026-11-03-a060281.json | a060281 | 14.2 (13.8-15.0) | 100.0 (100.0-103.7) | 95.3 (95.0-95.9) | 9.95 (9.87-10.44) | 54.9 |
| 2026-11-03-ca3f1b4.json | ca3f1b4 | 1.32 (1.22-1.40) | 24.0 (22.8-25.4) | 42.2 (42.0-42.6) | 10.2 (10.1-10.3) | 39.6 |
| 2026-11-03-ebe5f36.json | ebe5f36 | 4.94 (4.80-5.01) | 98.5 (96.0-100.0) | 105.9 (105.6-106.3) | 10.3 (10.2-10.4) | 44.4 |
| 2026-11-03-f2d6632.json | f2d6632 | 4.96 (4.89-5.05) | 99.9 (96.0-100.0) | 105.3 (104.4-105.6) | 10.0 (9.8-10.2) | 39.2 |

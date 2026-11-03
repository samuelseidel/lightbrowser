# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-03-af2c264.json` -- 2026-11-03, LightBrowser `af2c264`, binary sha256 `efed4704af536560`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v233. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 1.08 (1.07-1.09) | 0.99 (0.94-1.01) | 10.6 (10.4-10.7) | 3.37 (3.31-3.39) | 5.04 (4.91-5.07) |
| page median (ms) | 20.1 (20.0-20.7) | 18.2 (16.9-18.7) | 210.5 (205.7-213.7) | 67.4 (64.7-67.5) | 100.5 (99.6-101.0) |
| page p90 (ms) | 29.0 (28.6-29.7) | 27.1 (26.4-27.7) | 226.0 (222.9-230.4) | 70.8 (70.2-72.0) | 105.8 (100.7-107.0) |
| peak RSS (MB) | 22.1 (22.0-22.3) | 27.0 (26.6-27.2) | 2851.6 (2813.9-2875.6) | 24.5 (24.5-24.6) | 31.4 (31.3-31.5) |
| peak PSS (MB, sampled) | - | 24.6 (24.4-25.0) | 692.8 (681.1-712.9) | - | 29.4 (29.4-29.5) |
| CPU (s) | 0.88 (0.87-0.89) | 0.68 (0.64-0.69) | 26.6 (25.6-26.9) | 1.23 (1.19-1.25) | 0.95 (0.86-0.96) |
| idle RSS (MB) | - | 6.17 (6.01-6.32) | 1087.3 (1080.9-1089.6) | - | 12.4 (12.3-12.4) |
| idle PSS (MB) | - | 4.31 (4.13-4.46) | 378.4 (377.4-379.4) | - | 10.6 (10.5-10.6) |
| start to listening (ms) | - | 2.86 (2.66-3.21) | 110.7 (102.8-117.7) | - | 19.0 (16.1-25.9) |
| start to CDP connect (ms) | - | 7.56 (6.71-18.21) | 123.3 (116.5-132.0) | - | 26.8 (23.8-35.0) |
| first new page (ms) | - | 12.6 (10.9-21.8) | 94.2 (79.5-104.0) | - | 4.79 (3.92-9.58) |
| new page, median (ms) | - | 4.52 (3.93-4.79) | 73.4 (69.8-80.0) | - | 3.94 (3.70-4.66) |
| pages verified | 50/50 | 50/50 | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| lightbrowser | 40.4 |
| chromium (chrome binary only) | 293.3 |
| lightpanda | 77.3 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 2.68 (2.67-2.79) | 7.15 (7.03-7.22) | 1.37 (1.32-1.43) | 1.11 (1.09-1.12) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 6.172 | **pass** |
| binary size (stripped) | < 50 MB | 40.39 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.37 / 1.11 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 1.082 vs 3.373 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 22.059 vs 24.484 | **pass** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 0.878 vs 1.229 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 0.989 vs 5.042 | **pass** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 26.953 vs 31.398 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.68 vs 0.95 | **pass** |

## History

| file | sha | crawl cdp (s) | page median (ms) | peak RSS (MB) | idle RSS (MB) | binary (MB) |
|---|---|---|---|---|---|---|
| 2026-11-03-7c755a8.json | 7c755a8 | 1.04 (0.98-1.10) | 19.4 (18.3-20.2) | 28.0 (26.8-28.4) | 6.05 (5.90-6.20) | 39.7 |
| 2026-11-03-a060281.json | a060281 | 14.2 (13.8-15.0) | 100.0 (100.0-103.7) | 95.3 (95.0-95.9) | 9.95 (9.87-10.44) | 54.9 |
| 2026-11-03-af2c264.json | af2c264 | 0.99 (0.94-1.01) | 18.2 (16.9-18.7) | 27.0 (26.6-27.2) | 6.17 (6.01-6.32) | 40.4 |
| 2026-11-03-ca3f1b4.json | ca3f1b4 | 1.32 (1.22-1.40) | 24.0 (22.8-25.4) | 42.2 (42.0-42.6) | 10.2 (10.1-10.3) | 39.6 |
| 2026-11-03-ebe5f36.json | ebe5f36 | 4.94 (4.80-5.01) | 98.5 (96.0-100.0) | 105.9 (105.6-106.3) | 10.3 (10.2-10.4) | 44.4 |
| 2026-11-03-f2d6632.json | f2d6632 | 4.96 (4.89-5.05) | 99.9 (96.0-100.0) | 105.3 (104.4-105.6) | 10.0 (9.8-10.2) | 39.2 |

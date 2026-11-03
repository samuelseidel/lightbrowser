# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-03-cf3ac89.json` -- 2026-11-03, LightBrowser `cf3ac89`, binary sha256 `c3790318ba95e25f`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v233. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 1.10 (1.02-1.22) | 0.96 (0.93-1.09) | 10.5 (10.1-10.7) | 3.34 (3.30-3.45) | 5.08 (4.99-5.09) |
| page median (ms) | 20.6 (19.2-23.1) | 17.5 (17.2-19.3) | 207.4 (201.1-214.2) | 66.2 (65.1-68.0) | 100.5 (100.2-101.9) |
| page p90 (ms) | 29.1 (27.3-31.8) | 27.5 (26.4-28.7) | 223.0 (210.3-233.5) | 72.0 (68.7-74.2) | 105.3 (103.2-108.3) |
| peak RSS (MB) | 21.9 (21.9-22.1) | 26.8 (26.6-27.2) | 2856.6 (2847.9-2865.7) | 23.2 (23.0-23.3) | 29.1 (28.9-29.1) |
| peak PSS (MB, sampled) | - | 24.7 (24.6-24.7) | 695.0 (689.8-698.6) | - | 27.1 (26.9-27.1) |
| CPU (s) | 0.90 (0.84-0.99) | 0.66 (0.65-0.72) | 26.0 (24.8-26.3) | 1.23 (1.17-1.30) | 0.96 (0.89-0.99) |
| idle RSS (MB) | - | 6.15 (6.05-6.27) | 1086.3 (1082.1-1088.4) | - | 12.1 (11.9-12.1) |
| idle PSS (MB) | - | 4.27 (4.20-4.39) | 377.9 (377.4-379.2) | - | 10.2 (10.1-10.2) |
| start to listening (ms) | - | 2.93 (2.78-3.74) | 118.5 (110.4-127.7) | - | 15.5 (13.5-18.5) |
| start to CDP connect (ms) | - | 7.56 (6.88-18.93) | 131.8 (122.4-146.5) | - | 23.2 (23.0-27.6) |
| first new page (ms) | - | 12.0 (10.9-16.3) | 81.8 (75.9-100.8) | - | 4.69 (3.85-6.69) |
| new page, median (ms) | - | 4.20 (4.09-4.72) | 74.7 (66.3-76.7) | - | 4.00 (3.70-4.95) |
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
| 2.68 (2.66-2.81) | 7.17 (7.15-7.39) | 1.36 (1.33-1.62) | 1.10 (1.10-1.13) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 6.152 | **pass** |
| binary size (stripped) | < 50 MB | 40.41 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.36 / 1.1 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 1.096 vs 3.343 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 21.918 vs 23.191 | **pass** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 0.898 vs 1.234 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 0.961 vs 5.076 | **pass** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 26.762 vs 29.062 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.66 vs 0.96 | **pass** |

## History

| file | sha | crawl cdp (s) | page median (ms) | peak RSS (MB) | idle RSS (MB) | binary (MB) |
|---|---|---|---|---|---|---|
| 2026-11-03-02ca87f.json | 02ca87f | 0.99 (0.93-1.01) | 18.0 (17.0-18.5) | 27.5 (27.4-27.9) | 7.13 (6.88-7.27) | 40.4 |
| 2026-11-03-7c755a8.json | 7c755a8 | 1.04 (0.98-1.10) | 19.4 (18.3-20.2) | 28.0 (26.8-28.4) | 6.05 (5.90-6.20) | 39.7 |
| 2026-11-03-a060281.json | a060281 | 14.2 (13.8-15.0) | 100.0 (100.0-103.7) | 95.3 (95.0-95.9) | 9.95 (9.87-10.44) | 54.9 |
| 2026-11-03-af2c264.json | af2c264 | 0.99 (0.94-1.01) | 18.2 (16.9-18.7) | 27.0 (26.6-27.2) | 6.17 (6.01-6.32) | 40.4 |
| 2026-11-03-ca3f1b4.json | ca3f1b4 | 1.32 (1.22-1.40) | 24.0 (22.8-25.4) | 42.2 (42.0-42.6) | 10.2 (10.1-10.3) | 39.6 |
| 2026-11-03-cf3ac89.json | cf3ac89 | 0.96 (0.93-1.09) | 17.5 (17.2-19.3) | 26.8 (26.6-27.2) | 6.15 (6.05-6.27) | 40.4 |
| 2026-11-03-ebe5f36.json | ebe5f36 | 4.94 (4.80-5.01) | 98.5 (96.0-100.0) | 105.9 (105.6-106.3) | 10.3 (10.2-10.4) | 44.4 |
| 2026-11-03-f2d6632.json | f2d6632 | 4.96 (4.89-5.05) | 99.9 (96.0-100.0) | 105.3 (104.4-105.6) | 10.0 (9.8-10.2) | 39.2 |

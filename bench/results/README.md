# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-03-02ca87f.json` -- 2026-11-03, LightBrowser `02ca87f`, binary sha256 `f08456d26e01922e`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v233. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 1.06 (1.00-1.07) | 0.99 (0.93-1.01) | 10.2 (10.2-10.4) | 3.31 (3.25-3.38) | 4.99 (4.90-5.01) |
| page median (ms) | 19.7 (18.3-20.6) | 18.0 (17.0-18.5) | 203.1 (202.1-206.2) | 64.9 (64.4-67.2) | 100.2 (99.2-100.3) |
| page p90 (ms) | 27.9 (26.2-29.7) | 27.4 (25.7-29.5) | 217.3 (213.9-223.0) | 69.0 (68.5-71.6) | 103.2 (101.0-104.3) |
| peak RSS (MB) | 23.1 (23.0-23.3) | 27.5 (27.4-27.9) | 2825.0 (2823.3-2836.8) | 23.2 (22.9-23.2) | 29.1 (29.0-29.1) |
| peak PSS (MB, sampled) | - | 25.3 (25.2-25.8) | 695.2 (687.9-701.3) | - | 27.1 (27.0-27.1) |
| CPU (s) | 0.86 (0.81-0.87) | 0.67 (0.63-0.72) | 24.8 (24.6-24.9) | 1.16 (1.13-1.23) | 0.88 (0.82-0.90) |
| idle RSS (MB) | - | 7.13 (6.88-7.27) | 1083.2 (1045.7-1083.4) | - | 12.1 (11.9-12.1) |
| idle PSS (MB) | - | 5.26 (5.01-5.40) | 376.4 (370.0-377.7) | - | 10.2 (10.1-10.2) |
| start to listening (ms) | - | 2.78 (2.71-3.07) | 110.2 (105.0-212.4) | - | 15.6 (15.4-20.7) |
| start to CDP connect (ms) | - | 7.72 (7.19-17.66) | 123.4 (120.5-232.6) | - | 24.7 (23.0-30.1) |
| first new page (ms) | - | 12.0 (11.0-19.9) | 83.0 (70.8-94.3) | - | 4.64 (3.92-7.04) |
| new page, median (ms) | - | 4.47 (4.03-4.90) | 70.1 (68.6-79.5) | - | 3.97 (3.77-4.05) |
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
| 2.73 (2.61-2.82) | 7.20 (7.02-8.08) | 1.38 (1.28-1.45) | 1.13 (1.09-1.15) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 7.129 | **pass** |
| binary size (stripped) | < 50 MB | 40.38 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.38 / 1.13 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 1.06 vs 3.315 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 23.086 vs 23.195 | **pass** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 0.86 vs 1.161 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 0.994 vs 4.99 | **pass** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 27.477 vs 29.051 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.67 vs 0.88 | **pass** |

## History

| file | sha | crawl cdp (s) | page median (ms) | peak RSS (MB) | idle RSS (MB) | binary (MB) |
|---|---|---|---|---|---|---|
| 2026-11-03-02ca87f.json | 02ca87f | 0.99 (0.93-1.01) | 18.0 (17.0-18.5) | 27.5 (27.4-27.9) | 7.13 (6.88-7.27) | 40.4 |
| 2026-11-03-7c755a8.json | 7c755a8 | 1.04 (0.98-1.10) | 19.4 (18.3-20.2) | 28.0 (26.8-28.4) | 6.05 (5.90-6.20) | 39.7 |
| 2026-11-03-a060281.json | a060281 | 14.2 (13.8-15.0) | 100.0 (100.0-103.7) | 95.3 (95.0-95.9) | 9.95 (9.87-10.44) | 54.9 |
| 2026-11-03-af2c264.json | af2c264 | 0.99 (0.94-1.01) | 18.2 (16.9-18.7) | 27.0 (26.6-27.2) | 6.17 (6.01-6.32) | 40.4 |
| 2026-11-03-ca3f1b4.json | ca3f1b4 | 1.32 (1.22-1.40) | 24.0 (22.8-25.4) | 42.2 (42.0-42.6) | 10.2 (10.1-10.3) | 39.6 |
| 2026-11-03-ebe5f36.json | ebe5f36 | 4.94 (4.80-5.01) | 98.5 (96.0-100.0) | 105.9 (105.6-106.3) | 10.3 (10.2-10.4) | 44.4 |
| 2026-11-03-f2d6632.json | f2d6632 | 4.96 (4.89-5.05) | 99.9 (96.0-100.0) | 105.3 (104.4-105.6) | 10.0 (9.8-10.2) | 39.2 |

# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-04-e5abb20.json` -- 2026-11-04, LightBrowser `e5abb20`, binary sha256 `29bbf36ad60fcda2`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v233. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 0.97 (0.90-0.98) | 0.67 (0.64-0.71) | 7.22 (7.13-7.39) | 1.05 (1.04-1.09) | 0.76 (0.73-0.80) |
| page median (ms) | 19.0 (18.0-19.6) | 12.9 (12.8-14.1) | 142.6 (141.3-148.6) | 20.4 (20.3-21.4) | 14.5 (14.4-15.5) |
| page p90 (ms) | 22.1 (19.7-22.8) | 16.1 (14.8-17.1) | 160.8 (160.0-162.2) | 22.6 (22.2-23.9) | 16.1 (15.7-18.4) |
| peak RSS (MB) | 23.8 (23.7-23.9) | 29.0 (28.7-29.2) | 2970.0 (2867.4-3005.1) | 24.4 (24.4-24.4) | 31.3 (31.3-31.3) |
| peak PSS (MB, sampled) | - | 26.0 (25.8-26.7) | 836.2 (736.7-858.4) | - | 28.8 (28.5-29.4) |
| CPU (s) | 0.85 (0.80-0.87) | 0.57 (0.54-0.59) | 20.3 (19.8-20.5) | 1.04 (1.03-1.08) | 0.77 (0.76-0.80) |
| idle RSS (MB) | - | 6.40 (6.33-6.46) | 1078.1 (1075.5-1078.4) | - | 12.4 (12.4-12.4) |
| idle PSS (MB) | - | 4.54 (4.44-4.56) | 373.0 (372.3-374.2) | - | 10.5 (10.5-10.5) |
| start to listening (ms) | - | 2.75 (2.56-2.85) | 114.2 (109.1-119.9) | - | 15.5 (15.3-18.6) |
| start to CDP connect (ms) | - | 7.68 (6.41-19.50) | 130.9 (124.0-133.4) | - | 23.4 (22.5-26.6) |
| first new page (ms) | - | 10.9 (10.9-16.7) | 88.5 (77.8-104.5) | - | 3.88 (3.66-6.01) |
| new page, median (ms) | - | 3.28 (3.00-3.52) | 66.8 (58.7-74.1) | - | 3.94 (3.73-4.22) |
| pages verified | 50/50 | 50/50 | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| lightbrowser | 41.2 |
| chromium (chrome binary only) | 293.3 |
| lightpanda | 77.3 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 2.58 (2.51-2.67) | 7.17 (7.11-7.18) | 1.29 (1.26-1.33) | 0.82 (0.82-0.83) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 6.398 | **pass** |
| binary size (stripped) | < 50 MB | 41.22 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.29 / 0.82 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 0.969 vs 1.047 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 23.785 vs 24.398 | **pass** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 0.852 vs 1.036 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 0.673 vs 0.756 | **pass** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 29.012 vs 31.289 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.57 vs 0.77 | **pass** |

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
| 2026-11-04-14b1a77-1.json | 14b1a77 | 0.67 (0.66-0.72) | 13.5 (12.8-14.0) | 29.0 (28.2-29.4) | 6.27 (6.04-6.39) | 41.2 |
| 2026-11-04-27e7f1a.json | 27e7f1a | 0.80 (0.80-0.87) | 15.6 (15.2-16.6) | 28.9 (28.5-29.4) | 6.08 (5.93-6.17) | 41.2 |
| 2026-11-04-5490a8e.json | 5490a8e | 0.82 (0.81-0.91) | 15.8 (15.6-18.1) | 28.0 (27.4-28.4) | 6.12 (6.09-6.37) | 40.8 |
| 2026-11-04-67c904b.json | 67c904b | 0.84 (0.80-0.91) | 15.9 (15.3-17.8) | 28.8 (28.7-28.9) | 6.05 (6.02-6.11) | 41.0 |
| 2026-11-04-a4fc4ca.json | a4fc4ca | 0.84 (0.81-0.90) | 16.1 (15.4-16.9) | 28.6 (28.6-29.0) | 5.92 (5.90-6.01) | 41.2 |
| 2026-11-04-cb0a5b0.json | cb0a5b0 | 0.82 (0.80-0.86) | 15.7 (15.3-16.6) | 27.3 (26.9-27.8) | 6.29 (6.18-6.55) | 40.5 |
| 2026-11-04-cce236a.json | cce236a | 0.85 (0.82-0.90) | 16.2 (15.8-17.0) | 28.6 (28.1-28.8) | 6.45 (6.26-6.52) | 40.8 |
| 2026-11-04-e5abb20.json | e5abb20 | 0.67 (0.64-0.71) | 12.9 (12.8-14.1) | 29.0 (28.7-29.2) | 6.40 (6.33-6.46) | 41.2 |

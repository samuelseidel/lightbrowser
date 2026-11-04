# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-04-cb0a5b0.json` -- 2026-11-04, LightBrowser `cb0a5b0` (dirty tree), binary sha256 `69ff00a8071288b5`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v233. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 0.93 (0.90-0.97) | 0.82 (0.80-0.86) | 7.21 (7.19-7.46) | 1.09 (1.05-1.14) | 0.78 (0.73-0.81) |
| page median (ms) | 17.9 (17.3-18.7) | 15.7 (15.3-16.6) | 145.0 (143.7-149.7) | 21.4 (20.1-21.9) | 15.0 (14.3-15.7) |
| page p90 (ms) | 22.6 (21.4-23.9) | 20.3 (19.2-21.7) | 159.8 (155.1-164.2) | 23.7 (23.2-24.8) | 17.2 (15.8-18.1) |
| peak RSS (MB) | 22.4 (22.0-22.4) | 27.3 (26.9-27.8) | 2862.1 (2845.9-2909.8) | 24.4 (24.4-24.5) | 31.4 (31.3-31.5) |
| peak PSS (MB, sampled) | - | 25.1 (24.4-25.4) | 726.1 (705.7-777.6) | - | 29.4 (28.8-29.4) |
| CPU (s) | 0.82 (0.80-0.85) | 0.62 (0.60-0.64) | 20.4 (19.8-20.8) | 1.07 (1.03-1.12) | 0.80 (0.76-0.81) |
| idle RSS (MB) | - | 6.29 (6.18-6.55) | 1079.4 (1073.5-1085.7) | - | 12.4 (12.4-12.4) |
| idle PSS (MB) | - | 4.45 (4.34-4.73) | 372.9 (372.1-376.5) | - | 10.6 (10.5-10.6) |
| start to listening (ms) | - | 2.70 (2.42-2.75) | 111.4 (107.4-125.1) | - | 15.9 (15.3-18.5) |
| start to CDP connect (ms) | - | 7.62 (6.29-17.97) | 124.4 (118.2-138.2) | - | 24.4 (22.5-28.8) |
| first new page (ms) | - | 11.2 (10.8-17.0) | 78.8 (75.5-84.9) | - | 3.92 (3.84-5.12) |
| new page, median (ms) | - | 3.98 (3.91-4.49) | 67.2 (64.3-73.4) | - | 3.92 (3.64-4.36) |
| pages verified | 50/50 | 50/50 | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| lightbrowser | 40.5 |
| chromium (chrome binary only) | 293.3 |
| lightpanda | 77.3 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 2.58 (2.49-2.75) | 7.13 (7.04-7.18) | 1.31 (1.29-1.42) | 1.09 (1.08-1.11) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 6.289 | **pass** |
| binary size (stripped) | < 50 MB | 40.51 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.31 / 1.09 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 0.934 vs 1.093 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 22.406 vs 24.445 | **pass** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 0.824 vs 1.073 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 0.82 vs 0.776 | **fail** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 27.254 vs 31.379 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.62 vs 0.8 | **pass** |

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
| 2026-11-04-cb0a5b0.json | cb0a5b0 | 0.82 (0.80-0.86) | 15.7 (15.3-16.6) | 27.3 (26.9-27.8) | 6.29 (6.18-6.55) | 40.5 |

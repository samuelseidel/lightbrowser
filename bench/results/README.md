# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-04-cce236a.json` -- 2026-11-04, LightBrowser `cce236a`, binary sha256 `2edf2cd5051ba20e`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v233. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 0.98 (0.95-1.01) | 0.85 (0.82-0.90) | 7.21 (6.77-7.35) | 1.09 (1.05-1.13) | 0.76 (0.74-0.80) |
| page median (ms) | 18.8 (18.4-19.6) | 16.2 (15.8-17.0) | 143.7 (134.9-144.1) | 20.9 (20.5-22.2) | 15.0 (14.4-15.4) |
| page p90 (ms) | 24.1 (22.6-24.4) | 20.7 (19.9-22.7) | 159.1 (152.3-165.2) | 23.7 (22.4-24.7) | 16.7 (15.9-18.0) |
| peak RSS (MB) | 23.3 (23.2-23.6) | 28.6 (28.1-28.8) | 2868.4 (2857.4-2881.2) | 23.2 (22.9-23.3) | 29.0 (28.9-29.1) |
| peak PSS (MB, sampled) | - | 25.8 (25.4-26.3) | 714.8 (700.0-760.2) | - | 26.7 (26.4-26.9) |
| CPU (s) | 0.86 (0.83-0.88) | 0.63 (0.63-0.65) | 20.1 (19.1-20.4) | 1.07 (1.04-1.11) | 0.77 (0.76-0.80) |
| idle RSS (MB) | - | 6.45 (6.26-6.52) | 1086.3 (1077.3-1087.8) | - | 12.1 (12.0-12.1) |
| idle PSS (MB) | - | 4.56 (4.35-4.62) | 376.3 (375.3-378.3) | - | 10.1 (10.0-10.2) |
| start to listening (ms) | - | 2.80 (2.57-3.03) | 108.7 (99.5-114.6) | - | 16.4 (15.4-21.4) |
| start to CDP connect (ms) | - | 7.75 (6.78-17.93) | 124.9 (114.2-129.3) | - | 25.5 (23.0-30.3) |
| first new page (ms) | - | 11.8 (10.7-15.8) | 95.0 (72.0-98.5) | - | 4.36 (3.79-4.62) |
| new page, median (ms) | - | 4.20 (3.83-4.42) | 68.5 (67.3-76.0) | - | 3.79 (3.77-4.42) |
| pages verified | 50/50 | 50/50 | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| lightbrowser | 40.8 |
| chromium (chrome binary only) | 293.3 |
| lightpanda | 77.3 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 2.55 (2.48-2.62) | 7.12 (7.02-7.14) | 1.30 (1.25-1.33) | 1.09 (1.08-1.09) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 6.449 | **pass** |
| binary size (stripped) | < 50 MB | 40.76 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.3 / 1.09 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 0.982 vs 1.09 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 23.301 vs 23.18 | **fail** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 0.864 vs 1.068 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 0.851 vs 0.758 | **fail** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 28.574 vs 29.008 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.63 vs 0.77 | **pass** |

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
| 2026-11-04-cce236a.json | cce236a | 0.85 (0.82-0.90) | 16.2 (15.8-17.0) | 28.6 (28.1-28.8) | 6.45 (6.26-6.52) | 40.8 |

# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-04-67c904b.json` -- 2026-11-04, LightBrowser `67c904b` (dirty tree), binary sha256 `8876b7756e8ac147`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v233. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 0.99 (0.99-1.06) | 0.84 (0.80-0.91) | 7.13 (6.93-7.25) | 1.11 (1.07-1.14) | 0.75 (0.74-0.78) |
| page median (ms) | 19.3 (18.9-20.8) | 15.9 (15.3-17.8) | 142.8 (138.7-143.5) | 21.4 (20.8-22.2) | 14.6 (14.3-15.0) |
| page p90 (ms) | 24.4 (23.5-25.2) | 20.3 (19.6-22.2) | 156.4 (149.1-160.0) | 24.7 (23.0-25.6) | 16.7 (15.8-17.7) |
| peak RSS (MB) | 23.9 (23.8-23.9) | 28.8 (28.7-28.9) | 2902.1 (2844.5-3052.3) | 24.4 (24.4-24.4) | 31.3 (31.2-31.4) |
| peak PSS (MB, sampled) | - | 26.2 (25.1-26.6) | 730.4 (705.8-890.4) | - | 28.5 (28.1-29.0) |
| CPU (s) | 0.86 (0.86-0.92) | 0.64 (0.61-0.66) | 20.5 (19.9-20.7) | 1.10 (1.06-1.12) | 0.79 (0.75-0.80) |
| idle RSS (MB) | - | 6.05 (6.02-6.11) | 1091.4 (1083.8-1093.8) | - | 12.4 (12.3-12.4) |
| idle PSS (MB) | - | 4.17 (4.15-4.23) | 380.7 (379.3-381.3) | - | 10.5 (10.5-10.5) |
| start to listening (ms) | - | 2.91 (2.61-3.33) | 109.7 (108.9-130.9) | - | 16.3 (15.6-26.2) |
| start to CDP connect (ms) | - | 7.31 (7.15-17.87) | 122.5 (121.5-150.4) | - | 24.7 (23.6-33.9) |
| first new page (ms) | - | 11.2 (11.1-16.3) | 80.3 (73.0-91.8) | - | 3.75 (3.67-6.58) |
| new page, median (ms) | - | 4.14 (3.71-4.61) | 69.1 (65.3-71.1) | - | 3.78 (3.73-5.22) |
| pages verified | 50/50 | 50/50 | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| lightbrowser | 41.0 |
| chromium (chrome binary only) | 293.3 |
| lightpanda | 77.3 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 2.55 (2.53-2.59) | 7.03 (7.01-7.05) | 1.30 (1.26-1.34) | 1.08 (1.07-1.09) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 6.055 | **pass** |
| binary size (stripped) | < 50 MB | 40.95 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.3 / 1.08 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 0.993 vs 1.111 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 23.852 vs 24.402 | **pass** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 0.864 vs 1.104 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 0.837 vs 0.746 | **fail** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 28.805 vs 31.32 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.64 vs 0.79 | **pass** |

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
| 2026-11-04-5490a8e.json | 5490a8e | 0.82 (0.81-0.91) | 15.8 (15.6-18.1) | 28.0 (27.4-28.4) | 6.12 (6.09-6.37) | 40.8 |
| 2026-11-04-67c904b.json | 67c904b | 0.84 (0.80-0.91) | 15.9 (15.3-17.8) | 28.8 (28.7-28.9) | 6.05 (6.02-6.11) | 41.0 |
| 2026-11-04-cb0a5b0.json | cb0a5b0 | 0.82 (0.80-0.86) | 15.7 (15.3-16.6) | 27.3 (26.9-27.8) | 6.29 (6.18-6.55) | 40.5 |
| 2026-11-04-cce236a.json | cce236a | 0.85 (0.82-0.90) | 16.2 (15.8-17.0) | 28.6 (28.1-28.8) | 6.45 (6.26-6.52) | 40.8 |

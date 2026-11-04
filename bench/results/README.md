# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-04-14b1a77.json` -- 2026-11-04, LightBrowser `14b1a77`, binary sha256 `29bbf36ad60fcda2`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v233. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 0.97 (0.90-1.00) | 0.67 (0.66-0.72) | 7.31 (7.07-7.48) | 1.09 (1.02-1.12) | 0.77 (0.75-0.80) |
| page median (ms) | 19.1 (17.8-19.5) | 13.5 (12.8-14.0) | 146.3 (138.5-147.0) | 21.0 (20.2-22.0) | 15.1 (14.1-15.3) |
| page p90 (ms) | 22.5 (20.6-24.3) | 15.8 (15.1-17.9) | 165.5 (156.7-171.8) | 23.9 (21.2-25.3) | 17.7 (16.0-18.4) |
| peak RSS (MB) | 23.7 (23.6-24.1) | 29.0 (28.2-29.4) | 2865.4 (2818.8-2989.4) | 24.4 (24.3-24.4) | 31.3 (31.2-31.4) |
| peak PSS (MB, sampled) | - | 26.1 (25.7-26.7) | 733.7 (691.4-862.4) | - | 28.8 (28.1-29.1) |
| CPU (s) | 0.85 (0.80-0.88) | 0.57 (0.56-0.59) | 20.6 (20.0-21.2) | 1.08 (1.02-1.12) | 0.78 (0.77-0.81) |
| idle RSS (MB) | - | 6.27 (6.04-6.39) | 1077.3 (1073.2-1083.2) | - | 12.4 (12.4-12.4) |
| idle PSS (MB) | - | 4.38 (4.15-4.49) | 373.3 (372.7-374.5) | - | 10.5 (10.5-10.5) |
| start to listening (ms) | - | 2.88 (2.61-3.27) | 116.3 (103.1-121.9) | - | 15.6 (15.3-18.6) |
| start to CDP connect (ms) | - | 7.99 (6.57-18.47) | 130.0 (127.2-142.9) | - | 25.1 (22.3-27.5) |
| first new page (ms) | - | 11.6 (10.8-16.3) | 87.9 (75.2-112.6) | - | 4.17 (3.78-5.08) |
| new page, median (ms) | - | 3.54 (3.35-3.95) | 67.7 (61.6-72.1) | - | 4.04 (3.81-4.38) |
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
| 2.56 (2.52-2.81) | 7.20 (7.11-7.35) | 1.30 (1.28-1.34) | 0.82 (0.82-0.86) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 6.27 | **pass** |
| binary size (stripped) | < 50 MB | 41.22 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.3 / 0.82 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 0.968 vs 1.093 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 23.723 vs 24.355 | **pass** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 0.852 vs 1.084 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 0.67 vs 0.765 | **pass** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 29.016 vs 31.293 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.57 vs 0.78 | **pass** |

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
| 2026-11-04-14b1a77.json | 14b1a77 | 0.67 (0.66-0.72) | 13.5 (12.8-14.0) | 29.0 (28.2-29.4) | 6.27 (6.04-6.39) | 41.2 |
| 2026-11-04-27e7f1a.json | 27e7f1a | 0.80 (0.80-0.87) | 15.6 (15.2-16.6) | 28.9 (28.5-29.4) | 6.08 (5.93-6.17) | 41.2 |
| 2026-11-04-5490a8e.json | 5490a8e | 0.82 (0.81-0.91) | 15.8 (15.6-18.1) | 28.0 (27.4-28.4) | 6.12 (6.09-6.37) | 40.8 |
| 2026-11-04-67c904b.json | 67c904b | 0.84 (0.80-0.91) | 15.9 (15.3-17.8) | 28.8 (28.7-28.9) | 6.05 (6.02-6.11) | 41.0 |
| 2026-11-04-a4fc4ca.json | a4fc4ca | 0.84 (0.81-0.90) | 16.1 (15.4-16.9) | 28.6 (28.6-29.0) | 5.92 (5.90-6.01) | 41.2 |
| 2026-11-04-cb0a5b0.json | cb0a5b0 | 0.82 (0.80-0.86) | 15.7 (15.3-16.6) | 27.3 (26.9-27.8) | 6.29 (6.18-6.55) | 40.5 |
| 2026-11-04-cce236a.json | cce236a | 0.85 (0.82-0.90) | 16.2 (15.8-17.0) | 28.6 (28.1-28.8) | 6.45 (6.26-6.52) | 40.8 |

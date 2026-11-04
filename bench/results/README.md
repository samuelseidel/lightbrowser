# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-04-5490a8e.json` -- 2026-11-04, LightBrowser `5490a8e`, binary sha256 `f75601c43da07b80`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v233. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 0.95 (0.94-1.01) | 0.82 (0.81-0.91) | 7.02 (6.68-7.17) | 1.06 (1.04-1.08) | 0.74 (0.73-0.80) |
| page median (ms) | 18.2 (17.9-19.5) | 15.8 (15.6-18.1) | 140.8 (132.6-145.0) | 20.6 (20.4-20.9) | 14.3 (14.2-15.0) |
| page p90 (ms) | 22.7 (22.0-25.0) | 19.8 (19.0-21.5) | 151.8 (148.8-158.9) | 23.2 (21.8-24.2) | 16.1 (15.4-19.5) |
| peak RSS (MB) | 22.7 (22.5-23.2) | 28.0 (27.4-28.4) | 2882.6 (2869.5-2997.0) | 24.4 (24.3-24.4) | 31.3 (31.3-31.4) |
| peak PSS (MB, sampled) | - | 25.3 (25.1-25.7) | 747.8 (727.5-819.2) | - | 28.9 (28.5-29.3) |
| CPU (s) | 0.83 (0.82-0.89) | 0.63 (0.63-0.66) | 19.9 (19.1-20.3) | 1.05 (1.03-1.07) | 0.76 (0.75-0.85) |
| idle RSS (MB) | - | 6.12 (6.09-6.37) | 1083.9 (1078.5-1087.2) | - | 12.4 (12.4-12.4) |
| idle PSS (MB) | - | 4.22 (4.19-4.47) | 377.6 (376.7-378.4) | - | 10.5 (10.5-10.5) |
| start to listening (ms) | - | 2.64 (2.58-2.77) | 109.2 (100.5-122.0) | - | 15.6 (15.3-18.3) |
| start to CDP connect (ms) | - | 6.72 (6.15-17.75) | 123.7 (112.1-134.3) | - | 24.5 (22.2-26.0) |
| first new page (ms) | - | 10.9 (10.8-16.3) | 83.3 (78.5-98.8) | - | 3.84 (3.73-8.40) |
| new page, median (ms) | - | 3.93 (3.80-4.46) | 67.9 (61.2-71.0) | - | 4.04 (3.80-4.48) |
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
| 2.60 (2.52-2.64) | 7.07 (7.04-7.15) | 1.30 (1.28-1.32) | 1.10 (1.10-1.13) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 6.125 | **pass** |
| binary size (stripped) | < 50 MB | 40.76 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.3 / 1.1 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 0.949 vs 1.061 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 22.684 vs 24.406 | **pass** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 0.826 vs 1.049 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 0.825 vs 0.741 | **fail** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 27.969 vs 31.285 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.63 vs 0.76 | **pass** |

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
| 2026-11-04-cb0a5b0.json | cb0a5b0 | 0.82 (0.80-0.86) | 15.7 (15.3-16.6) | 27.3 (26.9-27.8) | 6.29 (6.18-6.55) | 40.5 |
| 2026-11-04-cce236a.json | cce236a | 0.85 (0.82-0.90) | 16.2 (15.8-17.0) | 28.6 (28.1-28.8) | 6.45 (6.26-6.52) | 40.8 |

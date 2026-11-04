# Bench results

Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. Loopback corpus, sequential crawl, see `bench/README.md` for definitions.

Latest: `2026-11-04-5294bae.json` -- 2026-11-04, LightBrowser `5294bae` (dirty tree), binary sha256 `68a51a20a1def860`, 5 runs x 50 pages.

Machine: Intel(R) Xeon(R) Processor, 4 cores, 16091 MB RAM, kernel 6.18.44-fc-v233. Chromium `Google Chrome for Testing 153.0.8010.12`, Playwright (python) 1.52.0, rustc 1.97.0 (2d8144b78 2026-07-07). Lightpanda `1.1.0-nightly.11582+592b76832` (binary sha256 `71ec3bd077e29a13f14b76a97a4a19d1a0fab8153b9af9490d105db4a4ee5118`), run with telemetry disabled.

## Crawl

| metric | fetch:lightbrowser | cdp:lightbrowser | cdp:chromium | fetch:lightpanda | cdp:lightpanda |
|---|---|---|---|---|---|
| crawl total (s) | 1.16 (1.10-1.20) | 0.79 (0.69-0.86) | 8.54 (8.45-8.93) | 1.36 (1.27-1.44) | 0.84 (0.83-0.91) |
| page median (ms) | 22.2 (21.9-23.0) | 15.1 (13.6-15.8) | 170.2 (169.0-171.7) | 26.0 (23.6-27.5) | 16.3 (16.0-17.4) |
| page p90 (ms) | 27.4 (24.4-29.1) | 19.5 (15.8-22.0) | 188.3 (185.1-208.6) | 30.8 (29.2-34.3) | 19.3 (18.8-21.2) |
| peak RSS (MB) | 21.7 (21.5-21.9) | 26.6 (26.5-27.2) | 2895.5 (2836.7-3030.4) | 23.2 (23.0-23.4) | 29.1 (28.9-29.1) |
| peak PSS (MB, sampled) | - | 24.0 (23.6-24.1) | 709.2 (700.1-878.2) | - | 26.9 (26.8-26.9) |
| CPU (s) | 1.00 (0.97-1.03) | 0.65 (0.60-0.70) | 23.9 (23.5-24.9) | 1.29 (1.22-1.40) | 0.83 (0.81-0.88) |
| idle RSS (MB) | - | 5.95 (5.90-6.01) | 1082.5 (1078.9-1086.6) | - | 12.1 (11.9-12.1) |
| idle PSS (MB) | - | 4.04 (4.00-4.07) | 377.8 (375.9-377.9) | - | 10.1 (10.0-10.2) |
| start to listening (ms) | - | 3.62 (3.21-5.96) | 120.2 (114.5-125.9) | - | 16.1 (15.7-20.7) |
| start to CDP connect (ms) | - | 8.78 (8.09-25.46) | 138.3 (127.5-144.0) | - | 28.3 (23.8-30.5) |
| first new page (ms) | - | 13.8 (11.6-35.4) | 94.7 (81.0-109.2) | - | 6.15 (4.00-8.54) |
| new page, median (ms) | - | 3.88 (3.11-4.28) | 77.9 (74.9-95.7) | - | 4.26 (4.01-4.47) |
| pages verified | 50/50 | 50/50 | 50/50 | 50/50 | 50/50 |

Fetch rows time one process per page, so they include process start. RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.

## Binary size

| target | MB |
|---|---|
| lightbrowser | 42.2 |
| chromium (chrome binary only) | 293.3 |
| lightpanda | 77.3 |

## Realm creation (snapshot, release, ms)

| engine | define | first realm | next realm |
|---|---|---|---|
| 2.82 (2.72-2.92) | 7.48 (7.31-7.62) | 1.42 (1.38-1.48) | 0.85 (0.83-0.92) |

## Budgets (docs/ARCHITECTURE.md)

| budget | target | measured | result |
|---|---|---|---|
| idle RSS (serve, no client, 1 s after listen) | < 15 MB | 5.953 | **pass** |
| binary size (stripped) | < 50 MB | 42.18 | **pass** |
| realm creation (first / next, snapshot) | < 5 ms | 1.42 / 0.85 | **pass** - lb-webapi example realm_bench |
| disk writes (default run) | none | files 0/0, write_bytes 0 | **pass** - files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl |
| crawl wall time vs Lightpanda (fetch) | <= Lightpanda | 1.156 vs 1.356 | **pass** |
| peak RSS vs Lightpanda (fetch) | <= Lightpanda | 21.746 vs 23.219 | **pass** |
| CPU time vs Lightpanda (fetch) | <= Lightpanda | 1.003 vs 1.292 | **pass** |
| crawl wall time vs Lightpanda (cdp) | <= Lightpanda | 0.792 vs 0.837 | **pass** |
| peak RSS vs Lightpanda (cdp) | <= Lightpanda | 26.633 vs 29.113 | **pass** |
| CPU time vs Lightpanda (cdp) | <= Lightpanda | 0.65 vs 0.83 | **pass** |

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
| 2026-11-04-5294bae.json | 5294bae | 0.73 (0.68-0.75) | 14.2 (13.7-14.9) | 26.9 (26.3-27.1) | 5.97 (5.93-6.04) | 42.2 |
| 2026-11-04-5294bae.json | 5294bae | 0.79 (0.69-0.86) | 15.1 (13.6-15.8) | 26.6 (26.5-27.2) | 5.95 (5.90-6.01) | 42.2 |
| 2026-11-04-5490a8e.json | 5490a8e | 0.82 (0.81-0.91) | 15.8 (15.6-18.1) | 28.0 (27.4-28.4) | 6.12 (6.09-6.37) | 40.8 |
| 2026-11-04-67c904b.json | 67c904b | 0.84 (0.80-0.91) | 15.9 (15.3-17.8) | 28.8 (28.7-28.9) | 6.05 (6.02-6.11) | 41.0 |
| 2026-11-04-7d6707e.json | 7d6707e | 0.71 (0.67-0.75) | 13.6 (13.2-14.5) | 28.5 (28.4-28.8) | 6.25 (6.23-6.34) | 41.7 |
| 2026-11-04-7d6707e.json | 7d6707e | 0.76 (0.73-0.99) | 15.0 (14.3-16.3) | 28.6 (28.4-29.1) | 6.41 (6.30-6.45) | 41.7 |
| 2026-11-04-a4fc4ca.json | a4fc4ca | 0.84 (0.81-0.90) | 16.1 (15.4-16.9) | 28.6 (28.6-29.0) | 5.92 (5.90-6.01) | 41.2 |
| 2026-11-04-a57aee9.json | a57aee9 | 0.64 (0.64-0.72) | 12.6 (12.3-14.1) | 28.4 (28.2-28.7) | 6.36 (6.21-6.40) | 41.5 |
| 2026-11-04-a57aee9.json | a57aee9 | 0.66 (0.64-0.75) | 12.9 (12.6-14.3) | 28.2 (28.0-28.5) | 6.34 (6.23-6.43) | 41.5 |
| 2026-11-04-cb0a5b0.json | cb0a5b0 | 0.82 (0.80-0.86) | 15.7 (15.3-16.6) | 27.3 (26.9-27.8) | 6.29 (6.18-6.55) | 40.5 |
| 2026-11-04-cce236a.json | cce236a | 0.85 (0.82-0.90) | 16.2 (15.8-17.0) | 28.6 (28.1-28.8) | 6.45 (6.26-6.52) | 40.8 |
| 2026-11-04-e5abb20.json | e5abb20 | 0.67 (0.64-0.71) | 12.9 (12.8-14.1) | 29.0 (28.7-29.2) | 6.40 (6.33-6.46) | 41.2 |
| 2026-11-04-fca2955.json | fca2955 | 0.72 (0.68-0.73) | 14.1 (13.4-14.4) | 28.5 (28.3-29.0) | 6.63 (6.46-6.84) | 42.1 |
| 2026-11-04-fca2955.json | fca2955 | 0.70 (0.68-0.73) | 13.6 (13.3-14.5) | 26.6 (26.1-26.9) | 5.89 (5.79-6.15) | 42.1 |

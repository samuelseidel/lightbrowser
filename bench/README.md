# bench

Placeholder. Milestone: **M6 Performance**.

Purpose: check the budgets in `docs/ARCHITECTURE.md` (wall time, peak RSS,
CPU time, binary size) against Lightpanda and headless Chrome.

## Methodology

- Workload: Lightpanda's public crawl benchmark, run unchanged against pages served from a local web server, so results are comparable and no external site is hit.
- Metrics per run: wall time, peak RSS, CPU time (user + sys), stripped binary size.
- Idle RSS and context/realm creation time are measured separately.
- Compared against: Lightpanda nightly and headless Chrome, on the same machine, same pages, same network conditions.
- Several runs per target; report median and spread. Pin the versions of every target.
- Results are committed to `bench/results/` with the machine description and commands used.

Target: LightBrowser is at or below Lightpanda on wall time, peak RSS and CPU
time. The other budgets (idle RSS, binary size, realm creation) are absolute
numbers in `docs/ARCHITECTURE.md`.

# Verify the Chrome baseline

Use the current worktree as the candidate. It contains pending V8 foreground task, compiled-script cache, hot-symbol and benchmark changes on top of `ec1c096`. Record the revision and whether the tree is dirty for each run.

1. Review pending diffs, especially isolate teardown, cache partitioning and benchmark methodology.
2. Run locked Chrome workspace tests and clippy, then Firefox package tests and clippy with only the Firefox feature enabled.
3. Build the Chrome binary and run the Playwright suite, WPT subset and local detection bench. Record counts and skipped cases.
4. Build release on the Linux benchmark host and capture the results required by task 04. Update the roadmap and stale repository summaries when supported by the results.

Done when the checks and known gaps are recorded against a reproducible revision.

### Execution log

- Darwin arm64, Cargo 1.99.1, dirty worktree at `ec1c096`.
- `cargo fmt --all --check` and `git diff --check` passed.
- Started `cargo test --locked --workspace`. Playwright dependencies and a standalone debug binary were absent at initial inspection.
- The first two workspace attempts stopped before tests ran. A cached `lb-layout` build script tried to read `profiles/local_fonts`, which the current script does not reference. Cleaned the `lb-layout` build artifacts and restarted the locked workspace suite. Follow the retry log at `/tmp/lightbrowser-baseline-test.log`.
- The retry progressed past font setup, then stopped compiling the WebGL pipeline test because Cargo reported a missing `Fixed::offset` field. Next diagnostic is to rerun with a fresh Cargo target directory to distinguish cached artifacts from a source failure. Workspace results remain unverified.
- Reproduced the compiler error with the existing target directory. With a fresh target directory, `cargo test --locked -p lb-gl` passed all four tests, including the pipeline test, without any source changes.
- The fresh workspace test run compiled successfully and started executing tests. It was interrupted when the volume ran low on space. The full workspace result is pending.
- A later Chrome build in the working checkout passed all 44 Playwright tests. The locked workspace test run reached the WPT subset. Firefox-only package tests passed for `lightbrowser` and `lb-js`.
- The upstream integration branch at `99a2d00` passed all 28 tests in its Playwright suite on Darwin arm64.

### Verification on 2026-11-19

- Dirty candidate based on `ec1c096`, Darwin arm64 and a source copy on
  `rtx-dev` in `/home/dev/lb-checks/codex-2026-11-19`.
- Fixed an isolate-address reuse race in task-queue cleanup (ADR 0038).
  The full 37-test V8 suite passed 20 consecutive runs after the fix.
- Chrome workspace clippy and Firefox package clippy passed. Firefox
  package tests passed all eight tests. Python SDK unit tests passed 69
  cases, detection-bench unit tests passed 43.
- Updated navigator and fingerprint-report probes to wait for asynchronous
  speech voices. Five focused Playwright frame/report/worker tests passed.
- Linux release navigator, human-input and CDP host tests passed 13 cases.
- Loopback T-010 on Linux passed fingerprint acceptance (`fp=0`), and
  human input passed behavior acceptance (`fp=0`, `beh=0`, human verdict).
  Stock Playwright input retained a bot verdict (`beh=92.3`).
- The Linux workspace run passed its WPT gate (184.83 s) and then stopped
  at `lb-webapi` snapshot restoration. Callable-object callbacks were missing
  from replay, shifting later callback indices. All four snapshot regressions
  passed after adding the missing callback replay entry.
  The Mac run also saw a real-clock hold assertion fail under load; the
  same human-input suite passed on Linux.
  The final full Playwright rerun passed all 44 tests after the report
  probe fix. The PDF test passed without changing its timeout.
- ADR 0041 removes the Lightpanda comparison gate by user decision. The
  five-by-50 Linux run met idle RSS (5.906 MB), size (43.44 MB), and disk
  write budgets. Realm creation was skipped because the example was absent.

### Verification on 2026-11-20

- Snapshot callback replay fix passed all four existing snapshot tests.
- Real-clock human replay now preserves each generated inter-event gap
  after the prior actual dispatch (ADR 0042). All three human-input tests,
  binary build and Chrome workspace clippy passed.
- Rebuilt debug browser against the loopback T-010 scenario on this Mac.
  Human input passed with `fp=0`, `beh=0`, human verdict. Stock Playwright
  input passed fingerprint acceptance (`fp=0`) and was still classified
  as a bot (`beh=91`).
- Firefox package clippy passed again. The combined locked workspace run
  completed successfully, including the WPT gate, snapshot regression and
  doc tests. The subsequent input-spacing change passed its targeted suite
  and workspace clippy.

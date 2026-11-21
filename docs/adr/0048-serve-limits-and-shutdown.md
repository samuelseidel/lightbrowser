# ADR 0048: `serve` and `mcp` as long-lived services: limits and shutdown

Status: Accepted (2026-11-16).

## Context

`serve` and `mcp` were built for one test run at a time. Run as services
(under systemd, a CI wrapper or a pool launcher) they had no limits on
connections, pages or the bytes queued for a slow client. `GET /json/new`
opened a page, which Chrome has refused since M111. SIGINT and SIGTERM
killed the process without closing pages. `tracing` events had no
subscriber, so they were lost. `--version` did not name a commit. A
release build aborts on panic (`panic = "abort"`) without saying where.

## Decision

- Limits are `lb_cdp::ServerConfig` fields with generous defaults:
  `max_connections` 512 (more connections get HTTP 503), `max_pages` 1000
  (`Target.createTarget` past it fails with CDP error -32000, `/json/new`
  with 503) and `max_queued_bytes` 256 MiB per WebSocket client. `serve`
  has `--max-pages` and `--max-connections`. The queue bound has no flag.
- The queue bound counts the bytes queued but not yet written. A message
  that would pass the bound closes that connection, with one line on
  stderr, unless nothing else is queued: then a single large reply still
  goes out, so a client that does read is never cut off for one big
  message.
- WebSocket limits are named constants at tungstenite's defaults (64 MiB
  per message, 16 MiB per frame), so the effective limits are unchanged.
- `/json/new` needs PUT. Other methods get Chrome 153's 405 and its text.
  The Host-header guard (IP literal, `localhost` or `*.localhost`) was
  already in place and is now tested.
- Signals use a self-pipe. The handler only writes one byte to a
  non-blocking pipe, or calls `_exit(130)` on the second signal. A
  dedicated thread blocks in `read`; on the first byte it wakes the
  server, which closes every page and context and returns. Dropping the
  host joins the worker threads, and the process exits 0. The same thread
  then waits `SHUTDOWN_DEADLINE` (5 s) and calls `_exit(130)` if the
  process is still alive. A script that never yields keeps its worker
  busy, and `join` would wait forever. Nothing runs while no signal
  arrives. `mcp` treats the signal like a closed stdin, even while a tool
  call runs.
- `--log error|warn|info|debug` on `serve` and `mcp` installs a small
  stderr subscriber written in the binary (no new crate). With no flag,
  no subscriber is set and output does not change.
- The binary's panic hook prints one line (version, family, commit,
  thread, location and message) before a release build aborts. In debug
  and test builds a panic in a host function still becomes a JS error.
- `--version` stays one fixed line. `--version --verbose` adds
  `commit <sha> [(dirty)]` from `build.rs`, from `LB_BUILD_COMMIT` if set
  or from git, else `unknown`.
- Run-time writes to stderr never panic: they use `let _ = writeln!`, so a
  parent that closed its end of the pipe cannot abort the server.

## Consequences

- Tools that open tabs with `GET /json/new` get 405 and must use PUT, as
  with Chrome. Playwright and the SDK do not use `/json/new`.
- A stuck page makes the first signal end the process 5 s later with
  status 130 instead of 0. Before this change the signal killed the
  process at once with no cleanup.
- The dirty flag is computed when the build script runs. That happens
  when HEAD or its branch moves, not on every edit, so it can say clean
  for a tree edited since. Builds outside git report `unknown` unless
  `LB_BUILD_COMMIT` is set.

## Open items

- Web Worker, SharedWorker and service worker threads have no cap.
- `mcp` still skips the V8 teardown on exit (`mem::forget`).
- No flag for the queue bound (add one if a client needs more in flight).

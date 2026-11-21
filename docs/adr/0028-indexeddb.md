# ADR 0028: IndexedDB in memory per browser context, with Chrome's timing

Status: Accepted (2026-11-04).

## Context

`indexedDB` was `undefined` in LightBrowser and an object in Chrome; many
sites and fingerprinting scripts open a database. Without it a page sees
a non-Chrome surface, and with an incomplete one its transaction and event
order differs. Storage must stay in memory (no disk writes by default) and
be shared by the windows and workers of one browser context, as Chrome's
backend is. Behaviour was recorded in Chrome for Testing 153.0.8010.12 on
loopback pages (`lb-page` `tests/idb/`, 10 cases).

## Decision

- **Backend** (`lb-webapi` `rt/idb/backend.rs`), one per browser context
  (`BrowserContext::idb`), `Send`, one lock: databases per origin, open
  connections, the per-database open/delete queue (`versionchange` to the
  other connections, an acknowledgement from each window, then `blocked`
  if one is still open), transaction scheduling (overlapping scopes with a
  writer wait, in creation order), key generators, unique and multiEntry
  indexes, cursors. A read-write transaction keeps a copy of its scope (a
  version change one of the whole database) and an abort restores it.
- **Replies as tasks.** A request of a started transaction runs when it is
  placed; its result goes to the owning window's inbox and becomes a task,
  in the order the backend produced it, like Chrome's IPC replies. This
  gives Chrome's interleaving of `success`, `complete` and other
  transactions' events (recorded case `idb_txn`).
- **Activation and auto-commit.** A transaction is active in the task that
  created it (including its microtasks) and while its request events are
  dispatched. `Window::take_task` marks the start of the next task
  (`idb::task_boundary`); a transaction that is then inactive with no
  pending request commits, as does one whose last request event ran.
  `commit()` and `abort()` as specified; an exception in a request handler
  aborts with `AbortError` ("Uncaught exception in event handler.").
- **Events** bubble request → transaction → database (`error`, `abort`);
  `IDBVersionChangeEvent` for `upgradeneeded`, `versionchange`, `blocked`
  and delete `success`.
- **Values** are V8 structured clones (Blobs carried as bytes); key paths
  are evaluated on the clone; a generated key is injected when a value is
  read, as Chrome does. Keys keep UTF-16 code units (lone surrogates) and
  order number < date < string < binary < array.
- Chrome's argument checks, exception order and texts throughout (recorded),
  `getAll`/`getAllKeys` options dictionaries, `getAllRecords`/`IDBRecord`,
  `DOMStringList`.

## Consequences

- Same model in pages and workers (dedicated, shared, service); databases
  are shared across them and across frames of the same origin.
- Opaque origins throw `SecurityError`. Nothing persists past the context.
- Not modelled: quota and storage pressure, `durability` effects,
  `IDBFactory.open` from detached frames, blob values read through
  `fetch()` after the database closed, the back/forward cache rules.

# ADR 0037: File uploads, permissions, geolocation, drag and drop and PDF for Playwright

Status: Accepted (2026-11-07).

## Context

Stock Playwright calls that common test suites use failed against
`serve`: `setInputFiles` (no `File` constructor, `DataTransfer` or
`input.files`), `context.grantPermissions` (`Browser.grantPermissions`
missing), a `geolocation` context option (every page failed:
`Emulation.setGeolocationOverride` was refused and `navigator.geolocation`
did not exist), `dragAndDrop` (it hung waiting for
`Input.dragIntercepted`) and `page.pdf` (`Page.printToPDF` missing).

## Decision

Each follows Chrome, checked against local Chrome for Testing 1243 where a
page can observe it:

- **Files** (`lb-webapi` `rt/files.rs`): `File` (a `Blob` with a name and
  `lastModified`), `FileList`, `DataTransfer` and its item list, a file
  input's `files` and `C:\fakepath\` value; forms post the chosen bytes
  as multipart. `DOM.setFileInputFiles` reads the paths the client sends
  (nothing else is read) and fires `input` and `change`.
- **Permissions**: CDP overrides live per browser context
  (`lb_webapi::host::PermissionOverrides`) and come before the persona's
  states. `grantPermissions` grants the listed permissions for an origin
  (or all) and denies every other one there, as Chrome's DevTools
  overrides do; `resetPermissions` returns to the persona's states.
- **Geolocation**: `navigator.geolocation` exists. Insecure origins and
  denied permission fail with Chrome's errors. A `prompt` state waits, as a
  desktop prompt nobody answers (the rule `Notification.requestPermission`
  already follows; the headless shell denies instead). When granted, the
  position is the page's override, or the request times out after its
  `timeout`.
- **Drag and drop** (`rt/input.rs`): a left drag past 4 px from a draggable
  element fires `dragstart`, then `pointercancel`; with
  `Input.setInterceptDrags` the drag goes to the client as
  `Input.dragIntercepted` and `Input.dispatchDragEvent` drives it,
  otherwise mouse input does. Targets see a separate store with
  `effectAllowed` from the operations; `drop` only after a canceled
  `dragover`. The event logs equal Chrome's (`dnd.spec.mjs`).
- **PDF**: there is no print layout engine. `Page.printToPDF` lays the page
  out again at the paper's content width (restored afterwards; the page
  sees no `resize`), cuts it into page-high slices, paints each at about
  150 dpi and embeds it as a JPEG in a minimal PDF written by hand.
  Paper size, margins, scale, landscape and page ranges are honoured;
  `transferMode: ReturnAsStream` goes through a new `IO.read`/`IO.close`.

## Consequences

- PDFs look like the page but hold no text (it cannot be selected or
  searched), media queries answer `screen`, `printBackground` is ignored
  (backgrounds always print), and headers and footers are not drawn.
- Not covered: permission overrides in workers' `navigator.permissions`,
  `watchPosition` updates after the first position, file drops from the
  OS other than through `Input.dispatchDragEvent`, `beforeunload`
  dialogs.

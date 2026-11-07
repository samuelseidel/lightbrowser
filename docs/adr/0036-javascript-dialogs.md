# ADR 0036: JavaScript dialogs wait on the page's worker for the CDP client

Status: Accepted (2026-11-07).

## Context

`alert`, `confirm` and `prompt` returned at once (nothing, `false`,
`null`) and no `Page.javascriptDialogOpening` was ever sent, so
Playwright's `page.on('dialog')` never fired and tests that accept a
`confirm` or answer a `prompt` could not run. Chrome with a DevTools
client that enabled the Page domain reports the dialog and keeps the page
paused in it until `Page.handleJavaScriptDialog` (Playwright dismisses a
dialog nobody listens for). Blink pauses every page of the renderer
while a modal dialog is up (`ScopedPagePauser`).

A dialog is a synchronous call from page JS on the page's worker thread
(ADR 0035), whose event loop cannot run until the call returns. The
answer arrives as a CDP command on the CDP thread.

## Decision

- `lb-webapi` asks a per-window `DialogHost` (`Window::set_dialog_host`,
  inherited by child frames). Without one, or when it declines, the old
  results stand (Chrome with nothing showing dialogs).
- `lb-cdp` tells the host whether a session attached to the page has the
  Page domain on (`PageHost::set_dialog_handling`, on enable, disable,
  detach and connection close).
- When one has, `lb-page`'s `Agent::dialog` puts a channel sender into
  `Shared::dialogs` for the page's target, sends
  `Page.javascriptDialogOpening` and blocks the worker on the receiver.
  `CdpHost::handle_javascript_dialog` runs on the CDP thread, takes the
  sender and answers. Then `Page.javascriptDialogClosed` is sent and the
  JS call returns `true`/`false`, the prompt text (its default when the
  client gave none) or `null`.
- Closing the page, or the last such session going away, drops the sender
  and the dialog is dismissed.

## Consequences

- Like Chrome's renderer, every page on the same worker waits while one of
  them shows a dialog; other workers' pages run on.
- CDP commands for a page that is in a dialog queue until it closes;
  sync host calls give up after their usual 30 s.
- Not covered: `beforeunload` dialogs, `print()`.

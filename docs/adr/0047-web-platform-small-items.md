# ADR 0047: Four small web-platform items: history rate limit, pointer events, StorageManager, speech synthesis

Status: Accepted (2026-11-07).

## Context

Four places where pages saw something other than Chrome 153: `history`
calls never stopped, `PointerEvent.getCoalescedEvents()` always returned
`[]`, `pointerrawupdate` and `UIEvent.sourceCapabilities` did not exist,
`navigator.storage` was `undefined` and `speechSynthesis.speak()` did
nothing. The cases are in `crates/lb-page/tests/webplat/` (`serve.py`,
`chrome.mjs` to re-record, `chrome.json`, included from `pages.rs`),
recorded in Chrome for Testing 153.0.8010.12 headless shell on loopback
(`chrome.mjs`: a regular profile directory for the cases with a
`persistent` file, else an off-the-record context).

## Decision

### History rate limit (`rt/mod.rs` `NavLimiter`, `rt/window.rs`)

Blink's `NavigationRateLimiter`: 200 counted calls in a window of 10 s,
then calls are ignored (no error), with one console warning ("Throttling
navigation to prevent the browser from hanging. See
https://crbug.com/1038223 ...") for each window in which something was
ignored. Counted: `pushState`, `replaceState`, `go`, `back`, `forward`,
`location` assignments, `assign`, `replace`, fragment navigations, link
clicks and form submits; not counted: `reload()`. The window starts at the
first counted call and restarts at the first one after it ended (recorded:
calls at 2 s to 9.9 s ignored, accepted again at 10.15 s). The brief
expected the limiter to belong to the frame; Chrome 153 keeps one per
document (a new document in the same frame starts with a full budget,
`history_frame`, `history_nav`), so it lives in the `Window`; it reads the
window's clock and works on the virtual clock (`history_window`).
`history.length` never shows more than 50 (`history_cap`).

### Pointer events (`rt/input.rs`, `rt/events.rs`)

- `getCoalescedEvents()` of a trusted `pointermove` and `pointerrawupdate`
  returns one event: the event's own sample, a separate non-bubbling,
  non-cancelable event with the same fields (the identical objects on each
  call, a new array); `pointerdown`, `pointerup`, `click` and the like give
  `[]`; constructor-made events return the list they were given
  (`coalescedEvents`, `predictedEvents`). The input pipeline delivers one
  sample per `Input.dispatchMouseEvent` (human replay and Playwright
  alike), so this is the pipeline's own list, as in Chrome for input that
  arrives one event at a time. Not implemented: `getPredictedEvents()` of
  trusted events returns `[]`; Chrome's predictor adds some (3 to 10) to a
  quick series of moves with times of its own, which cannot be compared
  between runs.
- `pointerrawupdate` is fired, first of a move's events and not cancelable,
  only while a listener for it exists anywhere in the page.
- `UIEvent.sourceCapabilities` (`InputDeviceCapabilities`,
  `firesTouchEvents` false for the mouse and keyboard the pipeline has) on
  trusted mouse and keyboard events, on `click`, `auxclick` and
  `contextmenu`, and on `focus`/`blur` a click or Tab caused; `null` on
  trusted `pointer*` events, wheel and input events, and on anything made
  by script (unless the constructor was given one). One object for all.
  `click`, `auxclick` and `contextmenu` are `PointerEvent`s with
  `isPrimary` false, `contextmenu` with `detail` 0.

### StorageManager (`rt/navigator.rs`)

- `estimate()` resolves `{quota, usage, usageDetails}`. Chrome 153 reports
  not the real quota but `usage + 10 GiB` (`CalculateReportedQuota`; for a
  disk under 10 GiB the disk size rounded up to whole GiB, which unit tests
  check against the source because no such disk can be recorded). Recorded:
  exactly 10 GiB on a 246 GB disk and on a 32 GB tmpfs. The disk constant
  is `STORAGE_DISK_BYTES` (256 GiB; not a persona field). Usage counts
  IndexedDB (`IdbBackend::usage`: keys, values, buffers, blobs, index
  keys, names) and the Cache API (`CacheStorageData::usage`: names, request
  and response URLs, headers, bodies), under `usageDetails.indexedDB` and
  `.caches` once they hold something. `localStorage`, `sessionStorage` and
  cookies do not count: in Chrome 153 they did not move the number
  (`storage_usage`), so, against the brief's list, they are not counted.
  Byte counts are the engine's own model; Chrome's carry database overhead
  and padding and are not compared.
- `persisted()` and `persist()` resolve true only if the persona's
  `persistent-storage` state is `granted`, else false and nothing changes
  (recorded: false, false, permission stays `prompt`).
- `getDirectory()` is not implemented: see Consequences. It rejects with
  `SecurityError` "Storage directory access is denied."; the opaque-origin
  rejections (`TypeError` for the other three, a sandbox `SecurityError`
  for this one) are coded as recorded but cannot be reached: `sandbox` is
  ignored by the frame code and `data:` iframes load empty.

### speechSynthesis (`rt/speech.rs`)

`SpeechSynthesisUtterance` (clamped `float` attributes, handlers),
`SpeechSynthesisEvent` and `SpeechSynthesisErrorEvent`, and `speak`,
`cancel`, `pause`, `resume`, `speaking`, `pending`, `paused` as Blink's queue
in front of Chrome's controller. With no voice (what the recording machine
has; `speech_none`, driven through a persona with no voices): `speak()`
without a (sticky) user activation fires `error` `not-allowed` inside the
call; with one `error` `synthesis-failed` follows in a later task;
`pause()` sets only the controller's flag (`paused` stays false) and holds
the next utterance until a `resume()` made while something is queued;
`cancel()` fires `canceled` for the utterance the controller holds and
nothing for the rest. With the persona's voices there is no browser to
record, so the events follow the same flow with a documented model: `start`,
for local voices one `boundary` (`word`) per word, `end` after
`words * 400 ms / rate` (150 words per minute at rate 1); voice picked
from `utterance.voice`, else the `lang`, else the persona's default;
`elapsedTime` in seconds; `pause` and `resume` events one task after the
call. The expected log of that case (`speech_voices`) is the model's, said
so in its test.

## Consequences

- The origin private file system is open: Chrome resolves `getDirectory()`
  with a `FileSystemDirectoryHandle` and the handle, file handle, `File`
  and `FileSystemWritableFileStream` surface is recorded
  (`cases/storage_opfs`, `chrome.json`) but not implemented or run. It
  needs a `WritableStream` (the engine has no streams), async iterators
  for `entries()`, `keys()` and `values()`, and a per-origin in-memory
  tree: more than the day the brief allowed.
- Predicted pointer events, Chrome's reason for a wrong-`this` rejection
  text on promise methods ("Failed to execute 'estimate' on
  'StorageManager': Illegal invocation"; the engine's brand check throws a
  bare `Illegal invocation`), `selectstart` on mouse down and
  platform-specific `contextmenu` timing remain as before.
- The pointer cases start with a move to 0,0 and a reset because Chrome
  moves the pointer there by itself when a page loads.
- Pages that do not use these features pay nothing: the speech state is a
  few empty fields in the window, nothing is registered per realm beyond
  table entries.

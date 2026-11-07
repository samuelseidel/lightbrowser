# ADR 0019: Style sheets load as Chrome loads them: script- and parser-blocking sheets, `@import`, CSSOM

Status: Accepted (2026-11-04). Closes the M2/M6 open item "stylesheets
are fetched only after DOMContentLoaded" and "stylesheets blocking scripts".

## Context

`<link rel=stylesheet>` sheets were fetched after `DOMContentLoaded`, so a
script that measured layout saw unstyled boxes, `@import` was parsed but
never fetched, `document.styleSheets` and `link.sheet` were spec-shaped
stubs (`undefined` / `null`), and script-inserted links never loaded.
Local Chrome for Testing 153.0.8010.12 on loopback pages (17 cases in
`lb-page` `tests/stylesheets/`, logs in `chrome.json`) shows:

- A parser-inserted classic script, `defer` scripts and module scripts
  wait for *script-blocking* sheets: parser-made `<link>`s whose `media`
  matches (not alternates) and parser-made `<style>`s whose `@import`s
  load (any import media). Async and script-inserted scripts do not wait;
  script-inserted links do not block.
- When the last blocking sheet loads, the waiting script (or the
  remaining `defer` scripts and `DOMContentLoaded`) runs before the
  sheet's `load` event; parsing resumes after it.
- A sheet inserted after `<body>` exists stops the parser right after the
  element until it loaded and its `load` event fired (print sheets do not).
- Every `<style>` fires `load` (or `error` when an import failed); a link
  whose import failed fires `error`; HTTP errors give an empty sheet that
  still counts in `document.styleSheets`; a `text/plain` sheet loads empty.
- Without scripts `DOMContentLoaded` does not wait for sheets; `load` does.
- `document.styleSheets` lists `<style>` sheets at once and link sheets
  once loaded (or failed), in tree order, without `disabled` links.

## Decision

- **Load on insertion** (`lb-webapi` `rt/sheets.rs`): mutation processing
  starts link loads when a link is connected or its `rel`/`href` changes,
  and `<style>` imports when the parser closes the element or its text
  changes. Imports load in parallel, nested, cycle-checked, against the
  sheet's URL; `url()`s in linked sheets resolve against the sheet URL.
- **Blocking counters** per window (script-blocking, parser-blocking,
  pending sheet events) and a *gate*: while a parser script waits for
  sheets, sheet events wait once no sheet blocks, so the script runs
  first. `lb-page` waits on the counters; `lb-dom` gains
  `ParseStep::StyleSheet` (a pause after `<link>` and `</style>`).
- **Ending parsing** runs the tasks queued so far before `DOMContentLoaded`
  (Chrome ends parsing in a later task).
- **CSSOM**: `StyleSheetList`, `CSSStyleSheet` (`insertRule`, `deleteRule`,
  `addRule`, `removeRule`, `disabled`, `cssRules`, `ownerRule`, ...),
  `MediaList`, `CSSRuleList` and rule objects from `lb_css::cssom` (top-level
  rules with their source, parsed only when script asks). Edits rewrite
  the sheet text and reach the cascade (`lb_css::SheetTree`, overrides).
  Cross-origin sheets throw `SecurityError` on `cssRules`.

## Alternatives considered

- **Fetch sheets during parsing but block nothing.** Fast, but scripts
  read unstyled layout and event order differs from Chrome.
- **Block the parser on every sheet.** Simple, but head sheets would
  delay `DOMContentLoaded` for script-free pages, unlike Chrome.
- **A full CSSOM object model kept in sync with the cascade.** Exact
  serialization, but much more code; the text round trip covers what
  CSS-in-JS libraries use (`insertRule`, `cssRules.length`).

## Consequences

- Event order, `getComputedStyle` seen by scripts and `document.styleSheets`
  match Chrome on all 17 cases; `@import`ed sheets apply.
- Declarations keep their source text in `cssText` and `rule.style`
  (Chrome normalizes values, e.g. `#fff` to `rgb(255, 255, 255)`).
- Not modelled: `<link rel=preload>`/preload scanner, `blocking=render`,
  `@import` inserted with `insertRule` (not fetched), constructable
  sheets (`new CSSStyleSheet`, `adoptedStyleSheets`), `@layer` order.

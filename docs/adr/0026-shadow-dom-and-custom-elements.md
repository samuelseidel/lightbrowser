# ADR 0026: Shadow DOM and custom elements as in Chrome

Status: Accepted (2026-11-04).

## Context

Pages had no `attachShadow`, `ShadowRoot`, slots or `customElements`; web
components (most design systems) did not render or run, and the missing
APIs were a visible difference from Chrome 153. Behaviour was recorded in
local Chrome for Testing 153.0.8010.12 on loopback pages (`lb-page`
`tests/shadow/`, 9 cases, same `serve.py` for both browsers).

## Decision

- **Tree model in `lb-dom`** (`shadow.rs`): a shadow root is an arena node
  of its host's document (`NodeData::ShadowRoot`, a `DocumentFragment` to
  scripts) with no parent link, so light-tree walks and `querySelector`
  never enter it. `is_connected` and `getRootNode({composed})` cross to
  the host. Slot assignment is computed on demand ("find a slot", "find
  slottables", flattening); only `slot.assign()` lists are stored.
  `flat_children` / `flat_parent` give the flat tree. Declarative roots
  come from html5ever's `<template shadowrootmode>` hooks (document parser
  and `setHTMLUnsafe`/`parseHTMLUnsafe`; not `DOMParser` or `innerHTML`).
  `getHTML` writes serializable or listed roots; clonable roots are cloned
  (even shallow), every root moves on adoption.
- **Style and layout**: each connected shadow root's `<style>`/`<link>`
  sheets apply to its own tree; `:host`, `:host()` (featureless host) and
  `::slotted()` rules apply at an inner cascade context (outer rules win
  for normal declarations, inner for `!important`). Inheritance, boxes,
  the AX tree, `offsetParent` and hit testing follow the flat tree; slots
  are `display: contents`; unslotted children of a host have no style or
  box. Nothing changes for documents without shadow roots.
- **Events**: the DOM dispatch algorithm with event path structs
  (shadow-adjusted targets, retargeted `relatedTarget`, closed-tree
  bookkeeping for `composedPath()`); UI events the browser fires are
  composed. `slotchange` is signalled by comparing assignments after each
  batch of mutations (plus `assign()` calls) and fired after the mutation
  observers in the same microtask. Focus inside a shadow tree matches
  `:focus` on its hosts and is retargeted for `activeElement`;
  `delegatesFocus` passes focus to the first focusable element.
- **Custom elements** (`lb-webapi` `rt/custom.rs`): definitions per
  window, Chrome's `define` checks and texts, the HTML element
  constructors (`[HTMLConstructor]`, with the construction stack for
  upgrades), customized built-ins (`is` value in `lb-dom`, serialized).
  Reactions queue per element and run when the operation ends (the
  page's `after_mutation`, which every DOM-changing binding calls),
  derived from mutation records; elements made by fragment parsing or
  cloning get their upgrade first. The parser pauses after each possible
  custom element once a definition exists
  (`ParseStep::CustomElement`); its attributes and children are taken off
  silently, the constructor runs, and they are put back, so callbacks see
  what Chrome's synchronous construction shows.
- **Form-associated custom elements** (`rt/internals.rs`):
  `ElementInternals` (form owner, `setFormValue`, validity, states,
  ARIA reflection), owner/disabled/reset callbacks, `form.elements`,
  `:state()`, `:invalid`, and `FormData` with string values.

## Consequences

- Shadow DOM costs nothing on pages that do not use it (every new path
  checks `shadow_roots()` first); custom element work starts with the
  first `define`.
- CDP `DOM.Node` lists `shadowRoots` (open and closed) with
  `shadowRootType`, their children with `pierce`, `assignedSlot` and
  `distributedNodes`, as Chrome does.
- Not modelled: scoped registries beyond `new CustomElementRegistry()`,
  constructable style sheets (`adoptedStyleSheets` is empty), `::part`,
  `:host-context()`, user-agent shadow roots, form state restore,
  `FormData` file entries, named access on `window` (affects some WPT
  files).

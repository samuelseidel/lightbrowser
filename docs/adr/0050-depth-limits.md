# ADR 0050: Depth limits: layout, CDP trees, the shader compiler and page stacks

Status: Accepted (2026-11-16).

## Context

A page controls how deep its DOM, its shader source and its `#if` lines nest.
Several walks recursed once per level on a page worker thread, which had
Rust's default 2 MiB stack (a debug build uses about 9 KiB per layout level).
An audit (GAP-094, GAP-093) reproduced stack overflows in the layout box
build, in `DOM.getDocument` with `depth: -1`, and in the GLSL compiler. A
stack overflow aborts the whole `serve` process, with every page in it.
A CDP client could also send a `frameId` that was not 32 hex digits, an
absurd `IO.read` size or an absurd `humanScroll` distance.

## Decision

- **Layout** (`lb-layout`): the box tree stops at `MAX_BOX_DEPTH` = 512
  levels, Chrome's cap for parser-built elements. A deeper element gets no
  box, as with `display: none`: zero rects, no hit testing. `bounding_client_rect`
  on a boxless node walks with an explicit stack. Page worker threads reserve
  16 MiB of stack (`lb_layout::PAGE_STACK`, set in `lb-page`); the memory is
  only reserved, and V8's own stack limit is unchanged.
- **CDP trees** (`lb-cdp`): `DOM.getDocument` and `DOM.describeNode` with depth
  -1, or deeper than 256, return 256 levels. The deepest nodes keep
  `childNodeCount`, so `DOM.requestChildNodes` can continue.
- **CDP inputs**: `frameId` is parsed as exactly 32 ASCII hex digits, anything
  else is an unknown frame (Chrome's error text). `IO.read` clamps its size
  and uses checked arithmetic. `LightBrowser.humanScroll` rejects deltas that
  are not finite or exceed 1,000,000 px, and replays at most 50,000 events;
  how events are generated is unchanged.
- **GLSL** (`lb-gl`): a compile runs on a scoped thread with a 64 MiB stack.
  The parser stops at 1100 nesting levels ("memory exhausted", as ANGLE's
  parser stack does); an expression tree deeper than 256 levels, counting
  binary, comma and postfix (`.x`, `[i]`, `++`) chains, is "Expression too
  complex." (ANGLE's `MaxExpressionComplexity`). A `#if` expression nests at
  most 512 levels (parentheses, prefix operators and `?:` count), else the line
  fails with "conditional expression too complex". A chain of more than 256
  nested function calls fails with "Call stack too deep (larger than 256)"
  (ANGLE's `MaxCallStackDepth`). Once the op limit is hit, inlining stops, so
  a call tree that doubles at each level cannot run for ever.
- Chrome-recorded logs cover the parser and expression limits
  (`crates/lb-gl/tests/logs.rs`). The `#if`, postfix and call-chain texts
  were not recorded; those tests check that the compile fails and ends.

## Consequences

- Differences from Chrome: elements nested deeper than 512 are not rendered
  (Chrome re-parents them at 512 and renders them); `DOM.getDocument` with
  depth -1 stops at 256 levels; the `#if` and call-chain error texts are
  ours; every shader compile starts a short-lived thread.
- Each page worker reserves 16 MiB of virtual stack instead of 2 MiB. RSS is
  unchanged unless the stack is used; the bench has not measured it.
- Regression tests build the deep inputs in code and fail on the old code
  (stack overflow, abort).

## Open items

- The HTML parser (`lb-dom`) has no 512 nesting cap, so parser-built content
  deeper than 512 is invisible here but shown by Chrome. Capping the sink as
  Blink does would remove the difference for parsed pages.
- Building a deep tree by script is slow outside layout (10,000 levels take
  about 4.5 s in a debug build, 100,000 did not finish). Not profiled.
- `getComputedStyle` of an element deeper than the box cap may recurse once
  per uncomputed ancestor; not checked.

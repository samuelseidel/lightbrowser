# ADR 0010: Our own box tree over taffy, and font metric tables compiled in

Status: Accepted (2026-11-03)

## Context

ADR 0004 chose `taffy` for layout and "skrifa metrics of the persona's fonts"
for text. Implementing it raised two questions:

1. `taffy` ships a `TaffyTree` that owns nodes. Our DOM already has an arena
   (`lb-dom`), styles are computed lazily (`lb-css`), and layout is rebuilt on
   demand. Mirroring every element into a second owned tree costs memory and
   keeps two trees in sync.
2. Fonts decide most of what layout-based fingerprinting sees: the widths
   scripts measure to detect installed fonts, line heights, and the
   `offsetWidth` of text. Reading real font files at runtime would make the
   result depend on the machine we run on, and a persona that claims Windows
   fonts on a Linux host would be wrong. Shipping font files is large and
   has licence issues for the real Windows and macOS fonts.

## Decision

**Own box tree.** `lb-layout` builds a box tree (`tree.rs`, `build.rs`) from
the DOM and computed styles and implements taffy's layout traits over it
(`LayoutPartialTree`, block, flexbox, grid and `compute_cached_layout`), so we
use taffy's algorithms (block, flex, grid, floats, absolute and fixed
positioning) without `TaffyTree`.

- One box per rendered element, text run, anonymous inline run, and replaced
  element. `display: none` and non-rendered nodes get none.
- The tree is built when geometry is first asked for and is cached until the
  document's render generation, the styles or the viewport change. Scrolling
  only re-runs placement. It can be dropped under memory pressure.
- Inline layout is approximated: a run of inline content becomes an anonymous
  wrapping flex row of text runs, inline boxes and atomic inlines. Tables are
  blocks with flex rows, and transforms are ignored for layout. Hit testing,
  scroll, box models and the accessibility tree work on the same boxes.

**Compiled font tables.** Text is measured from per-font metric tables in
`profiles/fonts/<os>/<family>.json` (`format: 1`): units per em, ascent,
descent, line gap and per-face advances for U+0020-007E, U+00A0-00FF and
common punctuation. They are generated offline with `skrifa`
(`cargo run -p lb-layout --example gen_font_tables`) and embedded in the
binary with `include_str!` (`BUNDLED` in `fonts.rs`). No font file is read at
runtime, and `skrifa` is a dev-dependency only.

- Width = sum of advances plus letter and word spacing. No shaping or kerning.
  Line breaking is greedy at spaces and line metrics are rounded the way
  Chrome rounds them.
- Which family a page gets comes from the persona: `FontAvailability` is built
  from `fonts.available`, and `resolve_font` takes the first CSS family the
  persona has (and we have a table for), then the generic mapping, then the
  OS standard font. A persona without Arial cannot measure with Arial.
- Tables from a metric-compatible font are exact (Arial from Liberation Sans,
  Times New Roman from Liberation Serif, Courier New from Liberation Mono,
  Calibri from Carlito). Others are marked `approximate: true` (Segoe UI,
  Verdana, Tahoma, Georgia, Helvetica, macOS system-ui) and use a similar
  design with the real font's documented vertical metrics.
- Persona fonts without a table get an approximate one from
  `profiles/fonts/derived.json`: a similar bundled table's advances times a
  per-font factor, with the font's own vertical metrics (in em). Values are
  estimates, not measured from the real fonts, but every installed font
  measures differently from the fallbacks, as on a real system, and fonts
  the persona lacks measure exactly like the fallback.
- Canvas text draws from small glyph outline packs in
  `profiles/fonts/outlines/` (about 205 KB, from Liberation fonts, SIL OFL).
  Widths still come from the tables.

## Alternatives considered

- **`TaffyTree` with a node per element.** Less code, but a second tree to
  keep in sync and a fixed memory cost on every page.
- **Read system fonts with a shaper (rustybuzz, swash).** Exact on the
  machine's own fonts, but results vary per host, need the fonts installed,
  and add a large dependency.
- **Bundle real font files.** Accurate, but large, and the Windows and macOS
  fonts cannot be redistributed.
- **A fixed average width per character.** Cheap, but font-detection probes
  would see identical widths for every font, itself a tell.

## Consequences

- Font metrics are reproducible across hosts and match the persona's OS.
  Binary size grows by the tables (about 420 KB of JSON including outlines).
- Open: only Latin-1 and common punctuation are covered; other characters use
  the face's default advance. CJK, emoji and complex scripts are not measured.
- Open: no kerning or shaping, so widths of text with ligatures or kerning
  pairs differ from Chrome by small amounts.
- Open: the `approximate` tables should be replaced with ones generated from
  the real fonts when we have licensed copies.
- Open: inline layout does not fragment lines exactly, so
  `getClientRects()` on a wrapped inline returns approximate boxes.

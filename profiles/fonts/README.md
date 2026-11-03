# Font metric tables

Per-OS advance-width and vertical-metric tables that `lb-layout` uses to
measure text. They are bundled into the binary; no font file is read at
runtime.

Layout: `profiles/fonts/<os>/<family>.json`, `os` = `windows`, `macos`,
`linux`. Regenerate with:

```bash
cargo run -p lb-layout --example gen_font_tables -- /usr/share/fonts profiles/fonts
```

and add new files to `BUNDLED` in `crates/lb-layout/src/fonts.rs` (a test
checks that every file is embedded).

## Format (`format: 1`)

| Field | Meaning |
|---|---|
| `family` | CSS family name the table answers for |
| `approximate` | `true` when built from a similar font, not a metric-compatible one |
| `source` | Font the numbers came from |
| `units_per_em` | Design units per em |
| `ascent`, `descent`, `line_gap` | Vertical metrics in design units (descent positive) |
| `faces[]` | `weight`, `style` (`normal`/`italic`), `default_advance` (OS/2 average width, used for unknown characters), `ranges`: `[first_codepoint, [advance, ...]]` |

Covered characters: U+0020-007E, U+00A0-00FF and common punctuation
(dashes, quotes, bullet, ellipsis, euro, trademark).

## Sources

- Exact advances via metric-compatible fonts: Arial (Liberation Sans),
  Times New Roman (Liberation Serif), Courier New (Liberation Mono),
  Calibri (Carlito); Linux: Liberation and DejaVu files themselves.
- Approximate (`approximate: true`): Segoe UI, Verdana, Tahoma, Georgia
  (similar designs with the real fonts' documented vertical metrics),
  Helvetica, Helvetica Neue, Times, Courier and macOS `system-ui` (Inter).
  Replace these with tables generated from the real fonts when available.

## Derived tables (`derived.json`)

Persona fonts without a table of their own (for example Consolas, Cambria,
MS Gothic, Menlo, Avenir) get an approximate one at load time: the `base`
table's advances times `scale`, with `ascent`, `descent` and `line_gap` in
em. The numbers are estimates (no proprietary font files were used); they
make installed fonts measurably different from the fallbacks, so width
probes detect exactly the persona's fonts. A real table in `<os>/` wins over
a derived one. `lb-layout` checks that every font in the reference personas
has a table or a derived entry.

## Glyph outlines (`outlines/`)

Canvas text draws glyph shapes from small outline packs (`*.lbgp`, format
in `crates/lb-paint/examples/gen_glyph_pack.rs`), one per style class and
weight: `sans`, `serif`, `mono` x `regular`, `bold`, about 205 KB in all.
They are derived from Liberation Sans/Serif/Mono 2.1.5 (SIL OFL 1.1, see
`outlines/LICENSE`; the packs do not use the reserved names) and cover the
same characters as the metric tables. Widths still come from the metric
tables above; italic is a synthetic skew. Regenerate with:

```bash
cargo run -p lb-paint --example gen_glyph_pack -- \
    /usr/share/fonts/truetype/liberation profiles/fonts/outlines
```

Only the `*.json` tables count for `BUNDLED`.

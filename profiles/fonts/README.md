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
  Calibri (Carlito); Linux: the Liberation, DejaVu, Carlito and Caladea
  files themselves (Carlito and Caladea added in M7 WP-C, SIL OFL, for
  fontconfig's Calibri and Cambria aliases).
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
has a table or a derived entry; `lb-persona` checks the same for the
generated personas and the catalog font sets (`profiles/catalog/fonts.json`).

M7 added derived entries for Windows 11 (Segoe UI Variable Display, Small
and Text; Segoe Fluent Icons) and for the Linux distro fonts (Ubuntu,
Ubuntu Sans, Noto, Adwaita, Cantarell, URW base35, FreeFont, Carlito,
Caladea, Noto Color Emoji). The FreeFont, Noto Color Emoji, Carlito,
Caladea and Inter-based Adwaita Sans scales follow the widths the local
Chrome for Testing 153 measured (`tests/fixtures/os/`); the rest are
estimates from published metrics. Regenerate them from the OFL font files
when those are available. Noto Color Emoji's vertical metrics (0.9277 /
0.2441 em) are the local font file's (`hhea` 1900/500 at 2048 upem); Linux
Carlito and Caladea now have real tables, which win over their derived
entries.

Matching (M7 WP-C, ADR 0015): a family the persona does not have resolves
through `PersonaProfile::font_aliases` (fontconfig's metric aliases on
Linux, Blink's alternates everywhere); generic keywords name
`font_defaults`. `crates/lb-layout/tests/os.rs` checks the Ubuntu and
Fedora personas against the captured Linux widths and heights (exact). A
persona font with no table for its OS (a Windows font on a Linux persona)
borrows another OS's table, so such a persona is detectable.

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

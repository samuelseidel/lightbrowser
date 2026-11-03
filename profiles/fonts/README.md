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

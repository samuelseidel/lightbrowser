# Local font faces

Face names (full and PostScript) that `FontFace` `local()` resolves on a
default install of an OS release, beyond the persona's `fonts.available`
families. `lb-webapi` `rt/fonts.rs` bundles them and picks one by the
persona's `os.name` and `os.version`.

| File | Recorded from |
|---|---|
| `macos_26.json` | Chrome 156 on macOS 26.4 (25E253): every face in `/System/Library/Fonts` tried with `local()`, the ones that loaded kept (989 of 4797) |

Only default-install system fonts are listed (no downloaded or user
fonts). Other macOS releases, Windows and Linux have no list yet, so
`local()` there resolves the persona's families only.

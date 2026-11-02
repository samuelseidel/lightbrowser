# Chromium IDL set

The WebIDL files from Blink's `third_party/blink/renderer/core/` and
`modules/` at Chromium tag `153.0.8010.12`. That is the same version as the
`chrome_153_linux` TLS profile and `tests/fixtures/chromium_153_surface.json`.

- 2,229 files. `FILES.txt` lists them.
- They are unmodified. The license is in `LICENSE` (Chromium, BSD-style). Some
  files carry their own header from WebKit or Apple, and those headers are
  kept.
- `tools/fetch_chromium_idl.py 153.0.8010.12 <out>` fetches them again. It
  lists directories from chromium.googlesource.com and downloads file bodies
  from the GitHub mirror.

`lb-webidl-gen` reads these files at build time. Do not edit them. To move to
a new Chrome version, fetch again, capture a new surface fixture with
`tools/dump_surface.js`, and regenerate.

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
a new Chrome version, fetch again, capture new surface fixtures with
`tools/dump_surface.js`, and regenerate (`cargo run -p lb-webidl-gen --
--family chrome`).

There are two fixtures, both from the local Chromium, loopback only:

- `chromium_153_surface_secure.json`: a secure context. Serve an empty page
  on loopback and run `node tools/dump_surface.js OUT http://127.0.0.1:PORT/`.
  This is the generator's default.
- `chromium_153_surface.json`: `about:blank` (`node tools/dump_surface.js
  OUT`), which is not a secure context. The generator uses it to mark
  secure-context-only members, which insecure realms leave out.

A third input, `tests/fixtures/chrome_153_os_overlay.json`, adds members
Chrome has on Windows and macOS only (Web Share, Web Bluetooth), which the
Linux captures lack. It is not captured (see its `_meta`); the generator
merges it into both fixtures and lists its members in `OS_GATED`, and
`lb-webapi` deletes them per realm for personas whose OS lacks them.

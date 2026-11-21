# surface_diff

Dumps the JS surface a page can observe (`dump.js`: own keys of the global
in order, every global and interface object, its prototype and their
descriptors, function names, lengths and `toString`, results of calls with
a wrong receiver, cross-realm and detached-frame behaviour, page changes to
interface objects) from one or more `lightbrowser` builds, for a Windows, a
macOS and a Linux reference persona, in `fetch` and through `serve` over
CDP, and diffs every build against the first. Used to show that start-up
and memory work does not change what pages see (ADR 0016).

```bash
python3 tools/surface_diff/run.py --out "$(mktemp -d)" OLD_BIN NEW_BIN
python3 tools/surface_diff/run.py --out DIR --chromium /opt/pw-browsers/chromium BIN
```

Loopback only (the page is served from this directory on 127.0.0.1).
Binaries must sit below the repository (they look for `profiles/` above
themselves). `--chromium` writes a local Chrome's dump next to the others
(no persona, so only the behaviour sections compare directly).

# os_truth

OS ground truth for M7 (OS personas), captured from the local Chrome for
Testing 153 (`/opt/pw-browsers/chromium`). Loopback only: `capture.js`
serves `page.html` on `127.0.0.1` and starts Chromium on that URL with a
throwaway profile, no CDP and no automation flags. The page loads nothing
from other hosts.

| File | What it is |
|---|---|
| `page.html` | Collects UA and UA-CH (all high-entropy hints), OS-gated APIs, screen and window, scrollbars, generic font resolution and font detection, `font:` keywords, emoji rendering, system colours, media queries, audio, devices, voices, codecs (`canPlayType`, MSE, `MediaRecorder`, `decodingInfo`, `encodingInfo`), WebGPU adapter, keyboard map, WebGL limits; then records wheel events. |
| `capture.js` | Runs one capture and writes JSON with a `provenance` block. |
| `xtest.py` | Moves the pointer and clicks wheel notches with XTest (ctypes, system libX11/libXtst), for headed runs. |
| `fontconf/{ubuntu24,fedora42}/fonts.conf` | Distro-like fontconfig files (`FONTCONFIG_FILE`) without this container's Inter preference. |
| `bench_fixtures.py` | Turns a recorded stock-Chrome T-010 session into `tests/detect-bench/fixtures/os_{linux,windows,mac}_chrome.json`. |

```bash
xvfb-run -a -s "-screen 0 1920x1080x24 -dpi 96" \
  node tools/os_truth/capture.js --mode headed --out tests/fixtures/os/linux_x11_cft153.json
xvfb-run -a -s "-screen 0 1920x1080x24 -dpi 96" \
  node tools/os_truth/capture.js --mode headed --fontconf tools/os_truth/fontconf/ubuntu24 \
  --gtk-font "Ubuntu Sans 11" --out tests/fixtures/os/linux_x11_cft153_ubuntu24fonts.json
xvfb-run -a -s "-screen 0 1920x1080x24 -dpi 96" \
  node tools/os_truth/capture.js --mode headed --fontconf tools/os_truth/fontconf/fedora42 \
  --gtk-font "Adwaita Sans 11" --out tests/fixtures/os/linux_x11_cft153_fedora42fonts.json
node tools/os_truth/capture.js --mode headless --out tests/fixtures/os/linux_x11_cft153_headless.json
```

Bench fixtures: run T-010 with stock Chromium headed under `xvfb-run`
(`--remote-debugging-port`, a fresh `--user-data-dir`) against
`tests/detect-bench/server.py` on loopback, then

```bash
python3 tools/os_truth/bench_fixtures.py <results>/sessions-*.jsonl \
  --wheel tests/fixtures/os/linux_x11_cft153.json --out tests/detect-bench/fixtures
```

## Limits of this machine

Recorded in every capture's `provenance.limits`:

- No GPU: WebGL is SwiftShader in `--headless=new` and unavailable headed;
  `navigator.gpu.requestAdapter()` gives `null`.
- No audio server: `AudioContext` shows the no-device fallback (44100 Hz,
  `baseLatency` 0.01, `outputLatency` 0).
- No speech-dispatcher: 0 voices.
- Container fonts: fontconfig's default sans-serif is Inter, and aliases map
  "Segoe UI", "Helvetica Neue" and `system-ui` to it. Ubuntu Sans,
  Cantarell, Adwaita and Noto (except Noto Color Emoji) are not installed
  and were not downloaded, so the `ubuntu24`/`fedora42` captures show the
  fontconfig fallbacks (DejaVu), not the distro fonts.
- No window manager under Xvfb: `avail*` equals the screen, no panel or dock.
- X11 only; Wayland is not captured.

Windows and macOS cannot be captured here. Their values in the bench
fixtures come from the M7 surface reference (`docs/ROADMAP.md`, "M7
status") and are marked `"captured": false`.

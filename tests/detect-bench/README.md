# detect-bench

LightBrowser's own detection bench: a small local site that records how a
client behaves and scores how likely it is to be automated. It is the
"detection engine" that `docs/ROADMAP.md` (M4, M5) and `docs/PLAN.md` test
against.

## Policy: own bench only

- Everything runs on loopback. `server.py` binds `127.0.0.1` or `::1` only
  and refuses any other address.
- `run.js` refuses any base URL, CDP URL or replay target whose host is not
  `127.0.0.1`, `::1` or `localhost`.
- The test pages load nothing from other hosts.
- Never point a scenario at a third-party site or a public detection
  service. Competitive benchmarking needs legal approval (see `CLAUDE.md`).
- Session records go to `results/`, which is git-ignored.

## Files

| File | What it is |
|---|---|
| `server.py` | Test site + collector endpoint + scoring API. Python stdlib only. |
| `sites/` | Shop, news (long scroll), login and search pages, `collector.js`, CSS, site JS. |
| `sites/collector.js` | Client probes and input-event trace; sends beacons. |
| `scoring.py` | Rule catalogue (data) and the score function. |
| `run.js` | Scenario runner (Node + Playwright). |
| `test_scoring.py`, `test_server.py` | Unit tests, using `fixtures/`. |
| `fixtures/T-*.json` | Sessions recorded by `run.js --save-fixtures`. |
| `fixtures/synthetic-human-trace.json` | Synthetic human-like trace for T-020 (curved paths, minimum-jerk speed, varied timing). Not a real person. |
| `fixtures/os_{linux,windows,mac}_chrome.json` | Real-Chrome-shaped sessions for the OS rules, made by `tools/os_truth/bench_fixtures.py`. Linux is a captured stock Chrome 153 session (headed, loopback); Windows 10 and macOS 15 are not captured (values from the M7 surface reference; see each file's `provenance`). |

## Quick start

```bash
cd tests/detect-bench
python3 server.py                    # http://127.0.0.1:8765/shop
# other shell
node run.js                          # T-001 and T-003 on headless Chromium
python3 -m unittest -v               # scoring + server tests
```

`run.js` exits non-zero if a scenario fails its expectation. Output on this
machine (Chrome for Testing 153, Playwright 1.63):

```
scenario                       score  fp   beh   verdict  expect       result  top fired rules
T-001 stock-chromium-headless  100    100  92.3  bot      >=90         pass    webdriver(95) ua_headless(95) ua_headless_header(95) ua_ch_headless(90) notification_permission(50) ch_header_mismatch(50)
T-003 scripted-slow            100    100  90.1  bot      >=70&fp>=70  pass    webdriver(95) ua_headless(95) ua_headless_header(95) ua_ch_headless(90) notification_permission(50) ch_header_mismatch(50)
```

`fp` is the score from non-behaviour rules only, `beh` from behaviour rules
only.

### Server options

```bash
python3 server.py --port 8765 [--host ::1] [--tls] [--config cfg.json] [--results-dir DIR] [--quiet]
```

- `--tls` serves `https://localhost:PORT` with a self-signed cert made by
  the `openssl` CLI in a temp dir, which is deleted as soon as the cert is
  loaded. The stdlib server speaks HTTP/1.1 only (ALPN `http/1.1`); it
  records TLS version, cipher and the number of shared ciphers. For
  ClientHello and HTTP/2 fingerprints (JA3/JA4, Akamai H2) use `lb-fptest`
  (see `tests/README.md`).
- `--config` overrides thresholds and weights (see "Scoring").

Every page response carries `Accept-CH` for the high-entropy client hints
(`Sec-CH-UA-Platform-Version`, `-Arch`, `-Bitness`, `-Full-Version-List`,
`-Model`, `-WoW64`, `-Form-Factors`) and `Critical-CH:
Sec-CH-UA-Platform-Version`, and the session records `accept_ch`. Stock
Chrome 153 retries the first navigation once with the hints (so a session
has one more document) and sends them on every later request.

### Endpoints

| Endpoint | |
|---|---|
| `GET /shop` (`/`), `/news`, `/login`, `/search?q=` | Test pages. The first document without a valid `lb_sid` cookie starts a session. |
| `POST /api/report` | Collector beacon. Rejected unless `sid` is a session the server issued. |
| `GET /api/results` | All sessions with score, verdict and fired rule ids. |
| `GET /api/score/<sid>` | Score, verdict, fired rules with details, per-category scores, server signals. |
| `GET /api/session/<sid>` | Full raw record (this is what fixtures are). |
| `GET /api/trace/<sid>` | Input-event trace per page, the format T-020 replays. |

Records are kept in memory and mirrored to
`results/sessions-<start time>.jsonl`, one JSON line per session.

### What the server records per session

- Every document request: HTTP version, header names in order and case,
  all header values (no cookies), whether the cookie came back, scheme and
  TLS info.
- Every subresource (CSS, JS, images, favicon), form post and beacon: path,
  kind, milliseconds since the last document, whether the session cookie was
  echoed, header names. Subresource URLs also carry `?s=<sid>`, so they are
  tied to the session even when the client drops cookies.
- All User-Agent values seen, beacon count, and per page the merged probes
  and the event trace.

## Collector

`sites/collector.js` runs on every page. Each probe returns
`{id, cat, value, suspicious, weight}`. Probes set `suspicious` only on
positive evidence, so a page with no data never fires a rule. Beacons go out
when the probes finish after `load`, 3 s after load, and on `pagehide`
(also on `visibilitychange` to hidden). Fingerprint probes are sent once per
page; behaviour probes are recomputed on every beacon. The trace is sent in
slices (`traceFrom` offset) so no beacon exceeds the 64 KB `sendBeacon`
limit. The collector defines no globals.

The page gets `sid`, the `User-Agent`, `Accept-Language`, `sec-ch-ua` and
`sec-ch-ua-platform` the server saw, in `<script type="application/json"
id="lb-ctx">`, so the client can compare header and JS values.

Trace events: `{t, type, tr}` plus `x,y` (client px), `mx,my`
(movementX/Y), `b` (button), `d` (detail), `cx,cy` (offset from the target's
centre), `w,h` (target size), `f` (`document.hasFocus()`), `k` (key;
password characters become `x`), `dx,dy,dm` (wheel), `sy` (scrollY), `it`
(inputType), `dl` (inserted length). `t` is `event.timeStamp` in ms.

## Scenarios

| Id | What | Expectation |
|---|---|---|
| `T-001 stock-chromium-headless` | `chromium.launch()`, instant `click`/`fill`, one big wheel. | `>=90` |
| `T-003 scripted-slow` | Same browser; `mouse.move` with steps, wheel ticks, `pressSequentially` with delay. | `>=70&fp>=70`: still caught by fingerprint rules |
| `T-010 cdp-endpoint` | `chromium.connectOverCDP(--cdp ws://...)`, context with `viewport: null` (ADR 0009), the T-001 flow. For LightBrowser. `--input human` runs the same flow (scroll 900 px, click `#add-3`, type `waterproof rain jacket` into `#q`, click `#search-btn`, click "Sign in", type an email and a password, click `#login-btn`) through `LightBrowser.humanScroll/humanClick/humanType` on the page's CDP session, with a reading pause per page (skimming at 600-700 words per minute, a third to two thirds of the page's words, 1.2-6 s; `--seed N` fixes the pauses). Default `--input playwright`. | set with `--expect` |
| `T-020 human-replay` | Replays a trace file (`--trace`) with its timing. Placeholder. | set with `--expect`, e.g. `beh<=30` |

Every scenario opens `/shop`, scrolls, adds a product to the cart, types a
query in the search box (`jacket` in T-003, `waterproof rain jacket` in
T-001 and T-010) and submits. T-001 and T-010 then open `/login`, fill in
a made-up email and password and submit. Each run waits 3.5 s for beacons
and fetches the score. The key rules judge the spread of one page's
keystrokes, so the flow types enough keys per page for that to mean
something: a single six-letter word gives a human-like typist about a 26 %
chance of firing `beh_key_rhythm` or `beh_key_dwell`
(`crates/lb-behavior/tests/key_spread.rs`).

```bash
node run.js --scenario T-001 --save-fixtures fixtures
node run.js --scenario T-010 --cdp ws://127.0.0.1:9222/devtools/browser/<id> --expect 'fp<=30'   # M4 target (ADR 0007)
node run.js --scenario T-010 --cdp ws://127.0.0.1:9222/devtools/browser/<id> --input human --expect 'beh<=30'   # M5 target
node run.js --scenario T-020 --trace fixtures/synthetic-human-trace.json --expect 'beh<=30'
node run.js --base https://localhost:8766      # against server.py --tls
```

Expectations: `>=N`, `<=N`, prefixed with `fp` (non-behaviour score) or
`beh` (behaviour score), joined with `&`. `--json` also prints the full
results.

To record a real human trace: start the server, browse
`http://127.0.0.1:8765/shop` in a desktop browser on the same machine, then
save `GET /api/trace/<sid>` (the `lb_sid` cookie) to a file.

## Scoring

Each fired rule has a weight 0-99, read as "how sure this rule alone makes
us". Weights combine as a noisy-OR:

```
score = 100 * (1 - prod(1 - w_i / 100))
```

So one 95 rule gives 95, two 50 rules give 75, and weak rules add up without
passing 100. The verdict is `bot` at `>= 70`, `suspect` at `>= 40`, else
`human`. A probe that is suspicious on any page of the session fires its
rule. Suspicious probes not in the catalogue still count, at their collector
weight capped at 50, and show as "uncatalogued".

Override with `--config cfg.json`:

```json
{"thresholds": {"bot": 80, "suspect": 50},
 "weights": {"timezone_utc": 0, "webgl_software": 60},
 "disabled": ["favicon_missing"],
 "beacon_grace_s": 4}
```

## Adding a rule

1. Client signal: add `probe('my_probe', 'consistency', 30, () => [value, suspicious])`
   in `sites/collector.js` (or an `add(...)` line in `behaviorProbes()`).
   Server or session signal: add a `chk_*(session, cfg)` function in
   `scoring.py` that returns a short detail string when it fires.
2. Add one entry to `scoring.RULES`:
   `P("my_probe", 30, "consistency", "why it means automation")` or
   `S("my_rule", 30, "server", chk_my_rule, "why")`.
3. Add a row to the catalogue below.
4. Run the scenarios, refresh fixtures with `--save-fixtures fixtures` if the
   record format changed, and run `python3 -m unittest`. The tests fail if
   the collector and the catalogue disagree, or if this README misses a rule.

## Rule catalogue

Weights are starting points, set by hand. Categories: `automation` (direct
evidence of a driver, a CDP client or patching), `consistency` (values that
contradict the claimed browser or OS), `fingerprint` (device surfaces),
`behavior` (input), `server` (network and request shape).

| Rule | Weight | Category | Rationale |
|---|---|---|---|
| `webdriver` | 95 | automation | navigator.webdriver is true: the browser was started for automation. |
| `ua_headless` | 95 | automation | navigator.userAgent says HeadlessChrome. |
| `ua_ch_headless` | 90 | automation | UA-CH brands (JS or sec-ch-ua) include HeadlessChrome. |
| `automation_keys` | 90 | automation | Driver globals or attributes (cdc_, __playwright, __pw_, _selenium, ...). |
| `cdp_runtime` | 80 | automation | Console argument getters ran: a CDP client has Runtime enabled. |
| `outer_size_zero` | 60 | automation | outerWidth/outerHeight is 0: no real window. |
| `native_functions` | 50 | automation | A native getter or method has visible source, a prototype or a wrapper frame. |
| `iframe_mismatch` | 45 | automation | A fresh iframe sees different navigator values: patching missed frames. |
| `worker_mismatch` | 45 | automation | A worker sees different navigator values: patching missed workers. |
| `webdriver_tamper` | 40 | automation | navigator.webdriver was deleted, moved or redefined. |
| `extra_globals` | 30 | automation | The page window has globals a clean iframe does not: injected script. |
| `property_order` | 30 | automation | navigator enumerates keys that are not on its prototype chain. |
| `stack_hooked` | 25 | automation | Error.prepareStackTrace set or stackTraceLimit changed: stack hiding. |
| `has_focus` | 10 | automation | Visible document without focus at load (common for headless). |
| `stack_engine` | 60 | consistency | Error.stack format does not match the engine the UA claims. |
| `ua_ch_mismatch` | 50 | consistency | userAgentData brands/platform/mobile disagree with the UA or sec-ch-ua. |
| `notification_permission` | 50 | consistency | Notification.permission disagrees with permissions.query(). |
| `webgl_os_mismatch` | 50 | consistency | WebGL renderer belongs to a different OS than the UA. |
| `plugins` | 45 | consistency | plugins/mimeTypes count, names or prototypes differ from Chrome/Firefox. |
| `chrome_object` | 40 | consistency | window.chrome missing (or present on Firefox) for the claimed UA. |
| `languages` | 40 | consistency | navigator.languages disagrees with the Accept-Language header. |
| `timezone_mismatch` | 40 | consistency | Intl time zone offset differs from Date.getTimezoneOffset(). |
| `screen_sanity` | 35 | consistency | Screen and viewport sizes that no real desktop has (viewport == screen, 800x600, ...). |
| `fonts` | 35 | consistency | Installed fonts do not fit the UA's OS. |
| `window_frame_absent` | 25 | consistency | outer == inner size: no tab strip or toolbar. |
| `hardware` | 20 | consistency | hardwareConcurrency/deviceMemory missing or impossible. |
| `locale_mismatch` | 15 | consistency | Intl locale language differs from navigator.language. |
| `perf_now_resolution` | 15 | consistency | performance.now() resolution differs from the claimed browser's. |
| `timezone_utc` | 10 | consistency | Time zone is UTC: typical of servers and containers, rare on desktops. |
| `screen_no_taskbar` | 10 | consistency | Windows/macOS screen without taskbar or menu bar. |
| `connection` | 10 | consistency | navigator.connection missing on Chrome or present on Firefox. |
| `battery` | 10 | consistency | getBattery missing on Chrome. |
| `webgl_software` | 45 | fingerprint | Software WebGL renderer (SwiftShader, llvmpipe): no GPU. |
| `canvas` | 30 | fingerprint | Canvas output changes between identical renders or solid fills: noise injection. |
| `audio` | 25 | fingerprint | OfflineAudioContext output unstable or missing. |
| `webgl_params` | 0 | fingerprint | Info only: WebGL parameter and extension hashes. |
| `beh_untrusted` | 70 | behavior | Input events with isTrusted=false: events dispatched by script. |
| `beh_typing_no_keys` | 45 | behavior | Text appeared in a field with no key events (CDP insertText, fill()). |
| `beh_click_center` | 40 | behavior | Clicks land within 1 px of the target's exact centre. |
| `beh_click_no_path` | 35 | behavior | Clicks with fewer than 3 mouse moves before them: the pointer teleported. |
| `beh_mouse_linear` | 35 | behavior | 85%+ of the mouse path is long dead-straight pieces (within 1 px of a line). |
| `beh_key_rhythm` | 35 | behavior | Inter-key intervals almost constant (MAD/median < 0.1). |
| `beh_key_dwell` | 35 | behavior | Key hold times under 10 ms, or all the same (MAD/median < 0.08). |
| `beh_click_hold` | 30 | behavior | Mouse button held under 8 ms. |
| `beh_movement_zero` | 30 | behavior | Pointer moves but movementX/Y stay 0. |
| `beh_input_without_focus` | 30 | behavior | Key or button input while document.hasFocus() is false. |
| `no_interaction` | 30 | behavior | No trusted mouse, key, wheel or touch input in the session. |
| `beh_mouse_velocity` | 25 | behavior | Flat speed profile: stroke middles no faster than their ends. |
| `beh_ttfi` | 25 | behavior | First click/key/wheel < 300 ms after navigation start or < 150 ms after load. |
| `beh_scroll_no_input` | 25 | behavior | Page scrolled with no wheel, key, touch or press before it. |
| `beh_wheel_pattern` | 20 | behavior | One huge wheel delta, or a perfectly regular wheel stream. |
| `page_dwell_short` | 15 | behavior | Left a page within 1.5 s of navigation after clicking. |
| `beh_counts` | 0 | behavior | Info only: event counts and page dwell. |
| `no_beacon` | 95 | server | The page loaded but no beacon came back: no JavaScript ran. |
| `ua_headless_header` | 95 | server | User-Agent header says HeadlessChrome. |
| `non_browser_ua` | 90 | server | User-Agent is not a browser (curl, python-requests, ...). |
| `no_subresources` | 60 | server | No CSS, JS or images fetched: an HTML-only client. |
| `js_ua_mismatch` | 60 | server | navigator.userAgent differs from the User-Agent header. |
| `ua_changed` | 50 | server | The User-Agent changed within one session. |
| `ch_header_mismatch` | 50 | server | sec-ch-ua brands do not match the UA major. |
| `missing_client_hints` | 40 | server | Chrome UA without sec-ch-ua. |
| `missing_fetch_metadata` | 40 | server | Browser UA without Sec-Fetch-* headers. |
| `accept_language_missing` | 40 | server | Browser UA without Accept-Language. |
| `cookie_not_echoed` | 40 | server | The session cookie was not sent back. |
| `http_version` | 40 | server | HTTP/1.0 or another version no current browser uses. |
| `header_order` | 30 | server | Header names, case or order differ from the claimed browser. |
| `images_blocked` | 30 | server | CSS/JS fetched but no images: image blocking. |
| `accept_encoding_mismatch` | 25 | server | Accept-Encoding differs from the claimed browser. |
| `accept_mismatch` | 20 | server | Navigation Accept header differs from the claimed browser. |
| `favicon_missing` | 10 | server | No favicon request (headless Chrome skips it). |
| `uach_platform_version` | 40 | consistency | UA-CH `platformVersion` impossible for the OS: Linux must be `""` (captured); Windows 1-10.0.0 or 13+.0.0; macOS 12-15, 26, 27. |
| `uach_arch_gpu` | 40 | consistency | UA-CH architecture/bitness/wow64 contradict the OS or GPU: arm Mac with a non-Apple GPU, Apple GPU with x86, Intel Mac on macOS 27, wow64 off Windows. |
| `ch_high_entropy` | 40 | server | After `Accept-CH`, a later document lacks the high-entropy `Sec-CH-UA-*` headers or they differ from `getHighEntropyValues`. |
| `api_throws` | 40 | consistency | `canPlayType`, `isTypeSupported`, `decodingInfo`, `getLayoutMap` or `requestAdapter` threw, or `mediaCapabilities`/`keyboard`/`gpu`/`MediaSource`/`MediaRecorder`/`speechSynthesis` is missing in a secure context. |
| `webgl_backend_os` | 35 | consistency | ANGLE backend or limits do not fit the OS: OpenGL on Windows/macOS, D3D11/Metal on Linux, D3D11 with line width > 1 or viewport < 16384, Metal with viewport > 16384 (Windows/macOS limits not captured). |
| `fonts_os_markers` | 35 | consistency | Fonts only another OS has: Linux fonts on Windows/macOS, real (non-alias) Windows/macOS fonts on Linux, Segoe UI Variable with a Windows 10 `platformVersion`. |
| `os_api_surface` | 30 | consistency | `navigator.share`/`canShare` present on Linux (captured) or missing on Windows/macOS (not captured). |
| `generic_fonts_os` | 30 | consistency | `system-ui` is not Segoe UI on Windows or is a real Segoe UI elsewhere; `sans-serif`/`serif` not Arial/Times New Roman (Helvetica/Times on macOS); no Arial (Liberation) on Linux. |
| `voices_os` | 30 | consistency | Microsoft voices off Windows, Apple voices off macOS, or no local voices on Windows/macOS. |
| `scrollbar_os` | 25 | consistency | Scrollbar width of another OS (Windows 10 16-17, Windows 11 0/15-17, macOS 0/15, Linux 15 captured), or scrollbars but none on an overflowing root. |
| `emoji_render` | 25 | fingerprint | An emoji drawn on a canvas has no coloured pixels: no colour emoji font. |
| `webgpu_os` | 25 | consistency | WebGPU adapter vendor differs from the WebGL GPU vendor. |
| `os_bluetooth` | 15 | consistency | `navigator.bluetooth`/`Bluetooth` present on Linux (captured) or missing on Windows/macOS (not captured). |
| `codecs_os` | 0 | consistency | Info until Windows/macOS captures (target 15): HEVC "probably" on Linux with Mesa, or H.264 1080p not power efficient on Windows/macOS with a GPU. |
| `screen_os` | 0 | consistency | Info until captures (target 15): macOS menu bar not 23-25/30-39 px, Windows taskbar not 40/48 px at the DPR. |
| `wheel_os` | 0 | behavior | Info until Windows/macOS captures (target 20): `wheelDeltaY/deltaY` not about -1 (Linux, captured), -1.2 (Windows) or -3 (macOS). |
| `os_surface` | 0 | consistency | Info only: scrollbars, OS-gated APIs, fonts, emoji, WebGL limits, screen. |
| `os_media` | 0 | consistency | Info only: high-entropy UA-CH, codecs, WebGPU, keyboard map, API errors. |
| `os_voices` | 0 | consistency | Info only: speech voices, read at every beacon (they load late). |
| `beh_wheel_deltas` | 0 | behavior | Info only: `deltaY` and `wheelDeltaY` of trusted wheel events. |

Notes:

- `cdp_runtime` (console argument getters run while a CDP client has
  `Runtime.enable` on) did not fire on Chrome 153: newer V8 no longer
  touches these getters when it serialises console arguments. Kept for older
  engines and as a check that LightBrowser never leaks it.
- Server header expectations (`header_order`, `accept_mismatch`,
  `accept_encoding_mismatch`) are in `scoring.EXPECTED`. Chrome values were
  captured from Chrome for Testing 153 on loopback; Firefox values are from
  Firefox 128 and must be re-captured before they are trusted.
- Playwright's headless shell sends no `Accept-Language`, reports 0 plugins,
  no `window.chrome`, `Notification.permission` `denied` against a `prompt`
  query, viewport equal to screen, and SwiftShader WebGL. A plain
  `--headless=new` Chrome over CDP (T-010 run here) hides some of these,
  but is still caught by its UA, software WebGL and input.
- Behaviour rules were checked against T-001/T-003 (they fire) and the
  synthetic T-020 trace (none fire). They have not been calibrated on
  recordings of real people yet; record some before trusting their weights.
- Fonts: on Linux, metric-compatible fonts (Carlito, Caladea) make Windows
  fonts look installed, so foreign fonts only count against Windows/macOS
  UAs.

## OS rules (M7)

The OS rules check the claimed OS (from the User-Agent) against surfaces
that differ between Windows, macOS and Linux. Info probes carry raw
values (`os_surface`, `os_media`, `os_voices`, plus `beh_wheel_deltas`) and
the `chk_*` functions in `scoring.py` judge them, so every rule has unit
tests on `fixtures/os_*_chrome.json`. Existing rules and probes are
unchanged.

Ground truth. Linux comes from the local Chrome for Testing 153, headed
under `xvfb-run` and `--headless=new`, captured on loopback by
`tools/os_truth` (`tests/fixtures/os/`). Windows and macOS values are from
the surface reference in `docs/ROADMAP.md` ("M7 status") and are not
captured; rules that rest only on them are marked below or start at weight
0. Captured facts that shaped the rules:

- Linux Chrome 153 reports `platformVersion` `""` (JS and
  `Sec-CH-UA-Platform-Version`), not the kernel version.
- `navigator.deviceMemory` is 16 on this 15 GB machine: Chrome 153 does not
  cap it at 8, so there is no cap rule.
- Linux scrollbars are 15 px; Playwright's headless shell hides them (0 px),
  which `scrollbar_os` catches.
- X11 wheel notches give `deltaY` 120 and `wheelDeltaY` -120.
- `navigator.share`, `canShare`, `bluetooth` are absent on Linux;
  `navigator.keyboard`, `gpu` (adapter `null` without a GPU),
  `mediaCapabilities` are present; none of the media/keyboard/WebGPU calls
  throw.
- This container's fontconfig maps "Segoe UI" and "Helvetica Neue" to Inter,
  so on Linux a foreign font only counts when its size differs from every
  other detected font (an alias has the same size as the font it maps to).

Stock check (2026-11-03): T-010 with stock Chrome 153 over CDP, headed
under `xvfb-run` and `--headless=new`, fires no OS rule, also with the info
rules at their target weights; T-001/T-003 still score 100. The Critical-CH
retry leaves `header_order` and `cookie_not_echoed` quiet. Re-checked at the
M7 close-out (after WP-E): still no OS rule fires, and all eight reference
LightBrowser personas score fp 0 (Playwright input) and beh 0 (`--input
human`) with the three info rules at their target weights.

## What LightBrowser needs from this bench

- M4: `node run.js --scenario T-010 --cdp <lightbrowser ws url> --expect 'fp<=30'`
  with a Chrome persona: no automation, consistency or fingerprint rule
  should fire, and the same seed should give the same `canvas`, `audio`,
  `webgl_params` and `fonts` hashes on every run.
- M5: with `human_*` input (`--input human`), `beh<=30`. With the
  default Playwright input the behaviour rules must keep firing: the bench
  still has to catch naive automation driving LightBrowser.

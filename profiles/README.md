# profiles

Persona profiles, TLS/H2 profiles, request-header profiles and font tables. Milestones: M1 (TLS), M4 (Chrome personas, headers, fonts; done), M7 (OS personas, catalog, generator), M4b (Firefox).

## Persona

- **persona_v2**: `profiles/schemas/persona_v2.schema.json`. The stealth-browser `persona_v1` schema plus `browser.family` (`chrome` | `firefox`), `client_hints` (Chrome only), `timing`, `behavior` and optional `audio`, `speech_voices`, `permissions`, `media_devices` (labels and per-kind counts), `battery`, `preferences` (color scheme, contrast, color gamut, scrollbar width, ...) and `hardware.js_heap_size_limit`. M7 (ADR 0015) adds optional OS fields: `os.kernel`, `os.distro` (`ubuntu-24.04`, `fedora-42`), `os.display_server` (`x11` | `wayland`), `screen.window_x/window_y/maximized`, `fonts.defaults`, `audio.output_latency`, `behavior.wheel`, `client_hints.form_factors`, `webgpu` (`null` = no adapter), `media` (HEVC, H.264 power efficiency, AV1), `apis` (`share`, `bluetooth`) and `provenance`. Each field is documented in the schema (`additionalProperties: false` everywhere; a test checks every persona file uses declared fields only). Absent optional fields take the OS default through the `PersonaProfile` accessors (`lb-persona` `os.rs`). Loaded by `lb-persona` (`PersonaProfile::from_path`).
- **v1 compatibility**: v1 personas load as `family = firefox` with no `client_hints`. They run on the firefox binary (M4b).
- **Consistency rules**: `lb-persona::consistency` (R01-R26). `lightbrowser validate <profile.json>` prints the report; `cargo test -p lb-persona` runs them on every file here. The JSON schema checks shape only; the rules check cross-field consistency. R17-R26 (Chrome only) check the persona against its OS: platform version, architecture and GPU, GPU backend and limits (and the kernel in Mesa strings), screen geometry, fonts, voices, audio (warning), preferences, hardware and macOS versions.
- **Provenance**: Linux values follow the local Chrome for Testing 153 capture (`tests/fixtures/os/`); Windows and macOS values are the M7 surface reference, not captures. Each M7 persona says so in `provenance`.

### Directories

- `reference/firefox/`: v1 files copied unchanged from stealth-browser (captured from real devices). The stealth-browser Apple Silicon references are not copied: they list `EXT_disjoint_timer_query`, which rule R04 rejects on Apple Silicon.
- `reference/chrome/`: v2 Chrome 153 personas, hand-built, not captured from devices; WebGL limits and extension lists are approximate. `lb-page`'s fingerprint-report gate runs every file here.

| File | OS | GPU (backend) | Screen |
|---|---|---|---|
| `chrome153_win10_rtx3060_1080p` | Windows 10 22H2 (`10.0.0`) | NVIDIA RTX 3060 (D3D11) | 1920x1080, taskbar 40 |
| `chrome153_win11_rtx4060_1080p` | Windows 11 24H2 (`19.0.0`), Segoe UI Variable | NVIDIA RTX 4060 (D3D11) | 1920x1080, taskbar 48 |
| `chrome153_win11_irisxe_laptop_1536x864` | Windows 11 23H2 (`15.0.0`), on battery | Intel Iris Xe (D3D11) | 1536x864 @1.25 |
| `chrome153_macos14_intel_irisplus_1440x900` | macOS 14.7.1, Intel (x86) | Intel Iris Plus (Metal) | 1440x900 @2, menu bar 25 |
| `chrome153_macos15_apple_m2_retina` | macOS 15.0, Apple M2 | Apple M2 (Metal) | 1470x956 @2, notch 37 |
| `chrome153_macos26_apple_m4_1512x982` | macOS 26.1, Apple M4 (10 threads) | Apple M4 (Metal) | 1512x982 @2, notch 38, HDR |
| `chrome153_ubuntu24_intel_uhd630_x11_1080p` | Ubuntu 24.04, X11, kernel 6.8 | Intel UHD 630 (Mesa GL) | 1920x1080, top bar 32, dock 64 |
| `chrome153_fedora42_amd_rx6600_wayland_1440p` | Fedora 42, Wayland | AMD RX 6600 (radeonsi GL, kernel in the string) | 2560x1440, avail = screen, window 0,0 |

  Voices: Windows has the 3 SAPI voices plus Chrome's 19 Google network voices; macOS has the voices macOS 13+ installs by default (compact, novelty and the Eloquence voices in 14 locales, 176 in all) plus the Google voices (reference, not captured); Linux has none (the capture had no speech-dispatcher).
- `catalog/`: what the generator samples: `os.json` (releases, UA OS tokens, locales), `cpu.json` (thread counts, RAM, Apple chips with thread counts and first macOS, Intel Macs with last macOS), `gpu.json` (WebGL per ANGLE backend: D3D11, Metal, Mesa GL; GPUs per OS), `screen.json`, `fonts.json` (installed families per release), `voices.json`, `audio.json` (rates, media devices). Hand-authored data; each file's `note` gives its provenance.
- `generated/{windows,macos,linux}/`: five personas per OS from `cargo run -p lb-persona --example gen_personas -- --os <os> --count 5 --seed 1`. The generator (`lb_persona::generate`) is deterministic per seed and refuses any persona that breaks a rule; a test generates 50 per OS and checks the committed files match seed 1.
- `generated/adversarial/`: intentionally invalid personas (`adversarial: true`). Tests assert which rules they trip. The M7 ones each break exactly one rule; serve them with the hidden `lightbrowser serve --unchecked-persona FILE` to see the bench catch them:

| File | Rule | Bench rule that fires (T-010) |
|---|---|---|
| `chrome153_linux_with_d3d11` | R19 | `webgl_backend_os`, `webgl_os_mismatch` |
| `chrome153_win_metal_renderer` | R19 | `webgl_backend_os`, `webgl_os_mismatch` |
| `chrome153_linux_mesa_kernel_mismatch` | R19 | none: validate-only (the page sees the Mesa renderer string but nothing it could compare the kernel with; ADR 0015) |
| `chrome153_mac_arm_intel_gpu` | R18 | `uach_arch_gpu` |
| `chrome153_win11_fonts_pv10` | R17 | `fonts_os_markers` |
| `chrome153_linux_segoe_fonts` | R21 | `fonts_os_markers` (since WP-C a persona font with no table for its OS borrows another OS's table) |
| `chrome153_wayland_avail_top` | R20 | `window_work_area` (info rule, target weight 15: window at 0,0 under a non-zero work-area offset) |
| `chrome153_mac_scrollbar17` | R24 | `scrollbar_os` (17 px scrollbars on macOS, since WP-C) |
| `chrome153_mac_microsoft_voices` | R22 | `voices_os` |
| `chrome153_intel_mac_macos27` | R26 | `uach_arch_gpu` |
| `chrome153_devicememory12` | R25 | `hardware` |

## TLS (`profiles/tls/`)

- `firefox_128_{linux,macos,win10}.json`: copied from `stealth-browser/profiles/tls`. Used by M4b.
- `chrome_153_linux.json`: captured from Chrome for Testing 153 with `lb-fptest` (ClientHello, H2 SETTINGS, pseudo-header order). Used for every Chrome persona; the TLS stack does not depend on the OS. Other Chrome majors need a re-capture (`tests/README.md`).

## Request headers (`profiles/headers/`)

- `chrome_153.json`: per request kind, the header slots in h2 order, the HTTP/1.1 rules and Blink's observed subresource orders. Values (UA, `sec-ch-ua*`, language) are never stored here; they come from the persona. Loaded by `lb_net::HeaderProfile` (ADR 0011).
- `captures/chrome_153_linux.jsonl`: the 69 requests captured from Chrome for Testing 153.0.8010.12 on loopback. Evidence for the profile (its h2 `cookie` position is an artifact of the decoder, ADR 0011).
- `captures/chrome_153_linux_accept_ch.jsonl`: 88 requests after `Accept-CH` and `Critical-CH` (HTTP/1.1 and h2, loopback, `tools/header_capture/accept_ch.js`). Evidence for the client-hint slots, the `document_restart` list and `renderer_group.observed_hints`; the golden test in `lb-net/tests/accept_ch.rs` replays it.

## Fonts (`profiles/fonts/`)

Per-OS metric tables, `derived.json` estimates for persona fonts without a table, and glyph outline packs for canvas text. See `profiles/fonts/README.md`.

# profiles

Persona profiles, TLS/H2 profiles, request-header profiles and font tables. Milestones: M1 (TLS), M4 (Chrome personas, headers, fonts; done), M4b (Firefox).

## Persona

- **persona_v2**: `profiles/schemas/persona_v2.schema.json`. The stealth-browser `persona_v1` schema plus `browser.family` (`chrome` | `firefox`), `client_hints` (Chrome only), `timing`, `behavior` and optional `audio`, `speech_voices`, `permissions`, `media_devices` (labels and per-kind counts), `battery`, `preferences` (color scheme, contrast, color gamut, scrollbar width, ...) and `hardware.js_heap_size_limit`. Each field is documented in the schema; absent optional fields take Chrome 153's desktop answer. Loaded by `lb-persona` (`PersonaProfile::from_path`).
- **v1 compatibility**: v1 personas load as `family = firefox` with no `client_hints`. They run on the firefox binary (M4b).
- **Consistency rules**: `lb-persona::consistency` (R01-R16). Run `validate_file(path)` (to become `lightbrowser validate <profile.json>`) or `cargo test -p lb-persona`. The JSON schema checks shape only; the rules check cross-field consistency.
- `reference/firefox/`: v1 files copied unchanged from stealth-browser (captured from real devices). The stealth-browser Apple Silicon references are not copied: they list `EXT_disjoint_timer_query`, which rule R04 rejects on Apple Silicon.
- `reference/chrome/`: v2 Chrome 153 personas (Windows 10, macOS 15). They are hand-built from known Chrome values, not captured from a device; WebGL limits and extension lists are approximate. The Windows speech voice list is Chrome's usual one (3 Microsoft + 19 Google voices); the macOS list is a representative subset of the Apple voices, not a capture. Re-capture before relying on them for a detection run.
- `generated/adversarial/`: intentionally invalid personas (`adversarial: true`). Tests assert which rules they trip.

## TLS (`profiles/tls/`)

- `firefox_128_{linux,macos,win10}.json`: copied from `stealth-browser/profiles/tls`. Used by M4b.
- `chrome_153_linux.json`: captured from Chrome for Testing 153 with `lb-fptest` (ClientHello, H2 SETTINGS, pseudo-header order). Used for every Chrome persona; the TLS stack does not depend on the OS. Other Chrome majors need a re-capture (`tests/README.md`).

## Request headers (`profiles/headers/`)

- `chrome_153.json`: per request kind, the header slots in h2 order, the HTTP/1.1 rules and Blink's observed subresource orders. Values (UA, `sec-ch-ua*`, language) are never stored here; they come from the persona. Loaded by `lb_net::HeaderProfile` (ADR 0011).
- `captures/chrome_153_linux.jsonl`: the 69 requests captured from Chrome for Testing 153.0.8010.12 on loopback. Evidence for the profile.

## Fonts (`profiles/fonts/`)

Per-OS metric tables, `derived.json` estimates for persona fonts without a table, and glyph outline packs for canvas text. See `profiles/fonts/README.md`.

# profiles

Placeholder. Persona profiles and TLS profiles. Milestones: M1 (TLS), M4 (Chrome personas), M4b (Firefox).

## Persona

- **persona_v2**: `profiles/schemas/persona_v2.schema.json`. The stealth-browser `persona_v1` schema plus `browser.family` (`chrome` | `firefox`), `client_hints` (Chrome only), `timing`, `behavior` and optional `audio`, `speech_voices`, `permissions`. Each field is documented in the schema. Loaded by `lb-persona` (`PersonaProfile::from_path`).
- **v1 compatibility**: v1 personas load as `family = firefox` with no `client_hints`. They run on the firefox binary (M4b).
- **Consistency rules**: `lb-persona::consistency` (R01-R16). Run `validate_file(path)` (to become `lightbrowser validate <profile.json>`) or `cargo test -p lb-persona`. The JSON schema checks shape only; the rules check cross-field consistency.
- `reference/firefox/`: v1 files copied unchanged from stealth-browser (captured from real devices). The stealth-browser Apple Silicon references are not copied: they list `EXT_disjoint_timer_query`, which rule R04 rejects on Apple Silicon.
- `reference/chrome/`: v2 Chrome 153 personas (Windows 10, macOS 15). They are hand-built from known Chrome values, not captured from a device; WebGL limits and extension lists are approximate. Re-capture before relying on them for a detection run.
- `generated/adversarial/`: intentionally invalid personas (`adversarial: true`). Tests assert which rules they trip.

## TLS (`profiles/tls/`)

- `firefox_128_{linux,macos,win10}.json`: copied from `stealth-browser/profiles/tls`. Used by M4b.
- Chrome profiles still to capture (ClientHello with tshark, plus H2 SETTINGS and header order) for Chrome on Windows, macOS and Linux. Needed for M1 and M4.

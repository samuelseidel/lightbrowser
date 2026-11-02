# profiles

Placeholder. Persona profiles and TLS profiles. Milestones: M1 (TLS), M4 (Chrome personas), M4b (Firefox).

## Persona

- **persona_v2** (planned, M4): extends the stealth-browser `persona_v1` schema with `browser.family` (`chrome` or `firefox`) and `client_hints`. Schema file: `profiles/schemas/persona_v2.schema.json`.
- **v1 compatibility**: existing v1 personas load as `family = firefox`. They run on the firefox binary (M4b).
- Reference personas are captured from real devices, not generated. Generated and adversarial sets follow the layout used in stealth-browser.

## TLS (`profiles/tls/`)

- `firefox_128_{linux,macos,win10}.json`: copied from `stealth-browser/profiles/tls`. Used by M4b.
- Chrome profiles still to capture (ClientHello with tshark, plus H2 SETTINGS and header order) for Chrome on Windows, macOS and Linux. Needed for M1 and M4.

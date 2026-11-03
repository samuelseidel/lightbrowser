# profiles

Placeholder. Persona profiles and TLS profiles. Milestones: M1 (TLS), M4 (Chrome personas), M4b (Firefox).

## Persona

- **persona_v2** (planned, M4): extends the stealth-browser `persona_v1` schema with `browser.family` (`chrome` or `firefox`) and `client_hints`. Schema file: `profiles/schemas/persona_v2.schema.json`.
- **v1 compatibility**: existing v1 personas load as `family = firefox`. They run on the firefox binary (M4b).
- Reference personas are captured from real devices, not generated. Generated and adversarial sets follow the layout used in stealth-browser.

## TCP (`profiles/tcp/`)

- `windows_65535.json`, `windows_64240.json`, `macos_14.json`, `linux_6.json`: what each stack puts in
  a client SYN (TTL, DF, window, MSS, window scale, option order, ECN). A persona
  picks one with `tcp_profile_id`; its OS must match the User-Agent. ADR 0007.
- `provenance.status`: `captured` (measured on a stack of ours; Linux),
  `public-data` (attested by real traffic in `reference/tcp_public.json`; Windows,
  macOS) or `documented-unverified`. `provenance.unverified` lists what the status
  does not cover (SYN retransmit timing). Re-capture steps are in `tests/README.md`.
- `reference/tcp_public.json`: shares from real client SYNs in the public Zardaxt
  dataset, rebuilt by `scripts/tcp_public_reference.py`. Aggregates only; the source
  file's licence forbids copying it. ADR 0009.
- `slot` must be unique: it selects the profile in the packet shim.
- `ip.id`: `kernel` (leave it), `zero` (macOS), `host_counter` (Windows).
- `syn_retransmit.schedule_ms`: when an unanswered SYN is retransmitted, in ms
  after the first; `option_variants` change the option layout from an attempt on
  (Windows drops window scale). Omit it to keep the kernel's schedule, which is
  kernel-dependent (Linux 6.1+: 1, 2, 3, 4, 6, 10 s).
- Sources for the values are in each profile's `provenance.notes` and ADR 0008.

## TLS (`profiles/tls/`)

- `firefox_128_{linux,macos,win10}.json`: copied from `stealth-browser/profiles/tls`. Used by M4b.
- Chrome profiles still to capture (ClientHello with tshark, plus H2 SETTINGS and header order) for Chrome on Windows, macOS and Linux. Needed for M1 and M4.

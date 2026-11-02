# tests

Placeholder. Test suites arrive with the milestones that need them (M1 onward).

Planned suites:

- **WPT subset** (M2): web-platform-tests for DOM, events, timers, fetch.
- **Playwright CDP suite** (M3): `connectOverCDP` with goto, evaluate, click, fill, getByRole, content.
- **Fingerprint tests** (M4, M4b): run against OUR detection engine only. Same seed must give the same fingerprint; Chrome personas must score as consistent.
- **Network fingerprint tests** (M1): a local TLS/H2 echo server reports the JA3/JA4 and Akamai H2 hash the client sent, compared with real Chrome (and Firefox 128 for M4b).

Unit tests stay inside each crate.

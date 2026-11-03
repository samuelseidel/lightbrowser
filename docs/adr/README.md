# Architecture Decision Records

Short records of the main decisions behind LightBrowser. Each has a Title,
Status, Context, Decision, Alternatives considered and Consequences.
`../ARCHITECTURE.md` and `../ROADMAP.md` summarize how they fit together.

| ADR | Title | Status |
|---|---|---|
| [0001](0001-rust.md) | Rust for everything we write | Accepted (2026-11-02) |
| [0002](0002-embedded-js-engines.md) | Embed the JS engine: V8 for Chrome, SpiderMonkey for Firefox | Accepted (2026-11-02) |
| [0003](0003-boringssl-h2-fork.md) | BoringSSL and a forked h2 for network fingerprints | Accepted (2026-11-02) |
| [0004](0004-lazy-layout.md) | Lazy style and layout, CPU canvas, no painting by default | Accepted (2026-11-02) |
| [0005](0005-cdp.md) | CDP as the control API for both families | Accepted (2026-11-02) |
| [0006](0006-browser-families.md) | Two browser families, one binary per family | Accepted (2026-11-02) |
| [0007](0007-tcp-stack-fingerprint.md) | TCP/IP stack fingerprint: socket options plus a netfilter shim | Accepted (2026-11-03) |

## Adding an ADR

1. Copy the format of an existing ADR. Use the next number: `NNNN-short-name.md`.
2. Keep it to 30-70 lines, in plain English.
3. Add a row to the table above, and to the Decisions table in
   `../ARCHITECTURE.md` if it changes the architecture.
4. Do not edit an accepted ADR to change its decision. Write a new ADR that
   supersedes it, and change the old one's status to
   `Superseded by NNNN`.

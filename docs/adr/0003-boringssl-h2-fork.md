# ADR 0003: BoringSSL and a forked h2 for network fingerprints

Status: Accepted (2026-11-02)

## Context

Servers fingerprint a client before any JavaScript runs:

- **TLS ClientHello:** JA3 and JA4 (cipher order, extensions and their order,
  curves, ALPN, GREASE, ALPS, certificate compression, ECH GREASE,
  delegated_credentials, record_size_limit).
- **HTTP/2:** the Akamai fingerprint (SETTINGS values and order, initial
  WINDOW_UPDATE, PRIORITY frames, pseudo-header order).
- **Headers:** order, `Accept`, `Accept-Language`, `Sec-CH-UA*`.

Chrome sends pseudo-headers as `m,a,s,p`. Firefox sends `m,p,a,s` and uses
PRIORITY frames. Chrome permutes its extension order per connection. Chrome
uses BoringSSL, Firefox uses NSS.

The stealth-browser repo already captured Firefox 128 ClientHellos as
`profiles/tls/firefox_128_*.json`.

## Decision

- **TLS:** BoringSSL through the `boring` and `tokio-boring` crates. A
  per-family profile drives the ClientHello. For Chrome it matches natively
  (GREASE, extension permutation, ALPS, brotli cert compression, ECH GREASE).
  For Firefox we configure BoringSSL to emit Firefox's order and extensions
  (delegated_credentials, record_size_limit), checked against the captured
  profiles.
- **HTTP/1.1:** hyper 1.x.
- **HTTP/2:** a fork of the `h2` crate that exposes SETTINGS order and values,
  the initial WINDOW_UPDATE, PRIORITY frames and pseudo-header order. The
  values come from the same TLS profile JSON (`http2_*` fields).
- **HTTP/3:** later, through quiche. Not scheduled in M0–M6.
- Header order, Accept and client hints are set in `lb-net` from the persona.

The target for M1 is that JA3/JA4 and the Akamai H2 hash match real Chrome when
measured on our local TLS/H2 echo server.

## Alternatives considered

- **rustls.** Pure Rust and pleasant, but it cannot do GREASE, extension
  permutation, ALPS, or Chrome's certificate compression set, and it fixes the
  extension order. It cannot reproduce either family's ClientHello.
- **NSS** (what Firefox uses). Would give exact Firefox TLS, but it means a
  second TLS stack, a C build dependency with a poor Rust story, and no help for
  Chrome. GeckoPhantom had to patch NSS directly; we want configuration, not
  patches.
- **OpenSSL.** Less control over extension order and GREASE than BoringSSL.
- **Replay a captured ClientHello as raw bytes.** Breaks on session resumption,
  key shares and any server-driven change, and cannot be tested well.
- **Unmodified `h2`.** Hard-codes SETTINGS order and pseudo-header order, so
  the Akamai fingerprint would not match.

## Consequences

- Chrome TLS fingerprints match without custom patches. This is the main
  reason to pick BoringSSL.
- Firefox is mostly reproducible. A few NSS-only details may need small
  BoringSSL patches (a known risk in the roadmap, checked in M4b).
- We maintain a fork of `h2`. We keep the diff small and rebase on upstream
  releases.
- BoringSSL has no stable API promise. We pin the version via the `boring`
  crate.
- Fingerprint data lives in versioned JSON profiles, so new browser versions
  need a new profile and tests, not code changes where possible.
- No rustls means no pure-Rust TLS build. BoringSSL needs a C toolchain.

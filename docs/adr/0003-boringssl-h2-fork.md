# ADR 0003: BoringSSL and a forked h2 for network fingerprints

Status: Accepted (2026-11-02). Implemented in M1 (2026-11-02); see
"M1 outcome" below.

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

## M1 outcome

What was built and measured, with `lb_net::fetch` against the local
`lb-fptest` capture server (`crates/lb-net/tests/fingerprint.rs`):

- **BoringSSL** comes from `boring` / `tokio-boring` / `boring-sys` 5.x.
  Knobs that `boring` does not wrap go through `boring-sys` FFI: the TLS 1.3
  cipher order, ALPS, `trust_anchors` and `server_padding`.
- **The h2 fork is vendored** in `vendor/h2` (upstream 0.4.20, same version
  number) and wired in with `[patch.crates-io]` in the workspace
  `Cargo.toml`. It adds SETTINGS order, pseudo-header order and the priority
  block on request HEADERS. It also writes SETTINGS, WINDOW_UPDATE and the
  first HEADERS before reading. Every change is listed in
  `vendor/h2/LIGHTBROWSER.md`.
- **Profiles** gained `server_padding_request`, `trust_anchor_ids` and
  `http2_priority.headers`. All three are captured by `lb-fptest`.

**Chrome (`chrome_153_linux`, captured from Chromium 153): exact match.** The
following match Chromium on every connection:

- JA4 `t13d1517h2_8daaf6152771_344585cda68d` and JA4_r.
- JA3 over the canonical extension order (`9bddd6d71939cb1fcf03871f2cc2adcf`).
  The wire order is permuted per connection, as in Chrome.
- Every ClientHello list: GREASE, X25519MLKEM768, the ML-DSA sigalgs, ALPS
  17613, brotli, and the trust_anchors and server_padding payloads.
- Akamai H2 `1:65536;2:0;4:6291456;6:262144|15663105|0|m,a,s,p`.
- HEADERS priority (exclusive, depends on 0, weight 256) and the frame order.

**Firefox (`firefox_128_win10`): partial match.** These match:

- Ciphers, including Firefox's TLS 1.3 order (JA4 cipher hash
  `5b57614c22b0`).
- Signature algorithms, ALPN and brotli certificate compression.
- The groups BoringSSL has.
- Every H2 value (`1:65536;2:0;4:131072;5:16384|12517377|0|m,p,a,s`).

Gaps, all closed in M4b with small BoringSSL patches or a re-capture:

| Gap | Cause |
|---|---|
| `delegated_credentials` (34) not sent | BoringSSL has no client side for it |
| `record_size_limit` (28) not sent | not in BoringSSL |
| `ffdhe2048`, `ffdhe3072` (256, 257) not sent | BoringSSL has no finite-field groups |
| Extension order is BoringSSL's, not Firefox's | BoringSSL fixes the order (or permutes it) |
| key_share is `[x25519]`, Firefox sends `[x25519, P-256]` | BoringSSL picks the key shares |
| No `pre_shared_key` (41) | no session cache / resumption yet |
| Profile disagrees with itself: `ja3_string` has 65037 (ECH GREASE) and no 18/27; `extensions_order` has 18, 21, 41 and no 65037; `ja4` is a hand-made raw string | the stealth-browser file was not produced by one capture. Re-capture Firefox 128 with `lb-fptest` |

So Firefox's JA3 and JA4 differ today. The test pins each of these gaps, so
both a regression and a fix make it fail.

HTTP/1.1 (no fingerprint hash, checked by hand against Chromium 153): Chrome
gets `Connection: keep-alive` after `Host` and no `Priority` header. hyper
title-cases names, so `sec-ch-ua*` goes out as `Sec-Ch-Ua*`, where Chrome
sends lowercase. Fixing that needs hyper's private header-case map; it is
open.

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
- Firefox is mostly reproducible. The details above need small BoringSSL
  patches (a known risk in the roadmap, done in M4b).
- A newer Chrome may need a newer BoringSSL. Chrome 153's `trust_anchors`,
  `server_padding` and ML-DSA are already in `boring-sys` 5.4.
- We maintain a fork of `h2` in `vendor/h2`. We keep the diff small (marked
  `LIGHTBROWSER patch`) and rebase on upstream releases.
- BoringSSL has no stable API promise. We pin the version via the `boring`
  crate.
- Fingerprint data lives in versioned JSON profiles, so new browser versions
  need a new profile and tests, not code changes where possible.
- No rustls means no pure-Rust TLS build. BoringSSL needs a C toolchain.

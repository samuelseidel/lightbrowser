# ADR 0033: Public data is the source of truth for TCP profiles

Numbered 0009 before the merge with the M2-M7 branch (2026-11-07), which already used 0007-0009; older commits and notes may use the old number.

Status: Accepted (2026-11-03). Corrects values in ADR 0031 and 0032.

## Context

ADR 0031 and 0032 took the Windows and macOS values from memory, p0f and vendor
documents, and marked them unverified. A public dataset of real client SYNs
exists: the Zardaxt OS database (1,453 fingerprints, 23,467 sessions, each
labelled with the OS its User-Agent claims) at
[NikolaiT/zardaxt](https://github.com/NikolaiT/zardaxt). It also publishes
aggregates in its README. Its licence is all-rights-reserved, so we may analyse
it and cite facts about it, but not copy the file.

## Decision

- **Real traffic outranks everything else.** Order of trust: public traffic data,
  then vendor source code (XNU), then vendor documents, then memory.
- **Commit aggregates only.** `scripts/tcp_public_reference.py` downloads the
  file to a temporary directory and writes the shares to
  `profiles/reference/tcp_public.json`. The file itself is never committed.
- **A test enforces it.** `cargo test -p lb-net --test public_reference` (no
  root) fails if a profile's option layout, TTL, window, window scale, ECN or IP ID
  zero/nonzero is seen in under 5% of real sessions of its OS. Linux's window and
  scale are exempt: that class is mostly Android, so they are measured on the
  kernel instead.
- **Provenance says what is covered.** `status` is `captured`, `public-data` or
  `documented-unverified`; `unverified` lists the parts no public data covers.

## What the data changed

| Trait | Before | Public data | Now |
|---|---|---|---|
| Windows window | 64240 only | 65535 66%, 64240 30%, independent of MSS | two profiles; default `windows_65535` |
| macOS ECE+CWR on SYN | off | set in 67% (73% of canonical sessions) | on |
| macOS window scale | 6 (p0f said 4) | 6 in 87% | kept; p0f is stale |
| macOS IP ID | 0 (from XNU source) | 0 in 92% | confirmed |
| Windows layout, scale 8, ECN off, nonzero ID | from memory | 82%, 82%, 99%, 98% | confirmed |

## Limits

- Labels come from the User-Agent, so classes include traffic behind proxies and
  VPNs (19% of Windows sessions have TTL 64) and one operator's visitors.
- The dataset has no SYN retransmit timing, no IP ID counting pattern and no
  post-handshake windows. Those stay `unverified`, resting on XNU source and
  vendor notes. The Windows IP ID host counter is the least supported.
- 27 to 33% of Macs send a plain SYN. We ship the majority; a `macos_14_plain`
  profile is cheap if needed. A Mac with ECN also marks data packets ECT; the
  shim sets the SYN flags only.

## Consequences

- The default Windows persona now sends window 65535, not 64240.
- Anyone changing a profile must justify it against `tcp_public.json`; refresh
  the file with the script when the source publishes a new build.
- Devices of ours remain the better ground truth: `lb-fptest tcp-capture
  --flow` output should replace `public-data` with `captured` per profile.

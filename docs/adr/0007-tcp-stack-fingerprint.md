# ADR 0007: TCP/IP stack fingerprint: socket options plus a netfilter shim

Status: Accepted (2026-11-03). Implemented for the SYN, IPv4 and IPv6. The
Windows and macOS profiles are unverified (see Consequences; checked against
public traffic data and corrected by [0009](0009-public-data-source-of-truth.md),
which changes the Windows window and macOS ECN in the table below). Extended by
[0008](0008-whole-connection-tcp-shaping.md), which shapes the packets after the
SYN too.

## Context

A server sees the TCP SYN before TLS or HTTP. Passive OS fingerprinting (p0f,
JA4T, and what CDNs and bot vendors build on them) reads it: TTL, DF bit,
window, MSS, window scale, and which options appear in which order. A persona
with a Chrome-on-Windows ClientHello and a `Windows NT` User-Agent still sends a
Linux SYN from a Linux host. Our detection engine should catch that mismatch,
so the browser must also be able to send the consistent version.

| | Windows 10/11 | macOS | Linux 5.x/6.x |
|---|---|---|---|
| TTL / window / scale | 128 / 64240 / 8 | 64 / 65535 / 6 | 64 / 64240 / 7 |
| Option order | MSS NOP WS NOP NOP SACK | MSS NOP WS NOP NOP TS SACK EOL | MSS SACK TS NOP WS |

## Decision

- **Profiles** in `profiles/tcp/<os>.json` (`TcpProfile`): TTL, DF, window, MSS,
  scale, options in wire order, ECN. A persona names one with `tcp_profile_id`;
  `NetContext::with_tcp_profile` refuses one whose OS differs from the UA's.
- **Tier 1, sockets (unprivileged).** Before `connect`: `IP_TTL`, `TCP_MAXSEG`
  (the kernel then also picks window 64240 for MSS 1460) and `TCP_WINDOW_CLAMP`
  (scale = `ilog2(clamp) - 15`). This alone gives the exact Linux SYN.
- **Tier 2, shim (`CAP_NET_ADMIN`).** Sockets cannot reorder options or drop
  timestamps. `lightbrowser tcp-shim` is an NFQUEUE client (own netlink code, no
  `libnetfilter_queue`) that rewrites the options, window and ECN flags of
  marked SYNs and fixes both checksums. A socket's mark, `"LB" << 16 | slot`,
  picks the profile, so Windows, macOS and Linux personas share one process.
- **The rewrite never contradicts the kernel.** It removes, reorders or shrinks
  what the kernel offered, never adds. MSS and scale are `min(kernel, profile)`
  (a smaller scale only makes the peer underestimate our window). Timestamps the
  kernel did not offer are reported, not invented. Dropping them is safe: the
  peer then sends none.
- **Fails closed.** The iptables rule has no `--queue-bypass`: a stopped shim
  drops marked SYNs. `with_tcp_profile` refuses a shim profile when it can see
  the rule is missing.

## Measured

`cargo test -p lb-net --test tcp_fingerprint` (loopback, Linux 6.18, independent
parser in `lb-fptest`): each profile's SYN matches exactly (TTL, DF, window, MSS,
scale, option bytes and order, JA4T) and a 3 MiB response completes through each
handshake. Without a profile the host SYN shows; with the shim stopped none leaves.

## Alternatives considered

- **Userspace TCP stack** (smoltcp over TUN, kernel RSTs suppressed). Full
  control of retransmit timers, timestamp clock and IP ID, but our own congestion
  control and loss recovery. The later option if the engine looks beyond the SYN.
- **eBPF `tc`.** No userspace hop, but harder to build and kernel-specific.
  NFQUEUE only sees SYNs, so its cost is irrelevant.
- **sysctls.** Global, and cannot reorder options. **nft `queue`:** not in the
  kernel we tested (iptables `NFQUEUE` is). **Proxy or VPN on the right OS:** no
  control, and it replaces the SYN.

## Consequences

- Linux only. Windows and macOS personas need root; a Linux persona does not.
- Only the first packet is shaped here; ADR 0008 adds SYN retransmit timing and
  IP ID. Still not mimicked: later window sizes, ECN and RST behaviour.
- Direct connections only. Behind a proxy (M2) the server sees the proxy's SYN.
  NAT, load balancers and MSS clamping can still alter packets.
- `TCP_WINDOW_CLAMP` caps the receive window at `2^(15+scale)` (2 MiB for macOS:
  about 20 MB/s at 100 ms RTT). Fine for pages.
- **Windows and macOS values come from public p0f and JA4T signatures, not our
  devices** (`provenance.status`). Re-capture with `lb-fptest tcp-capture`. Whether
  recent macOS sets ECE+CWR on the SYN is unconfirmed.
- Internal use only, on loopback and our own test servers.

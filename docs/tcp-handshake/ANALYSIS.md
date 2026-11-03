# TCP handshake fingerprint: analysis

Date: 2026-11-03. Scope: a Linux host whose browser must look like a Windows,
macOS or Linux client at the TCP/IP level, for validating our own bot
detection.

## 1. Why it matters

A server sees the TCP SYN before TLS or HTTP. Passive OS fingerprinting (p0f,
JA4T, and CDN and bot-vendor models built on them) reads it. A persona with a
Chrome-on-Windows ClientHello and a `Windows NT` User-Agent that sends a Linux
SYN contradicts itself across layers. Our detection engine should catch that,
and it should also be able to see a persona that is consistent.

## 2. What a server can read

"Shaped" is the state of this repo. "Evidence" is where the value comes from.

| Trait | Windows | macOS | Linux | Shaped | Evidence |
|---|---|---|---|---|---|
| TTL, DF | 128, DF | 64, DF | 64, DF | yes | public data (81%, 99%, 96%) |
| SYN option order | MSS NOP WS NOP NOP SACK | MSS NOP WS NOP NOP TS SACK EOL | MSS SACK TS NOP WS | yes | public data (82%, 93%, 98%) |
| SYN window | 65535 (66%) or 64240 (30%) | 65535 | 64240 (desktop) | yes | public data; Linux measured |
| Window scale | 8 | 6 | 7 | yes | public data (82%, 87%); Linux measured |
| Timestamps in SYN | no | yes | yes | yes | public data |
| ECE+CWR on SYN | no (99%) | yes (67%) | no | yes | public data |
| IPv4 ID | nonzero, host counter | 0 | per-socket counter | yes | public data; XNU source; Linux measured |
| SYN retransmit timing | 1, 3, 7, 15 s; scale dropped from the 2nd | 1, 2, 3, 4, 6, 10 s, ... | kernel: 1, 2, 3, 4, 6, 10 s (6.1+) | yes | vendor notes; XNU source; Linux measured |
| Timestamp clock | none | 1 kHz, random offset | 1 kHz, random offset | not needed | XNU source; Linux measured |
| Window after the handshake | autotuned | autotuned | autotuned | **no** | no source |
| ECT marking of data (macOS with ECN) | no | yes | no | **no** | none |
| RST and FIN behaviour | Windows | macOS | Linux | **no** | none |

## 3. Approaches considered

| Approach | Reach | Why not (or why) |
|---|---|---|
| Socket options (`IP_TTL`, `TCP_MAXSEG`, `TCP_WINDOW_CLAMP`) | TTL, MSS, scale, window | **Used.** Unprivileged. Cannot reorder options or drop timestamps. |
| Netfilter queue shim (rewrite marked packets) | Everything on the wire | **Used** for Windows and macOS. Needs root. Keeps the kernel's TCP engine. |
| `sysctl` (`tcp_timestamps`, TTL) | Timestamps, TTL | Global to the host and cannot reorder options. |
| eBPF `tc` | Same as the shim, no userspace hop | Harder to build, kernel-specific. The shim's cost is fine. |
| Userspace TCP stack (TUN or AF_PACKET) | Everything, including autotuning | Our own congestion control and loss recovery. The option if we must shape what the kernel's engine decides. |
| Proxy or VPN on the right OS | Whole path | No control, and the server sees the proxy's SYN. |
| nft `queue` | Same as the shim | Not compiled into the test kernel. iptables `NFQUEUE` is. |

## 4. How it is built

- **Profiles** in `profiles/tcp/*.json` describe each stack. A persona names one
  in `tcp_profile_id`, and `NetContext::with_tcp_profile` refuses one whose OS
  differs from the User-Agent's.
- **Tier 1, sockets.** Set before `connect`. Gives the exact Linux SYN, no root.
- **Tier 2, shim** (`lightbrowser tcp-shim`, `CAP_NET_ADMIN` and `CAP_NET_RAW`).
  A socket's mark selects its profile, so personas of different operating
  systems share one process. Windows and macOS marks also set a bit that sends
  every packet of the connection to the shim.
- **What the shim does:** rewrites the SYN; sets the IPv4 ID of every packet;
  drops the kernel's SYN retransmissions (same sequence number) and sends its own
  on the profile's schedule through a raw socket, with the profile's option
  variants and an advancing timestamp; stops when the handshake ACK passes.
- **It never contradicts the kernel.** It removes, reorders or shrinks what the
  kernel offered and never adds. MSS and window scale are `min(kernel, profile)`:
  a smaller scale only makes the peer underestimate our window. Timestamps the
  kernel did not offer are reported, not invented.
- **Fails closed.** No `--queue-bypass`: a stopped shim drops marked packets. A
  context also refuses a shim profile when the iptables rule is missing.
- **Checksums.** A queued packet carries only a partial TCP checksum (hardware
  offload) and a packet returned by the shim is treated as complete, so every
  modified packet gets both checksums recomputed.

## 5. Results (loopback, Linux 6.18)

`cargo test -p lb-net --test tcp_fingerprint`, with an independent parser in
`lb-fptest` that shares no code with the rewriter:

- Each profile's SYN matches exactly: TTL, DF, window, MSS, scale, option bytes
  and order, ECN flags, JA4T. A 3 MiB response completes through each handshake.
- IP ID: macOS all 0; Windows one +1 counter across two connections; Linux a
  per-connection +1 counter.
- Retransmits with the server ignoring SYNs: macOS at 0, 1, 2, 3 s; Windows at
  0, 1, 3, 7 s, window scale gone from the third SYN, and a working connection
  after that downgraded handshake. No kernel retransmission reached the wire.
- With no profile the host's SYN shows; with the shim stopped, none leaves.
- `cargo test -p lb-net --test public_reference` (no root): every profile trait is
  seen in at least 5% of real sessions of its OS. Mutation-checked.

## 6. What public data changed

The Zardaxt OS database (1,453 fingerprints, 23,467 sessions, labelled by
User-Agent) corrected the first profiles, which came from memory and p0f:

- Windows window is 65535 in 66% of sessions and 64240 in 30%, independent of
  MSS. We ship both; 65535 is the default.
- macOS sets ECE+CWR on the SYN in 67% of sessions (we had it off).
- macOS window scale is 6 (87%); p0f's 4 is out of date. macOS IP ID is 0 (92%).

## 7. Findings worth remembering

- Linux 6.1+ retransmits SYNs at 1, 2, 3, 4, 6, 10 s (`tcp_syn_linear_timeouts`),
  not 1, 3, 7, 15. A Windows persona needs the shim for this.
- A packet capture on loopback must use the packet direction, not de-duplication,
  and must check kernel drops: an early capture lost packets and made a correct
  IP ID counter look broken.
- p0f's `id+` for macOS predates XNU's current zero ID for DF packets.
- Windows sources disagree on SYN retransmits (Catchpoint observed 1, 3, 7, 15 s;
  Thales documents a 3 s timeout and 2 retransmits).

## 8. Limits

- Linux only. Windows and macOS personas need root and send every packet through
  userspace (one netlink round trip each). Linux personas are unaffected.
- Direct connections only: behind a proxy the server sees the proxy's SYN. NAT,
  load balancers and MSS clamping on the path can still alter packets.
- `TCP_WINDOW_CLAMP` caps the receive window at `2^(15+scale)` (2 MiB for macOS).
- The dataset's labels come from the User-Agent, so classes include proxy and VPN
  traffic (19% of Windows sessions have TTL 64) and one operator's visitors. Its
  "Linux" class is mostly Android, so it cannot confirm a desktop kernel.
- Unverified: the retransmit schedules (macOS assumes a 1 s base timeout), how
  Windows counts IP IDs, and anything in section 2 marked "no".
- IPv6 rewriting is unit-tested, not exercised live.

## 9. Sources

- Real traffic: [Zardaxt database](https://github.com/NikolaiT/zardaxt)
  (`database/newCleaned.json`). All rights reserved: only aggregates are in the
  repo (`profiles/reference/tcp_public.json`).
- [FoxIO JA4+](https://github.com/FoxIO-LLC/ja4): Windows 11 JA4T example.
- [p0f.fp](https://raw.githubusercontent.com/p0f/p0f/master/p0f.fp): SYN layouts.
- XNU: [tcp_timer.c](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/netinet/tcp_timer.c),
  [ip_output.c](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/netinet/ip_output.c),
  [tcp_output.c](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/netinet/tcp_output.c).
- Windows: [Catchpoint](https://docs.catchpoint.com/docs/catchpoint-nodes-tcp-syn-retries),
  [Clavister](https://kb.clavister.com/324735770/the-tcp-window-scale-log-event),
  [Microsoft Set-NetTCPSetting](https://learn.microsoft.com/en-us/powershell/module/nettcpip/set-nettcpsetting).

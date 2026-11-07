# ADR 0032: Shape the whole connection, not only the SYN

Numbered 0008 before the merge with the M2-M7 branch (2026-11-07), which already used 0007-0009; older commits and notes may use the old number.

Status: Accepted (2026-11-03). Extends ADR 0031. Windows and macOS values are
from public sources, not yet from our devices (SYN-level traits are now checked
against public traffic data, ADR 0033).

## Context

ADR 0031 shaped only the SYN. A server can also read the stack from what
follows. Each item below was checked against sources and, for Linux, measured
on the dev kernel (6.18).

| Trait | Linux | macOS | Windows | Here |
|---|---|---|---|---|
| SYN retransmits | 6.1+: 1, 2, 3, 4, 6, 10 s (`tcp_syn_linear_timeouts`); older 1, 3, 7, 15 | XNU `tcp_syn_backoff {1,1,1,1,1,2,4,8…}`: 1, 2, 3, 4, 6, 10 s if the base RTO is 1 s | observed 1, 3, 7, 15 s; window scale dropped from the 2nd retransmit | shaped |
| IP ID | per-socket counter, +1 per packet (measured) | 0 on DF packets (XNU `rfc6864 = 1`) | host-wide counter | shaped |
| TSval | 1000.0 Hz, random offset per connection (measured) | `t_ts_offset + tcp_now`, 1 ms ticks, random offset by default | none | same as Linux, nothing to do |
| Window after the handshake | autotuned | autotuned | autotuned | **not shaped** |

Sources: [FoxIO ja4](https://github.com/FoxIO-LLC/ja4) (Windows 11 JA4T),
[p0f.fp](https://raw.githubusercontent.com/p0f/p0f/master/p0f.fp) (SYN layouts),
XNU [tcp_timer.c](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/netinet/tcp_timer.c),
[ip_output.c](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/netinet/ip_output.c),
[tcp_output.c](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/netinet/tcp_output.c),
[Catchpoint](https://docs.catchpoint.com/docs/catchpoint-nodes-tcp-syn-retries) (Windows 10
retransmits), [Clavister](https://kb.clavister.com/324735770/the-tcp-window-scale-log-event) (Windows
drops window scale on a later SYN), [Microsoft](https://learn.microsoft.com/en-us/powershell/module/nettcpip/set-nettcpsetting).
They disagree on Windows retransmit counts (Thales: 3 s RTO, 2 retransmits), and
p0f's `id+` for macOS predates XNU's current zero ID.

## Decision

- **Mark bit `MARK_ALL`** (0x8000) sends every packet of a connection to the
  shim. Profiles that need it (`ip.id` not `kernel`, or `syn_retransmit`) set
  it; the rest still queue only SYNs. Two iptables rules, one per case.
- **IP ID policy** `kernel | zero | host_counter`, applied to every packet. A
  queued packet carries only a partial TCP checksum (offload), so every
  modified packet gets both checksums recomputed.
- **SYN retransmission.** The shim remembers the first SYN of a marked
  connection, drops the kernel's retransmissions (same sequence number) and
  sends its own on `syn_retransmit.schedule_ms` through a raw socket. Copies use
  the profile's `option_variants` for that attempt and a TSval advanced by the
  elapsed time. The handshake ACK passing through ends the attempt.
- **Timestamps:** no change; the kernel's clock already matches.
- **Window sizes after the handshake:** not shaped. No source gives the values,
  and a guess is worse than the kernel's. `lb-fptest tcp-capture --flow` records
  them from a real device; the per-packet path makes the rewrite small.

## Measured

`cargo test -p lb-net --test tcp_fingerprint` (loopback, lossless capture
checked with `PACKET_STATISTICS`):

- macOS: SYNs at 0, 1000, 2000, 3000 ms, all packets ID 0.
- Windows: SYNs at 0, 1000, 3000, 7000 ms, window scale gone from the 3rd on,
  one ID counter, +1 per packet, across two connections.
- Linux: untouched, per-connection counter.
- The kernel's own retransmissions never reach the wire, and a 3 MiB response
  completes after a handshake that fell back to no window scaling.

## Alternatives considered

- **Userspace TCP stack.** Still the way to control everything, including
  autotuned windows. Not needed for these traits.
- **Delay the kernel's retransmits** instead of replacing them: it cannot send
  earlier than the kernel, whose 6.1+ schedule is not 1, 3, 7.

## Consequences

- Windows and macOS personas now send every packet through userspace: one
  netlink round trip each. Fine for this use; Linux personas are unaffected.
- The shim needs `CAP_NET_RAW` too (raw socket for retransmissions).
- A Windows ID counter only counts our connections; a real host's also counts
  its other traffic. IPv6 has no ID field; IPv6 rewriting is unit-tested, not
  exercised live here.
- The retransmit schedules and Windows' counter are unverified until captured
  on our devices: `lb-fptest tcp-capture --flow` prints the SYN times, ID
  pattern and timestamp rate to compare.

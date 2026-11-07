# TCP handshake fingerprint

How LightBrowser makes its TCP connections look like they come from Windows,
macOS or Linux, why, how well it works, and what is left.

| File | Read it for |
|---|---|
| [ANALYSIS.md](ANALYSIS.md) | The problem, what a server can read, the options we weighed, how it is built, the measured results, what public data changed, the limits |
| [REMAINING-STEPS.md](REMAINING-STEPS.md) | What is not done, in priority order, each with a "done when" |

Status (2026-11-03): the SYN, IP ID and SYN retransmission timing are shaped
and tested on loopback for all three operating systems. The Windows and macOS
values are checked against public traffic data but not yet against our own
devices. Post-handshake window sizes are not shaped.

Decisions are in the ADRs:
[0031](../adr/0031-tcp-stack-fingerprint.md) (sockets plus a netfilter shim),
[0032](../adr/0032-whole-connection-tcp-shaping.md) (IP ID, SYN retransmits),
[0033](../adr/0033-public-data-source-of-truth.md) (public data as the source of
truth). Milestone: M1b in [../ROADMAP.md](../ROADMAP.md).

Internal use only: loopback and our own test servers, never third-party sites
or detection services.

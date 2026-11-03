# TCP handshake fingerprint: remaining steps

In priority order. Each step says what "done" means. IDs are for reference in
issues and commits. Context: [ANALYSIS.md](ANALYSIS.md).

## P0: ground truth from our own devices

| ID | Step | Done when |
|---|---|---|
| T1 | Capture a real Windows and a real Mac of ours against our test host: `lb-fptest tcp-capture --flow --allow-non-loopback` (steps in `tests/README.md`). Do it twice per device, once with the host ignoring SYNs for about 20 s. | Output saved with device, OS build and network noted (outside the repo if it holds addresses) |
| T2 | Compare the captures with `profiles/tcp/` and fix the profiles: SYN traits, retransmit schedule and base timeout, option variants on retransmits, how Windows counts IP IDs, whether the Mac sent ECE+CWR. | `provenance.status` is `captured` and `unverified` is empty for each profile; `tcp_fingerprint` and `public_reference` pass |
| T3 | Add `macos_14_plain` (no ECE+CWR; about a third of Macs) if the captures or a test need it. | Profile added, `public_reference` passes |

## P1: close the known gaps

| ID | Step | Done when |
|---|---|---|
| T4 | Shape post-handshake window sizes: the window field in the handshake ACK and the autotuning curve, from T1 data. The per-packet path exists. | A flow capture of a persona matches a real device's window fields within a stated tolerance |
| T5 | Mark data packets ECT(0) for a macOS persona that sent ECE+CWR (IP ToS ECN bits). | Captured data packets carry ECT(0) only when the SYN had ECE+CWR |
| T6 | Run an independent oracle beside `lb-fptest`: p0f, Zardaxt or the Wireshark JA4 plugin on the loopback capture. | The oracle names each persona's OS correctly in a script or CI note |
| T7 | Exercise IPv6 live (rewrite, hop limit, raw retransmission). It is unit-tested only. | A loopback test over `::1` passes, or the sandbox gap is documented |
| T8 | Refresh `profiles/reference/tcp_public.json` when the source publishes a new build, and add a second public source for cross-checking. | Script output committed with the new date; profiles still pass |

## P2: make it usable

| ID | Step | Done when |
|---|---|---|
| T9 | Run the shim inside the browser process, so the rules and the shim share one lifecycle. Then default `--tcp-profile persona` once packaged. | `fetch` with a persona needs no separate command; a crash leaves no rule that blocks the next run |
| T10 | Wire `tcp_profile_id` through the CDP browser context (M3) and Playwright launch helper (M5). | A context created over CDP sends its persona's SYN |
| T11 | Document and test running in a container: `NET_ADMIN`, `NET_RAW`, and iptables or nft availability. | A documented `docker run` line passes the TCP test |
| T12 | Measure the shim's cost: throughput, CPU and latency of a Windows persona against a Linux persona, in `bench/`. | Numbers in `bench/results/` and a stated limit |
| T13 | Proxies (M2): decide what the TCP profile means behind an HTTP or SOCKS proxy. | Behaviour documented, and `with_tcp_profile` refuses or warns when a proxy is set |
| T14 | Path variants: MSS other than 1460 (VPN, PPPoE) as profile options, since the data shows MSS spread. | A persona can pick an MSS, and the SYN window and JA4T follow |
| T15 | Vendor the TCP consistency rule (UA OS matches the TCP profile) into the M4 consistency crate. | The rule is in `lb-persona` and covered by its tests |

## Decisions that need an owner

1. **Dataset licence.** The repo holds aggregate shares derived from an
   all-rights-reserved dataset. Confirm that is acceptable, or replace it with
   our own captures (T1, T2).
2. **Non-loopback capture.** `tcp-capture --allow-non-loopback` is an opt-in
   exception to "capture on loopback only". Keep it, or restrict it.
3. **`unsafe`.** The TCP code uses `unsafe` for syscalls and netlink, each with a
   comment. CLAUDE.md allows it only for engine bindings.
4. **Merge target.** The repository's default branch is a session branch, with no
   `main`. Pick the long-lived branch.

## When to build the userspace stack

Not yet. Build it if, after T1 to T5, our detection engine or a vendor check
still tells a persona from a real device by something only the TCP engine
decides: the autotuning curve, loss recovery, congestion behaviour, or RST
timing. Until then the shim keeps the kernel's engine and costs far less.

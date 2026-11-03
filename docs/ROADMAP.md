# Roadmap

Each milestone ends with something we can run and measure.

| Milestone | Scope | Done when |
|---|---|---|
| **M0 Foundations** | Architecture docs, ADRs, workspace skeleton, CI, SFC32 port | `cargo build/test/clippy` green for both families ✅ |
| **M1 Network** | `lb-net`: BoringSSL ClientHello profiles, h1 + forked h2, cookies, decompression, proxy. `lightbrowser fetch <url>` | JA3/JA4 and Akamai H2 hash match real Chrome on our local TLS/H2 echo server ✅ (Chrome). Proxies moved to M2. |
| **M1b TCP stack** | `lb-net::tcpfp`: SYN profiles for Windows, macOS and Linux, socket options, `tcp-shim` (NFQUEUE): SYN, IP ID, SYN retransmit timing | Our own parser sees the profile's exact SYN, IP ID and retransmit timeline on loopback for all three ✅. Windows and macOS profiles re-captured from real devices ⏳ |
| **M2 DOM + V8** | Arena DOM, html5ever sink, selectors, WebIDL codegen (Chromium set), V8 backend + snapshot, event loop, timers, fetch/XHR | `fetch --dump html` prints the DOM *after* scripts run. A WPT DOM subset passes. |
| **M3 Layout + CDP core** | Lazy cascade, taffy layout, hit-testing, AX tree, CDP server with the domains in ARCHITECTURE.md | Playwright `connectOverCDP`: goto, evaluate, click, fill, getByRole, content all pass |
| **M4 Chrome fingerprints** | Persona v2, vendored consistency crate + Chrome rules, navigator/UA-CH, screen, canvas, WebGL, audio, fonts, Intl/timezone, WebRTC, permissions | Our detection engine scores the Chrome personas as consistent. Same seed gives the same fingerprint. |
| **M4b Firefox family** | SpiderMonkey backend (`mozjs`), Gecko IDL set, Firefox TLS/H2/header profiles | Existing Firefox 128 personas pass the same checks as on GeckoPhantom |
| **M5 Behavior + SDK** | Vendored behavior crate, `LightBrowser.human*`, Python SDK | `page.human_click` produces realistic event streams in the detection engine |
| **M6 Performance** | `bench/` vs Lightpanda and headless Chrome, memory tuning, optional screenshots, service workers | Meets the budgets in ARCHITECTURE.md. Results published in `bench/results/`. |

## M1 status (2026-11-02)

Done:

- `lb-net`: TLS from profiles, h1 (hyper), h2 (fork in `vendor/h2`), cookies,
  gzip/deflate/br/zstd, redirects.
- `lightbrowser fetch <url> [--dump html|text] [--persona chrome|firefox]
  [--insecure]`.
- `lb-fptest`, a local capture server, and the `chrome_153_linux` profile
  captured from Chromium 153.

Verified by `cargo test -p lb-net --test fingerprint` (loopback only):

- **Chrome:** exact match with Chromium 153 on every connection: JA3 (over
  the canonical extension order), JA4, JA4_r, every ClientHello list, the
  Akamai H2 text, HEADERS priority, and the frame order SETTINGS,
  WINDOW_UPDATE, HEADERS. Extension order is permuted per connection.
- **Firefox 128:** ciphers, sigalgs, ALPN, cert compression, groups (minus
  ffdhe) and all H2 values match. JA3 and JA4 do not match yet. The
  documented gaps are in ADR 0003 and pinned by the test.

Not done, moved:

- HTTP and SOCKS5 proxies → M2.
- Firefox TLS gaps and a Firefox 128 re-capture → M4b.
- Exact HTTP/1.1 header-name case (hyper title-cases `sec-ch-ua`) → M2.
- Connection reuse and a TLS session cache → M2. Today every fetch is a new
  connection, so there is no `pre_shared_key`.

## M1b status (2026-11-03)

Done (ADR 0007):

- `profiles/tcp/{windows_65535,windows_64240,macos_14,linux_6}.json`, `Persona::tcp_profile_id`
  and a UA-vs-profile OS check in `NetContext::with_tcp_profile`.
- Tier 1 (sockets): exact Linux SYN with no privileges.
- Tier 2 (`lightbrowser tcp-shim`): exact Windows and macOS SYN, IPv4 and IPv6
  rewrite, fail-closed, rule preflight.
- `cargo test -p lb-net --test tcp_fingerprint`, `lb-fptest tcp-capture`.
- ADR 0008: the shim also sets the IP ID of every packet (macOS 0, Windows a
  host counter) and replaces the kernel's SYN retransmissions with the OS
  schedule (Windows 1, 3, 7, 15 s without window scale from the 2nd; macOS 1, 2,
  3, 4, 6, 10 s…). The timestamp clock already matches, so it is untouched.

Open:

- Re-capture the Windows and macOS profiles on our own devices. Their SYN traits
  are attested by public traffic data (ADR 0009: macOS scale 6 and ECN on, Windows
  window 65535 or 64240). Still unverified: the retransmit base RTO and counts, and
  how Windows' IP ID counts.
- Not shaped: post-handshake window sizes (no source for the values; record them
  with `lb-fptest tcp-capture --flow`). A userspace TCP stack is the option if the
  detection engine checks them or more.
- Make the shim part of the process (one lifecycle) and `--tcp-profile persona`
  the default once packaged.

## Risks

- **TCP profiles drift from real stacks.** Windows and macOS values are from
  public signatures until re-captured; a wrong value is worse than the Linux
  default because it is consistently wrong. The profile's `provenance.status`
  says which are verified.

- **WebIDL breadth.** Chrome exposes 1,000+ interfaces. We generate all of them
  from IDL, but each needs an implementation or a spec-shaped stub. Plan: stub
  first, implement in order of what the detection engine and real pages touch.
- **SpiderMonkey embedding.** `mozjs` builds are slow and pin a SpiderMonkey
  ESR. Keeping it in its own feature/binary stops it from slowing Chrome work.
- **Firefox ClientHello on BoringSSL.** Most of it is reproducible, but a few
  NSS-only details may need small BoringSSL patches. M4b checks this against
  the captured `firefox_128_*.json`.
- **Layout fidelity.** Element sizes from a simplified layout won't match
  Chrome pixel-for-pixel. Detection relies mostly on font metrics and a few
  probes, which we cover with persona metric tables.

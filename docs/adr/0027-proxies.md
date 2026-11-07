# ADR 0027: Proxies per browser context as Chrome uses them

Status: Accepted (2026-11-04).

## Context

M1 left proxies open. Playwright's `newContext({ proxy })` sends
`Target.createBrowserContext { proxyServer, proxyBypassList }` and answers
a proxy's `407` through `Fetch.authRequired` when the context has
`httpCredentials`; Chrome takes a browser-wide `--proxy-server`. What
reaches a proxy is visible to it (and to anyone behind it), and the
ClientHello and HTTP/2 settings through a tunnel must stay the persona's.
Chrome for Testing 153.0.8010.12 was recorded on loopback through our own
logging proxies (`lb-net` `tests/proxy/proxy.py`, `chrome.jsonl`).

## Decision

- `lb_net::Proxy` (`proxy.rs`), one per browser context, shared by its
  network contexts and connection pool (it holds the credentials cache):
  - **HTTP proxy**: `https:`, `wss:` and `ws:` go through a `CONNECT`
    tunnel (`Host`, `Proxy-Connection: keep-alive`, the persona
    `User-Agent`, then `Proxy-Authorization` once known, as recorded);
    TLS runs inside it with the unchanged connector, so JA3/JA4 and the
    Akamai H2 fingerprint equal a direct connection's (tested).
    `http:` requests are forwarded in absolute form with `Connection`
    renamed `Proxy-Connection` and `Proxy-Authorization` after it and
    after `Pragma`/`Cache-Control` (recorded order).
  - **SOCKS5**: every scheme is tunnelled; the host name goes to the
    proxy (address type 3, remote DNS), greeting `05 01 00` as Chrome. With
    credentials (our extension; Chrome has no SOCKS auth) `05 02 00 02` and
    RFC 1929.
  - **Auth**: Chrome's round: the first request goes without credentials,
    a `407` with `Proxy-Authenticate: Basic` is answered (configured
    `user:pass@` credentials, or CDP `Fetch.authRequired` /
    `Fetch.continueWithAuth` when the client enabled `handleAuthRequests`)
    on the same connection when the `407` left it reusable, and the
    credentials are then sent up front. Unanswered: Chrome's headless error
    `net::ERR_INVALID_AUTH_CREDENTIALS`. Only `Basic`.
  - **Bypass**: Chrome's `ProxyBypassRules`: host patterns with `*`
    (leading `.` = `*.`), optional scheme and port, CIDR ranges,
    `<local>`, `*`; loopback and link-local destinations are never
    proxied unless the list has `<-loopback>` (Chrome's implicit rules).
- Where it is set: `Target.createBrowserContext` `proxyServer` /
  `proxyBypassList` per context; `--proxy` / `--proxy-bypass-list` for
  `serve` (every context without its own) and `fetch`.
- Requests to destinations that are not potentially trustworthy (`http:`
  to a non-loopback host, the usual case behind a forward proxy) now get
  Chrome's header set for them, recorded for every request kind: no
  `sec-ch-*`, no `Sec-Fetch-*`, `Accept-Encoding: gzip, deflate`,
  `Accept-Language` last. This was a gap for direct requests too.

## Consequences

- One code path: the pool dials the proxy (or a tunnel) in place of the
  destination; groups stay per destination. Nothing TCP/IP-level changes.
- Not done: HTTPS proxies (TLS to the proxy), SOCKS4, PAC scripts and
  per-scheme proxy lists beyond the `https=` entry, `Digest`/NTLM auth,
  HTTP/2 to the proxy. Server `401` auth through `Fetch.authRequired` is
  not wired (proxies only).

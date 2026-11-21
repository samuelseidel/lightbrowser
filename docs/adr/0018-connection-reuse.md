# ADR 0018: Connection reuse as Chrome does it

Status: Accepted (2026-11-04). Replaces the per-origin semaphore of M6
(`lb_page::BrowserContext`) and closes the M1 leftover "connection reuse
and a TLS session cache".

## Context

Every request opened its own TCP (and TLS) connection and closed it after
the response, at most 6 at a time per origin. Chrome keeps HTTP/1.1
connections alive, multiplexes HTTP/2, resumes TLS sessions, and a server
sees all of that: connection count, requests per connection, resumed
ClientHellos (`pre_shared_key`, JA4 `...1518...`), stream ids and HTTP/2
priority blocks after the first stream. Measured with `lb-fptest site`
(page server that records connection use) and local Chrome for Testing
153.0.8010.12 on loopback, after start-up traffic settled (fresh Chrome
flushes its socket pools once: "Cert verifier changed"):

- HTTP/1.1, page with 24 subresources: 6 connections, LIFO reuse, the
  page's connection reused; a socket idle 290 s is reused, 315 s is not.
- TLS: the 2 tickets of the first connection are used once each by the
  next two connections (`pre_shared_key` last, no early data).
- HTTP/2: one connection; each HEADERS block depends exclusively on the
  newest still-open stream of the same or a more urgent priority, weight
  256/220/183/147 for urgency 0-3 (the `priority` header's `u=`).
- 12 requests at once to an HTTPS server not seen before: 6 connections
  complete TLS with h2; one carries all 12 requests, the other 5 close
  without a frame.
- Socket pool groups include the top-level site and a cross-site bit
  (netlog: `http://127.0.0.1:18507 <http://127.0.0.1 same_site>`): a
  cross-site frame's request and the same URL as a page use two sockets.

## Decision

- **`lb_net::ConnectionPool`, one per browser context**, never shared:
  groups keyed by scheme, host, port and partition (top-level site,
  `same_site`/`cross_site`). Per group at most 6 sockets (busy, idle,
  connecting), 256 per pool. Idle HTTP/1.1 sockets: most recently used
  first, dropped after 300 s or when the peer closed or sent data
  (`IsConnectedAndIdle`), swept every 10 s. Waiting requests get freed
  sockets by urgency, then age. HTTP/1.1 reuse follows
  `HttpStreamParser::CanReuseConnection` (keep-alive response, delimited
  body fully read incl. chunk trailers, nothing extra buffered).
- **HTTP/2:** one session per group. Unknown server: waiting requests
  each start a connection (up to 6); the first h2 one becomes the
  session, the others are dropped after the handshake. Known h2 server
  without a session: one connection, the others wait up to 300 ms
  (Chromium's HTTP/2 throttle; from its source, not measured here).
  Sessions live until the server ends them. The h2
  fork gained `chrome_stream_dependencies` and a `client::Urgency` request
  extension; the first stream of a navigation still gets the captured
  block, so the Akamai text and the fingerprint test are unchanged.
- **Retry:** a request that fails on a reused connection before any
  response byte, or is refused by `GOAWAY`/a closed session, is sent once
  more on a new connection (`ShouldResendRequest`).
- **TLS resumption:** `TlsConnector::with_sessions` caches sessions per
  host, port and partition, two per key, TLS 1.3 single use, memory only.
  A `NetContext` without a pool (the fingerprint test) never resumes.
- **Threads:** pooled requests run on one lazily started network thread
  (`pool::net_runtime`), like Chrome's network service, so pages on
  different workers share connections. `lightbrowser fetch` (one page)
  keeps them on its page thread (`use_calling_thread`).

## Alternatives considered

- **Per-worker pools.** No extra thread, but a context's pages on two
  workers would open twice the connections Chrome does.
- **hyper's pool.** Neither Chrome's grouping, limits, h2 race nor its
  priority blocks, and we already write our own h1 (ADR 0011).
- **0-RTT.** Chrome 153 sends no `early_data` over TCP.

## Consequences

- Connection counts, reuse, resumed ClientHellos and HTTP/2 priority
  blocks match Chrome on the pages above (`lb-net` `tests/pool.rs`,
  `lb-page` `tests/connections.rs`).
- Not modelled: HTTP/2 coalescing across host names (IP pooling), privacy
  mode groups (credentials-less requests share the credentialed group;
  the credentials mode does not reach `lb-net`), preconnects, PRIORITY
  frames for re-prioritised images. The 256-socket and 300 ms limits come
  from Chromium's source; the others were measured.
- Servers with Nagle on and split writes add a delayed-ACK wait per
  reused request (Chrome and curl too); the bench corpus server now sets
  `TCP_NODELAY`.

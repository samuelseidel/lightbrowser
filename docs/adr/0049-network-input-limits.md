# ADR 0049: Network input limits: decoder caps, WebSocket message cap, timeouts

Status: Accepted (2026-11-16).

## Context

An audit (GAP-090, GAP-091, GAP-095) found that a server could crash, exhaust
or hang the browser: a huge HTTP/1.1 chunk size wrapped the body-cap check and
panicked; content decoding inflated a whole body before the 32 MiB cap was
checked (a few KiB could become gigabytes, more with stacked codings); the
WebSocket reader allocated up to 256 MiB from a frame's declared length and
had no cap on fragmented messages; and only the TCP connect had a time limit,
so a server that accepted and never answered held `fetch` for ever.

## Decision

- **Chunk sizes** (`lb-net` `h1.rs`): hex digits only, at most `i64::MAX`
  (Chrome's `HttpChunkedDecoder` limit); anything else is a bad chunked body.
  The cap check uses checked arithmetic; a size past `max_body` is
  `BodyTooLarge`.
- **Content decoding** (`decompress.rs`): gzip, deflate, br and zstd inflate
  in 64 KiB steps into a buffer that never grows past `max_body`; each layer
  of a stacked `Content-Encoding` stops at the cap with `BodyTooLarge`.
  Certificate decompression (`tls.rs`) is bounded too: BoringSSL limits the
  declared length and boring's writer has exactly that size; our helper also
  stops at the 24-bit maximum.
- **WebSocket** (`websocket.rs`): payloads are read as they arrive. A message
  (one frame, its fragments, or its inflated `permessage-deflate` form) over
  `NetContext::ws_max_message` (default `websocket::MAX_MESSAGE`, 64 MiB)
  fails the connection as Chrome fails a frame over its limit: close 1009 to
  the server, "Max frame length exceeded" and a 1006 close to the page.
- **Timeouts** (`NetContext::timeouts`, `NetTimeouts`), all overridable per
  context (tests lower them), no environment variables:

  | Limit | Default | Source |
  |---|---|---|
  | TLS handshake | 30 s | Chrome's SSL connect job |
  | Proxy tunnel (dial, `CONNECT`/SOCKS5, `407` rounds) | 30 s | Chrome's proxy connect jobs |
  | Response head, from the request written | 300 s | ours |
  | WebSocket opening handshake, connect included | 240 s | Chrome's |

  Bodies and open connections (streaming responses, WebSockets) have no
  limit, as in Chrome. Every timeout reads as `net::ERR_TIMED_OUT` to pages.
- **`fetch --timeout SECS`** bounds the whole load: navigation up to commit
  and the wait for `--wait-until` share one deadline. Past it before commit
  the CLI prints "timed out after N s waiting for a response from URL" and
  exits 1.

## Consequences

Deliberate differences from Chrome, all on hostile or broken input only:
- A WebSocket message over 64 MiB fails; Chrome's frame limit is 2^63 - 1.
- A server that sends no response head within 300 s fails; Chrome waits.
- Chunk sizes with a sign (`+5`), empty, or over 16 hex digits are errors;
  `+5` was accepted before.

Normal pages see no change. Regression tests: `lb-net` `tests/bounds.rs` (a
counting allocator: 1 GiB gzip, zstd and stacked bombs, WebSocket frames),
`tests/timeouts.rs` (silent HTTP, TLS, proxy and WebSocket servers), unit
tests in `h1.rs`, `h2.rs`, `decompress.rs`, `tls.rs`, `lb-page`
`cdp/net.rs`, and `lightbrowser` `tests/fetch_timeout.rs`.

## Open items

- The 1009 close was not recorded from Chrome (its limit cannot be reached
  on loopback); the text and code follow Chromium's `ERR_MSG_TOO_BIG` path.
- The proxy timeout text (`net::ERR_TIMED_OUT`) was not recorded from Chrome.
- `fetch --timeout` does not cover reading the DOM after the wait
  (`content()`, `text()`, `extract()`): a page script that never yields can
  still hold the CLI. That is script deadlines, not network input.

# ADR 0025: Web Crypto, WebSocket, dynamic `import()`, `blob:` fetches and synchronous XHR as in Chrome

Status: Accepted (2026-11-04).

## Context

Pages had no `crypto` (most sites call `crypto.getRandomValues` or
`randomUUID`), `new WebSocket()` threw `Illegal constructor`, `import()`
rejected, `fetch(blobURL)` failed and `open(..., false)` ran asynchronously.
Each is a visible difference from Chrome 153. Behaviour was recorded in
local Chrome for Testing 153.0.8010.12 on loopback pages (`lb-page`
`tests/apis/`, 15 cases, same `serve.py` for both browsers).

## Decision

- **Web Crypto** (`lb-webapi` `rt/crypto/`): `getRandomValues` and
  `randomUUID` from BoringSSL's `RAND_bytes` (the OS CSPRNG, never the
  persona seeds). `SubtleCrypto` on the BoringSSL lb-net already links,
  structured as Chromium: Blink's argument checks and algorithm
  normalization (`normalize.rs`: dictionaries, member order, `has` before
  `get`, error texts with the method prefix) and `components/webcrypto`'s
  operations and status texts (`ops.rs`, `jwk.rs`). SHA-1/256/384/512,
  HMAC, AES-GCM/CBC/CTR/KW, ECDSA/ECDH (P-256/384/521), RSASSA-PKCS1-v1_5,
  RSA-PSS, RSA-OAEP, PBKDF2, HKDF, Ed25519, X25519; raw, spki, pkcs8 and
  jwk; `wrapKey`/`unwrapKey`. Promises settle when the method returns
  except PBKDF2 derivations (a later task), as recorded. Keys are
  immutable `Arc`s: structured clone shares them with workers. Raw FFI
  (with `SAFETY` comments) only where `boring` has no wrapper: RSA-OAEP
  digests and labels, raw Ed25519/X25519 keys. `SubtleCrypto`,
  `CryptoKey` and `randomUUID` are `[SecureContext]` and absent from
  insecure realms (generated tables). `QuotaExceededError` is
  constructible.
- **WebSocket** (`lb-net` `websocket.rs`, `lb-webapi` `rt/websocket.rs`):
  an own RFC 6455 client on the network thread, one connection per socket
  (not pooled): Chrome's handshake headers in Chrome's order (captured),
  persona `User-Agent` and `Accept-Language`, the context's cookies sent
  and stored from the `101`, `permessage-deflate; client_max_window_bits`
  offered and every message compressed when accepted (uncompressed if the
  server limits our window), masked frames, pongs, both closing
  handshakes. `wss:` uses the persona's ClientHello with ALPN `http/1.1`
  alone and, as captured from Chrome, without ALPS or `server_padding`
  (JA4 equal). The API: constructor checks and texts, `http(s):` mapped to
  `ws(s):`, relative URLs, `bufferedAmount` (down once bytes are written),
  `binaryType`, `close()` checks, events in Chrome's order (`error` then
  `close` 1006 on failure, `readyState` 3 at `error`), `CloseEvent`.
- **Dynamic `import()`** (`lb-js` hook, `lb-webapi` `rt/modules.rs`): HTML
  specifier resolution (bare specifiers reject, also for static imports),
  module graphs fetched as scripts with CORS, strict JavaScript MIME check
  and `blob:`/`data:` locally, added to the window's module map as a
  whole; a graph already in the map settles in a microtask, a fetched one
  in a networking task. One V8 module record per URL and realm, shared
  with static imports; errors cached by V8. `import.meta.resolve`.
- **`blob:`/`data:` in `fetch()` and XHR**: answered without the network
  as Chrome's loaders do (GET only, single byte ranges with `206`, basic
  responses, `Content-Type`/`Content-Length`).
- **Synchronous XHR**: the request blocks the calling thread (another
  thread's runtime, as `importScripts`), then `readystatechange` (4),
  `load`, `loadend`; no `loadstart`/progress/upload events; network errors
  throw `NetworkError` without events; window-only `responseType`/`timeout`
  restrictions with Chrome's texts.
- `Illegal constructor` now reads `Failed to construct 'X': Illegal
  constructor` for every interface, as in Chrome.

## Consequences

- Page-visible surface is unchanged except: these APIs work,
  `WebSocket`/`CloseEvent`/`QuotaExceededError` are constructible, and the
  constructor message above (`tools/surface_diff`).
- Known gaps: a receiver check on a promise-returning method throws instead
  of rejecting (all interfaces); WebSocket over HTTP/2, proxies and
  `Sec-WebSocket-Protocol` duplicates across reconnects are not modelled;
  import maps and JSON/CSS modules are not supported; module scripts
  inserted by script still do not run; the cookie jar's order within one
  path is not Chrome's creation order (pre-existing, `cookie_store`).

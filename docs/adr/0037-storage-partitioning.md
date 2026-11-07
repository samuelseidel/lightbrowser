# ADR 0037: Storage partitioning by Chrome's storage key, CHIPS and SameSite

Status: Accepted (2026-11-07).

## Context

Chrome partitions third-party storage (`ThirdPartyStoragePartitioning`,
on by default): a frame's storage is looked up by its storage key, not by
its origin. rebrowser's "storagePartitioning" check shows it: the top page
(site A) writes `localStorage`, embeds a site B frame, which embeds a site
A frame; in Chrome that innermost frame sees no value, in LightBrowser it
saw the top page's, which looks like Playwright's
`--disable-features=ThirdPartyStoragePartitioning`. rebrowser runs it on
`127.0.0.1` against `localhost`. Cookies had no SameSite rules at all
(a cross-site frame read and set `Lax` cookies) and no `Partitioned`
(CHIPS) cookies.

Behaviour was recorded in Chromium 153.0.8010.12 (the full browser,
headless, Linux; started directly, since Playwright's launcher turns
partitioning off) on loopback pages, two sites on one port
(A = `127.0.0.1`, B = `localhost`), in frame chains A, AA, AB, ABA, AAB,
ABB and B, BA, BAB, across pages of one context (`lb-page`
`tests/partition/`, 9 cases). The 153 headless shell (Linux and macOS)
logs the same except the Storage Access API answers, and Chrome 156 on
macOS logs the same as Chromium 153.

## Decision

- **Storage key** (`lb-webapi` `rt/partition.rs`, `StorageKey`): origin,
  top-level site (scheme and registrable domain by the same two-label rule
  as `Sec-Fetch-Site` and the connection pool, or the IP literal) and the
  ancestor chain bit (the context or an ancestor frame is cross-site with
  the top-level site). Serialized as Chrome's `StorageKey::Serialize`
  (`origin/`, `origin/^0site`, `origin/^31`). A top-level document is
  first-party; a frame's key comes from its parent's key and its own
  (creator's, for `about:blank`/`srcdoc`) URL, set when the frame is
  linked (the page layer sets it first for network frames, to pick their
  storage areas). No nonces (credentialless and fenced frames).
- **Keyed by it**: `localStorage` and `sessionStorage` areas, IndexedDB
  databases (the backend's "origin" argument is the serialized key),
  Cache API storages, service worker registrations, ready waiters,
  clients and controllers (each window's `SwHost` is bound to its key; a
  frame navigation matches registrations of the frame's future key),
  shared workers (URL, name, creator's key), `BroadcastChannel` (context,
  key, name) and `blob:` URLs (fetch, XHR, workers, `importScripts`,
  modules answer only the creator's partition: `Failed to fetch`, XHR
  `error`, a worker `error` event; a cross-origin blob worker throws
  `SecurityError` as before).
- **Workers** get the key of the context that made them
  (`WorkerStart::storage_key`): dedicated and shared workers their
  creator's, a service worker its registration's. Their IndexedDB,
  caches, channels and requests use it.
- **Storage events**: a window that made its `Storage` object, or added a
  `storage` listener or `onstorage` handler (Blink's
  `DOMWindowStorageController`), watches the area; a change that alters
  the area sends a `StorageEvent` (key, old and new value, the writer's
  URL, the receiver's `storageArea`) to every other watching window,
  through its inbox (pages on other threads too). Same key only, since
  areas are per key.
- **Cookies** (`lb-net` `cookies.rs`): every access has a `CookieScope`
  (requester's top-level site and ancestor bit; requests carry their
  window's key in `NetRequest::storage_key`, for a frame navigation the
  parent's, and `RequestMeta::cookie_scope`; WebSocket handshakes use
  their document's scope too). A same-site access needs a non-null site
  for cookies and a URL same-site with it; otherwise only
  `SameSite=None` cookies are sent and stored (unspecified counts as
  `Lax`). A top-level navigation started by another site leaves out
  `Strict` cookies. `SameSite=None` and `Partitioned` need `Secure`;
  `Secure` needs https, `localhost` or loopback (Chrome sets and sends
  them on `http://127.0.0.1` and `http://localhost`). A `Partitioned`
  cookie is stored in, and sent only to, the partition of the access that
  set it: top-level site and `!site_for_cookies.IsFirstParty(url)`
  (Chrome's `CookiePartitionKey::FromNetworkIsolationKey`), so ABA's
  partition differs from A's. Unpartitioned third-party cookies stay
  shared (Chrome allows third-party cookies by default).
- **Storage Access API**: `hasStorageAccess()` and
  `hasUnpartitionedCookieAccess()` resolve `true`, `requestStorageAccess()`
  resolves at once in every frame, and `permissions.query` for
  `storage-access` reads `granted` at the top level and `prompt` in frames,
  as Chromium 153 and Chrome 156 answer with third-party cookies allowed.
  The 153 headless shell has no permission layer (`prompt` everywhere,
  `requestStorageAccess()` rejected in frames); personas present full
  Chrome, so the tests follow Chromium.
- Fixed on the way: methods read from a cross-origin window stand-in
  (`postMessage`, `close`, `focus`, `blur`) are bound to the target window
  (they threw `Illegal invocation`), and each read returns the same
  function.

## Consequences

- rebrowser's check and the recorded cases match Chrome; storage written
  in a third-party context is not visible first-party and the reverse.
- Partitioned cookies are not listed by CDP `Network.getCookies` /
  `Storage.getCookies` (Chrome lists them with `partitionKey`), and CDP
  cannot set them.
- `Clients.matchAll` still lists a window only once it uses
  `navigator.serviceWorker`, is controlled, or its partition had a
  registration when it was made (ADR 0022); Chrome lists every window of
  the partition (the recorded case lists the frames that use the API).
- Not modelled: `navigator.storage` (`StorageManager`: `estimate()` is
  per partition in Chrome, quota 4 GiB in an off-the-record context and
  10 GiB in the default one on the recording machine), Web Locks
  (`navigator.locks` is missing), Storage Buckets, `cookieStore`, the
  "Lax allowing unsafe" exception for young default-`Lax` cookies on
  cross-site top-level POSTs (they are sent as `Lax`), the public-suffix
  list (same two-label site rule as elsewhere), `StorageEvent` init
  dictionaries, third-party cookie blocking settings.

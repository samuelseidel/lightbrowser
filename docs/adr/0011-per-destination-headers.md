# ADR 0011: Request headers per destination, captured from local Chromium

Status: Accepted (2026-11-03)

## Context

The first network stack (ADR 0003) sent one header set for every request: the
top-level navigation set. The detection bench (ADR 0007) showed it:
`header_order` fired because hyper writes HTTP/1.1 names in Title-Case
(`Sec-Ch-Ua`) where Chrome writes `sec-ch-ua`, and subresources carried
`Upgrade-Insecure-Requests` and `Sec-Fetch-User`.

Capturing Chrome for Testing 153.0.8010.12 on loopback (a local logging
server, HTTP/1.1 raw bytes and HTTPS h2 decoded, `--headless=new`) showed more
than names and case:

- Each destination has its own list and order: navigation, iframe, script
  (classic and module), style, image, favicon, font, `fetch`/XHR, and the CORS
  preflight. `Accept`, `Sec-Fetch-Mode/Dest`, `Priority` (`u=0, i`, `u=0`,
  `u=1`, `u=2, i`, `u=1, i`) and `Referer` differ.
- Chrome 153 puts `accept-language` early (after `sec-ch-ua-platform`), not
  next to `accept-encoding` as Chrome 131 did, and `cookie` after `priority`.
- Subresource requests send `sec-ch-ua-platform, accept-language, sec-ch-ua,
  user-agent, sec-ch-ua-mobile` in that order, with the page's own headers
  mixed in (`Content-Type`, custom headers, `Accept` when the page sets it).
  That is a hash-table order inside Blink, not a fixed list. We reproduced it
  only by recording it (we could not match Blink's hash).
- HTTP/1.1 is the h2 list with `Host` and `Connection: keep-alive` in front and
  no `Priority`. Names are Title-Case except `sec-ch-*`, which are lowercase.
  Names a page sets keep the page's case.
- After a redirect on a navigation, Chrome puts `sec-ch-ua*` after
  `sec-fetch-dest`.
- Chrome asks for `/favicon.ico` after a top-level load when the page has no
  `<link rel=icon>`, with the image header set, `Referer` = the page URL and
  the context's cookies.

## Decision

- **Data, not code.** `profiles/headers/chrome_153.json` lists, per request
  kind, header slots in h2 order. A slot is a name whose value is computed
  (`user-agent`, `sec-ch-ua*`, `accept-language`, `referer`, `origin`, `cookie`,
  `content-length`, `sec-fetch-site/mode/user`), a fixed `name: value`, or the
  renderer group. `renderer_group` holds the observed orders per set of page
  headers. The raw capture (69 requests) is kept in
  `profiles/headers/captures/chrome_153_linux.jsonl` as evidence and as a test
  fixture.
- **Values from the persona.** The file never holds a UA, client hint or
  language. Fixed values are only `Accept`, `Accept-Encoding`, `Priority` and
  `Upgrade-Insecure-Requests`.
- **`lb_net::header_profile`** loads the file (built in with `include_str!`
  for Chrome) and builds the header list per hop: `RequestKind`, initiator,
  method, body, cookie and the page's headers go in; the list in wire order,
  with wire case for h1, comes out. Page headers that name a slot replace its
  value; others join the renderer group. `Sec-Fetch-Site` is the largest
  distance along a redirect chain; `Referer` follows
  `strict-origin-when-cross-origin`; `Origin` is sent as Chrome does (CORS
  requests, non-GET, module scripts and fonts).
- **Own HTTP/1.1 writer** (`lb-net::h1`). hyper cannot write per-name case,
  so the request head is written byte for byte and the response is read with
  `httparse`. hyper is no longer a dependency of `lb-net`.
- **Firefox** keeps the navigation set plus a small per-kind overlay
  (`headers::generic_request_headers`) until a Firefox capture exists (M4b).
- **Favicon hook.** `BrowserContext::request_favicon` fetches
  `/favicon.ico` (or the `<link rel=icon>` href) once per URL per context.
  Pages call it after `load`.

## Alternatives considered

- **Patch hyper** to keep a case map. Works, but ties us to a fork of a large
  crate for one feature; the writer is about 100 lines.
- **Hard-code the Chrome 153 lists in Rust.** Faster to write, but the next
  Chrome version would need a code change; a data file can be re-captured and
  diffed.
- **Emulate Blink's hash-table order.** We tried the WTF string hash and
  double hashing against 12 observed orders and matched none, so we use the
  recorded orders.

## Consequences

- Header names, case and order for the bench's `header_order` rule match
  Chrome 153 for the kinds listed. Re-capture for each Chrome major
  (`tests/README.md`).
- Open: page header sets we have not captured are placed right after
  `user-agent` and may differ from Chrome. Add them to `renderer_group`.
- The detection bench's `EXPECTED["chrome"]["doc_header_order"]` had Chrome
  131's slot for `Accept-Language`; it was corrected to Chrome 153's (see
  `ROADMAP.md`, "M4 status: detect-bench target and determinism").
- Open: form-post navigations (`Origin`, `Content-Type` positions), `Range`
  requests, `Cache-Control` revalidation headers and h3 are not captured.
- Open: `Sec-Fetch-User` for link clicks needs the page layer to say the
  navigation was user-activated.
- The profile is Linux-captured; ordering is OS independent, but the values
  (UA, platform) come from each persona.

## Update (M7, client hints)

A second capture (`tools/header_capture/accept_ch.js`, 88 requests in
`profiles/headers/captures/chrome_153_linux_accept_ch.jsonl`, headed and
`--headless=new` identical) recorded Chrome 153 after `Accept-CH` and
`Critical-CH`, over HTTP/1.1 and h2. It added to the profile and corrected
two points:

- **High-entropy hint slots.** Navigations and frames send
  `sec-ch-ua, sec-ch-ua-mobile, sec-ch-ua-full-version, sec-ch-ua-arch,
  sec-ch-ua-platform, sec-ch-ua-platform-version, sec-ch-ua-model,
  sec-ch-ua-bitness, sec-ch-ua-wow64, sec-ch-ua-full-version-list,
  sec-ch-ua-form-factors`, skipping hints the origin did not ask for.
  The `Critical-CH` retry has its own list (`document_restart`): the hints
  after `Accept`, before `Sec-Fetch-*`.
- **Subresources** put the hints in the renderer group, in yet another
  hash order. `renderer_group.observed_hints` keeps the orders seen per
  hint set (`tools/header_capture/derive_groups.py`); an unseen hint set
  uses the order without hints with the hints after `user-agent`.
- **Correction: `cookie` comes before `priority` on h2.** The first
  capture decoded h2 with Python's `h2` package, which moves `cookie` to
  the end when it joins cookie crumbs. Node's decoder keeps the wire
  order; both certificate modes give `cookie, priority`.
- **Correction: redirect `Referer`.** On a redirect hop `Referer` comes
  after the `sec-ch-ua*` block; the first capture had no redirect with a
  referrer.
- `lb-net/tests/accept_ch.rs` replays the capture through
  `HeaderProfile::build` (names, case, order and values) as the golden
  test. Not covered: the `priority: i` of CSS background images (not
  fetched yet) and `Sec-Fetch-Storage-Access` on cross-site frames of an
  https page, which LightBrowser does not send.

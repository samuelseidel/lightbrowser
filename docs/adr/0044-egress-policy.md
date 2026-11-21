# ADR 0044: Egress policy: the internal-use rule, enforced at the transport

Status: Accepted (2026-11-07). Revision 1 (2026-11-07): what an allowed name
may resolve to, pinned names, names through a proxy, `/0` blocks, flag
conflicts (see "Revision 1").

## Context

CLAUDE.md says LightBrowser is only pointed at loopback pages and our own
test servers. That was a convention. A script can follow a redirect, a
page can load a subresource or open a WebSocket, a worker can `fetch`, a
proxy can be configured: any of them can reach a host nobody meant to
contact. We want the rule to hold in code, as a switch a tool turns on,
without changing anything for users who do not (so the default stays open).

## Decision

### The policy

`lb_net::EgressPolicy` (`crates/lb-net/src/egress.rs`), an `Arc` owned by the
context's network stack (`NetContext.egress`, `ConnectParams.egress`),
public so other tools can use the same matcher.

Modes (`EgressMode`):

- `open`: no checks. The default of `fetch` and `serve`; the check is a
  single branch.
- `loopback`: loopback addresses (`127.0.0.0/8`, `::1`, also as
  IPv4-mapped IPv6), the names `localhost` and `*.localhost`, and the
  allow entries.
- `allowlist`: only the allow entries (no implicit loopback). Reachable
  through the library and `--egress allowlist`.

Allow entries (`--allow-host`, repeatable): a name glob (`*.lab.test`,
`build-*.internal`; `*` matches any run of characters, dots included), an
IP (`10.0.0.5`, `::1`, `[fd00::1]`), a CIDR block (`10.0.0.0/8`,
`fd00::/8`; host bits are cleared, `::ffff:a.b.c.d/N` with N >= 96 is the
IPv4 block) or a name glob pinned to addresses (`*.lab.test@10.0.0.0/8,
192.168.1.5`: the name may resolve only to those). Refused as mistakes: a
bare `*` or only wildcards, a block that holds every address (`0.0.0.0/0`,
`::/0`, `::ffff:0:0/96`) -- both are `--egress open` in disguise, use that
--, ports (`host:80`), a wildcard inside an internationalised label, a pin
on an address, an `--allow-host` with `--egress open`. Flags only: no
environment variable, no banner.

API:

- `EgressPolicy::parse(mode: &str, allow: &[impl AsRef<str>])` and
  `EgressPolicy::new(EgressMode, ..)`, `open()`, `loopback()`, `add(entry)`.
- `check_name(host) -> Result<(), Blocked>`: run before any DNS query. It
  takes a name, an IP literal (then it is the address check), `[v6]`, or
  either with `:port`. Normalisation as for URLs: ASCII case, one trailing
  dot, IDN to punycode, numeric IPv4 forms (`0x7f.1`, `2130706433`),
  IPv6 brackets. An invalid host is refused.
- `check_addr(ip)`: loopback (in `loopback` mode) or an allowed address; an
  IPv4-mapped IPv6 address counts as its IPv4 address.
- `check_resolved(host, ip)`: for an address a name resolved to. What a
  name may resolve to depends on how it was allowed (revision 1):
  - `localhost`, `*.localhost` and any name no entry lists: an address
    `check_addr` allows, so a hosts file or a DNS answer cannot turn
    `app.localhost` into an outside destination;
  - a name an unpinned entry lists: an internal address (loopback,
    RFC 1918, IPv6 unique-local) or one `check_addr` allows. Public,
    link-local (cloud metadata is `169.254.169.254`) and CGNAT addresses are
    not reachable by a name alone;
  - a name a pinned entry lists: its pins, nothing else (loopback
    included). Two entries that list one name give the union.
- `check_remote_name(host)`: the name check for a request that goes through a
  proxy, which resolves the name itself so its address cannot be checked
  here. Only a name an unpinned entry lists is allowed; `localhost` names
  and pinned names are refused (`BlockedKind::ProxiedName`); an IP literal is
  the address check.
- `Blocked { target, kind, mode }` (`Name`, `Address`, `Invalid`,
  `ProxiedName`), carried by `NetError::Blocked`.

Not loopback, on purpose: `0.0.0.0`, `::`, link-local, this machine's own
non-loopback addresses, `::7f00:1` and NAT64 forms. Allow them explicitly.

### Where it is enforced

| Point | Check |
|---|---|
| `fetch_once_inner` (every hop, redirects included) | `check_name(host)` (`check_remote_name` if a proxy applies) first: before header work, pool and DNS |
| `ConnectionPool::acquire` | `check_name(group host)` (`check_remote_name` when the group goes through a proxy) before the group entry is made: a refused origin never gets a group, a waiter, a connect or a TLS session (`ConnectionPool::groups()`, `cached_sessions` in tests). HTTP/2 coalescing across hosts is not implemented in this tree; when it is, a coalesced reuse must pass `check_name` for the new origin and `check_addr` for the session's address |
| `Dialer::connect` (the only place TCP is opened: direct, pool, proxy hop, WebSocket) | `check_name` before DNS; then `check_resolved` for each resolved address, refused ones dropped, none left means `Blocked`; only the survivors get a SYN |
| `Proxy::tunnel` (`CONNECT`, SOCKS5) | target name `check_remote_name` before the proxy is contacted (the proxy resolves it, so the name is all there is); the proxy address through `Proxy::dial`, which goes through `Dialer::connect`, so it must be allowed itself; a refusal there is reported as blocked, not as a failed proxy |
| Forward requests (`http:` over an HTTP proxy) | target by `fetch_once_inner`, proxy address by `dial` |
| WebSocket (`NetContext::websocket`) | `connect_tcp` (as above) |
| Workers, service workers, favicons, `fetch_page`, CDP `Fetch.continueRequest` | all use `BrowserContext::take_net`, which applies the context's policy. `BrowserContext::set_egress` is set once, by `serve`/`fetch`; a CDP client cannot change it |
| CDP-supplied responses (`Fetch.fulfillRequest`) | not network, stay allowed (tested through Playwright `route.fulfill`) |

There are no speculative requests (preconnect, prefetch, DNS prefetch,
beacons beyond `fetch`) in the tree; any added later must go through
`take_net` and so through the policy. The firefox build's `serve` uses
`MockHost`, which only loads loopback URLs; `open` and `loopback` are
accepted there (it is stricter), `allowlist` is refused.

### How a refused request looks

`NetError::Blocked` displays as
`net::ERR_BLOCKED_BY_CLIENT: egress policy (loopback) does not allow host
name "example.com"`. The prefix is Chrome's code for a request the client
blocked, the rest is for people (`fetch` CLI, logs). Chrome's text has no
detail, so `lb-page`'s `net_error_text` cuts it for every CDP-visible text.
Recorded from Chrome 153.0.8010.12 (`crates/lb-page/tests/egress/
chrome.json`, requests failed with `Fetch.failRequest` `BlockedByClient`,
which is what a client-side blocker or `route.abort('blockedbyclient')`
produces):

| Surface | What the page or client sees | Source |
|---|---|---|
| Navigation | `Page.navigate` `errorText` `net::ERR_BLOCKED_BY_CLIENT`; Playwright `page.goto: net::ERR_BLOCKED_BY_CLIENT at URL` | recorded |
| `fetch()` | rejects `TypeError: Failed to fetch` | recorded |
| XHR | `error` event, `status` 0, `readyState` 4 | recorded |
| `<img>`, `<script>`, parser-inserted `<script type=module>` (async and not) | `error` event | recorded |
| Worker `fetch()`, service worker `fetch()` | `TypeError: Failed to fetch` | recorded (worker); same path (service worker) |
| WebSocket | `error` then `close` 1006, not clean; console text `Error in connection establishment: net::ERR_BLOCKED_BY_CLIENT` | events recorded; Chrome cannot be told to block a WebSocket, the recorded line for an unresolvable host is the same text with `net::ERR_NAME_NOT_RESOLVED`, so the code is substituted |
| Proxy address refused | same blocked error (not `ERR_PROXY_CONNECTION_FAILED`: the proxy is fine, we refused it) | by design |

Why `ERR_BLOCKED_BY_CLIENT` and not `ERR_ADDRESS_UNREACHABLE` or
`ERR_NETWORK_ACCESS_DENIED`: it is the code Chrome gives when its embedder
refuses a request on purpose, it needs no new text anywhere, and a client
cannot confuse it with a real network failure.

### Tools

- `lightbrowser fetch`, `serve` and `fingerprint-report`: `--egress
  open|loopback|allowlist` and `--allow-host H` (repeatable); parse errors
  are usage errors (exit 2), and so are two different `--egress` values (the
  last one must not silently lift the first). `CdpHostOptions.egress` carries
  it to every context `serve` makes. The firefox build's `serve` (mock page
  host, loopback URLs only: at least as strict as `open` and `loopback`)
  accepts those two and refuses `--egress allowlist`, which it could not
  honour.
- Python SDK: the launcher passes `--egress loopback` by default;
  `Browser(egress="open")` or `egress=["10.0.0.5", "*.lab.test"]` (loopback
  plus those) override it (`lightbrowser.launcher.egress_args`). The words
  `open`, `loopback` and `allowlist` inside a list are an error (they would
  be host names), and `--egress`/`--allow-host` in `extra_args` is an error:
  the flags would follow the setting on the command line and could lift it.
- Playwright harness: `LIGHTBROWSER_SERVE_ARGS` adds arguments to its
  `serve` (the repository suites pass with `--egress loopback`).
- Test hook: `ConnectionPool::set_resolve_hook` / `NetContext::set_resolve_hook`
  / `BrowserContext::set_resolve_hook` (hidden) answer name lookups before
  DNS. It is how the tests prove that no lookup happens for a refused name
  and how they point a name at this machine's own address.

### The mcp subcommand

The `mcp` subcommand uses this policy for both of its checks (ADR 0041):
it builds one `EgressPolicy` (mode `loopback`, entries `*.test`, `test`
and its `--allow-host` values), hands the same `Arc` to every context
(`CdpHostOptions::egress`), and its up-front check in `browser_navigate` is
`EgressPolicy::check_url`, the name check `fetch` makes per hop of a direct
request (`fetch` calls `check_url` too). Its old tool-level glob matcher
(with `?`, and IP literals compared as text) is gone.

### Revision 1: findings of the independent review

- *An allowed name could be rebound to anything.* The first version trusted
  an allow entry's name for every address, so `*.lab.test` answered with a
  public address (or `169.254.169.254`) was dialled, against the aim that
  rebinding cannot get around the policy. Now an unpinned name entry reaches
  internal addresses only, and `name@cidr` pins a name to the addresses it
  may have (needed for a name on a public address, which was never reachable
  by IP-only entries because a name that is not allowed is refused before
  its lookup). Tests: `egress::tests` (matcher), `an_allowed_name_cannot_be_
  rebound_to_a_public_or_link_local_address`, `a_pinned_name_reaches_only_its_
  pins` (lb-net), `an_allowed_name_rebound_to_a_public_address_is_blocked_for_
  a_page` (lb-page).
- *A name through a proxy was checked by name only, and `a.localhost` passed
  as "loopback".* The proxy resolves it, so the implicit rule (must resolve
  to loopback) cannot hold. Now `check_remote_name`: only an unpinned entry
  vouches, `localhost` names and pinned names are refused before the proxy
  is contacted. Chrome sends `localhost` to a proxy only with `<-loopback>`;
  with the policy on, write `127.0.0.1` there. Tests: `a_name_through_a_proxy_
  needs_an_unpinned_entry`, the WebSocket case in `websocket_connections_are_
  refused_too`, `pool::tests::acquire_refuses_before_making_a_group`.
- *Test gaps by mutation.* The pool's check had no test of its own (the hop
  check refuses first); now `pool::tests` calls `acquire` directly. A
  regression that let a request through to a listener that never answers
  hung the test binary; the fetch helpers now have a 10 s limit and fail.
  Re-run of the single-point mutations (dialer without `check_resolved`,
  pool without its check, hop without its check, proxy dial with an open
  policy, tunnel without or with the local name check, matcher with
  unrestricted names or ignored pins): each fails a test, none hangs.
- *Flag conflicts and loopholes.* `--egress` twice with different values,
  `open`/`loopback`/`allowlist` as a list item in the SDK, the egress flags
  in the SDK's `extra_args`, and `0.0.0.0/0`-style entries are now errors.
  `fingerprint-report` takes the flags (it was always open); the firefox
  `serve` refuses an allowlist.
- *Module scripts.* A parser-inserted `<script type=module async src>` whose
  fetch failed fired no `error` (the deferred form did). Fixed in `lb-page`
  `nav.rs`; the two forms are in `probes.html` and `chrome.json` (Chrome:
  `error` for both). A module script inserted *by script* is skipped with a
  console warning ("not supported yet"), whatever the network does; that is a
  missing feature, not egress, and is left (ROADMAP open item).
- *Not egress-specific, left, and identical with `--egress open` against a
  refused port:* an iframe whose load fails shows `about:blank` where Chrome
  shows its error page; a refused `Page.navigate` keeps the old document
  where Chrome shows `chrome-error://chromewebdata/`; `fetch(..., {redirect:
  'manual'})` follows the redirect; `EventSource` is not implemented; refused
  requests leave no resource-timing entries; a refused XHR emits no
  `Network.loadingFailed`.

## Consequences

- Nothing changes without the flags: `open` is the default, and the cost on
  the hot path is a branch per hop and per connect.
- The check is at the transport, so scripts, redirects, workers and proxies
  cannot get around it. The trust an allow entry gives away is bounded: an
  allowed name reaches internal addresses only (pin it with `name@cidr` for
  anything else), and through a proxy only an unpinned entry's names are
  allowed, because the proxy resolves them.
- A proxy outside loopback needs its own `--allow-host`.
- The SDK is loopback-only unless told otherwise; code that used it for
  other hosts must pass `egress=`.
- Not covered: DNS used by anything outside lb-net (there is none), a proxy
  that itself reaches outside (it resolves the allowed name; a listed name
  through a proxy is trusted to the proxy), TCP shaped by
  the packet shim (it only rewrites packets of connections this code opened).

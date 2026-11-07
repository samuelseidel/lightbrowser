# ADR 0044: Egress policy: the internal-use rule, enforced at the transport

Status: Accepted (2026-11-07).

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
IP (`10.0.0.5`, `::1`, `[fd00::1]`) or a CIDR block (`10.0.0.0/8`,
`fd00::/8`; host bits are cleared, `::ffff:a.b.c.d/N` with N >= 96 is the
IPv4 block). Refused as mistakes: a bare `*` or only wildcards (use
`--egress open`), ports (`host:80`), a wildcard inside an internationalised
label, an `--allow-host` with `--egress open`. Flags only: no environment
variable, no banner.

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
- `check_resolved(host, ip)`: for an address a name resolved to. A name that
  an allow entry lists may resolve to any address (the operator vouched for
  it); every other name, `localhost` and `*.localhost` included, must
  resolve to an address `check_addr` allows. So a hosts file or a DNS answer
  (rebinding) cannot turn `app.localhost` into an outside destination, and an
  allowed name is useful for hosts that are not on loopback.
- `Blocked { target, kind, mode }` (`Name`, `Address`, `Invalid`), carried by
  `NetError::Blocked`.

Not loopback, on purpose: `0.0.0.0`, `::`, link-local, this machine's own
non-loopback addresses, `::7f00:1` and NAT64 forms. Allow them explicitly.

### Where it is enforced

| Point | Check |
|---|---|
| `fetch_once_inner` (every hop, redirects included) | `check_name(host)` first: before header work, pool and DNS |
| `ConnectionPool::acquire` | `check_name(group host)` before the group entry is made: a refused origin never gets a group, a waiter, a connect or a TLS session (`ConnectionPool::groups()`, `cached_sessions` in tests). HTTP/2 coalescing across hosts is not implemented in this tree; when it is, a coalesced reuse must pass `check_name` for the new origin and `check_addr` for the session's address |
| `Dialer::connect` (the only place TCP is opened: direct, pool, proxy hop, WebSocket) | `check_name` before DNS; then `check_resolved` for each resolved address, refused ones dropped, none left means `Blocked`; only the survivors get a SYN |
| `Proxy::tunnel` (`CONNECT`, SOCKS5) | target name `check_name` before the proxy is contacted (the proxy resolves it, so the name is all there is); the proxy address through `Proxy::dial`, which goes through `Dialer::connect`, so it must be allowed itself; a refusal there is reported as blocked, not as a failed proxy |
| Forward requests (`http:` over an HTTP proxy) | target by `fetch_once_inner`, proxy address by `dial` |
| WebSocket (`NetContext::websocket`) | `connect_tcp` (as above) |
| Workers, service workers, favicons, `fetch_page`, CDP `Fetch.continueRequest` | all use `BrowserContext::take_net`, which applies the context's policy. `BrowserContext::set_egress` is set once, by `serve`/`fetch`; a CDP client cannot change it |
| CDP-supplied responses (`Fetch.fulfillRequest`) | not network, stay allowed (tested through Playwright `route.fulfill`) |

There are no speculative requests (preconnect, prefetch, DNS prefetch,
beacons beyond `fetch`) in the tree; any added later must go through
`take_net` and so through the policy. The firefox build's `serve` uses
`MockHost`, which only loads loopback URLs; the flags are accepted there.

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
| `<img>`, `<script>` | `error` event | recorded |
| Worker `fetch()`, service worker `fetch()` | `TypeError: Failed to fetch` | recorded (worker); same path (service worker) |
| WebSocket | `error` then `close` 1006, not clean; console text `Error in connection establishment: net::ERR_BLOCKED_BY_CLIENT` | events recorded; Chrome cannot be told to block a WebSocket, the recorded line for an unresolvable host is the same text with `net::ERR_NAME_NOT_RESOLVED`, so the code is substituted |
| Proxy address refused | same blocked error (not `ERR_PROXY_CONNECTION_FAILED`: the proxy is fine, we refused it) | by design |

Why `ERR_BLOCKED_BY_CLIENT` and not `ERR_ADDRESS_UNREACHABLE` or
`ERR_NETWORK_ACCESS_DENIED`: it is the code Chrome gives when its embedder
refuses a request on purpose, it needs no new text anywhere, and a client
cannot confuse it with a real network failure.

### Tools

- `lightbrowser fetch` and `serve`: `--egress open|loopback|allowlist` and
  `--allow-host H` (repeatable); parse errors are usage errors (exit 2).
  `CdpHostOptions.egress` carries it to every context `serve` makes.
- Python SDK: the launcher passes `--egress loopback` by default;
  `Browser(egress="open")` or `egress=["10.0.0.5", "*.lab.test"]` (loopback
  plus those) override it (`lightbrowser.launcher.egress_args`).
- Playwright harness: `LIGHTBROWSER_SERVE_ARGS` adds arguments to its
  `serve` (the repository suites pass with `--egress loopback`).
- Test hook: `ConnectionPool::set_resolve_hook` / `NetContext::set_resolve_hook`
  / `BrowserContext::set_resolve_hook` (hidden) answer name lookups before
  DNS. It is how the tests prove that no lookup happens for a refused name
  and how they point a name at this machine's own address.

### The mcp subcommand

The `mcp` subcommand (branch `wf/borrow-mcp-r1`) has its own host check at
the tool layer and is not touched here. To adopt this later it replaces
that check with `lb_net::EgressPolicy::parse(..)` built from its own flags,
calls `check_name(host)` on the URL's host in the tool, and passes the same
`Arc<EgressPolicy>` to the `BrowserContext` (`set_egress`) so the transport
enforces what the tool promised, redirects and subresources included. Its
tool-level refusal can reuse `Blocked`'s display.

## Consequences

- Nothing changes without the flags: `open` is the default, and the cost on
  the hot path is a branch per hop and per connect.
- The check is at the transport, so scripts, redirects, workers and proxies
  cannot get around it. The one trust it gives away is an allowed *name*
  (an allow entry): its addresses are not checked, by design. Use CIDR
  entries to pin addresses.
- A proxy outside loopback needs its own `--allow-host`.
- The SDK is loopback-only unless told otherwise; code that used it for
  other hosts must pass `egress=`.
- Not covered: DNS used by anything outside lb-net (there is none), a proxy
  that itself reaches outside (it resolves the allowed name), TCP shaped by
  the packet shim (it only rewrites packets of connections this code opened).

# ADR 0041: `lightbrowser mcp`, a Model Context Protocol server over stdio

Status: Accepted (2026-11-07).

## Context

Agent frameworks drive browsers through MCP servers (Playwright MCP,
Lightpanda and several scraping CLIs ship one). We want the same for
LightBrowser so an AI agent can load our own test pages, read the
accessibility tree, click and type, with the trusted input pipeline and the
persona of every other entry point. The idea is borrowed; the code is ours.

Two rules shape it. The project is for internal bot-detection validation,
so a tool that lets an agent wander must stay on our own servers unless the
operator says otherwise. And an MCP server is started by the client as a
child process: it should expose nothing on the network.

## Decision

### Transport and protocol

`lightbrowser mcp [--allow-host GLOB]... [--persona F] [--profiles-dir D]
[--insecure] [--proxy URL] [--proxy-bypass-list L] [--timeout SECS]`
speaks JSON-RPC 2.0, one UTF-8 message per line, on stdin and stdout
(`bin/lightbrowser/src/mcp/rpc.rs`). On unix, fd 1 is copied for the
protocol and then pointed at stderr, so no library print can corrupt the
stream; logs go to stderr. It listens on nothing, and closing stdin ends
it with exit code 0.

- `initialize`: accepts `2025-11-25`, `2025-06-18`, `2025-03-26` and
  `2024-11-05`. The answer is the client's version when we speak it (the
  newest of them if it sends a list), else `2025-11-25`. `serverInfo` is
  `lightbrowser` plus the crate version; capabilities are
  `{tools: {listChanged: false}}`.
- `notifications/initialized` and any other notification: no answer
  (cancellation is ignored). `ping`, `tools/list`, `tools/call`. Batches are
  answered (2025-03-26 allows them).
- Unknown methods: `-32601`. Bad params, an unknown tool name or non-object
  `arguments`: `-32602`; unparsable input `-32700`; a non-request `-32600`.
- Tool failures (bad ref, blocked host, JavaScript exception, timeout) are
  results with `isError: true` and a text explanation, never protocol
  errors. Missing or mistyped arguments are tool errors too.
- Calls run one at a time, in order.

### Tools

| Tool | Arguments | Result |
|---|---|---|
| `browser_navigate` | `url`, `wait_until?` (`load`, `domcontentloaded`, `networkidle`) | final URL, title, status |
| `browser_snapshot` | `start_line?` | accessibility tree as text with refs, in parts of about 100 KB |
| `browser_click` | `ref` | what was clicked, and where the page went if it navigated |
| `browser_type` | `ref`, `text`, `submit?` | what was typed and the field's value afterwards; an error for elements that cannot take text |
| `browser_press_key` | `key` (`Enter`, `ArrowDown`, `Control+a`, ...) | confirmation |
| `browser_evaluate` | `expression` | the JSON result (pretty-printed) |
| `browser_screenshot` | none | MCP image content, base64 PNG |
| `browser_extract` | `format`: `text`, `html` or `markdown` | page text (`innerText`), HTML after scripts, or Markdown (ADR 0042) |
| `browser_close` | none | closes the page; the next call opens a fresh one |

Output above 100 KB is cut on a character boundary with a note saying how
much was shown. A snapshot is not cut but split: the first part holds the
first node lines that fit, and ends with `[snapshot cut: call
browser_snapshot with start_line=N for the rest]`; refs are assigned for
the whole tree, so refs in later parts are valid (see revision 1).

Snapshot format: one node per line, indented by depth, `- role "name"
[flags] [ref=eN] value="..."`. The nodes are `Accessibility.getFullAXTree`,
which `lb-cdp` builds from `lb_layout::AxTree`
(`convert::ax_nodes`), so roles, names and states are what Playwright's
`getByRole` sees. Ignored nodes (hidden subtrees, role-less generics) are
left out and their children move up; static text that repeats the name of
the nearest ancestor named by its content (a button, link, heading, tab,
cell and the like) is dropped, and no other text is (revision 1). Interactive roles (button, link, textbox, checkbox, radio,
combobox, option, tab, menuitem, slider, ...), landmarks (banner,
navigation, main, complementary, contentinfo, search, dialog, and named
form or region) and focusable nodes get a ref. A ref names a DOM node
(CDP backend node id): the same node keeps its ref across snapshots, and
all refs are forgotten when the main frame navigates. An unknown or stale
ref is a tool error that says to take a new snapshot. The AX tree only
knows the `value` attribute, so text fields are completed with their live
`value` property, read in the isolated world (every field in the part
returned, values cut at 2000 characters).

`browser_evaluate` and every other script the server runs use an isolated
world: they see the DOM, never the page's own globals, and the page sees
nothing of the server (nothing automation-related is visible to the page
world, as for every CDP helper).
`browser_extract` `markdown` is the converter of `lightbrowser fetch
--dump markdown` (ADR 0042) on the live document, with the page's computed
style deciding what is hidden: the driver calls `CdpHost::extract` (which
runs `lb_page::Page::extract`) directly, not through a CDP method, so no
CDP surface is added.

JavaScript dialogs would freeze the page with nobody to answer them, so the
server accepts `alert` and `beforeunload` and dismisses `confirm` and
`prompt`, and appends a note saying so to the next tool result.

### How the browser is driven: an in-process CDP client

Three ways were considered.

1. Call `lb_page::Page` directly. `Page` has navigate, evaluate and text,
   but no input, no accessibility tree, no node resolution and no
   screenshot: those exist only at the CDP layer (`PageHost` in `lb-cdp`
   plus `lb-page`'s `cdp/`). Using `Page` would mean a second
   implementation of hit testing, scroll into view, content quads,
   isolated worlds and dialogs, which then drifts from what Playwright
   exercises (38 passing tests).
2. A loopback CDP client: start `lb_cdp::Server` on `127.0.0.1:0` and
   connect a WebSocket client from the same process. It works, but it
   binds a port (against "listens on nothing"), needs a WebSocket client
   in the binary, and lets any local process drive the browser.
3. **An in-process CDP client over channels.** New in `lb-cdp`:
   `InProcess::connect(host)` builds the same browser state as `Server`
   and one browser connection, without a listener; `send` takes a command
   as JSON text and replies and events come back on a receiver, in the
   order a WebSocket client would see them (`server.rs`, 45 lines). The
   driver (`mcp/driver.rs`) is a small DevTools client on top: flat
   sessions, `Page.navigate` and lifecycle events, `Accessibility.*`,
   `DOM.scrollIntoViewIfNeeded` and `getContentQuads`, then
   `Input.dispatchMouseEvent` and `dispatchKeyEvent`, `Runtime.evaluate`,
   `Page.captureScreenshot`.

Option 3 is the simplest robust one: every tool is a command Playwright
would send, so input is the trusted pipeline (`isTrusted` events, real
focus, default actions) and nothing new is added to the page-visible
surface. A reader task keeps the page state (URL, load events, status,
dialogs) current between tool calls.

Typing: `browser_type` scrolls the element into view, focuses it, selects
its content (so the text replaces it), then sends `keyDown`/`keyUp` pairs
per character the way Playwright's keyboard does for the US layout
(`mcp/keys.rs`: `keyDown` with `text` for printable keys, `rawKeyDown` for
others, `\n` is Enter) and `Input.insertText` for the rest. Clicks go to
the centre of the element's first visible box: move, press, release.

### Host policy (internal-use rule)

By default the browser may only request loopback addresses (`127.0.0.0/8`,
`::1`), `localhost`, `*.localhost` and `*.test`. `--allow-host ENTRY`
(repeatable) adds hosts; an entry is an egress policy allow entry (ADR
0044): a name glob with `*` (no `?`), an IP address, a CIDR block, or a
name pinned to addresses. Names are compared after normalisation (case,
trailing dot, IDN to punycode); an IP literal in a URL is compared with
address entries only, never with name globs. An entry the egress policy
refuses (`?`, a port, a bare `*`, `0.0.0.0/0`) stops the server at
start-up with exit 1. `about:`, `data:` and `blob:` need no network and
are not blocked by the policy (`data:` and `blob:` work for subresources;
lb-page cannot load a `data:` URL as the top-level document, so
`browser_navigate` does not offer it); other schemes (`file:`, `ftp:`,
`javascript:`) are blocked.

Checking the URL in the tool is not enough: a redirect, a script setting
`location`, a meta refresh or a subresource would go around it, and
`Fetch` interception does not see each hop of a redirect chain (lb-net
follows redirects itself). So the policy is enforced in `lb-net`, which
every request of every page, worker and service worker passes:

- The policy is an `lb_net::EgressPolicy` in mode `loopback` with the
  entries `*.test`, `test` and the `--allow-host` entries (ADR 0044),
  handed to every context through `CdpHostOptions::egress`. lb-net asks it
  at the top of every hop of `fetch` and before a WebSocket handshake,
  before any DNS lookup or socket, and checks every resolved address
  again. A refusal is `NetError::Blocked`, whose text is Chrome's
  `net::ERR_BLOCKED_BY_CLIENT`.
- There is one parser and one matcher: `mcp/policy.rs` `HostPolicy` builds
  that policy and its up-front check in `browser_navigate` is
  `EgressPolicy::check_url`, the same name check lb-net's `fetch` makes
  per hop of a direct request. So an obvious violation never reaches the
  page, and the tool cannot allow what the transport refuses or the
  reverse (`tool_and_transport_agree_on_every_entry` runs one table of
  entries and hosts through both). Through `--proxy` the transport is
  stricter than the tool (`check_remote_name`: only a name an unpinned
  entry lists), never looser.
- Refusals become notes. The driver keeps the URL of every page request
  from `Network.requestWillBeSent` until `loadingFinished` or
  `loadingFailed`; a `loadingFailed` with `net::ERR_BLOCKED_BY_CLIENT`
  (the server uses no `Fetch` interception, so only the egress policy
  gives it) puts the URL in a `BlockedLog`. A blocked navigation is a tool
  error naming the URL and the flag (`--allow-host <entry>`); any other
  result ends with `Note: the host policy blocked requests to <urls>
  (--allow-host <entry> allows more hosts)` when a subresource, a fetch or
  a script-started navigation was refused since the last result (at most
  20 URLs, each once). Requests of workers are not seen (the server does
  not attach to worker targets), and a redirect that lb-net follows itself
  is named by the URL of its first hop.

Not set by default anywhere else: `serve` and `fetch` use `--egress`.
A pooled keep-alive connection is reused without asking again, which is
fine: its host was allowed when it was opened (pool groups are per host).

### Client configuration

Claude Code:

```sh
claude mcp add lightbrowser -- /abs/path/to/lightbrowser mcp
# more hosts:  ... -- /abs/path/to/lightbrowser mcp --allow-host '*.corp.example'
```

or `.mcp.json` in the project:

```json
{
  "mcpServers": {
    "lightbrowser": {
      "command": "/abs/path/to/lightbrowser",
      "args": ["mcp", "--profiles-dir", "/abs/path/to/repo/profiles/tls"]
    }
  }
}
```

Claude Desktop, `claude_desktop_config.json` (Settings, Developer, Edit
Config), the same `mcpServers` entry. The server finds `profiles/tls` above
its working directory or its own executable; a client that starts it
elsewhere should pass `--profiles-dir` (and `--persona` for another
persona). The firefox build has no JS engine: `mcp` there reports that and
exits 1.

## Consequences

- No new dependency. `serde_json`, `tokio`, `url` and `libc` were already in
  the binary; the in-process connection reuses `lb-cdp`'s protocol state.
- The page worker count is 2 (one page); a service worker or a frame uses
  the spare thread.
- Not covered yet: content of iframes (a frame is one `Iframe` node in the
  snapshot; refs and click coordinates are per frame), several pages or tabs, history navigation, hover, drag
  and drop, file upload, waiting for a selector, cookies, console output as a
  tool, MCP resources and prompts. Calls are serial, so a `ping` during a long
  navigation waits for it (the client's timeout then decides). A call in
  flight when stdin closes is finished and answered before the exit, up to
  `--timeout`: a client that pipes one request and closes stdin
  (`printf ... | lightbrowser mcp`) gets its answer.
- Tests: `bin/lightbrowser/tests/mcp.rs` spawns the binary against a loopback
  page server (protocol versions, `tools/list` schemas, navigate, snapshot,
  type and click on a form, evaluate, screenshot, extract (`markdown` equals
  the `fetch --dump markdown` goldens in `crates/lb-page/tests/extract/`),
  blocked hosts including a redirect and a script navigation, blocked-request
  notes for subresources and fetches, `alert`/`confirm`/`prompt` answered and
  noted, `--allow-host` through a
  loopback proxy (no name lookup), bad refs, unknown methods, no listening
  socket, exit on stdin EOF, and the cases of revision 1); unit tests cover
  the protocol, the policy (one table of entries and hosts through the tool
  check and the transport check), keys and snapshot rendering; the egress
  tests in `lb-net` cover redirect hops; `lb-layout` has the deep-page AX
  tests. `tests/mcp_firefox.rs` (firefox build) checks the refusal and
  exit code 1.

## Revision 1 (review round 2)

Independent checks of the first version found these; each has a test.

- Snapshot text lost: static text was dropped when it was a substring of
  the nearest named ancestor's name, which for any page is the root named
  with the page title (`<title>Hello World</title><p>Hello</p>` lost the
  paragraph). Now only an ancestor named by its content can swallow its
  own text, and the match is on the trimmed text.
- Stack overflow on a deep page (about 2000 nested elements killed the
  process, also through `serve`): `lb_layout::AxTree::build` walked the DOM
  recursively. It now uses an explicit stack, name-from-content recursion
  stops at 256 levels, and the snapshot renderer is iterative with
  indentation capped at 48 levels. `lb-layout` tests build 5000 and 3000
  levels on a 2 MiB test thread; the old code overflowed. This changes
  `lb-layout`, which `serve` shares, for the better.
- `--timeout` above one day (`u64::MAX` overflowed the clock and panicked at
  the first navigation) is refused at start-up with exit 2.
- The navigate tool no longer offers `data:`.
- `browser_type` checks the element first and refuses what cannot take text:
  detached (stale ref), disabled, read-only, `<select>`, links, buttons,
  checkboxes and the like, naming the reason. A contenteditable element's
  content is replaced (it was appended to). CR, LF and CRLF are each one
  Enter.
- Snapshots are returned in parts with `start_line`, and live values are
  read for every field shown (the limit of 50 is gone), so nothing is out
  of reach behind the 100 KB limit.
- A `#fragment` navigation (`browser_navigate` or a click on `#x`) reported
  the old URL: lb-page sends no `navigatedWithinDocument`, so the driver
  now reads `location.href` after navigating and around input, and says
  `The URL changed to ... without loading a new document`.
- The driver's per-navigation lifecycle and status maps are pruned at each
  main-frame commit.
- The `--allow-host` test no longer looks up a name: the allowed host is
  reached through a loopback proxy that the test runs.

Looked at and left: stdin closing during a call waits for it (above: the
piped one-shot use is worth more than a fast exit, and the wait is bounded
by `--timeout`); a call with a missing required argument is an `isError`
result, not `-32602` (the 2025-11-25 specification lists input validation
errors as tool execution errors, so the model can correct itself; an unknown
tool or non-object `arguments` is `-32602`); `target=_blank` and
`window.open` navigate the same page (lb-page has no popups); iframe
content is not in the snapshot.

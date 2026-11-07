# LightBrowser fork of h2 0.4.20

Copied from crates.io `h2-0.4.20` and wired in with `[patch.crates-io]` in
the workspace `Cargo.toml`. The version stays `0.4.20` so every crate that
depends on `h2` (including hyper, if its `http2` feature is enabled) gets this
fork. Removed from the copy: `.cargo_vcs_info.json`, `Cargo.toml.orig`,
`Cargo.lock`, `.github/`, `.cargo-ok`.

Why: the Akamai HTTP/2 fingerprint records the SETTINGS order and the request
pseudo-header order. Upstream hard-codes both (ADR 0003). Chrome also flags
priority on its request HEADERS, and browsers write SETTINGS, WINDOW_UPDATE
and the first HEADERS before reading anything from the server.

Defaults are upstream behaviour. Nothing changes unless a client calls the
new builder methods.

## Public API added

- `h2::client::PseudoHeader { Method, Scheme, Authority, Path }`
- `client::Builder::pseudo_header_order(&mut self, [PseudoHeader; 4])`:
  order of request pseudo-headers. A pseudo-header missing from the array is
  still sent, after the listed ones, in upstream order.
- `client::Builder::settings_order(&mut self, Vec<u16>)`: order of the
  parameters in the initial SETTINGS frame, by RFC 9113 id. Unknown ids are
  ignored, listed settings with no value are skipped, set settings that are
  not listed follow in upstream order. Duplicate ids are emitted once.
- `client::Builder::headers_priority(&mut self, depends_on: u32, weight: u16,
  exclusive: bool)`: every request HEADERS frame gets the PRIORITY flag and
  this RFC 7540 priority block. `weight` is 1 to 256. Chrome:
  `(0, 256, true)`. Trailers and responses are unchanged.
- `client::Urgency(pub u8)` request extension and
  `client::Builder::chrome_stream_dependencies(&mut self, on: bool)`: a
  request carrying an urgency (0 most urgent, 7 least; the `u=` of its
  `priority` header) gets Chrome's block instead (ADR 0018): exclusive,
  depending on the most recently opened still-open stream of the same or a
  more urgent urgency (0 if none), weight 256, 220, 183, 147, 110, 74, 37
  or 1 (`Spdy3PriorityToHttp2Weight`). Closed streams leave the lists.

Behaviour change (all clients, not opt-in): the first poll of the client
connection writes its queued frames (connection WINDOW_UPDATE, and HEADERS of
requests sent before the first poll) before it reads. Upstream reads first,
so when the server's SETTINGS were already in the socket, the SETTINGS ACK
went out before the WINDOW_UPDATE. Real browsers never do that.

Unchanged: `Builder::initial_connection_window_size(n)` already makes the
client send one connection-level WINDOW_UPDATE with increment `n - 65535`
right after SETTINGS and before the first HEADERS (window updates are
flushed before other pending frames in `Streams::buffer_pending`). Checked on
the wire by `lb-net`'s `h2` tests. The update is only sent when the increment
is at least half the current window, which holds for browser values.

## Changed files

| File | Change |
|---|---|
| `src/frame/headers.rs` | `PseudoHeader` enum, `DEFAULT_PSEUDO_ORDER`; `Iter` holds an order and yields pseudo-headers in it; `HeaderBlock::into_encoding` takes the order; `set_stream_dep` (a request's own block wins over the codec's); new `Headers::encode_with_order(.., order, priority)`, which sets the PRIORITY flag and writes the block for requests when `priority` is set (`encode` calls it with the defaults and is now only used by tests, hence an `allow(dead_code)`); `PushPromise` uses the default order. |
| `src/frame/mod.rs` | Re-export `PseudoHeader` and `DEFAULT_PSEUDO_ORDER`. |
| `src/frame/settings.rs` | `Settings.order: Option<Vec<u16>>` and `set_order`; `for_each` (used by encode, payload length and Debug) honours it; helpers `Settings::get(id)` and `Setting::id()`. |
| `src/codec/framed_write.rs` | Encoder holds `pseudo_order` and `headers_priority`, passes both to `Headers::encode_with_order`; `set_pseudo_order`, `set_headers_priority`. |
| `src/codec/mod.rs` | `Codec::set_send_pseudo_order`, `Codec::set_send_headers_priority`. |
| `src/client.rs` | `pub use PseudoHeader`; `Builder.pseudo_order`; `pseudo_header_order` and `settings_order`; `Builder.headers_priority` and `headers_priority`; `handshake2` applies the pseudo order and HEADERS priority to the codec; `Urgency`, `chrome_stream_dependencies`. |
| `src/frame/priority.rs` | `StreamDependency` is `Clone + Copy`; new `StreamDependency::encode`. |
| `src/proto/connection.rs` | `preface_flushed`: the first `poll` runs `streams.poll_complete` before reading. |
| `src/proto/streams/counts.rs` | `priorities` (open streams per urgency), `enable_chrome_priorities`, `chrome_dependency`; `transition_after` drops closed streams from the lists. |
| `src/proto/streams/streams.rs` | `send_request` takes the `Urgency` extension and sets the frame's priority block; `Streams::enable_chrome_priorities`. |

All additions are marked `LIGHTBROWSER patch` in comments.

## Not done yet

- PRIORITY frames in the preface (older Firefox profiles list them).
- SETTINGS ids h2 has no field for, e.g. `NO_RFC7540_PRIORITIES` (0x9).
  `ENABLE_CONNECT_PROTOCOL` (0x8) exists but has no client setter.

## Rebasing

Copy the new upstream release over this directory, re-apply the changes in
the table above (search for `LIGHTBROWSER`), keep the upstream version number
and run `cargo test -p lb-net` (its `tests/fingerprint.rs` checks the
preface on the wire). The crate's own test suite needs dev-dependencies that
are not vendored; run it online with `cargo test --manifest-path
vendor/h2/Cargo.toml` when rebasing.

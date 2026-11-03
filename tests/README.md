# tests

Test suites arrive with the milestones that need them. Unit tests stay
inside each crate.

## Network fingerprint test (M1, in place)

`crates/lb-net/tests/fingerprint.rs` runs `lb_net::fetch` against
`lb-fptest`'s capture server on 127.0.0.1 and compares the wire with
`profiles/tls/`:

```bash
cargo test -p lb-net --test fingerprint -- --nocapture   # prints measured JA3/JA4/H2
```

- **Chrome** (`chrome_153_linux`): everything must match exactly:
  - JA3 (over the canonical order), JA4 and JA4_r;
  - ciphers, extension set, groups and sigalgs (GREASE aside);
  - ALPN, ALPS and cert compression;
  - the trust_anchors and server_padding payloads;
  - H2 SETTINGS, WINDOW_UPDATE, pseudo-header order, HEADERS priority, frame
    order and the Akamai text;
  - per-connection extension permutation.
- **Firefox** (`firefox_128_win10`):
  - Must match: ciphers, sigalgs, ALPN, cert compression, groups (minus
    ffdhe) and all H2 values.
  - Pinned and printed as expected differences: the BoringSSL gaps listed in
    ADR 0003 and the profile's own `ja3_string` vs `extensions_order`
    mismatch.

## TCP SYN fingerprint test (M1b, in place)

`crates/lb-net/tests/tcp_fingerprint.rs` (ADR 0007), loopback only. Needs root
for raw capture, the netfilter queue and iptables; without them it prints
`SKIP` and passes.

```bash
cargo test -p lb-net --test tcp_fingerprint -- --nocapture --test-threads=1
```

For each `profiles/tcp/` profile it fetches from a local HTTP server with a
3 MiB body, captures the SYN on `lo` and asserts TTL, DF, window, MSS, scale,
option bytes and order, timestamps, JA4T and (for rewritten packets) both
checksums. Linux runs with no shim; Windows and macOS run with the shim
in-process. Also checked: no profile shows the host stack, a UA/OS mismatch is
refused, a stopped shim drops SYNs, and a shim profile is refused when the rule
is missing.

### Verifying a Windows or macOS profile on a real device

The Windows and macOS profiles are from public signatures. Check them on one of
our own machines connecting to our own test host:

```bash
# on the Linux test host (root); the device connects to HOST:18446
./target/debug/lb-fptest tcp-capture --listen HOST_IP:18446 --iface eth0 \
    --allow-non-loopback --count 3
# on the device:  curl http://HOST_IP:18446/   (or any TCP connect), three times
```

Compare `ja4t`, `ttl`, `df`, `option_bytes` with `profiles/tcp/<os>.json`. If
they differ, update the profile (and `expected.ja4t`), set `provenance.status`
to `captured`, and run the test above.

## Planned suites

- **WPT subset** (M2): web-platform-tests for DOM, events, timers, fetch.
- **Playwright CDP suite** (M3): `connectOverCDP` with goto, evaluate, click, fill, getByRole, content.
- **Fingerprint tests** (M4, M4b): run against OUR detection engine only. Same seed must give the same fingerprint; Chrome personas must score as consistent.
- **Firefox network fingerprint** (M4b): close the ADR 0003 gaps and re-capture Firefox 128 with `lb-fptest`.

## Re-capturing a Chrome TLS/H2 profile (`lb-fptest`)

`crates/lb-fptest` is test support only (never shipped). Its local server
reads the raw ClientHello, finishes TLS with an in-memory self-signed cert,
answers one tiny HTTP/2 or HTTP/1.1 response and records the H2 SETTINGS,
first WINDOW_UPDATE, pseudo-header order and HEADERS priority. It binds loopback only. Point
only a local browser or our own client at it.

1. Build and get the browser version:
   ```bash
   cargo build -p lb-fptest
   /opt/pw-browsers/chromium --version     # e.g. Google Chrome for Testing 153.0.8010.12
   ```
2. Start the capture (two connections, to see extension permutation):
   ```bash
   ./target/debug/lb-fptest capture --out profiles/tls/chrome_<MAJOR>_linux.json \
       --listen 127.0.0.1:18445 --count 2 --browser-version <FULL_VERSION>
   ```
3. In another shell, load the page twice, each time with a fresh profile dir
   so the second run does not resume the TLS session:
   ```bash
   for i in 1 2; do
     d=$(mktemp -d)
     timeout 12 /opt/pw-browsers/chromium --headless=new --no-sandbox \
       --ignore-certificate-errors --user-data-dir="$d" --no-proxy-server \
       --host-resolver-rules="MAP fptest.localhost 127.0.0.1" \
       --disable-background-networking --disable-component-update --disable-sync \
       --no-first-run --dump-dom https://fptest.localhost:18445/ >/dev/null
     rm -rf "$d"
   done
   ```
   Use a host name, not `127.0.0.1`: Chrome sends no SNI for IP literals.
   `--no-proxy-server` keeps Chrome off any `HTTPS_PROXY` in the environment.
4. Check the notes printed on stderr, then update `Persona::default_chrome()`
   (UA major, `sec-ch-ua`, `tls_profile_id`) if the major version changed.
   Point the Chrome test in `crates/lb-net/tests/fingerprint.rs` at the new
   id and run it.
5. If the test reports a TLS gap (`TlsConnector::gaps()`) or a missing
   extension, BoringSSL in `boring-sys` may be too old for that Chrome, or
   `lb-net/src/tls.rs` needs a new knob. Check
   `~/.cargo/registry/src/*/boring-sys-*/deps/boringssl/include/openssl/`.

What the tool does with the captures:

- Chromium opens one or two speculative sockets per load and drops them
  mid-handshake. Capture mode skips connections that fail or resume a
  session (`pre_shared_key`), and keeps only full handshakes that sent a
  request.
- If the extension orders differ between captures but the sets match, it sets
  `permute_extensions: true` and writes Chrome's canonical (un-permuted) order:
  `0,23,65281,10,11,35,16,5,13,18,51,45,43,27,ALPS,51764,4832,65037`, keeping only the
  extensions it saw. Extensions not in that list are appended at the end with
  a note. GREASE is not listed; `grease: true` covers it (BoringSSL puts
  GREASE first and last).
- `server_padding_request` and `trust_anchor_ids` (hex, without the outer
  length) are taken from the 4832 and 0xCA34 payloads, and
  `http2_priority.headers` from the first HEADERS frame. `lb-net` sends
  exactly these.
- `ja3`/`ja3_string` use the canonical order, so they are stable. Real Chrome
  has a new JA3 on every connection. `ja4` (hashed) and `ja4_r` are
  order-independent. `http2_akamai_fingerprint` is the Akamai H2 text.
- Unknown or GREASE H2 settings are dropped from `http2_settings` with a note,
  because `lb-net` rejects unknown setting names.

`lb-fptest serve --listen 127.0.0.1:PORT [--alpn http/1.1]` prints one JSON
line per connection (JA3, JA4, extension order, Akamai H2, and with
`--alpn http/1.1` the clear-text request headers). Use it for checking our own
client or for reading Chrome's `sec-ch-ua` and header order.

# header_capture

Captures the request headers the local Chromium sends after `Accept-CH`
and `Critical-CH`. Loopback only: every server binds 127.0.0.1 and
Chromium is pointed only at them. Evidence for
`profiles/headers/chrome_153.json` (ADR 0011, ADR 0015).

```bash
# --headless=new (default)
node tools/header_capture/accept_ch.js --out profiles/headers/captures/chrome_153_linux_accept_ch.jsonl
# headed, for comparison
xvfb-run -a -s "-screen 0 1920x1080x24 -dpi 96" \
  node tools/header_capture/accept_ch.js --mode headed --out /tmp/x/headed.jsonl
# subresource orders with hints, for renderer_group.observed_hints
python3 tools/header_capture/derive_groups.py profiles/headers/captures/chrome_153_linux_accept_ch.jsonl
```

- Servers: A, B, C over HTTP/1.1 (B is cross-origin for frames and CORS,
  C is a fresh origin for a hint subset) and H over h2 with a throwaway
  certificate made by `openssl`. Chromium trusts that key through
  `--ignore-certificate-errors-spki-list`; with
  `--ignore-certificate-errors` it ignores `Accept-CH` on the connection
  (`--cert-errors` keeps that mode for comparison).
- Flow and page contents are described at the top of `accept_ch.js`. The
  second launch reuses the profile to see stored hints on a typed
  navigation.
- Output: one JSON line of metadata, then one line per request with the
  header names, case and order as received (Node keeps the wire order on
  h2). The profile and certificate live in a temp dir that is removed.
- Headed and `--headless=new` gave the same names and order (88 requests,
  Chrome for Testing 153.0.8010.12).

# ADR 0036: Content Security Policy

Status: accepted (2026-11-07)

## Context

Nothing enforced Content Security Policy. Pages can tell: rebrowser's
bot-detector "bypassCsp" check puts a `<meta http-equiv=Content-Security-Policy>`
with a `script-src` host list on the page, then appends a script from a
host the list does not name. Chrome fires the script's `error` event and
sends no request; LightBrowser loaded and ran it, which is what Playwright's
`bypassCSP` (CDP `Page.setBypassCSP`) looks like. Other detection scripts
read `SecurityPolicyViolationEvent`s, `eval` errors and console texts.

## Decision

Enforce CSP Level 3 as Chrome 153 does, with Chrome's texts.

- **Policy model** in `lb-webapi` `rt/csp/policy.rs`, ported from
  Chromium's network service parser (`content_security_policy.cc`,
  `csp_source*.cc`) and Blink's checks (`csp_directive_list.cc`,
  `source_list_directive.cc`, `csp_source.cc`): directive names and
  fallbacks (`script-src-elem/-attr -> script-src -> default-src`, style
  likewise, `worker-src -> child-src -> script-src -> default-src`,
  `frame-src -> child-src -> default-src`), source expressions (`*`,
  schemes, hosts with wildcards, ports, paths ignored after redirects,
  `'self'`, `'none'`, `'unsafe-inline'`, `'unsafe-eval'`,
  `'wasm-unsafe-eval'`, `'unsafe-hashes'`, nonces, sha256/384/512 hashes on
  BoringSSL, `'strict-dynamic'` and its effect on host lists and
  `'unsafe-inline'`, `'report-sample'`), parse error texts, header vs
  `<meta>` vs report-only rules, `upgrade-insecure-requests`, `sandbox`,
  `report-uri`. It holds no window state: checks return violations.
- **Enforcement** in `rt/csp/mod.rs`: a `CspSide` per window and worker
  scope (the CSP list, CDP bypass, nonce slots, report de-duplication).
  Load paths call `Window::csp_url` (fetch directives), `csp_inline`
  (inline scripts, handlers, `javascript:` URLs, styles), `csp_navigation`
  (`frame-src`, `form-action`, with the network service's texts) or
  `csp_eval`. A window without policies, or one `Page.setBypassCSP`
  covers, returns at once.
- **Delivery**: `Content-Security-Policy` and `-Report-Only` headers of a
  document (every header, comma-separated policies) at commit; `<meta>`
  policies when inserted (enforce-only, in `<head>`, Chrome's console
  texts otherwise, `frame-ancestors`/`report-uri`/`sandbox` ignored).
  `about:blank` and `srcdoc` frames inherit the parent's list; workers take
  their script response's, `blob:`/`data:` workers their creator's. A
  frame document's `frame-ancestors` is checked against its ancestors
  before it commits (refused: Chrome's error page and console text).
- **eval and WebAssembly**: `lb-js` gets a `CodeGenPolicy` per realm
  (`RealmOps::set_code_generation`, `v8_engine/codegen.rs`). It turns V8's
  code generation from strings off with Blink's `EvalError` text
  (`Context::SetErrorMessageForCodeGenerationFromStrings`, called by its
  exported C++ symbol since rusty_v8 does not wrap it) and installs V8's
  modify-code-generation and wasm callbacks, which ask the window (so
  report-only policies report and allow). String timers are checked when
  made (a blocked one returns 0). Engine-specific code stays behind lb-js.
- **Violations**: Chrome's console message (error, or info for
  report-only), a `securitypolicyviolation` event in a networking task at
  the element (inline checks, while connected) or the document (the
  worker global in workers), bubbling and composed, with every field
  (`blockedURI` stripped as Blink does, `sourceFile`/line/column from the
  JS stack, else the parser position), and a `report-uri` POST
  (`application/csp-report`, Chrome's JSON key order, once per distinct
  report). `report-to` is parsed, not delivered.
- **Blocked loads** send no request: scripts fire `error`; images,
  background images, style sheets and `@import`s fail; `fetch` rejects
  with `TypeError: Failed to fetch` plus Chrome's "Fetch API cannot load"
  error; XHR fails (sync: `NetworkError`); `sendBeacon` returns true;
  WebSocket closes at once and fires `error`; workers fire `error` at their
  object; `serviceWorker.register` rejects with Chrome's `SecurityError`;
  frames show Chrome's error page (an opaque `chrome-error://` document);
  redirects are checked before lb-net follows them (a `RequestMeta`
  redirect check; a refused hop fails with `net::ERR_BLOCKED_BY_CSP` and is
  reported with the original URL as `blockedURI`);
  `upgrade-insecure-requests` rewrites `http:`/`ws:` subresource URLs that
  are not potentially trustworthy;
  forms are not sent; blocked `style` attributes keep the element's last
  allowed inline style (`lb-css` inline override); blocked `<base href>` is
  not used as base URL. The `nonce` IDL attribute is an internal slot and
  the content attribute is hidden once a header-delivered policy applies.
- **CDP**: `Page.setBypassCSP` sets the page's flag (documents committed
  later and the current frame tree); `lb_page::Page::set_bypass_csp` does the
  same without CDP.

## Consequences

- 37 Chrome-recorded cases (35 pages plus 2 with `bypassCSP`) in
  `lb-page` `tests/csp/` (`serve.py` for headers, `chrome.mjs` to
  re-record, `chrome.json`), compared as sorted line lists: the page's log
  with every violation's fields, the CSP console texts, and the requests
  the server saw. Chrome's own position for CSS background image
  violations varies between runs (style recalc time), so those three
  fields are not compared. Playwright `tests/playwright/csp.spec.mjs` checks
  the rebrowser page over CDP, with and without `bypassCSP`.
- The checks cost nothing on pages without CSP (one empty-list test per
  load; mutation handling only looks for `<meta>` there).
- Open: `report-to` delivery (Reporting API); `sandbox` beyond the opaque
  origin and the storage/cookie errors (scripts, forms, popups without
  their `allow-*`); CSP of service workers' own responses; `blob:`/`data:`
  documents inheriting; `frame-src` on frame redirects and on navigations a
  frame starts itself; `object-src`/`media-src`/`manifest-src` (no such
  loads yet); Trusted Types; nonce and `'strict-dynamic'` checks on script
  redirects (not checked); `importScripts` in workers. Not CSP: V8's async
  WebAssembly compile tasks are not pumped, so `WebAssembly.compile` never
  settles (the cases compile synchronously).

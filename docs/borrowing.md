# Borrowing from other browsers and tools

Date: 2026-11-07. Plain summary of a study of other projects and what we did
with it. Details of each package are in ADRs 0043 to 0047, 0051 (mcp) and 0052 (extract).

## Rules we followed

- Ideas and facts only. Licences differ (Camoufox patches MPL-2.0 and its
  recorded cursor data LGPL, Lightpanda AGPL-3.0, nodriver and zendriver
  AGPL-3.0, Scrapling BSD-3, Obscura Apache-2.0, fingerprint-suite Apache-2.0,
  BotD, CreepJS and FingerprintJS MIT, rebrowser's bot detector has no
  licence). No code or data file was copied.
- Nothing from those projects was installed or run, and no third-party site
  or detection service was visited. Browser behaviour was recorded from local
  Chrome 153 on loopback pages.
- LightBrowser exists to validate our own detection engine. We took
  detector-side work, tooling, safety and web-platform conformance.

## Projects studied

| Project | Licence | What it was useful for |
|---|---|---|
| Camoufox | MPL-2.0 | Its own change of mind: values that identify the host or network are spoofed, everything else stays stock because noise is itself a tell; getter-cost parity; per-OS font invariants |
| Scrapling | BSD-3 | Thin orchestration over curl_cffi and Patchright; MCP server and extract CLI; Referer plus `Sec-Fetch-Site: none` is an inconsistent pair |
| Obscura | Apache-2.0 | CLI modes, MCP, CDP gaps found by diffing client sources, a transport-level SSRF guard (we inverted it into the egress policy) |
| Lightpanda | AGPL-3.0 | Markdown dump, MCP, response-stage Fetch, Web Bot Auth signing |
| Patchright, rebrowser, nodriver and others | various | Which leaks current detector suites test (history throttle, CSP, storage partitioning, performance timeline, pointer provenance) |
| fingerprint-suite, BrowserForge | Apache-2.0 | Constraint pinning and relaxation order for persona generation (not adopted: star-shaped model, private data) |
| FingerprintJS/BotD, CreepJS, rebrowser's bot detector | MIT / none | Checklists for our detector |
| wreq, curl-impersonate, utls | Apache-2.0 / MIT / BSD | Our TLS and HTTP/2 numbers agree; open: HPACK policy, ECH GREASE and trust-anchor order, HTTP/3 |
| Playwright MCP, browser-use, Stagehand, Steel, Browserless | Apache-2.0 / MIT / SSPL | Which browser-side primitives agent tools need |

## What we took

See the table in `ROADMAP.md`, "Borrowed ideas".

## Not taken (scope)

Features whose purpose is to avoid detection: deriving a persona from a proxy
exit, making WebRTC or time values match a proxy, shaping input traces from
recordings of people, solving challenges, blocking third-party scripts,
extra browser brands as personas, rewriting console or error text to hide
the driver. They are out of scope for a validation browser. Camoufox's cursor
recordings are LGPL data and could not be copied anyway.

## Open for the owners

- TLS profile re-capture (ECH GREASE, `server_padding`, trust-anchor order).
- Whether Puppeteer's default viewport should be accepted (ADR 0009).
- Whether a verified-agent path exists in our engine (declared agents are
  parsed, signatures are not verified yet).
- Backlog listed in `ROADMAP.md`.

# ADR 0006: Two browser families, one binary per family

Status: Accepted (2026-11-02)

## Context

We need to validate our detection engine against both Chrome-like and
Firefox-like clients. The two differ in many places that detectors check:

- JS engine behavior (V8 vs SpiderMonkey, see ADR 0002)
- the set of exposed interfaces and their order (Chromium IDL vs Gecko IDL;
  `userAgentData` only in Chrome, `InstallTrigger` and `mozInnerScreenX` only
  in Firefox)
- TLS ClientHello (see ADR 0003)
- HTTP/2 SETTINGS, PRIORITY frames and pseudo-header order (Chrome `m,a,s,p`,
  Firefox `m,p,a,s`)
- header order and client hints (`sec-ch-ua*` in Chrome only)

The existing GeckoPhantom personas (`persona_v1`, Firefox 128) must keep
working. Binary size and RSS budgets are tight, and linking both JS engines
into one binary would cost both.

## Decision

- There are two families: `chrome` and `firefox`.
- **One binary per family**, chosen at build time with Cargo features:
  - default: `chrome` (V8)
  - `--no-default-features --features firefox` (SpiderMonkey)
- `lb-js::JsEngine` is the only engine-specific boundary. DOM, network, layout
  and CDP are shared code.
- Shared code picks per-family data from `persona.browser.family`:
  - IDL set (Chromium or Gecko), used by `lb-webidl-gen`
  - TLS profile (`profiles/tls/<family>_*.json`)
  - H2 settings and pseudo-header order
  - header order and client-hint behavior
- **The persona's family must match the binary.** At startup, and on
  `LightBrowser.setPersona`, a binary refuses any persona whose family differs
  from the engine compiled into it.
- Persona schema v2 adds `browser.family` and `client_hints`. A v1 persona is
  read as `family = firefox`, so existing Firefox 128 personas load unchanged.
- Per-family data lives in data files and generated code, not in `if family`
  branches scattered through the code.

## Alternatives considered

- **One binary with both engines.** Switchable at runtime, but the binary
  would carry two engines and blow the 40 MB budget, and the build would always
  pay for both V8 and `mozjs`. Idle RSS would also grow.
- **Chrome only.** Much simpler, but we could not test Firefox personas, and
  GeckoPhantom's existing profiles would have no home.
- **Firefox personas on the V8 binary** (fake the differences in JS). Engine
  quirks such as error text and stacks would still be V8, so the persona would
  be wrong at the engine level.
- **A runtime plugin system for engines** (dynamic libraries). More complexity
  and a larger attack surface, for little gain over a build-time feature.

## Consequences

- Each family can reach its binary size and RSS budget on its own.
- CI must build, test and lint both feature sets. A change to shared code can
  break only one family, so both must be tested.
- A persona/binary mismatch is an explicit startup error, not a silent
  inconsistent fingerprint.
- Adding a third family later (for example Safari) means a new engine behind
  `JsEngine`, a new IDL set, and new TLS/H2 data. The shared code should not
  need to change.
- The two families land at different milestones: Chrome by M4, Firefox in
  M4b. Until then the Firefox feature compiles but is incomplete.
- The Python SDK needs to pick the right binary for the persona it loads.

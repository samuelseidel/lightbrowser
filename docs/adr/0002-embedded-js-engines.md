# ADR 0002: Embed the JS engine instead of writing one

Status: Accepted (2026-11-02)

## Context

Detection scripts probe the JavaScript engine itself, not only the web APIs.
They look at error message text, stack trace format, `Function.prototype.toString`
output, `Math` precision, property enumeration order, regex behavior, `Intl`
output, and timing of common operations. A claimed Chrome must behave like V8
and a claimed Firefox must behave like SpiderMonkey. These quirks are almost
impossible to copy by hand.

We also need performance and spec coverage (ES2024+, modules, async, WeakRefs,
`Intl`, Temporal as it ships) that would take years to build.

Budgets matter too: idle RSS under 15 MB, isolate or realm creation under 5 ms,
and a stripped binary under 40 MB per family, most of which is the JS engine.

## Decision

Embed a production engine behind the `lb-js::JsEngine` trait:

- **Chrome family: V8**, through the `v8` crate (rusty_v8). We use a startup
  snapshot containing all generated bindings, pointer compression, a heap cap
  per isolate, and optional `--jitless`.
- **Firefox family: SpiderMonkey**, through Servo's `mozjs`, with a heap cap per
  realm.

Only one engine is compiled into a binary (see ADR 0006). We do not write our
own engine, and we do not ship a third engine.

## Alternatives considered

- **Write our own JS engine.** Years of work for an incomplete engine. Worse,
  it would not reproduce V8 or SpiderMonkey quirks, so it would be detectable
  by the very checks we exist to pass.
- **QuickJS.** Small (about 1 MB) and easy to embed, and good for RSS. But its
  error messages, stack format, `toString` output and `Math` results do not
  match Chrome or Firefox, it lacks `Intl` and full modern spec coverage, and
  it is much slower. Detection scripts would flag it quickly.
- **Boa** (Rust-native). Memory-safe and easy to build, but it is incomplete,
  slow, and has its own quirks that match no real browser. Not ready for
  production pages.
- **One engine for both families (V8 only).** Simpler, but a Firefox persona
  would show V8 error text and stack format. That breaks the Firefox persona.
- **JavaScriptCore or Hermes.** No browser family we imitate uses them, and
  Rust bindings are weaker.

## Consequences

- Engine-level fingerprints match the claimed browser by construction, with no
  patching of error strings.
- Binary size is dominated by the engine. The 40 MB budget needs watching,
  especially for SpiderMonkey.
- Builds are slow and the `mozjs` build pins a SpiderMonkey ESR version. Keeping
  each engine behind its own feature stops one from slowing work on the other.
- All engine-specific code stays behind `JsEngine`. The rest of the browser
  (DOM, net, layout, CDP) must not call V8 or SpiderMonkey APIs directly.
- The WebIDL bindings generator must emit glue for both engines.
- We depend on upstream releases of rusty_v8 and `mozjs` for security fixes.
  Upgrades are scheduled work.
- JS engines are single-threaded, so each worker thread owns its own instances
  (see ARCHITECTURE.md, threading model).

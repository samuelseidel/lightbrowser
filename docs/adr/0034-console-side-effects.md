# ADR 0034: Console calls run the page code Chrome runs, and no other

Status: Accepted (2026-11-07).

## Context

Our `console` methods turned each object argument into text with
`JSON.stringify`, or with `ToString` as a fallback, inside the page realm.
That ran the page's getters, `toJSON`, an object's own `toString` and
proxy traps on every call, with or without a CDP client. Chrome runs none
of them without DevTools, so `console.log({get x() { flag = 1 }})` told any
page it was not in Chrome. `console.log(new Proxy({}, …))` and
`console.log(Object(Symbol()))` also threw a `TypeError` into the page.
V8's own `count`, `time`, `group`, … ran no code at all, because no
inspector was installed. This is the console form of the `Runtime.enable`
side-effect probes that Patchright and rebrowser-patches work around.

Chrome does run some page code. Recorded in Chrome for Testing
153.0.8010.12 (`chrome-headless-shell --dump-dom`, no DevTools client) and
checked against V8's sources:

- **V8's `Formatter`** (`builtins-console.cc`) converts what a leading
  string's `%s` (`String`), `%d`/`%i` (`parseInt`) and `%f` (`parseFloat`)
  consume. This applies to `log`, `debug`, `info`, `warn`, `error`,
  `trace`, `group`, `groupCollapsed` and a failing `assert`. Throws
  propagate.
- **The inspector's `V8ValueStringBuilder`** builds the stored message
  text from every argument:
  - `Object.prototype.toString` for ordinary objects (a `Symbol.toStringTag`
    getter runs);
  - `ToString` for dates, functions, native errors and regexps;
  - array elements one by one (index getters run);
  - `[object Proxy]` for proxies.

  Throws are swallowed.
- **`firstArgToString`** converts the label of `count`, `countReset`,
  `time`, `timeLog`, `timeEnd`, `timeStamp`, `profile` and `profileEnd`.
  Throws propagate. `timeLog` reads its other arguments only while the
  timer exists.

## Decision

- `lb-js` `v8_engine/console.rs` does those steps exactly
  (`CallCtx::console_args`, `CallCtx::console_label`).
- The builder calls V8's own `Object.prototype.toString` and the wrappers'
  `valueOf`. These are taken from each realm before any page script runs,
  so replacing them in the page changes nothing.
- `lb-webapi` replaces all 22 console methods the inspector handles, with
  the same names, lengths (0) and property positions.
- The text we report (CDP `consoleAPICalled`, `console_messages`) is
  built afterwards and reads data properties only:
  - JSON of own enumerable data properties for plain objects and arrays,
    leaving accessors out and not calling `toJSON`;
  - the builder's text for everything else.

## Alternatives considered

- **Run no page code at all.** Then a probe that expects Chrome's
  conversions (a `Symbol.toStringTag` getter, `%s`) would see them
  missing.
- **Register a V8 inspector as the console delegate.** That brings V8's
  own builder, but adds a large component and its message storage just
  for this.

## Consequences

- Every case in `lb-page` `tests/console/` (75 hits, which calls throw)
  equals Chrome's recording, and our behaviour no longer changes when a
  CDP client attaches.
- `count`, `time`, `timeEnd`, `timeLog`, `group`, `groupEnd`, `clear` and
  `profile` still send no CDP message (only their side effects are
  modelled).
- `console.log()` with no arguments sends no message, as in Chrome.
- Six `v8::Global`s per realm.

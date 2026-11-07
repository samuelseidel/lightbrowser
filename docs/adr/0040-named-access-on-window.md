# ADR 0040: Named access on window, document, forms and collections

Status: Accepted (2026-11-07).

## Context

`window.someId`, a bare `someId`, `window.frameName`, `document.formName`
and `form.fieldName` did not work. `Object.getPrototypeOf(Window.prototype)`
was `EventTarget.prototype`. In Chrome it is the named properties object
(`[object WindowProperties]`). Most remaining WPT failures needed these
(ROADMAP, "Shadow DOM and custom elements"). Pages and detection scripts
reach them too: DOM clobbering checks, `window[iframeName]`, the
prototype chain of `window`.

Chrome 153 was recorded on loopback (headless shell 153.0.8010.12; the
cases are in `lb-page` `tests/named/`). Blink's bindings generator
(`bind_gen/interface.py`) and `LocalWindowProxy` explain what was
recorded.

**Window.** `WindowProperties` is a `[LegacyNoInterfaceObject]` interface
between `Window` and `EventTarget`:

- Its prototype object has no `constructor` (Blink deletes V8's). It has
  only `Symbol.toStringTag` "WindowProperties".
- The interface objects' chain skips it: `Object.getPrototypeOf(Window)
  === EventTarget`.
- Its `[[SetPrototypeOf]]` is immutable. The same holds for
  `Window.prototype`, `EventTarget.prototype` and the window itself
  (Blink calls `SetImmutableProto` on the prototype templates of a
  `[Global]` interface and of its ancestors).
- The named interceptor sits on its prototype template, flags
  `kOnlyInterceptStrings | kNonMasking`. Own properties of the window and
  members anywhere in the prototype chain win (`<p id=alert>` leaves
  `alert` alone). A `var` hides an element, a `let` does not.
- There is no query or enumerator (the getter is `[NotEnumerable]`).
  `Object.getOwnPropertyNames` of the object is `[]`. The descriptor is
  `{writable: true, enumerable: false, configurable: true}`.
- Every define and delete is refused. In strict code (and with
  `Object.defineProperty`) that is a `TypeError`, "Failed to set a named
  property 'x' on 'WindowProperties': Named property setter is not
  supported." or "Failed to delete … Named property deleter is not
  supported.". `window.x = 1` still makes an own property of the window.
- Index keys (`window[123]`, after the window's frames) go to the same
  getter as strings.
- The getter, `LocalDOMWindow::AnonymousNamedGetter`, returns:
  - the first child frame whose browsing context name is the key (its
    `WindowProxy`). The name comes from `<iframe name>` at creation or
    from `window.name`. Changing the attribute later does not rename the
    frame.
  - else the document tree's elements with that `id` (any namespace),
    or `img`/`form`/`embed`/`object` with that `name`. One element is
    returned itself. Several give one live `HTMLCollection` per name.
    Shadow trees, templates and detached nodes do not count.

**Document.** Chrome does not use an interceptor here.
`LocalWindowProxy::NamedItemAdded` puts an own native data property on
the main world's document wrapper when the first element registers a
name:

- `form`, `iframe`, `embed`, `object`, `img` register by `name`;
- `object` and `img` with a name also register by `id`.

`NamedItemRemoved` deletes the property when the last element leaves.
So:

- `Object.keys(document)` is `["location", …names in insertion order]`
  (`id` before `name` for one element). Names whose element is not
  exposed (an `object` with an `embed` inside) are still listed.
- The value follows `DocumentNameCollection`: exposed `embed`/`object`,
  an `iframe`'s content window, several elements in an `HTMLCollection`.
  With nothing matching now, the property reads the prototype chain's
  value.
- Assignments are ignored, even in strict code. `delete` removes the
  property for good (until the name is registered again).
- Documents without a window (`DOMParser`) and XML documents have none.

**Forms.** `HTMLFormElement` (`[LegacyOverrideBuiltIns,
LegacyUnenumerableNamedProperties]`, `[NotEnumerable]` getter) uses a
masking interceptor:

- `form.action` returns `<input name=action>`.
- Candidates: listed elements by `id` or `name` (the `form` attribute
  counts), else `img` elements. Several give a live `RadioNodeList`,
  shared with `elements.namedItem`.
- The past names map keeps a renamed element reachable while it stays in
  the form.
- Descriptors are `{writable: false, enumerable: false, configurable:
  true}`. Own property names are only the indices.
- Assigning a supported name is ignored, or throws in strict code.
  `defineProperty` throws. `delete` is not intercepted.

**Collections.** `HTMLCollection`, `HTMLFormControlsCollection`,
`HTMLOptionsCollection` and `HTMLAllCollection` use non-masking
interceptors:

- Prototype members win.
- Names are read-only, not enumerable, and listed by
  `getOwnPropertyNames` after the indices (`id`, then `name`, each once).
- Assign, define and delete of a supported name are refused as for
  `WindowProperties`.
- `document.all` matches names only on the "all named" elements and
  returns an `HTMLCollection` for several. `form.elements` returns a
  `RadioNodeList` for several.

## Decision

- `lb-js`: `InterfaceSpec::named_legacy` (`LegacyNamed`). It installs
  Blink's named callbacks:
  - getter, setter, definer, descriptor, and query/enumerator unless
    `not_enumerable`;
  - deleter unless `not_enumerable`, or for the named properties object;
  - `NON_MASKING` unless `override_builtins`;
  - for the named properties object, on the prototype template with
    index forwarders.

  The error texts are Blink's. Whether to throw is the call's
  `ShouldThrowOnError`. rusty_v8 binds that for `PropertyCallbackInfo<Value>`,
  which V8 always answers `false` for, so `should_throw` reads the mode
  slot after the holder's handle and, for `kInferShouldThrowMode` (store
  ICs), calls V8's exported `internal::ShouldThrowOnError(isolate)`.
  `InterfaceSpec::no_interface_object`
  deletes `constructor` per realm and links children's interface objects
  past it. `InterfaceSpec::immutable_proto_object` sets the prototype
  template's immutable proto. `RealmOps::register_native_getter`,
  `define_native_property` (V8's `SetNativeDataProperty` with a no-op
  setter, as Blink's, since without a setter V8 turns the property into a
  plain data property on assignment; a `None` from the getter reads the
  prototype chain) and `delete_property` are new.
  Interceptor results that are host objects are wrapped in the holder's
  realm, whose window owns them. The snapshot digest, callback replay
  and external references include all of it (blob magic `LBSNAP04`).
- `lb-webapi` `install.rs` defines `WindowProperties` (parent
  `EventTarget`) before `Window` and makes the global's chain immutable.
  It is defined eagerly with the window, so realm creation does no new
  per-interface work beyond one `constructor` delete.
- `lb-webapi` `rt/named.rs` has:
  - the window getter (frames through `frames::child_by_name`);
  - the document named items, kept in step incrementally from each
    mutation batch (per element contributions and per name counts, so a
    batch costs its mutated subtrees, not the document);
  - the form getter with its past names map.

  Lookups use one index of the document tree by `id`/`name`, rebuilt
  when the document generation changed.
- `rt/coll.rs`:
  - `Filter::WindowNamed`, `DocumentNamed`, `FormNamed`, `AllNamed`;
  - `RadioNodeList` (`value`);
  - collection enumerators; indices of `NodeList`, `HTMLCollection`,
    `HTMLAllCollection`, `HTMLFormControlsCollection` and
    `RadioNodeList` enumerate read-only (`IndexedLength`);
  - `form.elements` follows form owners (the `form` attribute), and
    `form[i]` is `form.elements[i]`.
- `rt/frames.rs`: `child_by_name`, `child_global`; a cross-origin
  frame's restricted stand-in for its parent is made when the frame
  commits, so `window[name]` and `contentWindow` are the same object.

## Consequences

- The surface matches Chrome's for these objects, including
  `getOwnPropertyNames`, descriptors, strict-mode errors and immutable
  prototypes.
- A global miss (`typeof notThere`) now asks the named getter. It looks
  up a hash map, rebuilt once after each document change. A loop that
  both mutates the document and reads missing globals rebuilds it each
  time.
- Open:
  - A cross-origin frame reached by name from a realm other than its
    parent's (a sibling) gets the raw global, which V8 refuses, until
    that realm reads its `contentWindow`.
  - `document.all` is neither callable (`document.all('x')`) nor
    undetectable (`typeof document.all` should be `"undefined"`):
    rusty_v8 binds neither a legacy caller for it nor
    `MarkAsUndetectable`.
  - `HTMLOptionsCollection`'s indexed setter is not implemented, so its
    indices do not enumerate.
  - Document named items of a document replaced by `document.open()` are
    not reset.
  - `<frame>` and `<object>` browsing contexts do not exist, so they are
    not found by name.
  - `NamedNodeMap`, `Storage`, `DOMStringMap` and `CSSStyleDeclaration`
    keep their simpler interceptors.

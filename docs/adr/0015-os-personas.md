# ADR 0015: OS personas: one OS axis, read through accessors

Status: Accepted (2026-11-03)

## Context

Until M7 the Chrome personas were Windows 10 and macOS 15, and the OS showed
up in scattered places: `os.name` string checks in several crates, a
Linux-flavoured interface set, fixed generic fonts, one wheel model. The
bench's OS rules (WP-A, ADR 0007) fire on each of these. Real Chrome's
answers depend on the OS, its release, the Linux distro and display server,
and the hardware (Apple chip or Intel Mac, GPU backend).

Ground truth is uneven: Linux was captured from the local Chrome for
Testing 153 (`tests/fixtures/os/`); Windows and macOS values come from the
M7 surface reference and are not captured. Some captured Linux facts
contradicted the plan (`platformVersion` is `""`, `deviceMemory` is not
capped at 8, one wheel notch is 120 px).

## Decision

- **One OS axis in `lb-persona`** (`os.rs`): `OsKind` (Windows, macOS,
  Linux), `DisplayServer` (X11, Wayland), `Distro` (Ubuntu, Fedora), from
  `os.name` and the new optional `os.kernel`, `os.distro`,
  `os.display_server`.
- **Accessors with OS defaults.** Layers read OS-dependent values through
  `PersonaProfile` methods, never through `os.name` checks of their own.
  Each returns the persona's value when the JSON has one, else the default
  for its OS and distro: `os_kind`, `display_server`, `linux_distro`,
  `windows_major`, `macos_version`, `apple_chip`, `font_defaults`
  (standard, serif, sans-serif, monospace, cursive, fantasy, system-ui,
  emoji, system font keyword family and size), `font_aliases` (fontconfig
  metric aliases on Linux, Blink's alternate names everywhere),
  `window_position`, `scrollbar_width`, `wheel` (px per notch,
  `wheelDelta` ratio), `audio_defaults`, `webgpu`, `codecs`, `os_apis`
  (`share`, `bluetooth`), `high_entropy_hints`.
- **Schema v2 stays compatible.** New fields are optional: `os.kernel`,
  `os.distro`, `os.display_server`, `screen.window_x/window_y/maximized`,
  `fonts.defaults`, `audio.output_latency`, `behavior.wheel`, `webgpu`
  (`null` = no adapter), `media`, `apis`, `client_hints.form_factors`,
  `provenance`. Existing personas load unchanged.
- **Rules R17-R26** (Chrome personas only): platform version per OS and
  build (Linux `""`), architecture and GPU, GPU backend and limits and the
  Mesa kernel, screen geometry per OS and display server, fonts per OS,
  release and distro (and installed defaults), voices, audio (warning),
  preferences, hardware (deviceMemory powers of two, Apple chip thread
  counts), macOS versions per Mac. R08 accepts 11, 14, 18 and 22 threads;
  R12's architecture check is now a violation.
- **Catalog and generator.** `profiles/catalog/` holds the OS releases,
  CPUs and Apple chips, GPUs with WebGL per ANGLE backend, screens, fonts,
  voices and audio. `lb_persona::generate` samples consistent personas from
  it with SFC32 and refuses any that break a rule.
- **Provenance is recorded.** Personas and catalog files say which values
  were captured and which come from the reference.
- **No TCP/IP layer.** TTL and TCP options always come from the host OS;
  personas have no field for them.
- **Rendering per OS (WP-C).** `MediaEnv` carries the persona's
  `scrollbar_width` and `font_defaults`; nothing in lb-css, lb-layout or
  lb-paint checks the OS for these any more.
  - Scrollbars take space as in Chrome: `overflow: scroll` always,
    `overflow: auto` and the viewport only when the content overflows
    (the viewport also when the root or body says `scroll`; never for
    `hidden`). Layout runs a second taffy pass with the gutters reserved
    when a first pass finds overflow. Textareas always include the
    scrollbar in their width (Blink).
  - Font matching: a family is used when the persona has it, else through
    `font_aliases` (Arial -> Liberation Sans on Linux, Helvetica -> Arial
    elsewhere); generic keywords name the `font_defaults` families; `ui-*`
    keywords are generic only on macOS; an unmatched list falls back to the
    standard font. The `font:` system keywords (`caption`, `menu`, ...)
    use the persona's system font; `-webkit-small-control` the UA's
    control font.
  - Emoji advance by the OS emoji font (Noto Color Emoji 136/109 em,
    captured; Segoe UI Emoji 1.3 and Apple Color Emoji 1.25, not
    captured) and grow `line-height: normal` to that font's metrics. Canvas
    and screenshots draw them in colour with flat stand-in shapes (a yellow
    face for smileys, a coloured disc otherwise): we bundle no emoji
    images, so emoji pixels differ from Chrome's.
  - A persona font with no table for its OS borrows another OS's table, so
    an inconsistent persona (Segoe UI on Linux) is detectable, as it would
    be on a real machine.

- **Web APIs and input per OS (WP-D).**
  - The interface set is the Linux capture plus an overlay
    (`tests/fixtures/chrome_153_os_overlay.json`) of what Chrome has on
    Windows and macOS only: `navigator.share`/`canShare` (Web Share),
    `navigator.bluetooth` and the `Bluetooth*` interfaces (Web Bluetooth).
    `lb-webidl-gen` puts them in Chrome's order and lists them as
    `OS_GATED`; the snapshot keeps them (no persona data). Creating a realm
    deletes the members of each gate the persona's `os_apis()` turns off.
    Deleting keeps the order of everything else, so a Linux realm equals
    the Linux capture.
  - Media type answers come from Chrome's codec tables (the Linux capture
    is the unit test) and `codecs()`; `navigator.gpu` from `webgpu()`;
    `AudioContext` from `audio_defaults()`; `screenX/Y` from
    `window_position()`; trusted `wheelDelta` from `wheel()`, which also
    sets the detent `LightBrowser.human*` scrolls in. WebGL limits default
    per ANGLE backend. Select-all uses the OS's shortcut modifier.

- **Client hints per origin (WP-E).**
  - `Persona` (the network summary) carries `high_entropy` from
    `high_entropy_hints()`, so the headers and `getHighEntropyValues`
    give the same values.
  - `lb_net::client_hints` keeps each origin's `Accept-CH` set in memory,
    one store per browser context (shared like the cookie jar). Only
    potentially trustworthy origins count (https, loopback http). A
    top-level navigation response, redirects included, replaces the
    origin's set; `Accept-CH` from frames and subresources is ignored.
  - A top-level navigation sends its own origin's hints. A frame or
    subresource request sends the top-level origin's hints only when the
    request, the frame that made it and the top-level document are all
    same-origin (`RequestMeta::top_frame`; no `Permissions-Policy`
    delegation). Preflights send none.
  - `Critical-CH`: when the final response names a hint its `Accept-CH`
    asked for and the request lacked, the navigation is sent again once.
    The first response is dropped; its cookies are kept.
  - Header order is data in `profiles/headers/chrome_153.json`, captured
    from the local Chrome for Testing 153 with
    `tools/header_capture/accept_ch.js` (ADR 0011).
  - Only the `Sec-CH-UA-*` hints are sent. Device and preference hints
    (`DPR`, `Viewport-Width`, `Sec-CH-Prefers-Color-Scheme`, ...) are not.

## Alternatives considered

- **Per-OS persona types or crates.** More code, and every layer would
  still need the same defaults. One profile plus accessors keeps the data in
  one place.
- **Hard-coded defaults in each layer.** That is what M7 replaces: it let
  layers disagree (Linux monospace was Liberation Mono in layout while
  Chrome uses DejaVu Sans Mono).
- **Required schema fields.** Would break the existing personas and v1
  files for no gain; defaults cover the common case.
- **Client hints in lb-page instead of lb-net.** The restart and the
  redirect hops happen inside `lb_net::fetch`, and the stand-alone `fetch`
  command needs the same behaviour, so the store and the decision live in
  lb-net; lb-page only says which document is top-level.
- **One generated table per OS, or adding gated members at realm setup.**
  Three tables would triple the generated data and the snapshot; adding
  members per realm cannot put them in Chrome's place in the prototype.
  One table with every member and deletion per realm keeps one snapshot
  and the order.

## Consequences

- WP-C (layout, fonts, scrollbars), WP-D (web APIs, input) and WP-E
  (client hints) read these accessors.
- A site that asks for hints in `Accept-CH` sees them from the second
  request on (from the first with `Critical-CH`), as with Chrome. Hint sets
  we have not captured on subresources use an approximate order (Blink's
  hash-table order is not modelled), see ADR 0011.
- Realm setup runs one short deletion script for personas whose OS lacks a
  gated API (Linux): realm creation stays at about 3.2 ms first, 2.5 ms
  next.
- Pages taller than the viewport (and overflowing `overflow: auto` boxes)
  cost a second taffy pass when scrollbars take space (Windows, Linux).
- Windows and macOS defaults are medium confidence until internal captures
  exist. Changing one is a data change plus a test update.
- `fonts.available` must have a metric table or a `derived.json` entry per
  font (`lb-persona` and `lb-layout` tests check this); the Linux and
  Windows 11 fonts got derived entries.
- A passive TCP/IP fingerprint can still contradict a Windows or macOS
  persona (Risks in `docs/ROADMAP.md`).

## Verification (2026-11-03)

Re-run at the M7 close-out: all 8 reference personas pass `fingerprint-report`
(0 mismatch), T-010 `fp<=30` (fp 0) and `--input human` `beh<=30` (beh 0)
with every OS rule at full weight; each adversarial persona fails with its
one rule; stock Chrome 153 fires no OS rule. Windows and macOS values are
not captured (see Consequences). Details: "M7 status: verification and
close-out" in `docs/ROADMAP.md`. No TCP/IP-level code exists by decision.

### Adversarial personas the bench sees

Of the M7 adversarial personas, `chrome153_wayland_avail_top` (R20) is seen
by the bench rule `window_work_area` (Linux window above or left of a
non-zero work-area offset; the page cannot read the display server, but it
reads `screenX/Y` and `availTop/Left`). `chrome153_linux_mesa_kernel_mismatch`
(R19) is validate-only: the page sees the Mesa renderer string, and nothing
in the UA, client hints or any API exposes the kernel it would be compared
with. Stock Chrome 153 (headed under `xvfb-run`, `--headless=new`) has no
work-area offset and fires neither rule.

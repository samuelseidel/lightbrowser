# ADR 0039: Time zone, locale and geolocation follow the proxy

Status: Accepted (2026-11-07). Revises ADR 0009 (Emulation checked
against the persona) for these overrides.

## Context

A persona behind a proxy (ADR 0027) must be able to show the exit's
location: its time zone, and often its language and position. ADR 0009
made `lb-cdp` reject `Emulation.setTimezoneOverride` and
`setLocaleOverride` values that differ from the persona, an
`acceptLanguage` that does not start with the persona's language, and
every `setGeolocationOverride` position. Chrome takes any valid value,
so Playwright's `newContext({timezoneId, locale, geolocation})`, the
natural way to match a proxy, failed, and the rejection is itself a
difference from Chrome. Operators without CDP had to edit persona files.

## Decision

**Accept what Chrome accepts, with Chrome's errors.** Recorded from
Chrome 153 (`lb-cdp` `tests/emulation/chrome.json`, raw CDP with two
sessions; `lb-page` `tests/locale/chrome.json`, Playwright):

- `setTimezoneOverride`: `timezoneId` is mandatory (`-32602` bindings
  error). The first override must be a zone ICU knows (`-32602` "Invalid
  timezone id"; ids are case-sensitive, `utc` and `Local` are refused);
  changing an override in effect ignores an unknown id silently. `""`
  clears. Offset ids (`+05:00`) are ICU's `GMT+05:00`, as in Chrome
  (`Intl` reports `+05:00`); aliases resolve as ICU canonicalizes them
  (`US/Pacific` is `America/Los_Angeles`, `EST` `America/Panama`). A second session of the page asking for another zone gets
  `-32000` "Timezone override is already in effect"; the same zone, or
  a clear, is a no-op. The override ends when its session detaches.
- `setLocaleOverride`: `locale` optional (absent or `""` clears). Blink's
  `LocaleController` check: ICU must find a language subtag (at most 11
  characters before `-`, `_`, `@`, `.`; an `i-` prefix belongs to it, an
  `x-` prefix leaves none; `root` is refused), else `-32000` "Invalid
  locale name". Non-ASCII characters reach ICU as `?` (`String::Ascii`).
  While another session of the page holds an override, every call,
  clears included, gets "Another locale override is already in effect".
  It sets ICU's default locale only: `Intl`, `toLocale*String`; not
  `navigator.language`.
- `acceptLanguage` (`Emulation`/`Network.setUserAgentOverride`) is no
  longer checked. It sets the target's `navigator.languages` (Blink's
  parse: split at commas, trimmed, `_` at index 2 becomes `-`, q-values
  kept; a new array on every read) and the `Accept-Language` of its
  requests (Chrome's q-values: `de-DE, en ,fr_FR` gives
  `de-DE, en ;q=0.9,fr_FR;q=0.8`). Per target, as in Chrome: a page's
  override does not reach its workers; Playwright 1.65 sends its own to
  dedicated and service worker targets, which LightBrowser applies.
- `setGeolocationOverride` / `clearGeolocationOverride`: latitude,
  longitude and accuracy all given make a position (`-32000` "Invalid
  geolocation" outside -90..90, -180..180, or a negative accuracy;
  a negative `altitudeAccuracy` or `speed`, or a `heading` outside
  0..360, reads `null`); otherwise `POSITION_UNAVAILABLE`.
  `navigator.geolocation` (new, `lb-webapi` `rt/emulation.rs`) answers
  with it when the persona grants `geolocation`; with `prompt` it
  reports `PERMISSION_DENIED` "User denied Geolocation" (headless: nobody
  answers the prompt); `timeout: 0` gives `TIMEOUT` "Timeout expired";
  without an override requests wait for their timeout. Each new override
  first fails running requests with `POSITION_UNAVAILABLE` (Chrome resets
  its provider: a waiting `getCurrentPosition` ends there), then gives
  watchers the new answer; a clear changes nothing for them. Argument
  errors, watch ids and `toJSON` are Blink's.
- The other `Emulation` checks of ADR 0009 stay (screen, UA string,
  platform, client hints, media, touch).

**Scope: the browser context.** ICU's default zone and locale are
process-wide (one renderer process per override in Chrome). The server
(`lb-cdp` `browser/overrides.rs`) keeps Chrome's per-session ownership;
the host applies a page's time zone and locale to its whole browser
context: `BrowserContext::effective_profile` is the persona with the
zone and locale replaced, so new documents, frames, dedicated, shared
and service workers and `fingerprint-report` use it, and every isolate
already running one of the context's realms is told (the page threads,
each worker through its inbox): V8's date cache is reset
(`DateTimeConfigurationChangeNotification`) and its cached default
locale and ICU objects dropped (`LocaleConfigurationChangeNotification`,
called through its C++ symbol: rusty_v8 does not bind it), so `Date` and
`Intl` change at once, as recorded. Each isolate reads the values when
its job or inbox task runs, under the context's lock
(`BrowserContext::with_intl`), so a late task never brings an old value
back. A new isolate is told too: V8 caches the host's default locale
while it sets up its first context, so before this a persona whose
language was not the host's never reached `Intl` (`toString` used it,
`Intl.DateTimeFormat().resolvedOptions().locale` did not). Pages of a context may all set the
same value (Playwright does, per page); the context's override ends when
the last page holding it detaches or closes.

**Operator path without CDP.** `lightbrowser serve|fetch|
fingerprint-report|validate --timezone IANA --locale TAG [--languages L]`
and the Python SDK's `Browser(..., timezone=, locale=, languages=)`
apply `lb_persona::LocaleOverride` to the persona before it is used
(`PersonaProfile::with_locale_override`): the zone (ICU must know it in
the chrome build), the ICU locale, `navigator.languages` (default
`[TAG, its language]`, the reference personas' shape) and
`Accept-Language` from Chrome's q-values. The consistency rules run on
the result; R09 (locale against zone) stays a warning, so a VPN user's
`en-US` with `Europe/*` passes.

## Alternatives considered

- **Keep rejecting, make operators edit personas.** One persona file per
  exit location, and stock Playwright options keep failing.
- **Per-page zones.** V8 reads ICU's process default; isolates are shared
  by the pages of a worker thread. Not possible without one isolate per
  page and per-isolate ICU state.
- **Accept but ignore.** Pages would see the persona's zone while the
  client believes the proxy's is in effect: the worst of both.

## Consequences

- Playwright `newContext({timezoneId, locale, geolocation})` works and the
  page sees what Chrome shows. `detect-bench` rules that compare surfaces
  with each other (`timezone_mismatch`, `locale_mismatch`, `languages`)
  stay quiet because every surface moves together; `fingerprint-report`
  compares with the effective persona.
- Differences from Chrome: a page of the same context that set no
  override sees the context's (Chrome: the host's zone, other renderer),
  and a page asking for another zone or locale than one another page of
  its context holds gets Chrome's "already in effect" error (Chrome: its
  own renderer would take it); contexts with different zones in one
  process share ICU's process default for `Intl` (as personas with
  different zones already did).
- `navigator.geolocation` needs the persona's `geolocation: granted`;
  `Browser.grantPermissions` is not implemented, so Playwright's
  `permissions: ['geolocation']` does not grant it yet. `maximumAge` and
  watcher timeouts after the first answer are not modelled. Worker
  requests do not carry the page's `acceptLanguage` (only their own
  target's).

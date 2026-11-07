# sdk/python: the `lightbrowser` Python package

A thin wrapper that starts the internal `lightbrowser` binary with a persona
and returns **stock Playwright objects** connected over CDP. It adds
`page.human_*` and `page.verify_fingerprint()`, backed by the
`LightBrowser.*` CDP methods. Control is CDP only; nothing else is needed on
the browser side, and stock Playwright keeps working without this package.

Internal use only. Point it at our own detection bench (`tests/detect-bench`,
ADR 0007) and our own test servers, on loopback. The package contains no
binaries, writes nothing to disk except temp dirs it removes on close, and
has no upload configuration.

## Install and requirements

```bash
cargo build -p lightbrowser                  # target/debug/lightbrowser (chrome family)
pip install -e sdk/python                    # or: PYTHONPATH=sdk/python
```

- Python 3.10+, `playwright` for Python. **Use Playwright >= 1.63.** Older
  versions (1.52 checked) define `__pwInitScripts` and
  `__playwright_builtins__` in every page's main world. The bench rule
  `automation_keys` (weight 90) fires on that whatever the browser is, and
  `verify_fingerprint()` reports `noTells.mainWorldGlobals`. The SDK warns
  once at launch when it finds an older Playwright.
- The binary is found from `executable_path=`, then `$LIGHTBROWSER_BIN`, then
  `target/release/lightbrowser` and `target/debug/lightbrowser` (current
  directory and its parents, then the checkout holding the SDK), then `$PATH`.
  Both engine families build to `target/release/lightbrowser`, so searched
  binaries are checked with `--version` and one built for another family is
  skipped.

## Use

```python
from lightbrowser import Browser, Persona

persona = Persona.load("profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json")
with Browser(persona=persona) as browser:            # starts `lightbrowser serve --port 0`
    page = browser.new_page()                        # stock Playwright Page, own context
    page.goto("http://127.0.0.1:8765/shop")          # your own local server
    page.human_scroll(900)                           # wheel events with acceleration
    x, y = page.human_click("#add-3")                # returns the point clicked
    page.human_type("#q", "jacket")                  # per-key timing, rare typo + Backspace
    report = page.verify_fingerprint()               # FingerprintReport
    assert report.ok, report
```

Async: `from lightbrowser.async_api import AsyncBrowser`, with `await` on
`launch/new_page/human_*/verify_fingerprint` (`async with AsyncBrowser(...)`).

### Browser

`Browser(persona=None, *, executable_path=None, port=0, insecure=False,
profiles_dir=None, workers=None, extra_args=(), startup_timeout=60,
validate=True, playwright=None)`

- `persona`: a `Persona`, a path, a dict (written to a private temp dir and
  removed on close), or `None` for the binary's default Chrome persona. With
  `validate=True` it first goes through `lightbrowser validate`, so a bad
  persona raises `PersonaValidationError` with the rule text.
- The server always binds 127.0.0.1; `port=0` lets the OS choose and the SDK
  reads the port from the `DevTools listening on ws://...` line on stderr.
- `new_context(**kw)` and `new_page(**kw)` return stock Playwright objects.
  Contexts default to `no_viewport=True`: the browser keeps the window size
  its persona defines (a fixed viewport would contradict it, ADR 0009).
  Passing `viewport=` makes `new_page()` fail with `Browser window not found`.
  Other `Browser` attributes (`contexts`, `version`, ...) come from the stock
  Playwright browser; `browser.browser` is that object, `browser.ws_endpoint`
  the CDP URL for any other client.
- Only one sync Playwright can run per thread. To open two browsers in one
  process pass `playwright=first.playwright`.
- `close()` (or leaving the `with` block) closes contexts, disconnects,
  terminates the process (SIGKILL after 5 s) and removes temp files.

`lightbrowser.attach(page)` adds the helpers to a page you got some other way
(for example from your own `chromium.connect_over_cdp(browser.ws_endpoint)`).
Contexts made by `Browser.new_context` attach them to every new page.

### Page helpers

| Call | CDP method | Notes |
|---|---|---|
| `page.human_move(x, y)` | `LightBrowser.humanMove` | curved path from the current pointer position |
| `page.human_click(selector)` / `human_click(x=, y=)` / `human_click(x, y)` | `humanClick` | returns the clicked `(x, y)`; `button=`, `click_count=`; scrolls into view first; the point is inside the element, not its centre; coordinates also scatter a little |
| `page.human_type(selector, text)` | `humanType` | `selector=None` types into the focused element |
| `page.human_scroll(dy)` / `human_scroll(selector=...)` | `humanScroll` | `dx=`, and `x=, y=` for the wheel position |
| `page.verify_fingerprint()` | `LightBrowser.fingerprintReport` | live JS surface vs the persona |

Selectors are plain CSS resolved inside the browser in Rust. No script runs
in the page's main world, so Playwright-only engines (`text=`, `xpath=`,
`>>`) do not work here. Calls return after the last event. Timing, pointer
position and the event stream come from the persona's `behavior` block (the
same seed gives the same stream). Failures raise `HumanInputError` with the
browser's text, for example `No node found for selector: #x`.

`FingerprintReport`: `ok`, `persona`, `family`, `summary` (counts per status),
`checks` (`Check(name, status, expected, actual)`), `mismatches`,
`unimplemented`, `surface`. `print(report)` lists the problems.

## Scenarios

`python -m lightbrowser.scenarios --bench http://127.0.0.1:PORT [--scenario T-004,T-005|all]
[--persona FILE] [--binary PATH] [--wait 3.5] [--seed N] [--json] [--list]`

The bench must already run (`python3 tests/detect-bench/server.py --port PORT
--results-dir "$(mktemp -d)"`). `--bench` is refused unless its host is
127.0.0.1, `::1` or `localhost` (exit 2), and a scenario errors if its browser
requests anything off loopback. Exit code 1 on a failed or errored scenario;
a missing stock Chromium skips T-001 and T-003. Every LightBrowser scenario
starts a fresh `serve`.

The flows are the ones in `tests/detect-bench/run.js` (shop, scroll, add to
cart, type a search, submit). "fp" is the score of non-behaviour rules, "beh"
of behaviour rules only.

| Id | Name | What runs | Passes when | GeckoPhantom scenario |
|---|---|---|---|---|
| T-001 | baseline-stock-chromium | Playwright's headless Chromium, instant flow | score >= 90 | T-001 "stock browser must look human". The bench is a bot catcher, and stock headless Chromium is a bot there, so this is now the control that proves the bench works. Equals bench T-001. |
| T-002 | naive-playwright-input | LightBrowser + persona, Playwright's own click/fill/wheel | fp <= 30 and beh >= 70 | T-002 "Playwright must be caught". The fingerprint is clean; naive input is what gives it away. |
| T-003 | scripted-slow-stock | Stock Chromium, slowed script with stepped mouse moves | score >= 70 and fp >= 70 | T-003 "no evasion is detectable". Equals bench T-003. |
| T-004 | full-persona-human-input | LightBrowser + persona + `page.human_*` | fp <= 30 and beh <= 30 | T-004 "full evasion must pass". Equals bench T-010 with `--input human`. |
| T-005 | consistency-violation | A persona whose platform contradicts its UA (rule R01) | `validate` exits 1 naming R01 and `serve` refuses to start | T-005 "bad profile must be flagged"; here the browser's own consistency rules do the flagging, before any page loads. |
| T-006 | behaviour-only | LightBrowser + persona, two pages, no input | fp <= 30 and beh >= 30 | T-006 "perfect fingerprint, no behaviour is flagged" (`no_interaction`). |
| T-007 | network-only | LightBrowser + persona, instant flow | server category <= 10 | T-007 "TLS/HTTP2 alone". The bench speaks HTTP/1.1, so it checks the server-side record (headers, order, cookies, favicon). JA3/JA4/Akamai H2 stay with `cargo test -p lb-net --test fingerprint`. |

Numbers on this machine (Playwright 1.63, debug binary, Chrome 153 Windows
persona): T-002 89.8 (fp 0), T-004 0 (fp 0, beh 0), T-006 30 (fp 0), T-007
server 0, T-001 and T-003 100. T-004 passes for the Windows persona; the macOS
reference persona fails the bench on `beh_key_dwell` for some seeds (see
`docs/ROADMAP.md`, "M5 status: human input"), so `--persona` matters.

## Tests

```bash
pip install pytest
cd sdk/python
python3 -m pytest tests -m "not integration"      # unit: no binary, about a second
python3 -m pytest tests -m integration            # launches target/debug/lightbrowser
python3 -m pytest tests                           # everything
```

Unit tests cover argument building, `DevTools listening` parsing, binary
lookup, the process wrapper (with fake shell-script binaries), the loopback
guard, parameter checks, scoring and the `--expect` grammar. Integration
tests start the binary, serve a small page from the test process, and check
`human_click/type/scroll/move` against trusted DOM events, errors,
`verify_fingerprint`, the async API, persona validation and the scenarios
(T-004 is marked `slow`). Scenario and bench tests are skipped under
Playwright < 1.63 because of the leak above; the human-input tests do not
depend on it. If a binary answers `LightBrowser.human*` with "not
implemented", those tests xfail instead of failing.

## Not done

- No `Locator`/`ElementHandle` wrappers: `human_*` takes CSS selectors only.
- Firefox personas: the SDK launches them, but the firefox build's `serve`
  still runs the mock page host.
- Hover dwell, drag and touch (not in the CDP contract).

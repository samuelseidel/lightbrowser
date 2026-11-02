# sdk/python

Placeholder. Milestone: **M5 Behavior + SDK**.

A thin Python wrapper over Playwright's `connect_over_cdp`:

- launches the `lightbrowser` binary with a persona and waits for the CDP endpoint
- returns a normal Playwright browser/context/page
- adds `human_click`, `human_type` and similar helpers, backed by the `LightBrowser.*` CDP domain and the `lb-behavior` crate

No custom protocol: stock Playwright must keep working without this package.

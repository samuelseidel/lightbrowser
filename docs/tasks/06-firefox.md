# Implement Firefox M4b

Start with a SpiderMonkey spike behind `JsEngine` covering evaluation, microtasks, host bindings and teardown. Verify the Firefox-only build in CI.

Then integrate Gecko bindings and the real page host. Add a Playwright loopback smoke test before expanding platform surfaces. Track the network gaps separately against [ADR 0003](../adr/0003-boringssl-h2-fork.md).

Done when Firefox runs scripts in real CDP pages and passes its family-specific compatibility and local network checks.

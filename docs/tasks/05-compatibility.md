# Close compatibility gaps affecting internal workloads

Use task 01 results and local workloads to rank failures. Start with child-frame media and lifecycle behavior, MediaQueryList change events and WindowProxy identity.

CDP console messages now resolve their execution context from the originating realm. The Playwright CDP suite includes an isolated-world regression test for this change.

For each selected gap, reproduce it against local stock Chromium, implement the smallest fix and add a regression test. Record remaining WPT failures and keep existing Playwright checks green.

Done when the selected internal workflows pass and the roadmap records remaining gaps.

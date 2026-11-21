# Apply TCP profiles to CDP contexts

Depends on task 01. See [T10 and T13](../tcp-handshake/REMAINING-STEPS.md).

1. Trace profile selection through CLI, context creation, persona updates and connection pooling.
2. Define behavior for direct connections, proxies, unsupported platforms and missing shim. Preserve the existing fail-closed behavior when shaping is required.
3. Apply context options through `serve`, CDP and the Python launcher. Cover context isolation and persona changes with tests.
4. On Linux, verify the emitted SYN using the existing local capture tests and document the behavior.

Done when a CDP context uses its selected TCP profile and unsupported combinations have explicit behavior.

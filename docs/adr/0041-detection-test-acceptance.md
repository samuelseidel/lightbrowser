# ADR 0041 — Internal detection tests define acceptance

Status — Accepted, 2026-11-19, user decision

## Context

The user clarified that the goal is a browser able to pass detection tests.
Outperforming Lightpanda is optional. Earlier documents treated six
comparative performance measurements as acceptance requirements.

## Decision

Prioritize coherent personas, network and device fingerprint correctness,
and realistic behavior against our own detection engine and test servers.
Keep the documented detection thresholds and regression checks. Record
remaining failures against the candidate revision and test environment.

Lightpanda comparisons remain informational and do not fail the benchmark
command. Retain the absolute resource budgets and page verification checks.

## Consequences

Historical benchmark results remain available. Work is prioritized by its
effect on internal detection outcomes and browser compatibility.

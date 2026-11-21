# ADR 0042: Preserve human input gaps under load

Status: Accepted (2026-11-20)

## Context

Scheduling every input event from one start time lets late events catch up
in a burst. Under host contention, the real-clock human-input test observed
a 9.3 ms press hold despite a generated hold of at least 30 ms. This changes
the behavior visible to our detection engine.

## Decision

For real-clock replay, wait each planned inter-event gap after the previous
actual dispatch completes. Processing and scheduling delays may extend the
stream; subsequent events preserve their planned spacing. Virtual-clock
replay retains its deterministic offsets.

## Consequences

A busy host can lengthen input playback. Press holds and key rhythm retain
the spacing generated from the persona. The existing real-clock and virtual
clock human-input tests cover those paths.

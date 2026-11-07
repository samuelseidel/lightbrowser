# ADR 0043: Detector rules, wave 2

Status: Accepted (2026-11-07)

## Context

ADR 0007 made `tests/detect-bench` our own detection engine. Its rules judge
one session at a time from probes, the request shape and the input trace.
Seven things it could not see: requests that contradict themselves or their
page, agents that declare themselves, one device image shared by many
sessions, a stored mouse path played again, clicks at a fixed offset, a
persona patched into the page but not into workers' text measurement, a font
list that claims more than any install, and WebRTC behaviour. All of them are
defender-side checks of what a client sends or does on our own loopback
server. None of them is a way to avoid detection, and none is added to
LightBrowser.

## Decision

Add seven items to the bench, each with a probe or server signal, a rule in
`scoring.py`, tests on synthetic sessions and on real-Chrome-shaped sessions,
and a row in the README. Ground truth for "must not fire": the `os_*_chrome`
and `T-0xx` fixtures and three new sessions recorded from stock Chrome 153
(headless=new over CDP, rtx-dev, loopback): `chrome153_browse.json`,
`chrome153_t010.json`, `chrome153_referer_case.json`.

1. `referer_context_mismatch` (server, 35). `server.py` keeps the values of
   `Referer`, `Origin` and `Sec-Fetch-Site/Mode/Dest/User`, the request's
   `Host` and its scheme, for documents and for every subresource. The rule
   fires on `Sec-Fetch-Site: none` on a subresource, a same-origin
   subresource whose Referer is not a page of the session (or is missing), a
   Referer naming another origin on a same-origin request, and a `none`
   document with a Referer. The expectations are what Chrome 153 sent when
   recorded (README table); the one contradiction stock Chrome sends is a
   driver-set Referer on a typed navigation.
2. `declared_agent` (server, a session class). `sigagent.py` parses
   `Signature-Agent`, `Signature-Input` and `Signature` (RFC 9421 over RFC
   8941 Structured Fields, strict). A session with a well-formed declaration is
   reported in `/api/score` as `agent_declared` (agent URL, label, keyid,
   created, expires, alg, tag, nonce, components, requests) with `verified:
   false`, and its verdict is `declared_agent`; `score` and `score_verdict`
   stay. Malformed headers are not accepted (`agent_rejected` says why).
3. `fleet_fingerprint_reuse` (server, 30) and `beh_path_repeat` (behavior,
   30). `/api/clusters` groups retained sessions by a hash of the stable
   high-entropy probes and by pointer-stroke shape hash. The fleet rule fires
   at `fleet_threshold` (default 3) sessions per hash and needs the
   cross-session index, so a single session scores as before.
   `beh_path_repeat` is within one session: two strokes of 40 px or more with
   shapes within 0.8 px RMS after normalising translation, rotation and scale
   (`shapes.py`). Straight strokes carry no shape and are not compared.
4. `beh_click_offset_repeat` (behavior, 30): 4 or more clicks on different
   targets at the same offset (within 1 px, from the corner or the centre),
   not at the centre.
5. `worker_measure_mismatch` (automation, 40): an `OffscreenCanvas` in a
   dedicated worker measures 40 strings and families; the page measures the
   same with its canvas; the worker also reports its language, locale, time
   zone and `devicePixelRatio`. Any difference beyond 0.02 px fires. Chrome
   workers have no `devicePixelRatio`: `null` is fine.
6. `fonts_overclaim` (consistency, 25): more of the probe's 36 candidate
   fonts than 18 (Windows), 19 (macOS) or 22 (Linux). Ceilings are the most
   our recorded installs show plus 6 for portable families (README).
7. `webrtc_candidates` (consistency, 0 = info). The server starts a loopback
   UDP STUN responder (RFC 5389 Binding, `--stun-port`, `--no-stun`); the
   collector gathers ICE candidates for up to 3 s per page with its own
   beacon. The reference is stock Chrome 153 (mDNS host candidate plus a
   server-reflexive candidate at the responder's view, first candidate 3-56
   ms, complete in 119-172 ms). The rule compares a session with it and
   becomes scoring once LightBrowser gathers server-reflexive candidates.

`score_session` gains `info_fired` (weight-0 rules whose check fires, so an
info rule is visible without being scored), `agent_declared` and an optional
`fleet` index; its other results are unchanged.

## Consequences

- The three T-0xx reference scores are unchanged (T-001 100, T-003 100 with
  fp 100); the 8 reference personas keep fp 0 (Playwright input) and beh 0
  (`--input human`), and none of the new rules fires on them. The info rule
  `webrtc_candidates` does: LightBrowser gathers a host candidate but no
  server-reflexive one.
- The fleet rule reads "this device image appears on many sessions". One real
  device that starts three sessions looks the same; `fleet_window_s` narrows
  it, and the weight is 30, not more.
- The path-repeat threshold rests on LightBrowser's human model and T-020,
  not on recordings of people; the margin is 1.5 px against 0.8 px. Re-check
  with real traces.
- The Linux font ceiling can be passed by a maxed-out desktop; at weight 25
  that is a soft signal.
- No new dependency; Python standard library only. Python 3.9 and later run
  the tests.

# PARTIAL: release MVP matrix

Stopped on purpose before the planned set finished. This is not a complete
acceptance run. Binary: `target/release/lightbrowser` (`lightbrowser 0.0.1 (chrome)`),
`tests/detect-bench/mvp.sh`, T-010, loopback only. A scored flag is a rule in
`fired` (weight > 0). Human runs used `--input human` and `--expect fp<=0&beh<=0`.

Planned: 8 reference personas × seeds 1–10 human, plus one scripted Playwright
baseline (`--input playwright`) per persona (88 runs). Finished: **64 human
runs**. Scripted baselines: **not started**. The in-progress run (win11 Iris Xe,
seed 5) was killed and is not counted.

## Tally

| | runs | exit 0 | score 0 | fp 0 | beh 0 | scored flags |
|---|---:|---:|---:|---:|---:|---|
| Human, finished | 64 | 64 | 64 | 64 | 64 | none |
| Human, not run | 16 | | | | | |
| Scripted baseline | 0 of 8 | | | | | not measured |

Every finished human run printed `fired` empty.

## By persona

| Persona | Human seeds finished | Result |
|---|---|---|
| `chrome153_fedora42_amd_rx6600_wayland_1440p` | 1–10 | 10/10 zero flags |
| `chrome153_macos14_intel_irisplus_1440x900` | 1–10 | 10/10 zero flags |
| `chrome153_macos15_apple_m2_retina` | 1–10 | 10/10 zero flags |
| `chrome153_macos26_apple_m4_1512x982` | 1–10 | 10/10 zero flags |
| `chrome153_ubuntu24_intel_uhd630_x11_1080p` | 1–10 | 10/10 zero flags |
| `chrome153_win10_rtx3060_1080p` | 1–10 | 10/10 zero flags |
| `chrome153_win11_irisxe_laptop_1536x864` | 1–4 | 4/4 zero flags; seeds 5–10 not run |
| `chrome153_win11_rtx4060_1080p` | none | not started |

## Not in this file

Release budgets (idle RSS, disk writes, Lightpanda bench) were not recorded
here. The scripted Playwright baseline, which is supposed to raise behaviour
flags, was not run.

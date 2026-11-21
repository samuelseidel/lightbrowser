#!/usr/bin/env python3
"""Make the per-OS detect-bench fixtures (tests/detect-bench/fixtures/os_*_chrome.json).

    python3 tools/os_truth/bench_fixtures.py SESSIONS.jsonl \\
        --wheel tests/fixtures/os/linux_x11_cft153.json --out tests/detect-bench/fixtures

SESSIONS.jsonl is a detect-bench results file holding one T-010 session of
the stock local Chrome for Testing 153, headed under xvfb-run and driven
over CDP (loopback only). That session becomes the Linux fixture: traces and
input-behaviour probes are dropped (the OS rules do not use them) and the
wheel pairs are replaced by the real XTest notches from the capture file.

Windows 10 and macOS 15 (Apple Silicon) fixtures are that session with the
OS-dependent values swapped for the surface reference in docs/ROADMAP.md
("M7 status"). They are NOT captured: `provenance.captured` is false. Font
sizes are illustrative; the rules only compare sizes with each other.
"""

from __future__ import annotations

import argparse
import copy
import json
import sys
from pathlib import Path

CHROME_BRANDS = '"Google Chrome";v="153", "Chromium";v="153", "Not_A Brand";v="8"'
FULL_LIST = [{"brand": "Google Chrome", "version": "153.0.8010.12"},
             {"brand": "Chromium", "version": "153.0.8010.12"},
             {"brand": "Not_A Brand", "version": "8.0.0.0"}]
FULL_HDR = ", ".join(f'"{b["brand"]}";v="{b["version"]}"' for b in FULL_LIST)

OS = {
    "windows": {
        "ua": ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) "
               "Chrome/153.0.0.0 Safari/537.36"),
        "platform": "Windows", "navPlatform": "Win32",
        "high": {"architecture": "x86", "bitness": "64", "model": "", "platformVersion": "10.0.0",
                 "wow64": False, "formFactors": ["Desktop"]},
        "scrollbar": 17,
        "fonts": {
            "base": {"monospace": [821, 82], "serif": [910, 83], "sans-serif": [965, 82]},
            "generic": {"system-ui": [962, 96], "sans-serif": [965, 82], "serif": [910, 83], "monospace": [821, 82]},
            "detected": {"Segoe UI": [962, 96], "Consolas": [752, 85], "Segoe UI Emoji": [1024, 99],
                         "Arial": [965, 82], "Times New Roman": [910, 83], "Courier New": [821, 82],
                         "Verdana": [1112, 87], "Tahoma": [1001, 87], "Georgia": [1106, 82]},
        },
        "gl": {"vendor": "Google Inc. (NVIDIA)",
               "renderer": "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 (0x00002504) Direct3D11 vs_5_0 ps_5_0, D3D11)",
               "viewport": [32767, 32767], "lineWidth": [1, 1], "pointSize": [1, 1024], "texture": 16384,
               "vertexUniforms": 4096},
        "hw": {"deviceMemory": 8, "hardwareConcurrency": 12},
        "screen": {"width": 1920, "height": 1080, "availWidth": 1920, "availHeight": 1040, "availLeft": 0,
                   "availTop": 0, "dpr": 1, "screenX": 0, "screenY": 0, "outerWidth": 1920, "outerHeight": 1040,
                   "innerWidth": 1920, "innerHeight": 937},
        "codecs": {"hevcCanPlay": "probably", "h264": {"supported": True, "powerEfficient": True},
                   "hevc": {"supported": True, "powerEfficient": True}},
        "gpu": {"vendor": "nvidia", "architecture": "ampere"},
        "voices": ["Microsoft David - English (United States)", "Microsoft Mark - English (United States)",
                   "Microsoft Zira - English (United States)"],
        "network_voices": 19,
        "wheel": [[100, -120, 0]] * 4 + [[-100, 120, 0]] * 2,
    },
    "mac": {
        "ua": ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) "
               "Chrome/153.0.0.0 Safari/537.36"),
        "platform": "macOS", "navPlatform": "MacIntel",
        "high": {"architecture": "arm", "bitness": "64", "model": "", "platformVersion": "15.1.0",
                 "wow64": False, "formFactors": ["Desktop"]},
        "scrollbar": 0,
        "fonts": {
            "base": {"monospace": [824, 84], "serif": [910, 83], "sans-serif": [968, 83]},
            "generic": {"system-ui": [1004, 86], "sans-serif": [968, 83], "serif": [910, 83], "monospace": [824, 84]},
            "detected": {"Helvetica Neue": [985, 88], "Menlo": [824, 84], "Apple Color Emoji": [1011, 90],
                         "Lucida Grande": [1035, 85], "Geneva": [1079, 90], "Arial": [965, 82],
                         "Helvetica": [968, 83], "Times New Roman": [910, 83], "Times": [910, 83],
                         "Courier New": [821, 82], "Verdana": [1112, 87], "Tahoma": [1001, 87],
                         "Georgia": [1106, 82]},
        },
        "gl": {"vendor": "Google Inc. (Apple)", "renderer": "ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)",
               "viewport": [16384, 16384], "lineWidth": [1, 1], "pointSize": [1, 511], "texture": 16384,
               "vertexUniforms": 1024},
        "hw": {"deviceMemory": 8, "hardwareConcurrency": 8},
        "screen": {"width": 1470, "height": 956, "availWidth": 1470, "availHeight": 919, "availLeft": 0,
                   "availTop": 37, "dpr": 2, "screenX": 0, "screenY": 37, "outerWidth": 1470, "outerHeight": 919,
                   "innerWidth": 1470, "innerHeight": 812},
        "codecs": {"hevcCanPlay": "probably", "h264": {"supported": True, "powerEfficient": True},
                   "hevc": {"supported": True, "powerEfficient": True}},
        "gpu": {"vendor": "apple", "architecture": "metal-3"},
        "voices": ["Albert", "Alice", "Daniel", "Fred", "Junior", "Karen", "Kathy", "Moira", "Ralph", "Rishi",
                   "Samantha", "Tessa", "Thomas", "Zarvox"],
        "network_voices": 19,
        "wheel": [[4, -12, 0], [8, -24, 0], [12, -36, 0], [6, -18, 0], [2, -6, 0]],
    },
}


def make_linux(s: dict, wheel_pairs: list) -> dict:
    s = copy.deepcopy(s)
    s["sid"] = "0" * 15 + "1"
    for pg in s["pages"].values():
        pg["trace"] = []
        pg["probes"] = {k: v for k, v in pg["probes"].items()
                        if not k.startswith("beh_") or k == "beh_wheel_deltas"}
        if "beh_wheel_deltas" in pg["probes"]:
            pg["probes"]["beh_wheel_deltas"]["value"] = {"n": 0, "pairs": []}
    first = sorted(s["pages"].values(), key=lambda p: p.get("received", 0))[0]
    first["probes"]["beh_wheel_deltas"] = {"id": "beh_wheel_deltas", "cat": "behavior", "suspicious": False,
                                           "weight": 0, "value": {"n": len(wheel_pairs), "pairs": wheel_pairs}}
    s["provenance"] = {
        "captured": True,
        "how": ("Chrome for Testing 153.0.8010.12, headed under xvfb-run (1920x1080x24, 96 dpi), started with "
                "--remote-debugging-port and driven by run.js T-010 over CDP on loopback; the wheel pairs are "
                "XTest notches from tests/fixtures/os/linux_x11_cft153.json."),
        "limits": "No GPU (SwiftShader WebGL), no window manager, no speech voices, container fonts (Inter).",
        "tool": "tools/os_truth/bench_fixtures.py",
    }
    return s


def make_other(linux: dict, os_name: str) -> dict:
    o = OS[os_name]
    s = copy.deepcopy(linux)
    s["sid"] = "0" * 15 + ("2" if os_name == "windows" else "3")
    s["user_agents"] = [o["ua"]]
    for i, d in enumerate(s["documents"]):
        h = d["headers"]
        h["user-agent"] = o["ua"]
        h["sec-ch-ua"] = CHROME_BRANDS
        h["sec-ch-ua-platform"] = f'"{o["platform"]}"'
        if "sec-ch-ua-platform-version" in h:
            hi = o["high"]
            h["sec-ch-ua-platform-version"] = f'"{hi["platformVersion"]}"'
            h["sec-ch-ua-arch"] = f'"{hi["architecture"]}"'
            h["sec-ch-ua-bitness"] = f'"{hi["bitness"]}"'
            h["sec-ch-ua-model"] = '""'
            h["sec-ch-ua-wow64"] = "?0"
            h["sec-ch-ua-full-version-list"] = FULL_HDR
            h["sec-ch-ua-form-factors"] = '"Desktop"'
    for r in s.get("requests", []):
        r["ua"] = o["ua"]
    for pg in s["pages"].values():
        p = pg["probes"]
        if "ua_headless" in p:
            p["ua_headless"]["value"] = o["ua"]
        if "os_surface" in p:
            v = p["os_surface"]["value"]
            v["os"] = os_name
            v["scrollbar"].update(div=o["scrollbar"], root=o["scrollbar"])
            v["apis"] = {"share": True, "canShare": True, "bluetooth": True, "Bluetooth": True}
            v["fonts"] = copy.deepcopy(o["fonts"])
            v["gl"] = dict(o["gl"])
            v["hw"] = dict(o["hw"])
            v["screen"] = dict(o["screen"])
        if "os_media" in p:
            v = p["os_media"]["value"]
            v["os"] = os_name
            v["high"] = dict(o["high"], brands=[{"brand": b["brand"], "version": b["version"].split(".")[0]}
                                                for b in FULL_LIST],
                             fullVersionList=FULL_LIST, mobile=False, platform=o["platform"])
            v["codecs"] = copy.deepcopy(o["codecs"])
            v["gpu"] = {"present": True, "adapter": dict(o["gpu"])}
            v["keyboardSize"] = 48
        if "os_voices" in p:
            p["os_voices"]["value"] = {"count": len(o["voices"]) + o["network_voices"], "local": len(o["voices"]),
                                       "localNames": list(o["voices"])}
        if "beh_wheel_deltas" in p and p["beh_wheel_deltas"]["value"].get("pairs"):
            p["beh_wheel_deltas"]["value"] = {"n": len(o["wheel"]), "pairs": [list(x) for x in o["wheel"]]}
    s["provenance"] = {
        "captured": False,
        "how": (f"Not captured: the Linux fixture with the {os_name} values of the M7 surface reference "
                "(docs/ROADMAP.md, \"M7 status\") swapped in. Replace with an internal capture when one exists."),
        "tool": "tools/os_truth/bench_fixtures.py",
    }
    return s


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("sessions")
    ap.add_argument("--wheel", required=True, help="tools/os_truth capture with XTest wheel events")
    ap.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    lines = [json.loads(x) for x in Path(a.sessions).read_text().splitlines() if x.strip()]
    if len(lines) != 1:
        print(f"want exactly one session in {a.sessions}, found {len(lines)}", file=sys.stderr)
        return 2
    cap = json.loads(Path(a.wheel).read_text())
    pairs = [[e["deltaY"], e["wheelDeltaY"], e["deltaMode"]] for e in cap["wheel"]["events"] if e["isTrusted"]]
    linux = make_linux(lines[0], pairs)
    out = Path(a.out)
    for name, s in (("linux", linux), ("windows", make_other(linux, "windows")), ("mac", make_other(linux, "mac"))):
        p = out / f"os_{name}_chrome.json"
        p.write_text(json.dumps(s, indent=1) + "\n")
        print(f"wrote {p}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

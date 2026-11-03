#!/usr/bin/env python3
"""Dump the page-observable JS surface of LightBrowser builds and diff them.

For each binary and persona, loads index.html (served on 127.0.0.1 from this
directory) with `lightbrowser fetch --dump html` and through `lightbrowser
serve` with Playwright `connectOverCDP` (two pages in one context), and
writes what dump.js reported. With two or more binaries, every dump is
compared with the first binary's and the run fails on any difference.

    python3 tools/surface_diff/run.py --out DIR BASE_BIN NEW_BIN
    python3 tools/surface_diff/run.py --out DIR --chromium /opt/pw-browsers/chromium BIN

`--chromium` also dumps the page in a local Chrome (headless, its own
surface, no persona) to DIR/chromium.txt, for checking a difference
between builds against Chrome by hand.

Loopback only. Playwright's own main-world binding (`__pwInitScripts`, a
random key per context) is normalised.
"""

import argparse
import difflib
import html
import http.server
import functools
import os
import re
import subprocess
import sys
import threading
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
PERSONAS = ["chrome153_win10_rtx3060_1080p", "chrome153_macos15_apple_m2_retina", "chrome153_ubuntu24_intel_uhd630_x11_1080p"]


def env():
    e = {k: v for k, v in os.environ.items() if k.lower() not in ("http_proxy", "https_proxy", "all_proxy")}
    e["NO_PROXY"] = e["no_proxy"] = "127.0.0.1,localhost"
    return e


def normalise(text: str) -> str:
    return re.sub(r"__pwInitScripts\.[0-9a-f]+", "__pwInitScripts.<key>", text)


def dump_fetch(binary, persona, url):
    p = subprocess.run([binary, "fetch", url, "--persona", f"profiles/reference/chrome/{persona}.json", "--dump", "html"],
                       cwd=ROOT, env=env(), capture_output=True, text=True, timeout=300)
    m = re.search(r'<pre id="o">(.*?)</pre>', p.stdout, re.S)
    return html.unescape(m.group(1)) if m else "NO OUTPUT\n" + p.stderr


def dump_cdp(binary, persona, url):
    from playwright.sync_api import sync_playwright
    sp = subprocess.Popen([binary, "serve", "--port", "0", "--persona", f"profiles/reference/chrome/{persona}.json"],
                          cwd=ROOT, env=env(), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    try:
        ws = None
        while ws is None:
            line = sp.stderr.readline()
            if not line:
                raise RuntimeError("serve exited")
            m = re.search(r"(ws://127\.0\.0\.1:\d+/\S+)", line)
            ws = m and m.group(1)
        out = []
        with sync_playwright() as pw:
            br = pw.chromium.connect_over_cdp(ws)
            ctx = br.new_context(no_viewport=True)
            for _ in range(2):  # the second page is a later realm of the same worker
                pg = ctx.new_page()
                pg.goto(url, wait_until="load")
                pg.wait_for_selector("html[data-done]", state="attached")
                out.append(pg.eval_on_selector("#o", "e => e.textContent"))
            br.close()
        return out[0] + ("" if out[0] == out[1] else "\nSECOND PAGE DIFFERS\n" + out[1])
    finally:
        sp.kill()
        sp.wait()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", required=True)
    ap.add_argument("--chromium", help="also dump the page in this local Chrome")
    ap.add_argument("binaries", nargs="+")
    a = ap.parse_args()
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(QuietHandler, directory=str(HERE)))
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{srv.server_address[1]}/index.html"
    dumps = {}
    for i, b in enumerate(a.binaries):
        d = Path(a.out) / f"{i}-{Path(b).name}"
        d.mkdir(parents=True, exist_ok=True)
        for persona in PERSONAS:
            for mode, fn in (("fetch", dump_fetch), ("cdp", dump_cdp)):
                t = normalise(fn(os.path.abspath(b), persona, url))
                (d / f"{mode}_{persona}.txt").write_text(t)
                dumps[(i, mode, persona)] = t
                print(f"{b} {mode} {persona}: {len(t.splitlines())} lines, {len(t)} bytes", flush=True)
    if a.chromium:
        from playwright.sync_api import sync_playwright
        with sync_playwright() as pw:
            br = pw.chromium.launch(executable_path=a.chromium, headless=True, args=["--no-sandbox", "--no-proxy-server"])
            pg = br.new_page()
            pg.goto(url, wait_until="load")
            pg.wait_for_selector("html[data-done]", state="attached")
            (Path(a.out) / "chromium.txt").write_text(normalise(pg.eval_on_selector("#o", "e => e.textContent")))
            br.close()
    srv.shutdown()
    bad = 0
    for (i, mode, persona), t in sorted(dumps.items()):
        if i == 0:
            if "DUMP FAILED" in t or "NO OUTPUT" in t or "SECOND PAGE DIFFERS" in t:
                print(f"base {mode} {persona}: incomplete"); bad += 1
            continue
        base = dumps[(0, mode, persona)]
        if t == base:
            print(f"{a.binaries[i]} {mode} {persona}: identical to {a.binaries[0]}")
        else:
            bad += 1
            diff = list(difflib.unified_diff(base.splitlines(), t.splitlines(), "base", "new", n=0, lineterm=""))
            print(f"{a.binaries[i]} {mode} {persona}: {len(diff)} diff lines")
            print("\n".join(diff[:40]))
    return 1 if bad else 0


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *a):
        pass


if __name__ == "__main__":
    sys.exit(main())

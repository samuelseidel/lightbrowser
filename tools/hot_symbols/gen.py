#!/usr/bin/env python3
"""Write bin/lightbrowser/hot-symbols.txt: the functions a page load runs.

The release link puts these first in `.text` (lld `--symbol-ordering-file`,
see bin/lightbrowser/build.rs), so the code every process runs sits on few
pages instead of being spread over the binary's 30 MB of V8 and Rust code.
The kernel maps file pages around each fault (64 KB), so a scattered hot
set costs resident memory and page faults; a packed one does not.

Training workload (loopback only, never the bench corpus, which is what the
result is measured on): the detect-bench pages served by
tests/detect-bench/server.py, the WPT subset served from tests/wpt, and
tools/hot_symbols/pages/common.html, a generic page (images of the common
formats, flex and grid layout, a table and a form, layout queries, number
and date formatting, timers, storage, XHR and fetch).
Each page is loaded with `lightbrowser fetch` (one process per page); one
`lightbrowser serve` loads them all twice through Playwright (new page,
goto, evaluate, content, close) and then idles; another `serve` only
idles. Each run goes under valgrind's callgrind. Every function of the
binary that ran is listed: first those an idle `serve` runs (start-up),
then by the share of `fetch` runs that used it, each source of pages
weighing the same (code every page load needs before code only some pages
need; code only the crawl ran counts as used by all), then by
instructions.

Needs valgrind, Python Playwright and an unstripped release build:

    cargo build --release -p lightbrowser \\
        --config 'profile.release.package.lightbrowser.strip=false'
    python3 tools/hot_symbols/gen.py
    cargo build --release -p lightbrowser    # relink with the new order

Symbols missing from a later build are ignored by the linker, so a stale
file only costs locality, never correctness.
"""

import argparse
import atexit
import collections
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "bin/lightbrowser/hot-symbols.txt"
SITES = ["/shop", "/news", "/search", "/login"]
# Every WPT file of the subset (DOM, events, HTML APIs) except frame
# tests (their child documents are separate pages).
WPT = sorted(
    str(p.relative_to(ROOT / "tests/wpt"))
    for p in (ROOT / "tests/wpt").rglob("*.html")
    if "iframe" not in p.name and "resources" not in p.parts
)


def loopback_env() -> dict:
    env = {k: v for k, v in os.environ.items() if "proxy" not in k.lower()}
    env["NO_PROXY"] = env["no_proxy"] = "127.0.0.1,localhost,::1"
    return env


def start(argv, cwd=None, env=None, pattern=r"(\d+)"):
    p = subprocess.Popen(argv, cwd=cwd, env=env, stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT, text=True)
    return p


def free_port() -> int:
    import socket
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_port(port: int, timeout: float = 30.0) -> None:
    import socket
    t0 = time.time()
    while time.time() - t0 < timeout:
        with socket.socket() as s:
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return
        time.sleep(0.1)
    sys.exit(f"nothing listening on 127.0.0.1:{port}")


def write_pages(www: Path) -> None:
    """The generic training page, with its images made here."""
    shutil.copytree(Path(__file__).resolve().parent / "pages", www)
    try:
        from PIL import Image
    except ImportError:
        Image = None
    if Image is None:
        return
    img = Image.new("RGB", (160, 100))
    for x in range(160):
        for y in range(100):
            img.putpixel((x, y), ((x * 3) % 256, (y * 5) % 256, (x + y) % 256))
    for ext, fmt in (("png", "PNG"), ("jpg", "JPEG"), ("gif", "GIF"), ("webp", "WEBP")):
        try:
            img.save(www / f"pic.{ext}", fmt)
        except (KeyError, OSError):
            pass


def parse(path: Path, cost: collections.Counter, binary: str) -> None:
    """Add self cost per function of `binary` from a callgrind file (with
    name compression)."""
    obn, fnn, ob, fn = {}, {}, None, None
    for line in path.open(errors="replace"):
        if line.startswith(("ob=", "cob=", "fn=", "cfn=")):
            m = re.match(r"(c?)(ob|fn)=\((\d+)\)(?: (.*))?", line.rstrip("\n"))
            if not m:
                continue
            table = obn if m.group(2) == "ob" else fnn
            if m.group(4):
                table[m.group(3)] = m.group(4)
            if not m.group(1):
                if m.group(2) == "ob":
                    ob = obn.get(m.group(3))
                else:
                    fn = fnn.get(m.group(3))
            continue
        if fn and ob == binary and (line[:1].isdigit() or line[:1] in "+-*"):
            parts = line.split()
            if len(parts) >= 2 and parts[1].isdigit():
                cost[fn] += int(parts[1])


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", default=str(ROOT / "target/release/lightbrowser"))
    ap.add_argument("--out", default=str(OUT))
    ap.add_argument("--keep", help="keep the callgrind profiles in this directory")
    ap.add_argument("--reuse", help="only rank: read the profiles a --keep run left here")
    args = ap.parse_args()
    binary = str(Path(args.binary).resolve())
    if b".symtab" not in subprocess.run(["readelf", "-S", binary], capture_output=True).stdout:
        sys.exit("the binary has no symbols: build it with strip=false (see --help)")
    if args.reuse:
        return rank(Path(args.reuse), binary, Path(args.out))
    env = loopback_env()
    if args.keep:
        tmp = Path(args.keep)
        tmp.mkdir(parents=True, exist_ok=True)
    else:
        tmp = Path(tempfile.mkdtemp(prefix="lbhot-"))
        atexit.register(shutil.rmtree, tmp, True)
    det_port, wpt_port, pages_port = free_port(), free_port(), free_port()
    write_pages(tmp / "www")
    servers = [
        subprocess.Popen([sys.executable, str(ROOT / "tests/detect-bench/server.py"), "--port", str(det_port),
                          "--results-dir", str(tmp / "results"), "--quiet"],
                         env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL),
        subprocess.Popen([sys.executable, "-m", "http.server", str(wpt_port), "--bind", "127.0.0.1"],
                         cwd=ROOT / "tests/wpt", env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL),
        subprocess.Popen([sys.executable, "-m", "http.server", str(pages_port), "--bind", "127.0.0.1"],
                         cwd=tmp / "www", env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL),
    ]
    urls = ([f"http://127.0.0.1:{det_port}{p}" for p in SITES]
            + [f"http://127.0.0.1:{wpt_port}/{p}" for p in WPT]
            + [f"http://127.0.0.1:{pages_port}/common.html"])
    vg = ["valgrind", "--tool=callgrind", "--demangle=no", "--fair-sched=yes"]
    # The lazily paged ICU data (lb-js icu_data) is filled in by a SIGSEGV
    # handler and the faulting load retried, which callgrind does not get
    # right (the retried load reads through a stale register): decompress
    # it at once instead.
    env["LB_ICU_EAGER"] = "1"
    outs = []
    idle = tmp / "idle"
    try:
        sp = subprocess.Popen(vg + [f"--callgrind-out-file={idle}", binary, "serve", "--port", "0"],
                              cwd=ROOT, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(20)
        sp.send_signal(signal.SIGTERM)
        sp.wait(timeout=300)
        wait_port(det_port)
        wait_port(wpt_port)
        wait_port(pages_port)
        for i, url in enumerate(urls):
            group = "sites" if i < len(SITES) else "wpt" if i < len(SITES) + len(WPT) else "common"
            out = tmp / f"fetch.{group}.{i}"
            subprocess.run(vg + [f"--callgrind-out-file={out}", binary, "fetch", url, "--dump", "html"],
                           cwd=ROOT, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=900)
            outs.append(out)
        # One `serve` crawling every page twice, like a client driving a
        # long-lived browser (garbage collection, tier-up, CDP).
        out = tmp / "serve"
        sp = subprocess.Popen(vg + [f"--callgrind-out-file={out}", binary, "serve", "--port", "0"],
                              cwd=ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        ws = None
        while ws is None:
            line = sp.stdout.readline()
            if not line:
                sys.exit("serve exited")
            m = re.search(r"(ws://127\.0\.0\.1:\d+/\S+)", line)
            ws = m.group(1) if m else None
        from playwright.sync_api import sync_playwright
        with sync_playwright() as pw:
            br = pw.chromium.connect_over_cdp(ws, timeout=600000)
            ctx = br.new_context(no_viewport=True)
            for url in urls + urls:
                pg = ctx.new_page()
                pg.goto(url, wait_until="load", timeout=600000)
                pg.evaluate("document.title")
                pg.content()
                pg.close()
            # A quiet browser's memory-reducing GC.
            time.sleep(8)
            ctx.close()
            br.close()
        sp.send_signal(signal.SIGTERM)
        sp.wait(timeout=300)
        serve_out = out
    finally:
        for s in servers:
            s.terminate()
            s.wait()
    return rank(tmp, binary, Path(args.out))


def rank(tmp: Path, binary: str, out: Path) -> int:
    """Rank the functions of the profiles in `tmp` and write the list."""
    cost: collections.Counter = collections.Counter()
    use: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    size: collections.Counter = collections.Counter()
    for o in sorted(tmp.glob("fetch.*")):
        group = o.name.split(".")[1]
        c: collections.Counter = collections.Counter()
        parse(o, c, binary)
        cost.update(c)
        use[group].update(c.keys())
        size[group] += 1
    serve: collections.Counter = collections.Counter()
    parse(tmp / "serve", serve, binary)
    cost.update(serve)
    start: collections.Counter = collections.Counter()
    parse(tmp / "idle", start, binary)
    if not start or not serve or not size:
        sys.exit("no profile data for the binary (callgrind output names another object?)")

    def share(n: str) -> float:
        # Each source of pages weighs the same, whatever its page count;
        # code only the crawl ran (CDP, GC, tier-up) ranks with code every
        # page load runs.
        s = sum(use[g][n] / size[g] for g in size) / len(size)
        return s if s > 0 else (1.0 if n in serve else 0.0)

    names = sorted(
        (n for n in cost.keys() | start.keys() if not n.startswith("0x") and "'" not in n),
        key=lambda n: (n not in start, -share(n), -cost[n], n),
    )
    out.write_text("\n".join(names) + "\n")
    print(f"{len(names)} functions -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""LightBrowser crawl benchmark (PLAN M6 item 2). Loopback only.

Serves the fixed corpus in bench/corpus on 127.0.0.1 and crawls every page
with each target, N times:

  fetch:lightbrowser   `lightbrowser fetch URL --dump html`, one process per page
  cdp:lightbrowser     `lightbrowser serve`, driven by Playwright connectOverCDP
  cdp:chromium         headless Chromium, same Playwright code (reference)
  fetch:lightpanda,
  cdp:lightpanda       only if a `lightpanda` binary is on PATH (otherwise skipped)

Per run it records wall time per page (median, p90) and for the whole crawl,
peak RSS, CPU time, startup to first CDP connect, idle RSS, disk writes.
Per target it records the binary size. It then checks the budgets in
docs/ARCHITECTURE.md and writes bench/results/<date>-<sha>.json and
bench/results/README.md.

    python3 bench/run.py                 # 5 runs, all available targets
    python3 bench/run.py --runs 1 --pages 10 --no-write

See bench/README.md.
"""

from __future__ import annotations

import argparse
import atexit
import datetime as dt
import hashlib
import importlib.metadata
import json
import os
import platform
import re
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path
from urllib.parse import urlparse

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
import gen_corpus  # noqa: E402

CORPUS = HERE / "corpus"
RESULTS = HERE / "results"
DEFAULT_CHROMIUM = "/opt/pw-browsers/chromium-1243/chrome-linux64/chrome"
MARK = "data-bench-rendered"
CLK = os.sysconf("SC_CLK_TCK")
LOOPBACK_HOSTS = {"127.0.0.1", "::1", "localhost"}

# Budgets from docs/ARCHITECTURE.md ("Goals and budgets").
BUDGET_IDLE_RSS_MB = 15.0
BUDGET_BINARY_MB = 50.0
BUDGET_REALM_MS = 5.0


# --- loopback guard ---------------------------------------------------------


def assert_loopback(url: str) -> str:
    """Return url if its host is 127.0.0.1, ::1 or localhost; exit 2 otherwise."""
    host = urlparse(url).hostname
    if host not in LOOPBACK_HOSTS:
        print(f"refusing non-loopback address {url!r}: the bench runs on 127.0.0.1 only", file=sys.stderr)
        raise SystemExit(2)
    return url


# --- /proc helpers ----------------------------------------------------------


def _stat_fields(pid: int) -> list[str] | None:
    try:
        raw = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return None
    # comm may contain spaces and parentheses: split after the last ')'.
    return raw[raw.rfind(")") + 2 :].split()


def children_map() -> dict[int, list[int]]:
    out: dict[int, list[int]] = {}
    for d in os.listdir("/proc"):
        if d.isdigit():
            f = _stat_fields(int(d))
            if f:
                out.setdefault(int(f[1]), []).append(int(d))  # f[1] = ppid
    return out


def proc_tree(root: int) -> list[int]:
    kids = children_map()
    out, todo = [], [root]
    while todo:
        p = todo.pop()
        if Path(f"/proc/{p}").exists():
            out.append(p)
            todo.extend(kids.get(p, []))
    return out


def status_kb(pid: int, key: str) -> int:
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith(key):
                return int(line.split()[1])
    except (OSError, ValueError, IndexError):
        pass
    return 0


def pss_kb(pid: int) -> int:
    try:
        for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
            if line.startswith("Pss:"):
                return int(line.split()[1])
    except (OSError, ValueError):
        pass
    return 0


def cpu_s(pid: int) -> float:
    """utime + stime + reaped children's, in seconds."""
    f = _stat_fields(pid)
    if not f:
        return 0.0
    # f[11], f[12] = utime, stime; f[13], f[14] = cutime, cstime
    return sum(int(x) for x in f[11:15]) / CLK


def io_write_bytes(pid: int) -> int:
    """Bytes the process caused to be written to storage (0 when unreadable)."""
    try:
        for line in Path(f"/proc/{pid}/io").read_text().splitlines():
            if line.startswith("write_bytes:"):
                return int(line.split()[1])
    except (OSError, ValueError):
        pass
    return 0


def tree_rss_mb(root: int) -> float:
    return sum(status_kb(p, "VmRSS:") for p in proc_tree(root)) / 1024


def tree_pss_mb(root: int) -> float:
    return sum(pss_kb(p) for p in proc_tree(root)) / 1024


def tree_cpu_s(root: int) -> float:
    return sum(cpu_s(p) for p in proc_tree(root))


def tree_write_bytes(root: int) -> int:
    return sum(io_write_bytes(p) for p in proc_tree(root))


class Sampler(threading.Thread):
    """Samples the process tree's summed RSS; keeps the maximum."""

    def __init__(self, root: int, interval: float = 0.05) -> None:
        super().__init__(daemon=True)
        self.root, self.interval = root, interval
        self.peak_mb = 0.0
        self.hwm_mb = 0.0  # largest sum of the tree's own VmHWM
        self.peak_pss_mb = 0.0  # PSS counts shared pages once; sampled less often (it is costly)
        self._halt = threading.Event()

    def run(self) -> None:
        tick = 0
        while not self._halt.is_set():
            pids = proc_tree(self.root)
            if tick % 4 == 0:
                self.peak_pss_mb = max(self.peak_pss_mb, sum(pss_kb(p) for p in pids) / 1024)
            tick += 1
            self.peak_mb = max(self.peak_mb, sum(status_kb(p, "VmRSS:") for p in pids) / 1024)
            self.hwm_mb = max(self.hwm_mb, sum(status_kb(p, "VmHWM:") for p in pids) / 1024)
            self._halt.wait(self.interval)

    def finish(self) -> None:
        self._halt.set()
        self.join()


# --- statistics -------------------------------------------------------------


def pctl(values: list[float], q: float) -> float:
    """Nearest-rank percentile."""
    s = sorted(values)
    return s[max(0, min(len(s) - 1, -(-len(s) * q // 100) - 1))] if s else float("nan")


def spread(values: list[float]) -> dict:
    v = [x for x in values if x is not None]
    if not v:
        return {"median": None, "min": None, "max": None, "n": 0}
    return {"median": round(statistics.median(v), 3), "min": round(min(v), 3), "max": round(max(v), 3), "n": len(v)}


# --- processes --------------------------------------------------------------

_children: list[subprocess.Popen] = []


def _kill_all() -> None:
    for p in _children:
        if p.poll() is None:
            try:
                p.terminate()
                p.wait(5)
            except Exception:
                try:
                    p.kill()
                except Exception:
                    pass


atexit.register(_kill_all)


def stop(p: subprocess.Popen) -> None:
    if p.poll() is None:
        p.send_signal(signal.SIGTERM)
        try:
            p.wait(5)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()


class Server:
    """bench/serve_corpus.py in its own process."""

    def __init__(self) -> None:
        self.proc = subprocess.Popen(
            [sys.executable, str(HERE / "serve_corpus.py"), "--port", "0"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
        )
        _children.append(self.proc)
        line = self.proc.stdout.readline()
        m = re.search(r"listening (127\.0\.0\.1):(\d+)", line)
        if not m:
            raise RuntimeError(f"corpus server did not start: {line!r}")
        self.base = assert_loopback(f"http://{m.group(1)}:{m.group(2)}")

    def close(self) -> None:
        stop(self.proc)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Spawned:
    """A browser process plus a thread draining its stderr and watching for the DevTools line."""

    WS = re.compile(r"(?:DevTools listening on|listening on|Listening on)\s+(wss?://\S+)")

    def __init__(self, argv: list[str], env: dict, cwd: str) -> None:
        self.t0 = time.perf_counter()
        self.proc = subprocess.Popen(
            argv, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, errors="replace",
        )
        _children.append(self.proc)
        self.ws: str | None = None
        self.t_listen: float | None = None
        self.tail: list[str] = []
        self._ev = threading.Event()
        threading.Thread(target=self._drain, daemon=True).start()

    def _drain(self) -> None:
        for line in self.proc.stderr:
            self.tail = (self.tail + [line.rstrip()])[-20:]
            m = self.WS.search(line)
            if m and self.ws is None:
                self.t_listen = time.perf_counter()
                self.ws = m.group(1)
                self._ev.set()
        self._ev.set()

    def wait_ws(self, timeout: float = 60.0) -> str:
        self._ev.wait(timeout)
        if self.ws is None:
            raise RuntimeError("no DevTools endpoint; stderr tail: " + " | ".join(self.tail[-5:]))
        return assert_loopback(self.ws)


def wait_json_version(sp: Spawned, port: int, timeout: float = 30.0) -> str:
    """Poll http://127.0.0.1:PORT/json/version until it answers; return its webSocketDebuggerUrl (loopback only)."""
    url = assert_loopback(f"http://127.0.0.1:{port}/json/version")
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    deadline = time.time() + timeout
    while time.time() < deadline:
        if sp.proc.poll() is not None:
            raise RuntimeError(f"exited with {sp.proc.returncode}; stderr tail: " + " | ".join(sp.tail[-5:]))
        try:
            with opener.open(url, timeout=0.5) as r:
                ws = json.loads(r.read()).get("webSocketDebuggerUrl")
            if ws:
                return assert_loopback(ws)
        except (OSError, ValueError):
            pass
        time.sleep(0.01)
    raise RuntimeError(f"no answer from {url}; stderr tail: " + " | ".join(sp.tail[-5:]))


def clean_env(tmp: str) -> dict:
    env = {k: v for k, v in os.environ.items() if k.lower() not in ("http_proxy", "https_proxy", "all_proxy")}
    env.update(HOME=tmp, TMPDIR=tmp, XDG_CACHE_HOME=tmp, XDG_CONFIG_HOME=tmp, NO_PROXY="127.0.0.1,localhost,::1", no_proxy="127.0.0.1,localhost,::1",
               # Lightpanda reports usage to its servers unless told not to; the bench stays on loopback.
               LIGHTPANDA_DISABLE_TELEMETRY="true")
    return env


def tree_files(path: str) -> list[str]:
    return sorted(str(p.relative_to(path)) for p in Path(path).rglob("*"))


# --- targets ----------------------------------------------------------------


class Target:
    """One browser under test."""

    def __init__(self, name: str, binary: str, fetch_argv, serve_argv, ws_from_port: bool = False, user_data: bool = False) -> None:
        self.name, self.binary = name, binary
        self.fetch_argv, self.serve_argv = fetch_argv, serve_argv
        self.ws_from_port, self.user_data = ws_from_port, user_data

    @property
    def size_mb(self) -> float:
        return Path(self.binary).stat().st_size / 1e6 if Path(self.binary).exists() else float("nan")


def lightbrowser_target(binary: str) -> Target:
    return Target(
        "lightbrowser", binary,
        lambda url: [binary, "fetch", url, "--dump", "html"],
        lambda port: [binary, "serve", "--port", str(port)],
    )


def chromium_target(binary: str) -> Target:
    return Target(
        "chromium", binary, None,
        lambda port: [binary, "--headless=new", "--no-sandbox", "--no-first-run", "--no-default-browser-check",
                      "--no-proxy-server", "--remote-debugging-port=0", "about:blank"],
        user_data=True,
    )


def lightpanda_target(binary: str) -> Target:
    return Target(
        "lightpanda", binary,
        lambda url: [binary, "fetch", "--dump", "html", url],
        lambda port: [binary, "serve", "--host", "127.0.0.1", "--port", str(port)],
        ws_from_port=True,
    )


# Runs `fetch` processes for `run_fetch` and reports their rusage. A child's
# ru_maxrss also counts the RSS its parent had when the child called exec
# (Linux copies the old mm's high-water mark at exec), so spawned straight
# from this process (tens of MB with Playwright loaded, growing over the
# run) every `fetch` process "peaked" at this process's size. This helper
# stays small (~10 MB, below any browser measured here), so the peak is the
# browser's own. Protocol: one argv per line (NUL-separated, hex-encoded);
# reply "exit utime stime maxrss_kb nbytes\n" plus the child's stdout.
SPAWNER = r"""
import os, sys
inp, out = sys.stdin.buffer, sys.stdout.buffer
for line in inp:
    argv = bytes.fromhex(line.strip().decode()).split(b"\0")
    r, w = os.pipe()
    fa = [(os.POSIX_SPAWN_DUP2, w, 1), (os.POSIX_SPAWN_CLOSE, r), (os.POSIX_SPAWN_CLOSE, w)]
    devnull = os.open(os.devnull, os.O_RDWR)
    fa += [(os.POSIX_SPAWN_DUP2, devnull, 0), (os.POSIX_SPAWN_DUP2, devnull, 2)]
    pid = os.posix_spawn(argv[0], argv, os.environ, file_actions=fa)
    os.close(w)
    os.close(devnull)
    chunks = []
    while True:
        b = os.read(r, 1 << 16)
        if not b:
            break
        chunks.append(b)
    os.close(r)
    _, status, ru = os.wait4(pid, 0)
    data = b"".join(chunks)
    out.write(f"{os.waitstatus_to_exitcode(status)} {ru.ru_utime} {ru.ru_stime} {ru.ru_maxrss} {len(data)}\n".encode())
    out.write(data)
    out.flush()
"""


def run_fetch(t: Target, urls: list[str], expect: list[int]) -> dict:
    """One process per page. Exact CPU and peak RSS come from wait4 rusage,
    taken in a small spawner process (see `SPAWNER`)."""
    walls, cpus, rss, verified, errors = [], [], [], 0, []
    with tempfile.TemporaryDirectory(prefix="lbbench-") as tmp:
        env = clean_env(tmp)
        sp = subprocess.Popen([sys.executable, "-I", "-S", "-c", SPAWNER], env=env, cwd=tmp,
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        _children.append(sp)
        try:
            crawl0 = time.perf_counter()
            for url, want in zip(urls, expect):
                argv = t.fetch_argv(url)
                argv[0] = shutil.which(argv[0]) or argv[0]
                t0 = time.perf_counter()
                sp.stdin.write("\0".join(argv).encode().hex().encode() + b"\n")
                sp.stdin.flush()
                head = sp.stdout.readline().split()
                code, utime, stime, maxrss_kb, n = int(head[0]), float(head[1]), float(head[2]), int(head[3]), int(head[4])
                out = sp.stdout.read(n)
                walls.append((time.perf_counter() - t0) * 1e3)
                cpus.append(utime + stime)
                rss.append(maxrss_kb / 1024)  # KB on Linux
                if code != 0:
                    errors.append(f"{url}: exit {code}")
                elif f'{MARK}="{want}"'.encode() in out:
                    verified += 1
                else:
                    errors.append(f"{url}: marker {MARK}={want} missing")
            total = time.perf_counter() - crawl0
        finally:
            sp.stdin.close()
            sp.wait()
        leftovers = tree_files(tmp)
    return {
        "pages": len(urls), "verified": verified, "errors": errors[:5],
        "pages_ms": [round(w, 1) for w in walls],
        "crawl_total_s": total, "page_median_ms": statistics.median(walls), "page_p90_ms": pctl(walls, 90),
        "peak_rss_mb": max(rss), "proc_peak_rss_median_mb": statistics.median(rss),
        "cpu_s": sum(cpus), "disk_files_created": len(leftovers),
    }


def run_cdp(t: Target, pw, urls: list[str], expect: list[int], new_page_reps: int = 15, settle: float = 1.0) -> dict:
    with tempfile.TemporaryDirectory(prefix="lbbench-") as tmp:
        env = clean_env(tmp)
        argv = t.serve_argv(free_port() if t.ws_from_port else 0)
        if t.user_data:
            argv.insert(1, f"--user-data-dir={tmp}/profile")
        port = None
        if t.ws_from_port:
            port = int(argv[argv.index("--port") + 1])
        sp = Spawned(argv, env, tmp)
        pid = sp.proc.pid
        sampler = Sampler(pid)
        sampler.start()
        res: dict = {"pages": len(urls)}
        browser = None
        try:
            if t.ws_from_port:
                ws = wait_json_version(sp, port)
                sp.t_listen = time.perf_counter()
            else:
                ws = sp.wait_ws()
            res["listen_ms"] = (sp.t_listen - sp.t0) * 1e3
            time.sleep(settle)
            res["idle_rss_mb"] = tree_rss_mb(pid)
            res["idle_pss_mb"] = tree_pss_mb(pid)
            tc = time.perf_counter()
            browser = pw.chromium.connect_over_cdp(ws)
            res["connect_ms"] = res["listen_ms"] + (time.perf_counter() - tc) * 1e3
            time.sleep(0.2)
            res["idle_connected_rss_mb"] = tree_rss_mb(pid)
            ctx = browser.new_context(no_viewport=True)
            t1 = time.perf_counter()
            pg = ctx.new_page()
            res["first_page_ms"] = (time.perf_counter() - t1) * 1e3
            pg.close()
            np_ms = []
            for _ in range(new_page_reps):
                t1 = time.perf_counter()
                pg = ctx.new_page()
                np_ms.append((time.perf_counter() - t1) * 1e3)
                pg.close()
            res["new_page_ms"] = statistics.median(np_ms)

            write0 = tree_write_bytes(pid)
            cpu0 = tree_cpu_s(pid)
            goto, cycle, verified, errors = [], [], 0, []
            crawl0 = time.perf_counter()
            for url, want in zip(urls, expect):
                t0 = time.perf_counter()
                try:
                    page = ctx.new_page()
                    resp = page.goto(url, wait_until="load", timeout=30000)
                    t_goto = time.perf_counter()
                    got = page.evaluate(f"document.documentElement.getAttribute('{MARK}')")
                    page.close()
                except Exception as e:  # noqa: BLE001 - record and continue
                    errors.append(f"{url}: {type(e).__name__}: {str(e).splitlines()[0][:100]}")
                    continue
                t_end = time.perf_counter()
                goto.append((t_goto - t0) * 1e3)
                cycle.append((t_end - t0) * 1e3)
                if resp is None or resp.status != 200:
                    errors.append(f"{url}: status {None if resp is None else resp.status}")
                elif got == str(want):
                    verified += 1
                else:
                    errors.append(f"{url}: {MARK}={got!r}, want {want}")
            total = time.perf_counter() - crawl0
            res["cpu_s"] = tree_cpu_s(pid) - cpu0
            res["disk_write_bytes"] = tree_write_bytes(pid) - write0
            time.sleep(settle)
            res["after_crawl_rss_mb"] = tree_rss_mb(pid)
            res.update(
                verified=verified, errors=errors[:5], crawl_total_s=total, pages_ms=[round(c, 1) for c in cycle],
                page_median_ms=statistics.median(cycle) if cycle else None, page_p90_ms=pctl(cycle, 90) if cycle else None,
                goto_median_ms=statistics.median(goto) if goto else None, goto_p90_ms=pctl(goto, 90) if goto else None,
            )
            ctx.close()
            browser.close()
            browser = None
        except Exception as e:  # noqa: BLE001
            res["fatal"] = f"{type(e).__name__}: {str(e).splitlines()[0][:200]}; stderr: {' | '.join(sp.tail[-3:])}"
        finally:
            sampler.finish()
            # Headline peak: summed per-process high-water marks (VmHWM) of the live tree, exact for a
            # single process and unaffected by the sampling interval; the plain simultaneous sum is kept too.
            res["peak_rss_mb"] = max(sampler.hwm_mb, sampler.peak_mb)
            res["peak_rss_sampled_mb"] = sampler.peak_mb
            res["peak_pss_mb"] = sampler.peak_pss_mb
            if browser is not None:
                try:
                    browser.close()
                except Exception:  # noqa: BLE001
                    pass
            stop(sp.proc)
        files = [f for f in tree_files(tmp) if not f.startswith("profile")]
        res["disk_files_created"] = len(files) if not t.user_data else None
    return res


# --- realm bench ------------------------------------------------------------

REALM_LINE = re.compile(r"^snapshot: engine ([\d.]+) ms, registry\+define ([\d.]+) ms, first realm ([\d.]+) ms, next realm ([\d.]+) ms", re.M)


def realm_bench(path: Path, runs: int) -> dict | None:
    """Run the lb-webapi `realm_bench` example (release) and parse the snapshot line."""
    if not path.is_file():
        return None
    rows = []
    for _ in range(runs):
        out = subprocess.run([str(path)], capture_output=True, text=True, timeout=300, cwd=tempfile.gettempdir()).stdout
        m = REALM_LINE.search(out)
        if m:
            rows.append([float(x) for x in m.groups()])
    if not rows:
        return None
    keys = ["engine_ms", "define_ms", "first_realm_ms", "next_realm_ms"]
    return {
        "path": str(path.relative_to(ROOT)) if path.is_relative_to(ROOT) else str(path),
        "mtime": dt.datetime.fromtimestamp(path.stat().st_mtime, dt.timezone.utc).isoformat(timespec="seconds"),
        **{k: spread([r[i] for r in rows]) for i, k in enumerate(keys)},
    }


# --- environment ------------------------------------------------------------


def sh(argv: list[str], cwd: Path | None = None) -> str:
    try:
        return subprocess.run(argv, capture_output=True, text=True, timeout=20, cwd=cwd).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for blk in iter(lambda: f.read(1 << 20), b""):
            h.update(blk)
    return h.hexdigest()


def versions(lb: Target, chrome: Target | None, lp: Target | None) -> dict:
    sha = sh(["git", "rev-parse", "--short", "HEAD"], ROOT) or "nogit"
    dirty = bool(sh(["git", "status", "--porcelain", "--untracked-files=no"], ROOT))
    cpu = ""
    try:
        cpu = next(l.split(":", 1)[1].strip() for l in Path("/proc/cpuinfo").read_text().splitlines() if l.startswith("model name"))
    except (OSError, StopIteration):
        pass
    try:
        pw_ver = importlib.metadata.version("playwright")
    except importlib.metadata.PackageNotFoundError:
        pw_ver = "unknown"
    mem_kb = int(re.search(r"MemTotal:\s+(\d+)", Path("/proc/meminfo").read_text()).group(1))
    v = {
        "lightbrowser_git_sha": sha,
        "git_dirty": dirty,
        "lightbrowser_version": sh([lb.binary, "--version"]),
        "lightbrowser_binary_sha256": sha256_file(lb.binary)[:16],
        "lightbrowser_binary_mtime": dt.datetime.fromtimestamp(Path(lb.binary).stat().st_mtime, dt.timezone.utc).isoformat(timespec="seconds"),
        "playwright_python": pw_ver,
        "rustc": sh(["rustc", "--version"], ROOT),
        "python": platform.python_version(),
        "kernel": platform.release(),
        "cpu": cpu,
        "cores": os.cpu_count(),
        "mem_mb": mem_kb // 1024,
        "corpus_sha256": gen_corpus.disk_digest(),
    }
    if chrome:
        v["chromium"] = sh([chrome.binary, "--version"])
    if lp:
        v["lightpanda"] = sh([lp.binary, "version"]) or sh([lp.binary, "--version"]) or "unknown"
        v["lightpanda_binary_sha256"] = sha256_file(lp.binary)
    return v


# --- budgets ----------------------------------------------------------------


def med(runs: list[dict], key: str):
    return spread([r.get(key) for r in runs if r.get(key) is not None])["median"]


def budgets(summary: dict, lb_bin_mb: float, realm: dict | None) -> list[dict]:
    out = []

    def add(name, target, value, ok, note=""):
        out.append({"budget": name, "target": target, "value": value, "status": "skip" if ok is None else ("pass" if ok else "fail"), "note": note})

    cdp = summary.get("cdp:lightbrowser", {})
    fetch = summary.get("fetch:lightbrowser", {})
    idle = cdp.get("idle_rss_mb", {}).get("median")
    add("idle RSS (serve, no client, 1 s after listen)", f"< {BUDGET_IDLE_RSS_MB:g} MB", idle, None if idle is None else idle < BUDGET_IDLE_RSS_MB)
    add("binary size (stripped)", f"< {BUDGET_BINARY_MB:g} MB", round(lb_bin_mb, 2), lb_bin_mb < BUDGET_BINARY_MB)
    if realm:
        first, nxt = realm["first_realm_ms"]["median"], realm["next_realm_ms"]["median"]
        add("realm creation (first / next, snapshot)", f"< {BUDGET_REALM_MS:g} ms", f"{first} / {nxt}", max(first, nxt) < BUDGET_REALM_MS, "lb-webapi example realm_bench")
    else:
        add("realm creation", f"< {BUDGET_REALM_MS:g} ms", None, None, "realm_bench example binary not built")
    files = (cdp.get("disk_files_created", {}) or {}).get("max"), (fetch.get("disk_files_created", {}) or {}).get("max")
    wb = (cdp.get("disk_write_bytes", {}) or {}).get("max")
    if files[0] is None and files[1] is None:
        add("disk writes (default run)", "none", None, None, "not measured")
    else:
        bad = (files[0] or 0) + (files[1] or 0) + (wb or 0)
        add("disk writes (default run)", "none", f"files {files[0]}/{files[1]}, write_bytes {wb}", bad == 0, "files left in cwd/HOME/TMPDIR; /proc/PID/io write_bytes during crawl")
    for mode in ("fetch", "cdp"):
        lb, lp = summary.get(f"{mode}:lightbrowser"), summary.get(f"{mode}:lightpanda")
        for key, label in (("crawl_total_s", "crawl wall time"), ("peak_rss_mb", "peak RSS"), ("cpu_s", "CPU time")):
            name = f"{label} vs Lightpanda ({mode})"
            if not lp or not lb or "fatal" in lp:
                add(name, "<= Lightpanda", lb.get(key, {}).get("median") if lb else None, None, "lightpanda not available on this machine")
            else:
                a, b = lb[key]["median"], lp[key]["median"]
                add(name, "<= Lightpanda", f"{a} vs {b}", a <= b)
    return out


# --- results files ----------------------------------------------------------

REPORT_KEYS = [
    ("crawl_total_s", "crawl total (s)"), ("page_median_ms", "page median (ms)"), ("page_p90_ms", "page p90 (ms)"),
    ("peak_rss_mb", "peak RSS (MB)"), ("peak_pss_mb", "peak PSS (MB, sampled)"), ("cpu_s", "CPU (s)"), ("idle_rss_mb", "idle RSS (MB)"), ("idle_pss_mb", "idle PSS (MB)"),
    ("listen_ms", "start to listening (ms)"), ("connect_ms", "start to CDP connect (ms)"),
    ("first_page_ms", "first new page (ms)"), ("new_page_ms", "new page, median (ms)"),
]


def fmt(s: dict | None) -> str:
    if not s or s.get("median") is None:
        return "-"
    m, lo, hi = s["median"], s["min"], s["max"]
    d = 1 if abs(m) >= 10 else 2
    return f"{m:.{d}f} ({lo:.{d}f}-{hi:.{d}f})"


def render_readme(result: dict, history: list[dict]) -> str:
    s = result["summary"]
    cols = [c for c in ("fetch:lightbrowser", "cdp:lightbrowser", "cdp:chromium", "fetch:lightpanda", "cdp:lightpanda") if c in s]
    v = result["versions"]
    L = [
        "# Bench results", "",
        "Written by `bench/run.py`. Each cell is the median over runs, with min-max in brackets. "
        "Loopback corpus, sequential crawl, see `bench/README.md` for definitions.", "",
        f"Latest: `{result['file']}` -- {result['date']}, LightBrowser `{v['lightbrowser_git_sha']}`"
        f"{' (dirty tree)' if v['git_dirty'] else ''}, binary sha256 `{v['lightbrowser_binary_sha256']}`, "
        f"{result['runs']} runs x {result['pages']} pages.", "",
        f"Machine: {v['cpu']}, {v['cores']} cores, {v['mem_mb']} MB RAM, kernel {v['kernel']}. "
        f"Chromium `{v.get('chromium', 'n/a')}`, Playwright (python) {v['playwright_python']}, {v['rustc']}."
        + (f" Lightpanda `{v['lightpanda']}` (binary sha256 `{v.get('lightpanda_binary_sha256', '?')}`), run with telemetry disabled."
           if v.get("lightpanda") else ""), "",
        "## Crawl", "", "| metric | " + " | ".join(cols) + " |", "|---|" + "---|" * len(cols),
    ]
    for key, label in REPORT_KEYS:
        if any(key in s[c] for c in cols):
            L.append(f"| {label} | " + " | ".join(fmt(s[c].get(key)) for c in cols) + " |")
    L.append("| pages verified | " + " | ".join(f"{s[c].get('verified_min', '-')}/{result['pages']}" for c in cols) + " |")
    L += ["", "Fetch rows time one process per page, so they include process start. "
          "RSS sums over Chromium's processes count shared pages once per process; PSS counts them once. Peak RSS for fetch is the largest single process; for CDP it is the sum of the process tree's own high-water marks (`VmHWM`, read every 50 ms from spawn), which is exact for LightBrowser's single process.", "",
          "## Binary size", "", "| target | MB |", "|---|---|"]
    for name, mb in result["binary_mb"].items():
        L.append(f"| {name} | {mb:.1f} |")
    if result.get("realm_bench"):
        r = result["realm_bench"]
        L += ["", "## Realm creation (snapshot, release, ms)", "",
              "| engine | define | first realm | next realm |", "|---|---|---|---|",
              f"| {fmt(r['engine_ms'])} | {fmt(r['define_ms'])} | {fmt(r['first_realm_ms'])} | {fmt(r['next_realm_ms'])} |"]
    if result.get("skipped"):
        L += ["", "## Skipped", ""] + [f"- {x}" for x in result["skipped"]]
    L += ["", "## Budgets (docs/ARCHITECTURE.md)", "", "| budget | target | measured | result |", "|---|---|---|---|"]
    for b in result["budgets"]:
        L.append(f"| {b['budget']} | {b['target']} | {'-' if b['value'] is None else b['value']} | **{b['status']}**{' - ' + b['note'] if b['note'] else ''} |")
    if len(history) > 1:
        L += ["", "## History", "", "| file | sha | crawl cdp (s) | page median (ms) | peak RSS (MB) | idle RSS (MB) | binary (MB) |", "|---|---|---|---|---|---|---|"]
        for h in history:
            hs = h["summary"].get("cdp:lightbrowser", {})
            L.append(f"| {h['file']} | {h['versions']['lightbrowser_git_sha']} | {fmt(hs.get('crawl_total_s'))} | {fmt(hs.get('page_median_ms'))} | "
                     f"{fmt(hs.get('peak_rss_mb'))} | {fmt(hs.get('idle_rss_mb'))} | {h['binary_mb'].get('lightbrowser', 0):.1f} |")
    return "\n".join(L) + "\n"


# --- main -------------------------------------------------------------------


def find_binary(explicit: str | None) -> str:
    for c in (explicit, os.environ.get("LIGHTBROWSER_BIN"), str(ROOT / "target/release/lightbrowser")):
        if c and Path(c).is_file():
            return c
    sys.exit("no lightbrowser binary: build with `cargo build --release -p lightbrowser` or pass --binary")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--runs", type=int, default=5, help="repetitions per target (default 5)")
    ap.add_argument("--pages", type=int, default=0, help="crawl only the first N corpus pages (default: all)")
    ap.add_argument("--binary", help="lightbrowser binary (default: $LIGHTBROWSER_BIN or target/release/lightbrowser)")
    ap.add_argument("--chromium", default=DEFAULT_CHROMIUM, help="reference Chromium (default: %(default)s)")
    ap.add_argument("--targets", default="fetch:lightbrowser,cdp:lightbrowser,cdp:chromium,fetch:lightpanda,cdp:lightpanda")
    ap.add_argument("--realm-bench", default=str(ROOT / "target/release/examples/realm_bench"),
                    help="lb-webapi realm_bench example binary (release); skipped if missing")
    ap.add_argument("--out-dir", default=str(RESULTS))
    ap.add_argument("--no-write", action="store_true", help="print only, write no result files")
    args = ap.parse_args()

    if not (CORPUS / "manifest.json").is_file():
        gen_corpus.write(gen_corpus.build())
    manifest = json.loads((CORPUS / "manifest.json").read_text())
    if gen_corpus.disk_digest() != manifest["sha256"]:
        sys.exit("bench/corpus does not match its manifest; regenerate with `python3 bench/gen_corpus.py`")
    pages = manifest["pages"][: args.pages or None]
    expect = [p["expected_rendered"] for p in pages]

    lb_bin = find_binary(args.binary)
    lb = lightbrowser_target(lb_bin)
    chrome = chromium_target(args.chromium) if Path(args.chromium).is_file() else None
    lp_path = shutil.which("lightpanda")
    lp = lightpanda_target(lp_path) if lp_path else None
    wanted = args.targets.split(",")
    skipped = []
    if chrome is None and "cdp:chromium" in wanted:
        skipped.append(f"cdp:chromium: {args.chromium} not found")
    if lp is None and any(w.endswith(":lightpanda") for w in wanted):
        skipped.append("fetch/cdp:lightpanda: no `lightpanda` binary on PATH (not available offline); budgets vs Lightpanda are not checked")
    targets = {"fetch:lightbrowser": lb, "cdp:lightbrowser": lb, "cdp:chromium": chrome, "fetch:lightpanda": lp, "cdp:lightpanda": lp}
    plan = [w for w in wanted if targets.get(w) is not None]

    # Everything below talks to 127.0.0.1 only; keep proxies out of the way.
    for k in ("http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "all_proxy"):
        os.environ.pop(k, None)
    os.environ["NO_PROXY"] = os.environ["no_proxy"] = "127.0.0.1,localhost,::1"
    os.environ["LIGHTPANDA_DISABLE_TELEMETRY"] = "true"  # also for `lightpanda version` below

    server = Server()
    pw_ctx = None
    pw = None
    if any(w.startswith("cdp:") for w in plan):
        from playwright.sync_api import sync_playwright

        pw_ctx = sync_playwright()
        pw = pw_ctx.start()
    print(f"corpus {len(pages)} pages at {server.base}; lightbrowser {lb_bin}; targets {', '.join(plan)}; {args.runs} runs", flush=True)
    urls = [server.base + p["path"] for p in pages]
    runs: dict[str, list[dict]] = {k: [] for k in plan}
    try:
        for rep in range(args.runs):
            for name in plan:
                t = targets[name]
                mode = name.split(":")[0]
                r = run_fetch(t, urls, expect) if mode == "fetch" else run_cdp(t, pw, urls, expect)
                runs[name].append(r)
                head = f"[run {rep + 1}/{args.runs}] {name:20s}"
                if "fatal" in r:
                    print(f"{head} FAILED: {r['fatal']}", flush=True)
                else:
                    print(f"{head} total {r['crawl_total_s']:.2f}s  page med {r['page_median_ms']:.1f} ms  p90 {r['page_p90_ms']:.1f} ms  "
                          f"peak {r['peak_rss_mb']:.0f} MB  cpu {r['cpu_s']:.2f}s  verified {r['verified']}/{r['pages']}", flush=True)
                    for e in r["errors"][:2]:
                        print(f"      {e}", flush=True)
    finally:
        if pw_ctx:
            try:
                pw_ctx.stop()
            except Exception:  # noqa: BLE001
                pass
        server.close()

    summary: dict[str, dict] = {}
    for name, rs in runs.items():
        good = [r for r in rs if "fatal" not in r]
        keys = {k for r in good for k, v in r.items() if isinstance(v, (int, float)) and not isinstance(v, bool)}
        summary[name] = {k: spread([r.get(k) for r in good]) for k in sorted(keys)}
        summary[name]["verified_min"] = min((r["verified"] for r in good), default=0)
        if len(good) != len(rs):
            summary[name]["fatal"] = [r["fatal"] for r in rs if "fatal" in r][:3]
    realm = realm_bench(Path(args.realm_bench), args.runs)
    if realm is None:
        skipped.append("realm_bench example binary missing or unreadable: realm creation budget not checked")
    binary_mb = {"lightbrowser": lb.size_mb}
    if chrome:
        binary_mb["chromium (chrome binary only)"] = chrome.size_mb
    if lp:
        binary_mb["lightpanda"] = lp.size_mb
    ver = versions(lb, chrome, lp)
    today = dt.date.today().isoformat()
    fname = f"{today}-{ver['lightbrowser_git_sha']}.json"
    result = {
        "file": fname, "date": today, "runs": args.runs, "pages": len(pages), "versions": ver,
        "binary_mb": {k: round(v, 2) for k, v in binary_mb.items()}, "realm_bench": realm,
        "summary": summary, "skipped": skipped, "raw_runs": runs,
        "budgets": budgets(summary, lb.size_mb, realm),
    }
    bad_verify = [n for n in plan if n.endswith(("lightbrowser", "chromium")) and summary[n].get("verified_min", 0) < len(pages)]

    print("\nBudgets:")
    for b in result["budgets"]:
        print(f"  [{b['status'].upper():4s}] {b['budget']}: {b['value']} (target {b['target']}){' - ' + b['note'] if b['note'] else ''}")
    for s in skipped:
        print(f"  note: {s}")
    for n in bad_verify:
        print(f"  WARNING: {n} did not render every page ({summary[n].get('verified_min')}/{len(pages)})")

    if not args.no_write:
        out = Path(args.out_dir)
        out.mkdir(parents=True, exist_ok=True)
        (out / fname).write_text(json.dumps(result, indent=1, sort_keys=True) + "\n")
        history = [json.loads(p.read_text()) for p in sorted(out.glob("*.json"))]
        (out / "README.md").write_text(render_readme(result, history))
        print(f"\nwrote {out / fname} and {out / 'README.md'}")
    return 1 if any(b["status"] == "fail" for b in result["budgets"]) or bad_verify else 0


if __name__ == "__main__":
    sys.exit(main())

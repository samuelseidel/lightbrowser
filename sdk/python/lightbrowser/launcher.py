"""Find and run `lightbrowser serve`.

The SDK starts the binary on 127.0.0.1, waits for the line Chrome also
prints (`DevTools listening on ws://...`) on stderr, and hands the
WebSocket URL to Playwright. Nothing here imports Playwright.
"""

from __future__ import annotations

import collections
import os
import re
import shutil
import subprocess
import tempfile
import threading
from pathlib import Path
from typing import Sequence

from .errors import BinaryNotFoundError, LaunchError
from .loopback import assert_loopback

ENV_BIN = "LIGHTBROWSER_BIN"

_WS_LINE = re.compile(r"DevTools listening on (wss?://\S+)")
_VERSION = re.compile(r"\((chrome|firefox)\)")


def parse_devtools_line(line: str) -> str | None:
    """The `ws://...` URL in a "DevTools listening on" line, or None."""
    m = _WS_LINE.search(line)
    return m.group(1) if m else None


def binary_family(exe: str | os.PathLike[str], timeout: float = 10.0) -> str | None:
    """The family a binary was compiled for (`chrome` or `firefox`), from `--version`."""
    try:
        out = subprocess.run(
            [str(exe), "--version"], capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    m = _VERSION.search(out)
    return m.group(1) if m else None


def _checkout_roots() -> list[Path]:
    """cwd and its parents, then the checkout this SDK lives in."""
    here = Path(__file__).resolve()
    roots = [Path.cwd(), *Path.cwd().parents]
    roots += [p for p in here.parents if p not in roots]
    return roots


def candidate_binaries() -> list[Path]:
    """`target/release` before `target/debug`, nearest checkout first."""
    out: list[Path] = []
    for root in _checkout_roots():
        for profile in ("release", "debug"):
            p = root / "target" / profile / "lightbrowser"
            if p.is_file() and os.access(p, os.X_OK) and p not in out:
                out.append(p)
    return out


def find_binary(explicit: str | os.PathLike[str] | None = None, family: str | None = None) -> str:
    """Locate the `lightbrowser` executable.

    Order: `explicit`, `$LIGHTBROWSER_BIN`, `./target/{release,debug}/lightbrowser`
    (cwd and its parents, then the checkout holding this SDK), then `$PATH`.
    An explicit path or the environment variable is used as given. For the
    searched locations, `family` skips binaries built for the other engine:
    both families write `target/release/lightbrowser`, and the last build wins.
    """
    if explicit:
        p = Path(explicit)
        if not p.is_file():
            raise BinaryNotFoundError(f"lightbrowser executable not found: {p}")
        return str(p)
    env = os.environ.get(ENV_BIN)
    if env:
        if not Path(env).is_file():
            raise BinaryNotFoundError(f"${ENV_BIN} points to a missing file: {env}")
        return env
    cands = [str(p) for p in candidate_binaries()]
    which = shutil.which("lightbrowser")
    if which and which not in cands:
        cands.append(which)
    wrong: list[str] = []
    for c in cands:
        if family is None:
            return c
        fam = binary_family(c)
        if fam == family:
            return c
        wrong.append(f"{c} ({fam or 'unknown'})")
    detail = f"; skipped, wrong family: {', '.join(wrong)}" if wrong else ""
    raise BinaryNotFoundError(
        f"no lightbrowser executable found. Build one (`cargo build -p lightbrowser`), "
        f"set ${ENV_BIN}, or pass executable_path{detail}"
    )


def build_serve_args(
    binary: str,
    *,
    persona_path: str | os.PathLike[str] | None = None,
    port: int = 0,
    insecure: bool = False,
    profiles_dir: str | os.PathLike[str] | None = None,
    workers: int | None = None,
    extra_args: Sequence[str] = (),
) -> list[str]:
    """The command line for `lightbrowser serve`. The server always binds 127.0.0.1."""
    if not 0 <= int(port) <= 65535:
        raise ValueError(f"port out of range: {port}")
    cmd = [str(binary), "serve", "--port", str(int(port))]
    if persona_path is not None:
        cmd += ["--persona", str(persona_path)]
    if profiles_dir is not None:
        cmd += ["--profiles-dir", str(profiles_dir)]
    if insecure:
        cmd.append("--insecure")
    if workers is not None:
        if workers < 1:
            raise ValueError("workers must be >= 1")
        cmd += ["--workers", str(int(workers))]
    cmd += [str(a) for a in extra_args]
    return cmd


class ServerProcess:
    """A running `lightbrowser serve`. Terminated by `close()`.

    A thread keeps draining stderr after the endpoint line, so the child
    never blocks on a full pipe; the last lines are kept for error messages.
    """

    def __init__(self, cmd: Sequence[str], *, startup_timeout: float = 60.0, env: dict[str, str] | None = None) -> None:
        self.cmd = list(cmd)
        self.ws_endpoint: str = ""
        self._tail: collections.deque[str] = collections.deque(maxlen=200)
        self._found = threading.Event()
        self._closed = False
        try:
            self.proc = subprocess.Popen(
                self.cmd,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
                errors="replace",
                env=env,
            )
        except OSError as e:
            raise LaunchError(f"cannot start {self.cmd[0]}: {e}") from e
        self._reader = threading.Thread(target=self._drain, name="lightbrowser-stderr", daemon=True)
        self._reader.start()
        try:
            self._wait_ready(startup_timeout)
        except BaseException:
            self.close()
            raise

    # -- startup --------------------------------------------------------------

    def _drain(self) -> None:
        assert self.proc.stderr is not None
        try:
            for line in self.proc.stderr:
                self._tail.append(line.rstrip("\n"))
                if not self._found.is_set():
                    ws = parse_devtools_line(line)
                    if ws:
                        self.ws_endpoint = ws
                        self._found.set()
        except (OSError, ValueError):
            pass
        finally:
            self._found.set()  # EOF: wake the waiter so it can report the exit

    def _wait_ready(self, timeout: float) -> None:
        if not self._found.wait(timeout):
            raise LaunchError(f"no 'DevTools listening on' line within {timeout:g}s\n{self.stderr_tail()}")
        if not self.ws_endpoint:
            code = self.proc.wait(timeout=5)
            raise LaunchError(f"lightbrowser exited with status {code} before it was ready\n{self.stderr_tail()}")
        # Only ever hand out a loopback endpoint.
        assert_loopback(self.ws_endpoint, "CDP endpoint")

    # -- state ----------------------------------------------------------------

    @property
    def pid(self) -> int:
        return self.proc.pid

    @property
    def port(self) -> int:
        m = re.match(r"wss?://[^/]*:(\d+)/", self.ws_endpoint)
        return int(m.group(1)) if m else 0

    def alive(self) -> bool:
        return self.proc.poll() is None

    def stderr_tail(self, n: int = 20) -> str:
        return "\n".join(list(self._tail)[-n:])

    # -- shutdown -------------------------------------------------------------

    def close(self, grace: float = 5.0) -> None:
        """Terminate the process (SIGTERM, then SIGKILL after `grace`). Idempotent."""
        if self._closed:
            return
        self._closed = True
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(grace)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self._reader.join(timeout=2)
        if self.proc.stderr is not None:
            self.proc.stderr.close()

    def __enter__(self) -> "ServerProcess":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


def make_tempdir() -> tempfile.TemporaryDirectory[str]:
    """A temp dir the SDK removes on close (for in-memory personas)."""
    return tempfile.TemporaryDirectory(prefix="lightbrowser-sdk-")

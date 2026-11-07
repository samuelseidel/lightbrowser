"""Launch preparation shared by the sync and async Browser classes."""

from __future__ import annotations

import os
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Sequence

from .launcher import build_serve_args, find_binary
from .persona import Persona


@dataclass
class LaunchPlan:
    """Everything needed to start the server, plus a temp dir to clean up."""

    cmd: list[str]
    persona: Persona | None
    tmpdir: tempfile.TemporaryDirectory[str] | None

    def cleanup(self) -> None:
        if self.tmpdir is not None:
            self.tmpdir.cleanup()
            self.tmpdir = None


def as_persona(persona: "Persona | Mapping | str | os.PathLike | None") -> Persona | None:
    if persona is None or isinstance(persona, Persona):
        return persona
    if isinstance(persona, Mapping):
        return Persona(persona)
    return Persona.load(persona)


def plan_launch(
    *,
    persona: "Persona | Mapping | str | os.PathLike | None",
    executable_path: str | os.PathLike[str] | None,
    port: int,
    insecure: bool,
    profiles_dir: str | os.PathLike[str] | None,
    workers: int | None,
    extra_args: Sequence[str],
    validate: bool,
) -> LaunchPlan:
    """Resolve binary and persona and build the `serve` command line.

    A persona given as a dict is written to a private temp dir that
    `LaunchPlan.cleanup()` removes. With `validate=True` the persona goes
    through `lightbrowser validate` first, so a bad file fails here with
    the rule text instead of as a bare startup error.
    """
    p = as_persona(persona)
    family = p.family if p is not None else "chrome"
    exe = find_binary(executable_path, family=family)
    if p is not None and validate:
        p.validate(exe)
    tmpdir: tempfile.TemporaryDirectory[str] | None = None
    path: Path | None = None
    if p is not None:
        if p.path is not None:
            path = p.path
        else:
            tmpdir = tempfile.TemporaryDirectory(prefix="lightbrowser-sdk-")
            path = p.write_to(tmpdir.name)
    cmd = build_serve_args(
        exe, persona_path=path, port=port, insecure=insecure, profiles_dir=profiles_dir,
        workers=workers, extra_args=extra_args,
    )
    return LaunchPlan(cmd=cmd, persona=p, tmpdir=tmpdir)


# Playwright for Python 1.52 puts `__pwInitScripts` and `__playwright_builtins__`
# into the page's main world; the detect-bench rule `automation_keys` (weight
# 90) and the fingerprint report's `noTells.mainWorldGlobals` then fire.
# Playwright 1.63 does not (tests/playwright runs on it with a clean main world).
# Versions in between were not checked.
CLEAN_PLAYWRIGHT = (1, 63)


def playwright_version() -> tuple[int, ...] | None:
    from importlib import metadata

    try:
        parts = metadata.version("playwright").split(".")
        return tuple(int(p) for p in parts[:3] if p.isdigit())
    except (metadata.PackageNotFoundError, ValueError):
        return None


def leaky_playwright() -> bool:
    """True when the installed Playwright is older than the first version verified clean."""
    v = playwright_version()
    return v is not None and v < CLEAN_PLAYWRIGHT


_warned = False


def warn_if_leaky_playwright() -> None:
    """Warn once per process about a Playwright that exposes globals to the page."""
    global _warned
    if _warned or not leaky_playwright():
        return
    _warned = True
    import warnings

    v = ".".join(str(p) for p in playwright_version() or ())
    warnings.warn(
        f"Playwright {v} defines __pwInitScripts and __playwright_builtins__ in the page's main "
        f"world, which any detection script can see (detect-bench: automation_keys, +90). "
        f"Use Playwright >= {'.'.join(map(str, CLEAN_PLAYWRIGHT))}.",
        UserWarning,
        stacklevel=3,
    )

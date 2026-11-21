"""Persona files: load, inspect, validate.

A persona is the JSON file that defines everything the browser presents
(UA, screen, GPU, fonts, seeds, TLS profile, behaviour profile). The
binary owns the schema and the consistency rules; this class only reads
the file for a few display fields and asks `lightbrowser validate` for the
verdict, so the SDK never disagrees with the browser.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping

from .errors import PersonaValidationError
from .launcher import find_binary


@dataclass(frozen=True)
class ValidationReport:
    """Result of `lightbrowser validate` for one persona."""

    ok: bool
    warnings: int
    violations: int
    output: str

    def __str__(self) -> str:
        return self.output


_VIOLATIONS = re.compile(r"INVALID:\s*(\d+) violation\(s\),\s*(\d+) warning\(s\)")
_WARNINGS = re.compile(r"OK:\s*(\d+) warning\(s\)")


def parse_validate_output(output: str, returncode: int) -> ValidationReport:
    """Turn the text printed by `lightbrowser validate` into a report."""
    violations = warnings = 0
    m = _VIOLATIONS.search(output)
    if m:
        violations, warnings = int(m.group(1)), int(m.group(2))
    else:
        m = _WARNINGS.search(output)
        if m:
            warnings = int(m.group(1))
    # A load error ("ERROR: ...") has no summary line but exits non-zero.
    ok = returncode == 0 and violations == 0 and "ERROR:" not in output
    return ValidationReport(ok=ok, warnings=warnings, violations=violations, output=output.strip())


def repo_reference_dir() -> Path | None:
    """`profiles/reference` of the checkout this SDK lives in, if any."""
    for start in (Path(__file__).resolve().parent, Path.cwd()):
        for d in (start, *start.parents):
            cand = d / "profiles" / "reference"
            if cand.is_dir():
                return cand
    return None


class Persona:
    """A persona, from a file or an in-memory dict.

    >>> p = Persona.load("profiles/reference/chrome/chrome153_win10_rtx3060_1080p.json")
    >>> p.family, p.os_name
    ('chrome', 'Windows')
    >>> p.validate()            # raises PersonaValidationError on a violation
    """

    def __init__(self, data: Mapping[str, Any], path: str | os.PathLike[str] | None = None) -> None:
        self.data: dict[str, Any] = dict(data)
        self.path: Path | None = Path(path) if path is not None else None

    # -- construction -------------------------------------------------------

    @classmethod
    def load(cls, path: str | os.PathLike[str]) -> "Persona":
        """Read a persona JSON file. Does not validate; call `validate()`."""
        p = Path(path)
        try:
            data = json.loads(p.read_text(encoding="utf-8"))
        except (OSError, ValueError) as e:
            raise PersonaValidationError(f"cannot read persona {p}: {e}") from e
        if not isinstance(data, dict):
            raise PersonaValidationError(f"persona {p} is not a JSON object")
        return cls(data, p)

    @classmethod
    def reference(cls, name: str, family: str = "chrome") -> "Persona":
        """A reference persona shipped in the repo's `profiles/reference/<family>/`.

        `name` is the file stem, for example `chrome153_win10_rtx3060_1080p`.
        """
        root = repo_reference_dir()
        if root is None:
            raise PersonaValidationError("no profiles/reference directory found above the SDK or the cwd")
        return cls.load(root / family / f"{name}.json")

    @classmethod
    def references(cls, family: str = "chrome") -> list[Path]:
        """Paths of the reference personas for `family` (empty if none found)."""
        root = repo_reference_dir()
        d = root / family if root else None
        return sorted(d.glob("*.json")) if d and d.is_dir() else []

    # -- fields (display only; the binary is the authority) -------------------

    @property
    def profile_id(self) -> str:
        return str(self.data.get("profile_id", ""))

    @property
    def name(self) -> str:
        return str(self.data.get("profile_name", ""))

    @property
    def family(self) -> str:
        return str(self.data.get("browser", {}).get("family") or "chrome")

    @property
    def os_name(self) -> str:
        return str(self.data.get("os", {}).get("name", ""))

    @property
    def user_agent(self) -> str:
        return str(self.data.get("browser", {}).get("user_agent", ""))

    # -- validation -----------------------------------------------------------

    def check(self, binary: str | os.PathLike[str] | None = None, timeout: float = 30.0) -> ValidationReport:
        """Run `lightbrowser validate` and return its report (never raises on violations)."""
        exe = find_binary(binary, family=self.family)
        if self.path is not None:
            return _run_validate(exe, self.path, timeout)
        with tempfile.TemporaryDirectory(prefix="lb-persona-") as d:
            tmp = Path(d) / "persona.json"
            tmp.write_text(json.dumps(self.data), encoding="utf-8")
            return _run_validate(exe, tmp, timeout)

    def validate(self, binary: str | os.PathLike[str] | None = None, timeout: float = 30.0) -> ValidationReport:
        """Like `check`, but raise `PersonaValidationError` when the persona is not valid."""
        rep = self.check(binary, timeout)
        if not rep.ok:
            raise PersonaValidationError(f"persona {self.path or self.profile_id or '<dict>'} is invalid", rep.output)
        return rep

    # -- helpers for the launcher -------------------------------------------

    def write_to(self, directory: str | os.PathLike[str]) -> Path:
        """Write the persona into `directory` and return the file path."""
        out = Path(directory) / "persona.json"
        out.write_text(json.dumps(self.data), encoding="utf-8")
        return out

    def __repr__(self) -> str:
        return f"Persona({self.profile_id or '?'}, family={self.family!r}, path={str(self.path) if self.path else None!r})"


def _run_validate(exe: str, path: Path, timeout: float) -> ValidationReport:
    proc = subprocess.run(
        [exe, "validate", str(path)],
        capture_output=True,
        text=True,
        timeout=timeout,
        stdin=subprocess.DEVNULL,
    )
    return parse_validate_output(proc.stdout + proc.stderr, proc.returncode)

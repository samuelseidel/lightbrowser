"""Result of `LightBrowser.fingerprintReport`: the live fingerprint surface vs the persona."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Mapping


@dataclass(frozen=True)
class Check:
    """One compared value. `status` is `ok`, `mismatch`, `info` or `unimplemented`."""

    name: str
    status: str
    expected: Any = None
    actual: Any = None


@dataclass
class FingerprintReport:
    """What the page sees (JS surface) compared with what the persona says.

    `ok` is True when no check is a mismatch. `summary` holds the counts per
    status; `surface` is the raw values the page reported.
    """

    ok: bool
    persona: str = ""
    family: str = ""
    url: str = ""
    summary: dict[str, int] = field(default_factory=dict)
    checks: list[Check] = field(default_factory=list)
    surface: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "FingerprintReport":
        checks = [
            Check(
                name=str(c.get("name", "")),
                status=str(c.get("status", "")),
                expected=c.get("expected"),
                actual=c.get("actual"),
            )
            for c in data.get("checks", [])
            if isinstance(c, Mapping)
        ]
        return cls(
            ok=bool(data.get("ok", False)),
            persona=str(data.get("persona", "")),
            family=str(data.get("family", "")),
            url=str(data.get("url", "")),
            summary={str(k): int(v) for k, v in dict(data.get("summary", {})).items()},
            checks=checks,
            surface=dict(data.get("surface", {})),
        )

    @property
    def passed(self) -> bool:
        return self.ok

    @property
    def mismatches(self) -> list[Check]:
        return [c for c in self.checks if c.status == "mismatch"]

    @property
    def unimplemented(self) -> list[Check]:
        return [c for c in self.checks if c.status == "unimplemented"]

    def __str__(self) -> str:
        s = self.summary
        head = (
            f"LightBrowser fingerprint report ({self.family or '?'}, persona {self.persona or '?'}): "
            f"{'OK' if self.ok else 'MISMATCH'}: "
            f"{s.get('ok', 0)} ok, {s.get('mismatch', 0)} mismatch, "
            f"{s.get('unimplemented', 0)} unimplemented, {s.get('info', 0)} info"
        )
        lines = [head]
        for c in self.mismatches + self.unimplemented:
            lines.append(f"  {c.status}: {c.name}: expected {c.expected!r}, got {c.actual!r}")
        return "\n".join(lines)

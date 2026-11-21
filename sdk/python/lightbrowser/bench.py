"""Client for the local detect-bench server (`tests/detect-bench`, ADR 0007).

Loopback only: the base URL is checked on construction and bypasses any
HTTP proxy from the environment.
"""

from __future__ import annotations

import json
import re
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Any

from .errors import LightBrowserError
from .loopback import assert_loopback

_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


class BenchError(LightBrowserError):
    """The bench server is unreachable or answered with an error."""


@dataclass
class Score:
    """A scored session from `GET /api/score/<sid>`."""

    sid: str
    score: float
    verdict: str
    by_category: dict[str, float]
    fired: list[dict[str, Any]] = field(default_factory=list)
    signals: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> "Score":
        return cls(
            sid=str(d.get("sid", "")),
            score=float(d.get("score", 0)),
            verdict=str(d.get("verdict", "")),
            by_category={k: float(v) for k, v in dict(d.get("by_category", {})).items()},
            fired=list(d.get("fired", [])),
            signals=dict(d.get("signals", {})),
        )

    def part(self, behavior: bool) -> float:
        """Combined score of the behaviour rules only, or of all the others (the bench's `beh` and `fp`)."""
        keep = 1.0
        for f in self.fired:
            if (f.get("cat") == "behavior") == behavior:
                keep *= 1 - min(float(f.get("weight", 0)), 99) / 100
        return round(1000 * (1 - keep)) / 10

    @property
    def fp(self) -> float:
        return self.part(False)

    @property
    def beh(self) -> float:
        return self.part(True)

    def metric(self, name: str) -> float:
        if name == "score":
            return self.score
        if name == "fp":
            return self.fp
        if name == "beh":
            return self.beh
        if name in self.by_category:
            return self.by_category[name]
        raise KeyError(name)

    def top_rules(self, n: int = 6) -> str:
        return " ".join(f"{f['id']}({f['weight']})" for f in self.fired[:n])


class Bench:
    """Talks to a running `tests/detect-bench/server.py`."""

    def __init__(self, base: str, timeout: float = 10.0) -> None:
        self.base = assert_loopback(base, "bench URL").rstrip("/")
        self.timeout = timeout

    def _get(self, path: str) -> Any:
        url = f"{self.base}{path}"
        try:
            with _OPENER.open(url, timeout=self.timeout) as r:
                return json.load(r)
        except (urllib.error.URLError, OSError, ValueError) as e:
            raise BenchError(f"{url}: {e}") from e

    def check_reachable(self) -> None:
        self._get("/api/results")

    def score(self, sid: str) -> Score:
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,64}", sid):
            raise BenchError(f"bad session id {sid!r}")
        return Score.from_dict(self._get(f"/api/score/{sid}"))

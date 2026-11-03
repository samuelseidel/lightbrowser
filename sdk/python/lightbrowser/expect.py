"""Expectations on a bench score, same grammar as `tests/detect-bench/run.js --expect`.

    ">=90"                 total score at least 90
    "fp<=30&beh<=30"       non-behaviour rules at most 30 and behaviour rules at most 30
    "beh>=70&fp<=30"
    "server<=10"           any bench category: automation, consistency, fingerprint, behavior, server
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from .bench import Score

_PART = re.compile(r"^(?P<m>[a-z]+)?\s*(?P<op>>=|<=)\s*(?P<v>\d+(?:\.\d+)?)$")


@dataclass(frozen=True)
class Expectation:
    text: str
    terms: tuple[tuple[str, str, float], ...]

    @classmethod
    def parse(cls, text: str) -> "Expectation":
        terms = []
        for part in text.split("&"):
            m = _PART.match(part.strip())
            if not m:
                raise ValueError(f"bad expectation {text!r} (use e.g. '>=90', 'fp<=30&beh<=30')")
            terms.append((m["m"] or "score", m["op"], float(m["v"])))
        return cls(text, tuple(terms))

    def check(self, score: Score) -> bool:
        for metric, op, bound in self.terms:
            got = score.metric(metric)
            if (op == ">=" and got < bound) or (op == "<=" and got > bound):
                return False
        return True

    def describe(self, score: Score) -> str:
        return ", ".join(f"{m}={score.metric(m):g}" for m, _, _ in self.terms)

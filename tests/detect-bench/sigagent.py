"""Signed-agent declarations: RFC 9421 style `Signature-Agent`, `Signature-Input`
and `Signature` headers, as a defender sees them.

An agent that says who it is (the Web Bot Auth drafts build on RFC 9421) is a
different thing from a bot that hides: the bench reports such a session as
`agent_declared` and gives it the verdict `declared_agent`. This module only
*recognises* a well-formed declaration. It checks no signature: the bench has
no key directory, so `verified` is always false.

The headers are Structured Fields (RFC 8941). `parse_dictionary` and
`parse_item` are a strict parser for the part of that grammar these headers
use (strings, tokens, integers, decimals, booleans, byte sequences, inner
lists, parameters). Anything malformed raises `SFError` and the declaration is
not accepted.

    python3 -m unittest test_sigagent
"""

from __future__ import annotations

import base64
import binascii
import re
from urllib.parse import urlsplit

MAX_HEADER = 4096
MAX_URL = 2048

_KEY_FIRST = re.compile(r"[a-z*]")
_KEY_REST = re.compile(r"[a-z0-9_\-.*]")
_TCHAR = re.compile(r"[A-Za-z0-9!#$%&'*+\-.^_`|~]")
_B64 = re.compile(r"[A-Za-z0-9+/=]*")


class SFError(ValueError):
    """Not a valid Structured Field."""


class Token(str):
    """A Structured Fields token (as opposed to a string)."""


class Bytes(bytes):
    """A Structured Fields byte sequence."""


class _Reader:
    def __init__(self, text: str):
        self.s = text
        self.i = 0

    def peek(self) -> str:
        return self.s[self.i] if self.i < len(self.s) else ""

    def eat(self, ch: str) -> bool:
        if self.peek() == ch:
            self.i += 1
            return True
        return False

    def skip_sp(self) -> None:
        while self.peek() == " ":
            self.i += 1

    def skip_ows(self) -> None:
        while self.peek() in (" ", "\t") and self.peek():
            self.i += 1

    def end(self) -> bool:
        return self.i >= len(self.s)

    def key(self) -> str:
        c = self.peek()
        if not c or not _KEY_FIRST.fullmatch(c):
            raise SFError(f"bad key at {self.i}")
        start = self.i
        self.i += 1
        while self.peek() and _KEY_REST.fullmatch(self.peek()):
            self.i += 1
        return self.s[start:self.i]

    def bare_item(self):
        c = self.peek()
        if c == '"':
            return self.string()
        if c == ":":
            return self.byte_sequence()
        if c == "?":
            self.i += 1
            if self.eat("1"):
                return True
            if self.eat("0"):
                return False
            raise SFError("bad boolean")
        if c == "-" or c.isdigit():
            return self.number()
        if c and (c.isalpha() and c.isascii() or c == "*"):
            start = self.i
            self.i += 1
            while self.peek() and (_TCHAR.fullmatch(self.peek()) or self.peek() in ":/"):
                self.i += 1
            return Token(self.s[start:self.i])
        raise SFError(f"bad item at {self.i}")

    def string(self) -> str:
        self.i += 1  # opening quote
        out = []
        while True:
            c = self.peek()
            if not c:
                raise SFError("unterminated string")
            self.i += 1
            if c == "\\":
                n = self.peek()
                if n not in ('"', "\\"):
                    raise SFError("bad escape")
                self.i += 1
                out.append(n)
            elif c == '"':
                return "".join(out)
            elif not (0x20 <= ord(c) <= 0x7E):
                raise SFError("non-printable character in string")
            else:
                out.append(c)

    def byte_sequence(self) -> Bytes:
        self.i += 1
        end = self.s.find(":", self.i)
        if end < 0:
            raise SFError("unterminated byte sequence")
        raw = self.s[self.i:end]
        self.i = end + 1
        if not _B64.fullmatch(raw):
            raise SFError("byte sequence is not base64")
        try:
            # Parsers must accept missing padding (RFC 8941 4.2.7).
            return Bytes(base64.b64decode(raw + "=" * (-len(raw) % 4), validate=True))
        except (binascii.Error, ValueError) as e:
            raise SFError(f"byte sequence: {e}") from None

    def number(self):
        start = self.i
        self.eat("-")
        digits = 0
        while self.peek().isdigit():
            self.i += 1
            digits += 1
        if not digits:
            raise SFError("bad number")
        if self.eat("."):
            frac = 0
            while self.peek().isdigit():
                self.i += 1
                frac += 1
            if not 1 <= frac <= 3 or digits > 12:
                raise SFError("bad decimal")
            return float(self.s[start:self.i])
        if digits > 15:
            raise SFError("integer too long")
        return int(self.s[start:self.i])

    def parameters(self) -> dict:
        params: dict = {}
        while self.peek() == ";":
            self.i += 1
            self.skip_sp()
            k = self.key()
            params[k] = self.bare_item() if self.eat("=") else True
        return params

    def item(self):
        v = self.bare_item()
        return v, self.parameters()

    def inner_list(self):
        if not self.eat("("):
            raise SFError("expected (")
        items = []
        while True:
            self.skip_sp()
            if self.eat(")"):
                return items, self.parameters()
            if self.end():
                raise SFError("unterminated inner list")
            items.append(self.item())
            if self.peek() not in (" ", ")"):
                raise SFError("inner list members need a space between them")

    def item_or_inner_list(self):
        return self.inner_list() if self.peek() == "(" else self.item()


def parse_item(text: str):
    """One Item: (value, params). Raises SFError."""
    if len(text) > MAX_HEADER:
        raise SFError("header too long")
    r = _Reader(text.strip(" "))
    out = r.item()
    if not r.end():
        raise SFError("trailing characters")
    return out


def parse_dictionary(text: str) -> dict:
    """A Dictionary: key -> (value | list of (value, params), params). Raises SFError."""
    if len(text) > MAX_HEADER:
        raise SFError("header too long")
    r = _Reader(text.strip(" "))
    out: dict = {}
    if r.end():
        raise SFError("empty dictionary")
    while True:
        k = r.key()
        if r.eat("="):
            out[k] = r.item_or_inner_list()
        else:
            out[k] = (True, r.parameters())
        r.skip_ows()
        if r.end():
            return out
        if not r.eat(","):
            raise SFError(f"expected , at {r.i}")
        r.skip_ows()
        if r.end():
            raise SFError("trailing comma")


def _agent_url(value) -> str | None:
    if not isinstance(value, str) or isinstance(value, Bytes) or not value or len(value) > MAX_URL:
        return None
    if any(c.isspace() or ord(c) < 0x21 or ord(c) > 0x7E for c in value):
        return None
    try:
        u = urlsplit(value)
        host = u.hostname
        u.port  # noqa: B018 - raises ValueError for a bad port
    except ValueError:
        return None
    if u.scheme not in ("http", "https") or not host or u.username is not None or u.password is not None:
        return None
    return value


def _signature_agent(raw: str, label: str):
    """The agent URL from a Signature-Agent value: a string or token item, or
    a dictionary of labelled URLs (the member for `label`, else the only one)."""
    try:
        v, _ = parse_item(raw)
        return _agent_url(v) if isinstance(v, str) else None
    except SFError:
        pass
    try:
        d = parse_dictionary(raw)
    except SFError:
        return None
    cands = {k: m[0] for k, m in d.items() if isinstance(m[0], str)}
    if label in cands:
        return _agent_url(cands[label])
    return _agent_url(next(iter(cands.values()))) if len(cands) == 1 else None


def parse(headers: dict) -> tuple[dict | None, list[str]]:
    """Recognise a signed-agent declaration in `headers` (lowercase names).

    Returns (declaration, problems). The declaration is the first well-formed
    signature label that has all of: a Signature-Input member that is an inner
    list of string component identifiers with a string `keyid` and an integer
    `created` (an integer `expires` is optional and may not precede it), a
    `Signature` member of the same label that is a byte sequence, and a
    `Signature-Agent` naming an http(s) URL. `problems` says what was wrong
    when there is none. Nothing is verified.
    """
    problems: list[str] = []
    raw = {k: headers.get(k) for k in ("signature-agent", "signature-input", "signature")}
    absent = [k for k, v in raw.items() if not v]
    if len(absent) == 3:
        return None, []
    if absent:
        return None, [f"missing {', '.join(absent)}"]
    try:
        inputs = parse_dictionary(raw["signature-input"])
    except SFError as e:
        return None, [f"Signature-Input: {e}"]
    try:
        sigs = parse_dictionary(raw["signature"])
    except SFError as e:
        return None, [f"Signature: {e}"]
    for label, member in inputs.items():
        value, params = member
        if not isinstance(value, list):
            problems.append(f"{label}: Signature-Input member is not an inner list")
            continue
        comps = []
        for item in value:
            if not isinstance(item[0], str) or isinstance(item[0], Token) or not item[0]:
                comps = None
                break
            comps.append(item[0])
        if comps is None:
            problems.append(f"{label}: component identifiers must be strings")
            continue
        keyid, created, expires = params.get("keyid"), params.get("created"), params.get("expires")
        if not isinstance(keyid, str) or isinstance(keyid, Token) or not keyid:
            problems.append(f"{label}: no keyid")
            continue
        if not isinstance(created, int) or isinstance(created, bool):
            problems.append(f"{label}: no integer created")
            continue
        if expires is not None and (not isinstance(expires, int) or isinstance(expires, bool) or expires < created):
            problems.append(f"{label}: bad expires")
            continue
        sig = sigs.get(label)
        if sig is None or not isinstance(sig[0], Bytes) or not len(sig[0]):
            problems.append(f"{label}: no byte-sequence Signature")
            continue
        url = _signature_agent(raw["signature-agent"], label)
        if url is None:
            problems.append("Signature-Agent is not an http(s) URL")
            continue
        text = lambda k: params[k] if isinstance(params.get(k), str) and not isinstance(params.get(k), Token) else None  # noqa: E731
        return {
            "agent": url,
            "label": label,
            "keyid": keyid,
            "created": created,
            "expires": expires,
            "alg": text("alg"),
            "tag": text("tag"),
            "nonce": text("nonce"),
            "components": comps,
            "covers_agent": "signature-agent" in comps,
            "signature_bytes": len(sig[0]),
            "verified": False,
            "unverified_because": "no key directory: this bench never verifies signatures",
        }, []
    return None, problems or ["no signature label"]

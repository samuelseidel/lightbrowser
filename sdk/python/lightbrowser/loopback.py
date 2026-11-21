"""Loopback guard.

LightBrowser is for our own detection bench and our own test servers only
(see CLAUDE.md). Anything that takes a URL from the caller and talks to it
checks it here first.
"""

from __future__ import annotations

from urllib.parse import urlsplit

from .errors import NonLoopbackError

LOOPBACK_HOSTS = frozenset({"127.0.0.1", "::1", "localhost"})


def is_loopback_url(url: str) -> bool:
    """True when `url` has an explicit host that is 127.0.0.1, ::1 or localhost."""
    try:
        parts = urlsplit(url)
        host = parts.hostname  # strips brackets and user info, lower-cases
        parts.port  # noqa: B018 - raises ValueError on a bad port
    except ValueError:
        return False
    return bool(parts.scheme) and host in LOOPBACK_HOSTS


def assert_loopback(url: str, what: str = "URL") -> str:
    """Return `url` unchanged, or raise `NonLoopbackError`."""
    if not is_loopback_url(url):
        raise NonLoopbackError(
            f"refusing {what} {url!r}: only 127.0.0.1, ::1 or localhost are allowed"
        )
    return url

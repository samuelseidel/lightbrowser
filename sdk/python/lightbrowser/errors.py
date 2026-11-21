"""Exceptions raised by the LightBrowser SDK."""

from __future__ import annotations


class LightBrowserError(Exception):
    """Base class for all SDK errors."""


class BinaryNotFoundError(LightBrowserError):
    """No usable `lightbrowser` executable was found."""


class LaunchError(LightBrowserError):
    """The browser process failed to start or never printed its CDP endpoint."""


class PersonaValidationError(LightBrowserError):
    """A persona file failed `lightbrowser validate`."""

    def __init__(self, message: str, report: str = "") -> None:
        super().__init__(message)
        self.report = report


class NonLoopbackError(LightBrowserError, ValueError):
    """A URL points somewhere other than 127.0.0.1, ::1 or localhost."""


class HumanInputError(LightBrowserError):
    """A `LightBrowser.human*` call failed (for example, no node for a selector)."""

"""LightBrowser Python SDK.

Starts the internal `lightbrowser` headless browser with a persona and
returns stock Playwright objects connected over CDP. Adds
`page.human_move/click/type/scroll` and `page.verify_fingerprint()`,
backed by the `LightBrowser.*` CDP methods. Loopback and our own test
servers only; see README.md.

The async API is in `lightbrowser.async_api`.
"""

from .errors import (
    BinaryNotFoundError,
    HumanInputError,
    LaunchError,
    LightBrowserError,
    NonLoopbackError,
    PersonaValidationError,
)
from .fingerprint import Check, FingerprintReport
from .launcher import find_binary
from .loopback import assert_loopback, is_loopback_url
from .persona import Persona, ValidationReport
from .sync_api import Browser, Human, attach, attach_context

__version__ = "0.1.0"

__all__ = [
    "Browser",
    "Human",
    "Persona",
    "ValidationReport",
    "FingerprintReport",
    "Check",
    "attach",
    "attach_context",
    "find_binary",
    "assert_loopback",
    "is_loopback_url",
    "LightBrowserError",
    "BinaryNotFoundError",
    "LaunchError",
    "PersonaValidationError",
    "NonLoopbackError",
    "HumanInputError",
]

"""Parameter building and error mapping for the `LightBrowser.*` CDP methods.

Shared by the sync and async APIs; no I/O here. The contract (page-session
methods):

    LightBrowser.humanMove   {x, y}                                  -> {}
    LightBrowser.humanClick  {selector? | x, y, button?, clickCount?} -> {x, y}
    LightBrowser.humanType   {text, selector?}                       -> {}
    LightBrowser.humanScroll {deltaY?, deltaX?, x?, y?, selector?}   -> {}
    LightBrowser.fingerprintReport {}                                -> report

Selectors are plain CSS, resolved inside the browser in Rust (never by
script in the page), so Playwright-only selector engines (`text=`,
`xpath=`, `>>`) do not work here.
"""

from __future__ import annotations

import numbers
from typing import Any

from .errors import HumanInputError

BUTTONS = ("left", "middle", "right")


def _num(value: Any, name: str) -> float:
    if isinstance(value, bool) or not isinstance(value, numbers.Real):
        raise TypeError(f"{name} must be a number, not {type(value).__name__}")
    v = float(value)
    if v != v or v in (float("inf"), float("-inf")):
        raise ValueError(f"{name} must be finite")
    return v


def _selector(value: Any) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError("selector must be a non-empty CSS selector string")
    return value


def move_params(x: float, y: float) -> dict[str, Any]:
    return {"x": _num(x, "x"), "y": _num(y, "y")}


def click_params(
    selector: Any = None,
    x: Any = None,
    y: Any = None,
    *,
    button: str = "left",
    click_count: int = 1,
) -> dict[str, Any]:
    """Params for humanClick. `human_click(10, 20)` is accepted as x, y."""
    if selector is not None and not isinstance(selector, str):
        # human_click(10, 20): the numbers landed in the first two slots.
        if x is not None and y is None:
            x, y = selector, x
            selector = None
        else:
            raise TypeError("selector must be a string; pass coordinates as x= and y=")
    if selector is not None:
        if x is not None or y is not None:
            raise ValueError("pass either a selector or x and y, not both")
        out: dict[str, Any] = {"selector": _selector(selector)}
    else:
        if x is None or y is None:
            raise ValueError("human_click needs a selector, or both x and y")
        out = {"x": _num(x, "x"), "y": _num(y, "y")}
    if button not in BUTTONS:
        raise ValueError(f"button must be one of {BUTTONS}, not {button!r}")
    if isinstance(click_count, bool) or not isinstance(click_count, int) or not 1 <= click_count <= 3:
        raise ValueError("click_count must be an integer from 1 to 3")
    if button != "left":
        out["button"] = button
    if click_count != 1:
        out["clickCount"] = click_count
    return out


def type_params(selector: Any, text: Any) -> dict[str, Any]:
    """Params for humanType. `selector` may be None to type into the focused element."""
    if not isinstance(text, str):
        raise TypeError(f"text must be a string, not {type(text).__name__}")
    out: dict[str, Any] = {"text": text}
    if selector is not None:
        out["selector"] = _selector(selector)
    return out


def scroll_params(
    delta_y: Any = None,
    delta_x: Any = None,
    *,
    selector: Any = None,
    x: Any = None,
    y: Any = None,
) -> dict[str, Any]:
    """Params for humanScroll: a wheel distance, or scroll until `selector` is in view."""
    if delta_y is None and delta_x is None and selector is None:
        raise ValueError("human_scroll needs delta_y, delta_x or a selector")
    out: dict[str, Any] = {}
    if delta_y is not None:
        out["deltaY"] = _num(delta_y, "delta_y")
    if delta_x is not None:
        out["deltaX"] = _num(delta_x, "delta_x")
    if selector is not None:
        out["selector"] = _selector(selector)
    if (x is None) != (y is None):
        raise ValueError("pass both x and y or neither")
    if x is not None:
        out["x"] = _num(x, "x")
        out["y"] = _num(y, "y")
    return out


def click_point(result: dict[str, Any]) -> tuple[float, float]:
    """The (x, y) a humanClick result reports."""
    try:
        return float(result["x"]), float(result["y"])
    except (KeyError, TypeError, ValueError) as e:
        raise HumanInputError(f"unexpected humanClick result: {result!r}") from e


def to_human_error(method: str, exc: BaseException) -> HumanInputError:
    """Wrap a Playwright error from a `LightBrowser.*` call, keeping the browser's text."""
    text = str(exc).strip()
    # Playwright prefixes "Protocol error (LightBrowser.humanClick): ".
    marker = f"Protocol error ({method}): "
    if text.startswith(marker):
        text = text[len(marker):]
    return HumanInputError(f"{method}: {text.splitlines()[0] if text else 'failed'}")

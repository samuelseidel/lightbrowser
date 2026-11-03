"""Synchronous API: launch LightBrowser and get stock Playwright objects.

    from lightbrowser import Browser, Persona

    with Browser(persona=Persona.load("persona.json")) as browser:
        page = browser.new_page()
        page.goto("http://127.0.0.1:8765/shop")
        page.human_scroll(900)
        page.human_click("#add-3")
        page.human_type("#q", "jacket")
        print(page.verify_fingerprint())

`new_page()` and `new_context()` return the ordinary Playwright `Page` and
`BrowserContext`. The only addition is a handful of bound methods on the
page (`human_*`, `verify_fingerprint`), added by `attach()`, which also
works on pages from a Playwright `Browser` you connected yourself.
Contexts default to `no_viewport=True`, so the browser keeps the window
size its persona defines (a fixed viewport would contradict the persona).
"""

from __future__ import annotations

import os
from typing import Any, Mapping, Sequence

from playwright.sync_api import (
    Browser as PwBrowser,
    BrowserContext as PwContext,
    CDPSession,
    Error as PlaywrightError,
    Page,
    Playwright,
    sync_playwright,
)

from . import _human
from ._common import LaunchPlan, plan_launch, warn_if_leaky_playwright
from .errors import HumanInputError, LaunchError
from .fingerprint import FingerprintReport
from .launcher import ServerProcess
from .persona import Persona

_ATTR = "_lightbrowser_human"


class Human:
    """Human-input and fingerprint calls for one page, over its own CDP session."""

    def __init__(self, page: Page) -> None:
        self.page = page
        self._session: CDPSession | None = None

    def _call(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        try:
            if self._session is None:
                self._session = self.page.context.new_cdp_session(self.page)
            return self._session.send(method, params or {})  # type: ignore[arg-type]
        except PlaywrightError as e:
            raise _human.to_human_error(method, e) from e

    def move(self, x: float, y: float) -> None:
        """Move the pointer to (x, y) along a human path."""
        self._call("LightBrowser.humanMove", _human.move_params(x, y))

    def click(
        self,
        selector: str | None = None,
        x: float | None = None,
        y: float | None = None,
        *,
        button: str = "left",
        click_count: int = 1,
    ) -> tuple[float, float]:
        """Click a CSS selector (a point inside its box) or the point (x, y). Returns the point clicked."""
        res = self._call("LightBrowser.humanClick", _human.click_params(selector, x, y, button=button, click_count=click_count))
        return _human.click_point(res)

    def type(self, selector: str | None, text: str) -> None:
        """Type `text` with human timing; click `selector` first when given."""
        self._call("LightBrowser.humanType", _human.type_params(selector, text))

    def scroll(
        self,
        delta_y: float | None = None,
        delta_x: float | None = None,
        *,
        selector: str | None = None,
        x: float | None = None,
        y: float | None = None,
    ) -> None:
        """Scroll with wheel events by a distance, or until `selector` is in view."""
        self._call("LightBrowser.humanScroll", _human.scroll_params(delta_y, delta_x, selector=selector, x=x, y=y))

    def fingerprint_report(self) -> FingerprintReport:
        """Compare the page's live surface with the persona (`LightBrowser.fingerprintReport`)."""
        return FingerprintReport.from_dict(self._call("LightBrowser.fingerprintReport"))

    def detach(self) -> None:
        s, self._session = self._session, None
        if s is not None:
            try:
                s.detach()
            except PlaywrightError:
                pass


def attach(page: Page) -> Human:
    """Add `human_move/click/type/scroll` and `verify_fingerprint` to `page`. Idempotent."""
    existing = getattr(page, _ATTR, None)
    if existing is not None:
        return existing
    h = Human(page)
    setattr(page, _ATTR, h)
    page.human_move = h.move  # type: ignore[attr-defined]
    page.human_click = h.click  # type: ignore[attr-defined]
    page.human_type = h.type  # type: ignore[attr-defined]
    page.human_scroll = h.scroll  # type: ignore[attr-defined]
    page.verify_fingerprint = h.fingerprint_report  # type: ignore[attr-defined]
    return h


def attach_context(context: PwContext) -> PwContext:
    """`attach()` every current and future page of `context`."""
    for p in context.pages:
        attach(p)
    context.on("page", attach)
    return context


class Browser:
    """A LightBrowser process plus a Playwright connection to it."""

    def __init__(
        self,
        persona: Persona | Mapping[str, Any] | str | os.PathLike[str] | None = None,
        *,
        executable_path: str | os.PathLike[str] | None = None,
        port: int = 0,
        insecure: bool = False,
        profiles_dir: str | os.PathLike[str] | None = None,
        workers: int | None = None,
        extra_args: Sequence[str] = (),
        startup_timeout: float = 60.0,
        validate: bool = True,
        playwright: Playwright | None = None,
    ) -> None:
        self._plan: LaunchPlan | None = plan_launch(
            persona=persona, executable_path=executable_path, port=port, insecure=insecure,
            profiles_dir=profiles_dir, workers=workers, extra_args=extra_args, validate=validate,
        )
        self.persona: Persona | None = self._plan.persona
        self._startup_timeout = startup_timeout
        self._own_pw = playwright is None
        self._pw: Playwright | None = playwright
        self._server: ServerProcess | None = None
        self._browser: PwBrowser | None = None
        self._contexts: list[PwContext] = []

    # -- lifecycle ------------------------------------------------------------

    def launch(self) -> "Browser":
        if self._browser is not None:
            return self
        if self._plan is None:
            raise LaunchError("this Browser was closed; create a new one")
        warn_if_leaky_playwright()
        try:
            self._server = ServerProcess(self._plan.cmd, startup_timeout=self._startup_timeout)
            if self._pw is None:
                self._pw = sync_playwright().start()
            self._browser = self._pw.chromium.connect_over_cdp(self._server.ws_endpoint)
        except BaseException:
            self.close()
            raise
        return self

    @property
    def ws_endpoint(self) -> str:
        """`ws://127.0.0.1:PORT/devtools/browser/ID`, for any CDP client."""
        return self._server.ws_endpoint if self._server else ""

    @property
    def pid(self) -> int | None:
        return self._server.pid if self._server else None

    @property
    def playwright(self) -> Playwright:
        """The Playwright instance in use. Only one sync Playwright can run per thread:
        pass this as `playwright=` when you open a second browser next to this one."""
        if self._pw is None:
            raise LaunchError("browser is not launched")
        return self._pw

    @property
    def browser(self) -> PwBrowser:
        """The stock Playwright `Browser`."""
        if self._browser is None:
            raise LaunchError("browser is not launched; use `with Browser(...)` or call launch()")
        return self._browser

    def stderr_tail(self, n: int = 20) -> str:
        return self._server.stderr_tail(n) if self._server else ""

    def new_context(self, **kwargs: Any) -> PwContext:
        """A stock Playwright context. `no_viewport=True` unless you pass `viewport`."""
        if "viewport" not in kwargs:
            kwargs.setdefault("no_viewport", True)
        ctx = self.browser.new_context(**kwargs)
        self._contexts.append(ctx)
        return attach_context(ctx)

    def new_page(self, **context_kwargs: Any) -> Page:
        """A page in a fresh context (own cookies and storage), with the helper methods."""
        page = self.new_context(**context_kwargs).new_page()
        attach(page)
        return page

    def close(self) -> None:
        """Close contexts, disconnect Playwright, stop the process, remove temp files. Idempotent."""
        browser, self._browser = self._browser, None
        if browser is not None:
            for ctx in self._contexts:
                try:
                    ctx.close()
                except PlaywrightError:
                    pass
            try:
                browser.close()
            except PlaywrightError:
                pass
        self._contexts.clear()
        pw, self._pw = self._pw, None
        if pw is not None and self._own_pw:
            try:
                pw.stop()
            except PlaywrightError:
                pass
        server, self._server = self._server, None
        if server is not None:
            server.close()
        plan, self._plan = self._plan, None
        if plan is not None:
            plan.cleanup()

    def __enter__(self) -> "Browser":
        return self.launch()

    def __exit__(self, *exc: object) -> None:
        self.close()

    def __getattr__(self, name: str) -> Any:
        # contexts, version, is_connected, ... come from the stock browser.
        if name.startswith("_"):
            raise AttributeError(name)
        return getattr(self.browser, name)

    def __repr__(self) -> str:
        return f"Browser(pid={self.pid}, ws={self.ws_endpoint!r})"


__all__ = ["Browser", "Human", "attach", "attach_context", "HumanInputError"]

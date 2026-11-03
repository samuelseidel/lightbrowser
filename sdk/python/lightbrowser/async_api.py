"""Asynchronous API: the same as `sync_api`, with Playwright's async objects.

    import asyncio
    from lightbrowser.async_api import AsyncBrowser

    async def main():
        async with AsyncBrowser() as browser:
            page = await browser.new_page()
            await page.goto("http://127.0.0.1:8765/shop")
            await page.human_click("#add-3")
            print(await page.verify_fingerprint())

    asyncio.run(main())

Starting and stopping the process blocks briefly, so it runs in a worker
thread (`asyncio.to_thread`).
"""

from __future__ import annotations

import asyncio
import os
from typing import Any, Mapping, Sequence

from playwright.async_api import (
    Browser as PwBrowser,
    BrowserContext as PwContext,
    CDPSession,
    Error as PlaywrightError,
    Page,
    Playwright,
    async_playwright,
)

from . import _human
from ._common import LaunchPlan, plan_launch, warn_if_leaky_playwright
from .errors import LaunchError
from .fingerprint import FingerprintReport
from .launcher import ServerProcess
from .persona import Persona

_ATTR = "_lightbrowser_human"


class AsyncHuman:
    """Human-input and fingerprint calls for one page, over its own CDP session."""

    def __init__(self, page: Page) -> None:
        self.page = page
        self._session: CDPSession | None = None
        self._lock = asyncio.Lock()

    async def _call(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        try:
            async with self._lock:
                if self._session is None:
                    self._session = await self.page.context.new_cdp_session(self.page)
                session = self._session
            return await session.send(method, params or {})  # type: ignore[arg-type]
        except PlaywrightError as e:
            raise _human.to_human_error(method, e) from e

    async def move(self, x: float, y: float) -> None:
        await self._call("LightBrowser.humanMove", _human.move_params(x, y))

    async def click(
        self,
        selector: str | None = None,
        x: float | None = None,
        y: float | None = None,
        *,
        button: str = "left",
        click_count: int = 1,
    ) -> tuple[float, float]:
        res = await self._call(
            "LightBrowser.humanClick", _human.click_params(selector, x, y, button=button, click_count=click_count)
        )
        return _human.click_point(res)

    async def type(self, selector: str | None, text: str) -> None:
        await self._call("LightBrowser.humanType", _human.type_params(selector, text))

    async def scroll(
        self,
        delta_y: float | None = None,
        delta_x: float | None = None,
        *,
        selector: str | None = None,
        x: float | None = None,
        y: float | None = None,
    ) -> None:
        await self._call("LightBrowser.humanScroll", _human.scroll_params(delta_y, delta_x, selector=selector, x=x, y=y))

    async def fingerprint_report(self) -> FingerprintReport:
        return FingerprintReport.from_dict(await self._call("LightBrowser.fingerprintReport"))

    async def detach(self) -> None:
        s, self._session = self._session, None
        if s is not None:
            try:
                await s.detach()
            except PlaywrightError:
                pass


def attach(page: Page) -> AsyncHuman:
    """Add `human_*` coroutines and `verify_fingerprint` to `page`. Idempotent."""
    existing = getattr(page, _ATTR, None)
    if existing is not None:
        return existing
    h = AsyncHuman(page)
    setattr(page, _ATTR, h)
    page.human_move = h.move  # type: ignore[attr-defined]
    page.human_click = h.click  # type: ignore[attr-defined]
    page.human_type = h.type  # type: ignore[attr-defined]
    page.human_scroll = h.scroll  # type: ignore[attr-defined]
    page.verify_fingerprint = h.fingerprint_report  # type: ignore[attr-defined]
    return h


def attach_context(context: PwContext) -> PwContext:
    for p in context.pages:
        attach(p)
    context.on("page", attach)
    return context


class AsyncBrowser:
    """A LightBrowser process plus an async Playwright connection to it."""

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

    async def launch(self) -> "AsyncBrowser":
        if self._browser is not None:
            return self
        if self._plan is None:
            raise LaunchError("this AsyncBrowser was closed; create a new one")
        plan = self._plan
        warn_if_leaky_playwright()
        try:
            self._server = await asyncio.to_thread(ServerProcess, plan.cmd, startup_timeout=self._startup_timeout)
            if self._pw is None:
                self._pw = await async_playwright().start()
            self._browser = await self._pw.chromium.connect_over_cdp(self._server.ws_endpoint)
        except BaseException:
            await self.close()
            raise
        return self

    @property
    def ws_endpoint(self) -> str:
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
        if self._browser is None:
            raise LaunchError("browser is not launched; use `async with AsyncBrowser(...)` or await launch()")
        return self._browser

    def stderr_tail(self, n: int = 20) -> str:
        return self._server.stderr_tail(n) if self._server else ""

    async def new_context(self, **kwargs: Any) -> PwContext:
        if "viewport" not in kwargs:
            kwargs.setdefault("no_viewport", True)
        ctx = await self.browser.new_context(**kwargs)
        self._contexts.append(ctx)
        return attach_context(ctx)

    async def new_page(self, **context_kwargs: Any) -> Page:
        ctx = await self.new_context(**context_kwargs)
        page = await ctx.new_page()
        attach(page)
        return page

    async def close(self) -> None:
        browser, self._browser = self._browser, None
        if browser is not None:
            for ctx in self._contexts:
                try:
                    await ctx.close()
                except PlaywrightError:
                    pass
            try:
                await browser.close()
            except PlaywrightError:
                pass
        self._contexts.clear()
        pw, self._pw = self._pw, None
        if pw is not None and self._own_pw:
            try:
                await pw.stop()
            except PlaywrightError:
                pass
        server, self._server = self._server, None
        if server is not None:
            await asyncio.to_thread(server.close)
        plan, self._plan = self._plan, None
        if plan is not None:
            plan.cleanup()

    async def __aenter__(self) -> "AsyncBrowser":
        return await self.launch()

    async def __aexit__(self, *exc: object) -> None:
        await self.close()

    def __getattr__(self, name: str) -> Any:
        if name.startswith("_"):
            raise AttributeError(name)
        return getattr(self.browser, name)

    def __repr__(self) -> str:
        return f"AsyncBrowser(pid={self.pid}, ws={self.ws_endpoint!r})"


__all__ = ["AsyncBrowser", "AsyncHuman", "attach", "attach_context"]

"""Async API against the real binary."""

from __future__ import annotations

import asyncio
import warnings

import pytest

from lightbrowser import HumanInputError
from lightbrowser.async_api import AsyncBrowser

pytestmark = pytest.mark.integration


@pytest.fixture
def human_ready_async(binary):
    """Skip-as-xfail when the binary has no LightBrowser.human* yet."""

    async def probe():
        async with AsyncBrowser(executable_path=binary) as b:
            p = await b.new_page()
            try:
                await p.human_move(10, 10)
            except HumanInputError as e:
                if "not implemented" in str(e) or "not supported" in str(e):
                    pytest.xfail(f"LightBrowser.human* not available in this binary: {e}")
                raise

    with warnings.catch_warnings():
        warnings.simplefilter("ignore", UserWarning)
        asyncio.run(probe())


def test_async_api(binary, page_url, human_ready_async):
    async def main():
        async with AsyncBrowser(executable_path=binary) as b:
            assert b.ws_endpoint.startswith("ws://127.0.0.1:")
            page = await b.new_page()
            await page.goto(page_url)
            x, y = await page.human_click("#btn")
            box = await page.locator("#btn").bounding_box()
            assert box["x"] <= x <= box["x"] + box["width"]
            await page.human_type("#q", "async")
            assert await page.input_value("#q") == "async"
            await page.human_scroll(300)
            assert await page.evaluate("scrollY") > 0
            with pytest.raises(HumanInputError):
                await page.human_click("#nope")
            report = await page.verify_fingerprint()
            assert report.summary["ok"] > 50
            proc = b._server.proc
        assert proc.poll() is not None

    with warnings.catch_warnings():
        warnings.simplefilter("ignore", UserWarning)
        asyncio.run(main())



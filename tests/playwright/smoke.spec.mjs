// connectOverCDP smoke test against `lightbrowser serve` (loopback only).
//
//   cargo build -p lightbrowser
//   node --test tests/playwright/

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages, html, chromium } from './harness.mjs';

pages['/smoke'] = html(
  'LightBrowser smoke',
  '<h1>Smoke</h1><button id="go" type="button">Log in</button><script>document.body.dataset.ran = "yes"</script>',
);

test('connectOverCDP, newContext, newPage, goto, evaluate, content', async () => {
  const { browser, base } = await env();
  assert.match(browser.version(), /^153\./);
  const { context, page } = await newPage();
  const response = await page.goto(`${base}/smoke`);
  assert.equal(response.status(), 200);
  assert.equal(page.url(), `${base}/smoke`);
  assert.equal(await page.evaluate('1+1'), 2);
  assert.equal(await page.evaluate(() => 1 + 1), 2);
  assert.equal(await page.evaluate((a) => a * 2, 21), 42);
  assert.deepEqual(await page.evaluate(() => ({ a: [1, 'x', null], b: { c: true } })), {
    a: [1, 'x', null],
    b: { c: true },
  });
  assert.equal(await page.evaluate(async () => { await new Promise((r) => setTimeout(r, 10)); return 'later'; }), 'later');
  await assert.rejects(page.evaluate(() => { throw new TypeError('boom'); }), /TypeError: boom/);
  // Scripts ran.
  assert.equal(await page.evaluate(() => document.body.dataset.ran), 'yes');
  assert.equal(await page.title(), 'LightBrowser smoke');
  const html = await page.content();
  assert.match(html, /^<!DOCTYPE html><html><head><meta charset="utf-8"><title>LightBrowser smoke<\/title>/);
  assert.match(html, /<button id="go" type="button">Log in<\/button>/);
  // Handles: element handles and JS handles.
  const h1 = await page.$('h1');
  assert.equal(await h1.evaluate((e) => e.tagName), 'H1');
  const obj = await page.evaluateHandle(() => ({ x: 1, y: 'two' }));
  const props = await obj.getProperties();
  assert.deepEqual([...props.keys()].sort(), ['x', 'y']);
  assert.equal(await (await obj.getProperty('y')).jsonValue(), 'two');
  // The persona is enforced: a contradicting viewport is an error.
  await assert.rejects(page.setViewportSize({ width: 5000, height: 5000 }), /contradicts the persona/);
  await context.close();
});

test('serve + connectOverCDP + newPage + goto wall time', async () => {
  const { ws, base, startedMs } = await env();
  // A second, cold connection: connect, new context and page, goto.
  const t0 = performance.now();
  const browser = await chromium.connectOverCDP(ws);
  const context = await browser.newContext({ viewport: null });
  const page = await context.newPage();
  await page.goto(`${base}/smoke`);
  const warm = performance.now() - t0;
  await context.close();
  console.log(`# serve start + connect: ${startedMs.toFixed(0)} ms; connect + newPage + goto: ${warm.toFixed(0)} ms`);
  assert.ok(warm < 10000);
});

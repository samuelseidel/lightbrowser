// Workers as CDP targets (ADR 0029): page.workers(), worker.evaluate, the
// 'worker' and 'serviceworker' events, worker console, errors and
// requests, fromServiceWorker. The expected values are what the same
// scenario gave in local Chrome for Testing 153 (workers/chrome.json,
// recorded with workers/record.mjs).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { env, pages } from './harness.mjs';
import { workerPages, runScenario } from './workers/scenario.mjs';

Object.assign(pages, workerPages);
const chrome = JSON.parse(readFileSync(new URL('./workers/chrome.json', import.meta.url), 'utf8'));

test('dedicated, shared and service workers as with Chrome', async () => {
  const { browser, base } = await env();
  const got = await runScenario(browser, base);
  for (const [k, want] of Object.entries(chrome.results)) {
    if (k === 'consoles') continue;
    assert.deepEqual(got[k], want, k);
  }
  // Console messages of the worker. Not compared: the page's own favicon
  // 404 message, and how an object argument is printed (LightBrowser
  // sends console arguments as one formatted string, see ROADMAP).
  const workerLines = (l) => l.filter((x) => x.includes('worker') || x.startsWith('warning: warned'));
  const want = workerLines(chrome.results.consoles);
  const have = workerLines(got.consoles);
  assert.equal(have.length, want.length, have.join(' | '));
  for (let i = 0; i < want.length; i++) {
    if (want[i].startsWith('warning: warned')) assert.ok(have[i].startsWith('warning: warned '), have[i]);
    else assert.equal(have[i], want[i]);
  }
});

test('no CDP helper is visible to the worker', async () => {
  const { browser, base } = await env();
  const context = await browser.newContext({ viewport: null });
  const page = await context.newPage();
  const [worker] = await Promise.all([page.waitForEvent('worker'), page.goto(`${base}/wk/`)]);
  await worker.evaluate(() => 1);
  // Own globals of the scope after Playwright evaluated in it: nothing
  // beyond what the worker script set.
  const extra = await worker.evaluate(() =>
    Object.getOwnPropertyNames(self).filter((n) => /utility|playwright|__pw|cdp|binding/i.test(n)),
  );
  assert.deepEqual(extra, []);
  await context.close();
});

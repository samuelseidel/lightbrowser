// Automation must stay invisible to the page's main world.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages, html } from './harness.mjs';

pages['/plain'] = html(
  'Plain',
  `<button id="b">Press</button><input id="i">
  <script>
    window.baseline = Object.getOwnPropertyNames(window);
    window.events = [];
    document.getElementById('b').addEventListener('click', (e) => window.events.push(e.isTrusted));
  </script>`,
);

test('the main world sees no Playwright or CDP globals; navigator.webdriver is false', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/plain`);
  // Drive the page through the utility world.
  await page.click('#b');
  await page.fill('#i', 'x');
  await page.getByRole('button', { name: 'Press' }).waitFor();
  await page.waitForSelector('#b');
  const r = await page.evaluate(() => {
    const now = Object.getOwnPropertyNames(window);
    return {
      added: now.filter((n) => !window.baseline.includes(n) && !['baseline', 'events'].includes(n)),
      suspicious: now.filter((n) => /playwright|__pw|puppeteer|cdc_|\$cdc|^__.*binding|webdriver|domAutomation/i.test(n)),
      webdriver: navigator.webdriver,
      webdriverOwn: Object.getOwnPropertyNames(navigator).includes('webdriver'),
      trusted: window.events,
    };
  });
  assert.deepEqual(r.suspicious, []);
  assert.deepEqual(r.added, []);
  assert.equal(r.webdriver, false);
  assert.equal(r.webdriverOwn, false);
  assert.deepEqual(r.trusted, [true]);
  // A fresh document after navigation is just as clean.
  await page.goto(`${base}/plain`);
  const again = await page.evaluate(() =>
    Object.getOwnPropertyNames(window).filter((n) => /playwright|__pw|^__.*binding|cdc_/i.test(n)),
  );
  assert.deepEqual(again, []);
  await context.close();
});

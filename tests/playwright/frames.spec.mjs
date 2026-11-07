// Child frames: srcdoc and same-origin iframes over CDP (frame events,
// execution contexts per frame, frame locators).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages, html } from './harness.mjs';

pages['/frames'] = html(
  'Frames',
  `<iframe id="f" srcdoc="<p id='p'>inside</p><button id='b'>Go</button>"></iframe>
   <iframe name="second" src="/frame-child"></iframe>`,
);
pages['/frame-child'] = html('Child', '<h1>child doc</h1>');

test('frameLocator reads a srcdoc iframe; frames and their contexts are reported', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/frames`);
  const fl = page.frameLocator('#f');
  assert.equal(await fl.locator('#p').textContent(), 'inside');
  assert.equal(await fl.getByRole('button', { name: 'Go' }).count(), 1);
  assert.equal(await fl.locator('#b').innerText(), 'Go');

  assert.equal(page.frames().length, 3);
  const second = page.frame({ name: 'second' });
  assert.ok(second, 'named frame');
  assert.equal(second.url(), `${base}/frame-child`);
  assert.equal(second.parentFrame(), page.mainFrame());
  assert.equal(await second.locator('h1').textContent(), 'child doc');
  assert.equal(
    await second.evaluate(() => [location.pathname, parent === top, frameElement.name].join()),
    '/frame-child,true,second',
  );

  // Frames created and removed by script are attached and detached.
  await page.evaluate(() => {
    const f = document.createElement('iframe');
    f.id = 'dyn';
    document.body.appendChild(f);
  });
  await page.waitForFunction(() => document.getElementById('dyn').contentDocument !== null);
  assert.equal(page.frames().length, 4);
  const detached = new Promise((resolve) => page.once('framedetached', resolve));
  await page.evaluate(() => document.getElementById('dyn').remove());
  await detached;
  assert.equal(page.frames().length, 3);
  await context.close();
});

test('LightBrowser.fingerprintReport matches the persona and leaves no trace', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/frame-child`);
  const before = await page.evaluate(() => Object.getOwnPropertyNames(window).length);
  const cdp = await context.newCDPSession(page);
  const r = await cdp.send('LightBrowser.fingerprintReport');
  const bad = r.checks.filter((c) => c.status === 'mismatch');
  assert.deepEqual(bad, []);
  assert.equal(r.ok, true);
  assert.ok(r.summary.ok > 80, JSON.stringify(r.summary));
  // Collected in a throwaway isolated world: the main world is unchanged
  // and no execution context was announced for it.
  assert.equal(await page.evaluate(() => Object.getOwnPropertyNames(window).length), before);
  await cdp.detach();
  await context.close();
});

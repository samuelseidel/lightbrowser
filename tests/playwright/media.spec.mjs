// page.emulateMedia (Emulation.setEmulatedMedia): what the client asks for
// applies to media queries and matchMedia, also on the page's next
// documents, and clearing returns to the persona's values (ADR 0009
// revision 3). Results equal Chrome for Testing 1243's.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages } from './harness.mjs';

pages['/media'] = `<style>p{color:rgb(1,1,1)} @media (prefers-color-scheme: dark){p{color:rgb(2,2,2)}}
  @media print{p{color:rgb(3,3,3)}} @media (prefers-reduced-motion: reduce){p{background-color:rgb(9,9,9)}}</style><p id=p>x</p>`;

test('emulateMedia: color scheme, reduced motion, print, across navigations, cleared', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/media`);
  const probe = () =>
    page.evaluate(() => [
      matchMedia('(prefers-color-scheme: dark)').matches,
      matchMedia('print').matches,
      matchMedia('screen').matches,
      matchMedia('(prefers-reduced-motion: reduce)').matches,
      getComputedStyle(document.getElementById('p')).color,
      getComputedStyle(document.getElementById('p')).backgroundColor,
    ]);
  assert.deepEqual(await probe(), [false, false, true, false, 'rgb(1, 1, 1)', 'rgba(0, 0, 0, 0)']);
  await page.emulateMedia({ colorScheme: 'dark' });
  assert.deepEqual(await probe(), [true, false, true, false, 'rgb(2, 2, 2)', 'rgba(0, 0, 0, 0)']);
  await page.emulateMedia({ reducedMotion: 'reduce' });
  assert.deepEqual(await probe(), [true, false, true, true, 'rgb(2, 2, 2)', 'rgb(9, 9, 9)']);
  await page.emulateMedia({ media: 'print' });
  assert.deepEqual(await probe(), [true, true, false, true, 'rgb(3, 3, 3)', 'rgb(9, 9, 9)']);
  await page.goto(`${base}/media`);
  assert.deepEqual(await probe(), [true, true, false, true, 'rgb(3, 3, 3)', 'rgb(9, 9, 9)']);
  await page.emulateMedia({ media: null, colorScheme: null, reducedMotion: null });
  assert.deepEqual(await probe(), [false, false, true, false, 'rgb(1, 1, 1)', 'rgba(0, 0, 0, 0)']);
  // A dark context from the start.
  const { browser } = await env();
  const dark = await browser.newContext({ viewport: null, colorScheme: 'dark' });
  const p2 = await dark.newPage();
  await p2.goto(`${base}/media`);
  assert.equal(await p2.evaluate(() => matchMedia('(prefers-color-scheme: dark)').matches), true);
  await dark.close();
  await context.close();
});

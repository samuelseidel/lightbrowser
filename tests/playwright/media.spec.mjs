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

test('MediaQueryList change fires on emulateMedia and viewport resize', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/media`);
  await page.evaluate(() => {
    window.__mql = [];
    const dark = matchMedia('(prefers-color-scheme: dark)');
    const wide = matchMedia('(min-width: 500px)');
    const listener = (mql, tag) => (e) =>
        window.__mql.push([tag, e.matches, e.media, e.target === mql, e instanceof MediaQueryListEvent]);
    const onDark = listener(dark, 'dark');
    dark.addEventListener('change', onDark);
    dark.addListener(onDark);
    wide.addListener(listener(wide, 'wide'));
    window.__dark = dark;
    window.__wide = wide;
  });
  assert.deepEqual(await page.evaluate(() => [window.__dark.matches, window.__wide.matches]), [
    false,
    true,
  ]);
  await page.emulateMedia({ colorScheme: 'dark' });
  assert.deepEqual(
    await page.evaluate(() => window.__mql),
    [
      ['dark', true, '(prefers-color-scheme: dark)', true, true],
    ],
  );
  await page.evaluate(() => {
    window.__mql = [];
  });
  await page.setViewportSize({ width: 400, height: 600 });
  assert.deepEqual(
    await page.evaluate(() => window.__mql),
    [
      ['wide', false, '(min-width: 500px)', true, true],
    ],
  );
  assert.equal(await page.evaluate(() => window.__wide.matches), false);
  await context.close();
});

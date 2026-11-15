// Request interception, network events, cookies, pages and contexts.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages, html, requests } from './harness.mjs';

pages['/net'] = html(
  'Net',
  `<p id="out">waiting</p>
  <script>
    fetch('/api/data').then((r) => r.json()).then((j) => { document.getElementById('out').textContent = j.msg; },
      (e) => { document.getElementById('out').textContent = 'failed: ' + e.name; });
  </script>`,
);
pages['/api/data'] = (req, res) => {
  res.writeHead(200, { 'content-type': 'application/json' });
  res.end(JSON.stringify({ msg: 'from server' }));
};
pages['/set-cookie'] = (req, res) => {
  res.writeHead(200, { 'content-type': 'text/html', 'set-cookie': 'server=yes; Path=/' });
  res.end(html('Cookie', '<p>set</p>'));
};
pages['/echo-cookie'] = (req, res) => {
  res.writeHead(200, { 'content-type': 'text/html' });
  res.end(html('Echo', `<pre id="c">${req.headers.cookie ?? ''}</pre>`));
};

test('page.route fulfill and abort, request and response events', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  const seen = [];
  page.on('request', (r) => seen.push(`${r.resourceType()} ${new URL(r.url()).pathname}`));
  const responses = [];
  page.on('response', (r) => responses.push(`${r.status()} ${new URL(r.url()).pathname}`));
  await page.route('**/api/data', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ msg: 'mocked' }) }),
  );
  await page.goto(`${base}/net`);
  await page.waitForFunction(() => document.getElementById('out').textContent !== 'waiting');
  assert.equal(await page.textContent('#out'), 'mocked');
  assert.ok(seen.includes('document /net'), seen.join(','));
  assert.ok(seen.includes('fetch /api/data'), seen.join(','));
  assert.ok(responses.includes('200 /net'), responses.join(','));
  await page.unroute('**/api/data');

  await page.route('**/api/data', (route) => route.abort());
  await page.goto(`${base}/net`);
  await page.waitForFunction(() => document.getElementById('out').textContent !== 'waiting');
  assert.equal(await page.textContent('#out'), 'failed: TypeError');
  await page.unroute('**/api/data');

  // A routed navigation: fulfilled without touching the server, or aborted.
  await page.route('**/virtual', (route) => route.fulfill({ contentType: 'text/html', body: html('Virtual', '<p id="v">made up</p>') }));
  const before = requests.length;
  const resp = await page.goto(`${base}/virtual`);
  assert.equal(resp.status(), 200);
  assert.equal(await page.textContent('#v'), 'made up');
  assert.equal(requests.filter((r) => r.url === '/virtual').length, 0);
  assert.ok(requests.length >= before);
  await page.route('**/blocked', (route) => route.abort());
  await assert.rejects(page.goto(`${base}/blocked`), /net::ERR_FAILED/);
  // Unrouted requests reach the server.
  await page.goto(`${base}/net`);
  await page.waitForFunction(() => document.getElementById('out').textContent !== 'waiting');
  assert.equal(await page.textContent('#out'), 'from server');
  await context.close();
});

test('cookies: addCookies, Set-Cookie, document.cookie and context.cookies', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await context.addCookies([{ name: 'added', value: '1', url: base }]);
  await page.goto(`${base}/set-cookie`);
  await page.evaluate(() => (document.cookie = 'script=2; path=/'));
  await page.goto(`${base}/echo-cookie`);
  const header = await page.textContent('#c');
  for (const c of ['added=1', 'server=yes', 'script=2']) assert.ok(header.includes(c), header);
  const names = (await context.cookies(base)).map((c) => `${c.name}=${c.value}`).sort();
  assert.deepEqual(names, ['added=1', 'script=2', 'server=yes']);
  const all = await context.cookies();
  assert.equal(all.find((c) => c.name === 'server').domain, '127.0.0.1');
  await context.clearCookies();
  assert.deepEqual(await context.cookies(), []);
  await context.close();
});

test('two pages in a context and two isolated contexts', async () => {
  const { base, browser } = await env();
  const a = await browser.newContext({ viewport: null });
  const b = await browser.newContext({ viewport: null });
  const a1 = await a.newPage();
  const a2 = await a.newPage();
  const b1 = await b.newPage();
  await Promise.all([a1.goto(`${base}/set-cookie`), a2.goto(`${base}/net`), b1.goto(`${base}/echo-cookie`)]);
  assert.equal(await a1.title(), 'Cookie');
  assert.equal(await a2.title(), 'Net');
  await a1.evaluate(() => (window.mine = 'a1'));
  assert.equal(await a2.evaluate(() => window.mine), undefined);
  // Pages of one context share cookies and storage; contexts do not.
  await a1.evaluate(() => localStorage.setItem('k', 'v'));
  assert.equal(await a2.evaluate(() => localStorage.getItem('k')), 'v');
  await b1.goto(`${base}/net`);
  assert.equal(await b1.evaluate(() => localStorage.getItem('k')), null);
  await a2.goto(`${base}/echo-cookie`);
  assert.match(await a2.textContent('#c'), /server=yes/);
  await b1.goto(`${base}/echo-cookie`);
  assert.equal(await b1.textContent('#c'), '');
  assert.equal(a.pages().length, 2);
  await a2.close();
  assert.equal(a.pages().length, 1);
  assert.equal(await a1.evaluate(() => 6 * 7), 42);
  await a.close();
  await b.close();
});

pages['/lazy-images'] = html(
  'Lazy images',
  '<div style="height:6000px"></div><img id="lazy" loading="lazy" src="/lazy-image.svg" width="20" height="20">',
);
pages['/lazy-image.svg'] = (_req, res) => {
  res.writeHead(200, { 'content-type': 'image/svg+xml' });
  res.end('<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20"></svg>');
};

test('scrolling starts a deferred lazy image request', { timeout: 15_000 }, async (t) => {
  const { base } = await env();
  const actions = {
    'window.scrollTo': (page) => page.evaluate(() => scrollTo(0, 6000)),
    'element.scrollIntoView': (page) => page.locator('#lazy').evaluate((image) => image.scrollIntoView()),
    'Playwright scrollIntoViewIfNeeded': (page) => page.locator('#lazy').scrollIntoViewIfNeeded(),
    'wheel input': async (page) => {
      await page.mouse.move(50, 50);
      await page.mouse.wheel(0, 6000);
    },
  };
  for (const [name, scroll] of Object.entries(actions)) {
    await t.test(name, async () => {
      const { context, page } = await newPage();
      try {
        await page.goto(`${base}/lazy-images`);
        assert.equal(await page.locator('#lazy').evaluate((image) => image.complete), false);
        const response = page.waitForResponse('**/lazy-image.svg', { timeout: 3000 });
        const [loaded] = await Promise.all([response, scroll(page)]);
        assert.equal(loaded.status(), 200);
        await page.waitForFunction(() => document.querySelector('#lazy').naturalWidth === 20);
      } finally {
        await context.close();
      }
    });
  }
});

pages['/navigation-redirect'] = (_req, res) => {
  res.writeHead(302, { location: '/navigation-redirect-second' });
  res.end();
};
pages['/navigation-redirect-second'] = (_req, res) => {
  res.writeHead(302, { location: '/navigation-redirect-end' });
  res.end();
};
pages['/navigation-redirect-cross'] = (req, res) => {
  const url = new URL('/navigation-redirect-second', `http://${req.headers.host}`);
  url.hostname = 'localhost';
  res.writeHead(302, { location: url.href });
  res.end();
};
pages['/navigation-redirect-end'] = html('Redirect destination', '');

test('navigation redirect counts exclude cross-origin chains', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  try {
    for (const [path, count] of [['/navigation-redirect', 2], ['/navigation-redirect-cross', 0]]) {
      await page.goto(base + path);
      assert.ok(page.url().endsWith('/navigation-redirect-end'));
      assert.deepEqual(await page.evaluate(() => [
        performance.navigation.redirectCount,
        performance.navigation.toJSON().redirectCount,
      ]), [count, count]);
    }
  } finally {
    await context.close();
  }
});

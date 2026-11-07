// Session history, referrers and the CDP methods stock Playwright sends that
// had no test: goBack/goForward (cross-document and pushState), goto with
// `referer`, storageState with indexedDB, ariaSnapshot, networkidle,
// addInitScript, request.redirectedFrom, offline, javaScriptEnabled: false.
// ADR 0045. Each expectation is what Chrome 153 does (tests/cdp-raw records
// the same sequences from Chrome).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages, html, requests } from './harness.mjs';

pages['/h/a'] = html('Page A', '<a id="b" href="/h/b">to b</a>');
pages['/h/b'] = html('Page B', '<p>b</p>');
pages['/h/c'] = html('Page C', '<p>c</p>');
pages['/h/redirect'] = (req, res) => {
  res.writeHead(302, { location: '/h/final' });
  res.end();
};
pages['/h/redirect2'] = (req, res) => {
  res.writeHead(301, { location: '/h/redirect' });
  res.end();
};
pages['/h/final'] = html('Final', '<p>final</p>');
pages['/h/aria'] = html(
  'Aria',
  '<h1>Title</h1><nav aria-label="Main"><a href="/x">Home</a><a href="/y">About</a></nav><button>Go</button><ul><li>one</li><li>two</li></ul>',
);
pages['/h/idle'] = html(
  'Idle',
  '<p id="out">waiting</p><script>setTimeout(() => fetch("/h/data").then((r) => r.text()).then((t) => { document.getElementById("out").textContent = t; }), 300)</script>',
);
pages['/h/data'] = (req, res) => {
  res.writeHead(200, { 'content-type': 'text/plain' });
  res.end('late data');
};
pages['/h/init'] = html('Init', '<script>window.__seen = window.__init ? window.__init.slice() : null;</script>');
pages['/h/withframe'] = html('Frames', '<iframe id="f" src="/h/b"></iframe>');
pages['/h/js'] = html('No JS', '<script>document.title = "script ran"</script>');
pages['/h/hash'] = html('Hash', '<a id="t" name="target">t</a><p>hash</p>');

const lastRequest = (path) => [...requests].reverse().find((r) => r.url === path);

test('goBack and goForward move the page through its history', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/h/a`);
  await page.click('#b');
  await page.waitForURL(`${base}/h/b`);
  await page.goto(`${base}/h/c`);
  assert.equal(await page.evaluate(() => history.length), 4); // about:blank, a, b, c

  const back = await page.goBack();
  assert.equal(page.url(), `${base}/h/b`);
  assert.equal(back.status(), 200);
  assert.equal(await page.title(), 'Page B');
  await page.goBack();
  assert.equal(page.url(), `${base}/h/a`);
  assert.equal(await page.title(), 'Page A');
  // The history keeps its length and moves its index.
  assert.equal(await page.evaluate(() => history.length), 4);

  await page.goForward();
  assert.equal(page.url(), `${base}/h/b`);
  const fwd = await page.goForward();
  assert.equal(page.url(), `${base}/h/c`);
  assert.equal(fwd.status(), 200);
  // At the end: no entry, no navigation, null.
  assert.equal(await page.goForward(), null);
  assert.equal(page.url(), `${base}/h/c`);

  // A new navigation after going back drops the forward entries.
  await page.goBack();
  await page.goto(`${base}/h/c`);
  assert.equal(await page.evaluate(() => history.length), 4); // about:blank, a, b, c
  assert.equal(await page.goForward(), null);

  await context.close();
});

test('Page.getNavigationHistory, navigateToHistoryEntry and resetNavigationHistory', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  const cdp = await context.newCDPSession(page);
  await page.goto(`${base}/h/a`);
  await page.click('#b');
  await page.waitForURL(`${base}/h/b`);
  await page.goto(`${base}/h/a`);
  const h = await cdp.send('Page.getNavigationHistory');
  assert.equal(h.currentIndex, 3);
  assert.deepEqual(
    h.entries.map((e) => [e.url.replace(base, ''), e.title, e.transitionType]),
    [['about:blank', '', 'typed'], ['/h/a', 'Page A', 'typed'], ['/h/b', 'Page B', 'link'], ['/h/a', 'Page A', 'typed']],
  );
  assert.equal(new Set(h.entries.map((e) => e.id)).size, 4);
  await cdp.send('Page.navigateToHistoryEntry', { entryId: h.entries[2].id });
  await page.waitForURL(`${base}/h/b`);
  assert.equal((await cdp.send('Page.getNavigationHistory')).currentIndex, 2);
  await assert.rejects(cdp.send('Page.navigateToHistoryEntry', { entryId: 123456789 }), /No entry with passed id/);
  await cdp.send('Page.resetNavigationHistory');
  assert.equal((await cdp.send('Page.getNavigationHistory')).entries.length, 1);
  assert.equal(await page.evaluate(() => history.length), 1);
  assert.equal(await page.goBack(), null);
  await context.close();
});

test('goBack and goForward over pushState, hash changes and popstate', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/h/a`);
  await page.evaluate(() => {
    window.pops = [];
    addEventListener('popstate', (e) => pops.push(JSON.stringify(e.state) + ' ' + location.pathname));
    history.pushState({ n: 1 }, '', '/h/p1');
    history.pushState({ n: 2 }, '', '/h/p2');
  });
  assert.equal(page.url(), `${base}/h/p2`);
  assert.deepEqual(await page.evaluate(() => [history.length, history.state]), [4, { n: 2 }]); // about:blank, a, p1, p2

  // Same-document: goBack resolves with null and the URL changes.
  assert.equal(await page.goBack(), null);
  assert.equal(page.url(), `${base}/h/p1`);
  assert.deepEqual(await page.evaluate(() => history.state), { n: 1 });
  assert.equal(await page.goBack(), null);
  assert.equal(page.url(), `${base}/h/a`);
  assert.equal(await page.evaluate(() => history.state), null);
  assert.equal(await page.goForward(), null);
  assert.equal(await page.goForward(), null);
  assert.equal(page.url(), `${base}/h/p2`);
  assert.deepEqual(await page.evaluate(() => pops), ['{"n":1} /h/p1', 'null /h/a', '{"n":1} /h/p1', '{"n":2} /h/p2']);

  // replaceState replaces the entry.
  await page.evaluate(() => history.replaceState({ r: 1 }, '', '/h/r'));
  assert.equal(await page.evaluate(() => history.length), 4);
  assert.equal(page.url(), `${base}/h/r`);

  // Back across documents to a pushState entry: the URL is loaded again
  // and the entry's state comes back.
  await page.goto(`${base}/h/b`);
  await page.goBack();
  assert.equal(page.url(), `${base}/h/r`);
  assert.deepEqual(await page.evaluate(() => history.state), { r: 1 });

  // Hash changes are entries and fire hashchange.
  await page.goto(`${base}/h/hash`);
  await page.evaluate(() => {
    window.hashes = [];
    addEventListener('hashchange', (e) => hashes.push(e.newURL.split('#')[1] ?? ''));
    location.hash = '#target';
  });
  await page.waitForURL(`${base}/h/hash#target`);
  await page.goBack();
  assert.equal(page.url(), `${base}/h/hash`);
  await page.goForward();
  assert.equal(page.url(), `${base}/h/hash#target`);
  assert.deepEqual(await page.evaluate(() => hashes), ['target', '', 'target']);
  // goto to another fragment of the document stays in it.
  const same = await page.goto(`${base}/h/hash#other`);
  assert.equal(same, null);
  assert.equal(page.url(), `${base}/h/hash#other`);
  await context.close();
});

test('page.goto with a referer: Referer, Sec-Fetch-Site and document.referrer', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/h/b`, { referer: 'http://ref.example.test/start?x=1#frag' });
  const seen = lastRequest('/h/b');
  // Playwright sends referrerPolicy unsafeUrl: the whole URL, no fragment.
  assert.equal(seen.headers.referer, 'http://ref.example.test/start?x=1');
  assert.equal(seen.headers['sec-fetch-site'], 'none');
  assert.equal(await page.evaluate(() => document.referrer), 'http://ref.example.test/start?x=1');
  // No referer option: none.
  await page.goto(`${base}/h/c`);
  assert.equal(lastRequest('/h/c').headers.referer, undefined);
  assert.equal(await page.evaluate(() => document.referrer), '');
  // The other policies, through raw CDP.
  const cdp = await context.newCDPSession(page);
  const origin = 'https://ref.example.test';
  const cases = [
    ['noReferrer', undefined],
    ['origin', `${origin}/`],
    ['sameOrigin', undefined],
    ['strictOriginWhenCrossOrigin', undefined], // https to http is a downgrade
    ['unsafeUrl', `${origin}/page?q=1`],
  ];
  for (const [referrerPolicy, want] of cases) {
    await cdp.send('Page.navigate', { url: `${base}/h/b`, referrer: `${origin}/page?q=1#f`, referrerPolicy });
    await page.waitForLoadState('load');
    assert.equal(lastRequest('/h/b').headers.referer, want, referrerPolicy);
  }
  await assert.rejects(cdp.send('Page.navigate', { url: `${base}/h/b`, referrerPolicy: 'bogus' }), /Invalid referrerPolicy/);
  await context.close();
});

test('storageState with indexedDB, saved and restored', async () => {
  const { base, browser } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/h/a`);
  await page.evaluate(async () => {
    localStorage.setItem('k', 'v');
    await new Promise((res, rej) => {
      const r = indexedDB.open('db1', 1);
      r.onupgradeneeded = () => {
        const s = r.result.createObjectStore('store', { keyPath: 'id' });
        s.put({ id: 1, name: 'one' });
      };
      r.onsuccess = () => {
        r.result.close();
        res();
      };
      r.onerror = () => rej(r.error);
    });
  });
  const state = await context.storageState({ indexedDB: true });
  const origin = state.origins.find((o) => o.origin === base);
  assert.ok(origin, JSON.stringify(state.origins));
  assert.deepEqual(origin.localStorage, [{ name: 'k', value: 'v' }]);
  assert.equal(origin.indexedDB.length, 1);
  assert.equal(origin.indexedDB[0].name, 'db1');
  assert.equal(origin.indexedDB[0].stores.length, 1, JSON.stringify(origin.indexedDB));
  assert.equal(origin.indexedDB[0].stores[0].name, 'store');
  assert.match(JSON.stringify(origin.indexedDB[0].stores[0].records), /one/);
  await context.close();

  const restored = await browser.newContext({ storageState: state, viewport: null });
  const page2 = await restored.newPage();
  await page2.goto(`${base}/h/a`);
  assert.equal(await page2.evaluate(() => localStorage.getItem('k')), 'v');
  const name = await page2.evaluate(
    () =>
      new Promise((res, rej) => {
        const r = indexedDB.open('db1');
        r.onsuccess = () => {
          const g = r.result.transaction('store').objectStore('store').get(1);
          g.onsuccess = () => res(g.result?.name ?? null);
          g.onerror = () => rej(g.error);
        };
        r.onerror = () => rej(r.error);
      }),
  );
  assert.equal(name, 'one');
  await restored.close();
});

test('locator.ariaSnapshot', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/h/aria`);
  const snap = await page.locator('body').ariaSnapshot();
  assert.match(snap, /- heading "Title" \[level=1\]/);
  assert.match(snap, /- navigation "Main":/);
  assert.match(snap, /- link "Home":/);
  assert.match(snap, /- \/url: \/x/);
  assert.match(snap, /- button "Go"/);
  assert.match(snap, /- list:/);
  assert.match(snap, /- listitem: one/);
  await page.locator('nav').ariaSnapshot().then((s) => assert.match(s, /- navigation "Main":/));
  await context.close();
});

test('waitForLoadState networkidle waits for the late request', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/h/idle`, { waitUntil: 'commit' });
  await page.waitForLoadState('networkidle');
  assert.equal(await page.textContent('#out'), 'late data');
  // Already idle: resolves at once.
  await page.waitForLoadState('networkidle');
  await page.goto(`${base}/h/a`, { waitUntil: 'networkidle' });
  assert.equal(await page.title(), 'Page A');
  await context.close();
});

test('addInitScript on the context and the page runs before page scripts, in frames too', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await context.addInitScript(() => {
    window.__init = ['context'];
  });
  await page.addInitScript((arg) => window.__init.push(arg.tag), { tag: 'page' });
  await page.goto(`${base}/h/init`);
  assert.deepEqual(await page.evaluate(() => window.__seen), ['context', 'page']);
  await page.goto(`${base}/h/a`);
  assert.deepEqual(await page.evaluate(() => window.__init), ['context', 'page']);
  // Reload and history navigation run it again.
  await page.reload();
  assert.deepEqual(await page.evaluate(() => window.__init), ['context', 'page']);
  await page.goBack();
  await page.goForward();
  assert.deepEqual(await page.evaluate(() => window.__init), ['context', 'page']);
  // Child frames.
  await page.goto(`${base}/h/withframe`);
  const frame = page.frames().find((f) => f !== page.mainFrame());
  await frame.waitForLoadState('load');
  assert.deepEqual(await frame.evaluate(() => window.__init), ['context', 'page']);
  await context.close();
});

test('request.redirectedFrom and redirectedTo along a chain', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  const resp = await page.goto(`${base}/h/redirect`);
  assert.equal(page.url(), `${base}/h/final`);
  assert.equal(resp.status(), 200);
  const req = resp.request();
  assert.equal(req.url(), `${base}/h/final`);
  const from = req.redirectedFrom();
  assert.equal(from.url(), `${base}/h/redirect`);
  assert.equal(from.redirectedTo(), req);
  assert.equal((await from.response()).status(), 302);
  assert.equal(from.redirectedFrom(), null);

  // Two hops: each request points at the one before it.
  const chain = await page.goto(`${base}/h/redirect2`);
  const last = chain.request();
  assert.equal(last.url(), `${base}/h/final`);
  const mid = last.redirectedFrom();
  assert.equal(mid.url(), `${base}/h/redirect`);
  const first = mid.redirectedFrom();
  assert.equal(first.url(), `${base}/h/redirect2`);
  assert.equal((await first.response()).status(), 301);
  assert.equal(first.redirectedFrom(), null);
  await context.close();
});

test('context.setOffline fails requests and navigations, and recovers', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/h/a`);
  await context.setOffline(true);
  assert.equal(await page.evaluate(() => fetch('/h/data').then(() => 'ok', () => 'failed')), 'failed');
  // (Chrome shows an error page after a failed navigation, LightBrowser
  // stays on the document: nothing is evaluated in between.)
  await assert.rejects(page.goto(`${base}/h/b`), /ERR_INTERNET_DISCONNECTED/);
  await context.setOffline(false);
  await page.goto(`${base}/h/b`);
  assert.equal(await page.title(), 'Page B');
  assert.equal(await page.evaluate(() => fetch('/h/data').then((r) => r.text())), 'late data');
  await context.close();
});

test('javaScriptEnabled: false stops the page scripts, not CDP evaluation', async () => {
  const { base, browser } = await env();
  const context = await browser.newContext({ viewport: null, javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto(`${base}/h/js`);
  assert.equal(await page.title(), 'No JS');
  assert.equal(await page.evaluate(() => 1 + 1), 2);
  await context.close();
  const on = await browser.newContext({ viewport: null });
  const page2 = await on.newPage();
  await page2.goto(`${base}/h/js`);
  assert.equal(await page2.title(), 'script ran');
  await on.close();
});

test('serviceWorkers: block and route.abort with blocked URL patterns still load pages', async () => {
  const { base, browser } = await env();
  const context = await browser.newContext({ viewport: null, serviceWorkers: 'block' });
  const page = await context.newPage();
  await page.goto(`${base}/h/a`);
  assert.equal(await page.title(), 'Page A');
  // Network.setBlockedURLs: a URL pattern fails requests with ERR_BLOCKED_BY_CLIENT.
  const cdp = await context.newCDPSession(page);
  await cdp.send('Network.enable');
  await cdp.send('Network.setBlockedURLs', { urlPatterns: [{ urlPattern: `${base}/h/data`, block: true }] });
  const blocked = await page.evaluate(() => fetch('/h/data?x=1').then(() => 'ok', () => 'blocked'));
  assert.equal(blocked, 'blocked');
  await cdp.send('Network.setBlockedURLs', { urlPatterns: [] });
  assert.equal(await page.evaluate(() => fetch('/h/data').then((r) => r.text())), 'late data');
  await context.close();
});

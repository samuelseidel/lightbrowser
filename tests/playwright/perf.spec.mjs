// Resource Timing of responses a client answers through CDP
// `Fetch.fulfillRequest` (Playwright `route.fulfill`): the entry keeps what
// the network layer would report (ADR 0046). The expected values are what
// Chrome 153 printed for the same requests (`crates/lb-page/tests/perf/
// fulfill.mjs` against the headless shell, loopback): an entry exists, its
// `nextHopProtocol` is empty, the sizes are the body's plus 300 bytes of
// header allowance, and every step of the fetch has a time.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages, html } from './harness.mjs';

pages['/perf'] = html('Perf', '<p>perf</p>');
pages['/perf/real.txt'] = (req, res) => {
  res.writeHead(200, { 'content-type': 'text/plain' });
  res.end('abc');
};

const CHAIN = [
  'startTime', 'fetchStart', 'domainLookupStart', 'domainLookupEnd', 'connectStart', 'connectEnd',
  'requestStart', 'responseStart', 'responseEnd',
];

test('resource entries of fulfilled responses', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.route('**/fulfilled/**', (route) => {
    const u = route.request().url();
    if (u.includes('twelve')) {
      return route.fulfill({ status: 200, contentType: 'text/plain', headers: { 'Timing-Allow-Origin': '*' }, body: 'twelve bytes' });
    }
    if (u.includes('missing')) return route.fulfill({ status: 404, contentType: 'text/plain', body: 'none' });
    return route.fulfill({ status: 200, contentType: 'text/plain', body: 'hello' });
  });
  await page.goto(`${base}/perf`);
  const rows = await page.evaluate(async (chain) => {
    const out = [];
    for (const path of ['/fulfilled/a.txt', '/fulfilled/twelve.txt', '/fulfilled/missing.txt', '/perf/real.txt']) {
      await (await fetch(path)).text();
      const e = performance.getEntriesByName(new URL(path, location.href).href).pop();
      out.push({
        path, found: !!e, initiatorType: e.initiatorType, proto: e.nextHopProtocol, status: e.responseStatus,
        ct: e.contentType, enc: e.encodedBodySize, dec: e.decodedBodySize, xfer: e.transferSize,
        zero: chain.filter((k) => e[k] === 0), workerStart: e.workerStart,
      });
    }
    return out;
  }, CHAIN);
  const want = [
    { path: '/fulfilled/a.txt', proto: '', status: 200, enc: 5, xfer: 305 },
    { path: '/fulfilled/twelve.txt', proto: '', status: 200, enc: 12, xfer: 312 },
    { path: '/fulfilled/missing.txt', proto: '', status: 404, enc: 4, xfer: 304 },
    { path: '/perf/real.txt', proto: 'http/1.1', status: 200, enc: 3, xfer: 303 },
  ];
  for (const [i, w] of want.entries()) {
    const r = rows[i];
    assert.equal(r.found, true, r.path);
    assert.equal(r.initiatorType, 'fetch', r.path);
    assert.equal(r.proto, w.proto, r.path);
    assert.equal(r.status, w.status, r.path);
    assert.equal(r.ct, 'text/plain', r.path);
    assert.equal(r.enc, w.enc, r.path);
    assert.equal(r.dec, w.enc, r.path);
    assert.equal(r.xfer, w.xfer, r.path);
    // Chrome gives every step of the fetch a time, startTime aside.
    assert.deepEqual(r.zero.filter((k) => k !== 'startTime'), [], r.path);
    assert.equal(r.workerStart, 0, r.path);
  }
  await context.close();
});

test('the timeline of a page: navigation entry, resources, marks, observers', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/perf`);
  const r = await page.evaluate(async () => {
    const seen = [];
    const po = new PerformanceObserver((l) => l.getEntries().forEach((e) => seen.push(`${e.entryType}:${e.name}`)));
    po.observe({ entryTypes: ['mark', 'measure'] });
    performance.mark('a');
    performance.mark('b');
    performance.measure('ab', 'a', 'b');
    await new Promise((r) => setTimeout(r, 50));
    const nav = performance.getEntriesByType('navigation')[0];
    return {
      seen,
      navName: nav.name === location.href,
      type: nav.type,
      order: nav.responseEnd >= nav.requestStart && nav.loadEventEnd >= nav.domContentLoadedEventEnd,
      types: PerformanceObserver.supportedEntryTypes.includes('resource'),
    };
  });
  // One callback with the entries by start time (a and b may share a tick).
  assert.deepEqual([...r.seen].sort(), ['mark:a', 'mark:b', 'measure:ab']);
  assert.equal(r.navName, true);
  assert.equal(r.type, 'navigate');
  assert.equal(r.order, true);
  assert.equal(r.types, true);
  await context.close();
});

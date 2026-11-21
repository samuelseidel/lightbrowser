// Resource Timing of responses answered through CDP Fetch.fulfillRequest
// (Playwright route.fulfill), in Chrome 153 (or LightBrowser over CDP).
// Prints, per request, the entry's fields that do not depend on timing; the
// expected values in tests/playwright/perf.test.mjs come from running this
// against Chrome. Usage:
//   python3 serve.py 18991 18992 &
//   PLAYWRIGHT_MJS=.../playwright/index.mjs CHROME=... node fulfill.mjs http://127.0.0.1:18991
// With LB_CDP=ws://... it connects with connectOverCDP instead of launching.
const { chromium } = await import(process.env.PLAYWRIGHT_MJS);
const base = process.argv[2];
const b = process.env.LB_CDP
  ? await chromium.connectOverCDP(process.env.LB_CDP)
  : await chromium.launch({ executablePath: process.env.LB_CHROME || process.env.CHROME, args: ['--no-sandbox', '--disable-background-networking'] });
const ctx = b.contexts()[0] && process.env.LB_CDP ? b.contexts()[0] : await b.newContext();
const p = await ctx.newPage();
await p.route('**/fulfilled/**', (route) => {
  const u = route.request().url();
  if (u.includes('gz')) return route.fulfill({ status: 200, contentType: 'text/plain', headers: { 'Timing-Allow-Origin': '*' }, body: 'twelve bytes' });
  if (u.includes('missing')) return route.fulfill({ status: 404, contentType: 'text/plain', body: 'none' });
  return route.fulfill({ status: 200, contentType: 'text/plain', body: 'hello' });
});
await p.goto(`${base}/res_conn/?x=1`, { waitUntil: 'load' });
await new Promise((r) => setTimeout(r, 300));
const rows = await p.evaluate(async () => {
  const out = [];
  for (const path of ['/fulfilled/a.txt', '/fulfilled/gz.txt', '/fulfilled/missing.txt', '/res_conn/c.txt?size=3&i=9']) {
    await (await fetch(path)).text();
    const e = performance.getEntriesByName(new URL(path, location.href).href).pop();
    const chain = ['startTime', 'fetchStart', 'domainLookupStart', 'domainLookupEnd', 'connectStart', 'connectEnd', 'requestStart', 'responseStart', 'responseEnd'];
    out.push(JSON.stringify({
      path, found: !!e, initiatorType: e && e.initiatorType, proto: e && e.nextHopProtocol, status: e && e.responseStatus, ct: e && e.contentType,
      enc: e && e.encodedBodySize, dec: e && e.decodedBodySize, xfer: e && e.transferSize,
      zero: e && chain.filter((k) => e[k] === 0), nonzero: e && chain.filter((k) => e[k] !== 0),
      workerStart: e && e.workerStart,
    }));
  }
  return out;
});
console.log(rows.join('\n'));
if (!process.env.LB_CDP) await b.close();
else await p.close();

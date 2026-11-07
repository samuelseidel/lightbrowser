// Load each case in local Chrome 153 (loopback only) and print its log once
// the page logged 'end' (or after 10 s). The page URL carries ?x=PORT2, the
// port of the second origin. Usage:
//   python3 serve.py 18991 18992 &
//   PLAYWRIGHT_MJS=.../playwright/index.mjs CHROME=.../chrome-headless-shell \
//     node chrome.mjs http://127.0.0.1:18991 18992 case... > chrome.json
// With RAW=1 the log is printed as is (for probing).
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || '/opt/node22/lib/node_modules/playwright/index.mjs');
const [base, xport, ...cases] = process.argv.slice(2);
const b = await chromium.launch({
  executablePath: process.env.CHROME,
  args: ['--no-sandbox', '--disable-background-networking', '--no-proxy-server'],
});
console.error(b.version());
const out = {};
for (const c of cases) {
  const ctx = await b.newContext();
  const p = await ctx.newPage();
  p.on('pageerror', (e) => console.error(c, 'pageerror', e.message));
  await p.goto(`${base}/${c}/?x=${xport}`, { waitUntil: 'load' });
  // Poll: a case may navigate (reload, history) before it logs 'end'.
  const t0 = Date.now();
  for (;;) {
    const log = await p.evaluate(() => (typeof L !== 'undefined' ? L.join('\n') : '')).catch(() => '');
    if (log.endsWith('\nend') || log === 'end' || Date.now() - t0 > 15000) { out[c] = log; break; }
    await new Promise((r) => setTimeout(r, 50));
  }
  await ctx.close();
}
await b.close();
if (process.env.RAW) for (const c of cases) console.log(out[c]);
else console.log(JSON.stringify(out, null, 1));

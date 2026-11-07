// Load each case in local Chrome (loopback) and print its log once the
// page logged 'end' (or after 10 s), as JSON. Serve `cases/` first:
//   python3 -m http.server 19180 --bind 127.0.0.1 --directory cases &
//   node chrome.mjs http://127.0.0.1:19180 $(ls cases | grep html$ | grep -v child | sed 's/.html//') > chrome.json
// PLAYWRIGHT_MJS: playwright(-core)'s index.mjs; CHROME: the browser binary.
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || 'playwright-core');
const [base, ...cases] = process.argv.slice(2);
const b = await chromium.launch({
  executablePath: process.env.CHROME,
  args: ['--disable-background-networking', '--no-proxy-server'],
});
console.error(b.version());
const out = {};
for (const c of cases) {
  const ctx = await b.newContext();
  const p = await ctx.newPage();
  await p.goto(`${base}/${c}.html`, { waitUntil: 'load' });
  await p.waitForFunction(() => L[L.length - 1] === 'end', null, { timeout: 10000 }).catch(() => {});
  out[c] = await p.evaluate(() => L.join('\n'));
  await ctx.close();
}
await b.close();
console.log(JSON.stringify(out, null, 1));

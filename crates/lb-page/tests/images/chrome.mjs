// Load each image case in local Chrome for Testing (loopback); print its
// log and the server's request counts. Usage: node chrome.mjs BASE case...
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || '/opt/node22/lib/node_modules/playwright/index.mjs');
const [base, ...cases] = process.argv.slice(2);
const b = await chromium.launch({ executablePath: process.env.CHROME || '/opt/pw-browsers/chromium', args: ['--disable-background-networking', '--no-proxy-server'] });
console.error(b.version());
const out = {};
for (const c of cases) {
  const ctx = await b.newContext();
  const p = await ctx.newPage();
  await p.goto(`${base}/${c}/`, { waitUntil: 'load' });
  await p.waitForTimeout(1000);
  const log = await p.evaluate(() => L.join('\n'));
  const hits = await (await fetch(`${base}/__hits/${c}`)).json();
  out[c] = { log, hits };
  await ctx.close();
}
await b.close();
console.log(JSON.stringify(out, null, 1));

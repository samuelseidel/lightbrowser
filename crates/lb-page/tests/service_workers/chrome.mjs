// Load each case in local Chrome for Testing (loopback), each in a fresh
// context, and print its log. A page ends a load with 'end' or 'again';
// after 'again' the page is reloaded and logs again (at most 5 loads).
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || '/opt/node22/lib/node_modules/playwright/index.mjs');
const [base, ...cases] = process.argv.slice(2);
const b = await chromium.launch({ executablePath: process.env.CHROME || '/opt/pw-browsers/chromium', args: ['--disable-background-networking', '--no-proxy-server'] });
console.error(b.version());
const out = {};
for (const c of cases) {
  const ctx = await b.newContext();
  const p = await ctx.newPage();
  const logs = [];
  await p.goto(`${base}/${c}/`, { waitUntil: 'load' });
  for (let i = 0; i < 5; i++) {
    await p.waitForFunction(() => /^(end|again)$/.test(L[L.length - 1]), null, { timeout: 5000 }).catch(() => {});
    const l = await p.evaluate(() => L.join('\n'));
    logs.push(l);
    if (!l.endsWith('again')) break;
    await p.reload({ waitUntil: 'load' });
  }
  out[c] = logs.join('\n');
  await ctx.close();
}
await b.close();
console.log(JSON.stringify(out, null, 1));

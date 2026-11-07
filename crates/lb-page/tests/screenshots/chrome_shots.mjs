// Screenshots of pages/*.html in local Chrome for Testing (loopback):
// node chrome_shots.mjs http://127.0.0.1:PORT outdir [cdp-ws-url]
// With a CDP URL, the same shots from a running `lightbrowser serve`.
import fs from 'node:fs';
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || '/opt/node22/lib/node_modules/playwright/index.mjs');
const [base, out, cdp] = process.argv.slice(2);
const pages = ['images', 'gradients', 'radius_shadow', 'root'];
const b = cdp ? await chromium.connectOverCDP(cdp) : await chromium.launch({ executablePath: process.env.CHROME || '/opt/pw-browsers/chromium', args: ['--disable-background-networking', '--no-proxy-server'] });
const ctx = cdp ? b.contexts()[0] || await b.newContext() : await b.newContext({ viewport: { width: 400, height: 300 }, deviceScaleFactor: 1 });
fs.mkdirSync(out, { recursive: true });
for (const name of pages) {
  const p = await ctx.newPage();
  await p.goto(`${base}/${name}.html`, { waitUntil: 'load' });
  await p.waitForTimeout(300);
  await p.screenshot({ path: `${out}/${name}.png`, clip: { x: 0, y: 0, width: 400, height: 300 } });
  await p.close();
}
await b.close();

// Records shader info logs (logs.html) in local Chrome for Testing on a
// loopback server. Usage: python3 -m http.server 18991 --bind 127.0.0.1 &
//   node record.mjs http://127.0.0.1:18991/logs.html > logs.json
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || '/opt/node22/lib/node_modules/playwright/index.mjs');
const b = await chromium.launch({ executablePath: process.env.CHROME || '/opt/pw-browsers/chromium', args: ['--disable-background-networking', '--no-proxy-server'] });
const p = await b.newPage();
p.on('pageerror', e => console.error('pageerror:', e.message));
await p.goto(process.argv[2]);
await p.waitForFunction(() => window.OUT !== undefined, null, { timeout: 60000 });
console.log(JSON.stringify(await p.evaluate(() => window.OUT), null, 1));
await b.close();

// Load http://insecure.test:PORT/ in local Chrome for Testing (loopback via
// --host-resolver-rules) so the request headers to a non-trustworthy
// origin can be recorded by insecure_serve.py. Usage: node insecure_chrome.mjs PORT
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || '/opt/node22/lib/node_modules/playwright/index.mjs');
const port = process.argv[2];
const b = await chromium.launch({
  executablePath: process.env.CHROME || '/opt/pw-browsers/chromium',
  args: ['--disable-background-networking', '--no-proxy-server', '--host-resolver-rules=MAP insecure.test 127.0.0.1'],
});
const p = await (await b.newContext()).newPage();
await p.goto(`http://insecure.test:${port}/`, { waitUntil: 'load' });
await p.waitForFunction(() => document.title === 'done', null, { timeout: 5000 }).catch(() => {});
await p.waitForTimeout(500);
await b.close();

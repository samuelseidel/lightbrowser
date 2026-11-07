// Record what Chrome for Testing sends to a proxy (loopback only).
// Usage: node chrome.mjs MODE PROXY_URL TARGET_URL... ; MODE: plain | auth
// Targets use names that do not resolve (proxied.test); proxy.py maps every
// host to 127.0.0.1. Prints one line per navigation (status or error).
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || '/opt/node22/lib/node_modules/playwright/index.mjs');
const [mode, server, ...targets] = process.argv.slice(2);
const b = await chromium.launch({
  executablePath: process.env.CHROME || '/opt/pw-browsers/chromium',
  args: ['--disable-background-networking'],
  proxy: { server, bypass: '<-loopback>' },
});
console.error(b.version());
const ctx = await b.newContext({
  ignoreHTTPSErrors: true,
  ...(mode === 'auth' ? { httpCredentials: { username: 'u1', password: 'p1' } } : {}),
});
const p = await ctx.newPage();
for (const t of targets) {
  try {
    const r = await p.goto(t, { waitUntil: 'load', timeout: 10000 });
    console.log(t, r && r.status());
  } catch (e) {
    console.log(t, 'error', e.message.split('\n')[0]);
  }
}
await b.close();

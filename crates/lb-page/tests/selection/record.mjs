// Record a selection page (modify.html, api.html) in local Chrome or in
// LightBrowser over CDP, loopback only, and print its result (window.R) as
// JSON once the page set its title to 'done'.
//   python3 -m http.server 19160 --bind 127.0.0.1   (in this directory)
//   node record.mjs http://127.0.0.1:19160 modify.html [CHROME_EXE | http://127.0.0.1:CDP_PORT]
// Chrome 153 headless shell: the macOS recording (*.mac.json) on a Mac, the
// Linux one (*.linux.json) on Ubuntu 24.04. Set PLAYWRIGHT_MJS to the
// playwright(-core) entry point if 'playwright' does not resolve.
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || 'playwright');
const [base, page, target] = process.argv.slice(2);
let b, ctx;
if (target && target.startsWith('http')) {
  const v = await (await fetch(`${target}/json/version`)).json();
  b = await chromium.connectOverCDP(v.webSocketDebuggerUrl);
  ctx = b.contexts()[0] ?? (await b.newContext());
} else {
  b = await chromium.launch({ executablePath: target || process.env.CHROME, args: ['--disable-background-networking', '--no-proxy-server'] });
  ctx = await b.newContext();
}
console.error(b.version());
const p = await ctx.newPage();
p.on('pageerror', (e) => console.error('pageerror', String(e)));
await p.goto(`${base}/${page}`, { waitUntil: 'commit', timeout: 300000 });
await p.waitForFunction(() => document.title === 'done', null, { timeout: 300000, polling: 200 });
console.log(await p.evaluate(() => JSON.stringify(window.R)));
await p.close();
await b.close();

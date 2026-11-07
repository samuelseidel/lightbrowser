// Load each CSP case in local Chrome (loopback only) and print, per case,
// the page's log (once it logged 'end', or after 10 s), the console
// messages about Content Security Policy (CDP Log.entryAdded) and the
// requests the server saw under /<case>/.
// Usage: CHROME=/path/to/chrome-headless-shell node chrome.mjs BASE case...
// (PLAYWRIGHT_MJS: path of playwright-core's index.mjs if not installed.)
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || 'playwright-core');
const [base, ...cases] = process.argv.slice(2);
const b = await chromium.launch({
  executablePath: process.env.CHROME,
  args: ['--disable-background-networking', '--no-proxy-server'],
});
console.error(b.version());
const CSP = /Content[- ]Security[- ]Policy/i;
const out = {};
for (const c of cases) {
  // CASE@bypass: the same page with Playwright's bypassCSP (Page.setBypassCSP).
  const [dirName, mode] = c.split('@');
  const ctx = await b.newContext(mode === 'bypass' ? { bypassCSP: true } : {});
  const seen = (await (await fetch(`${base}/__log/${dirName}`)).json()).length;
  const p = await ctx.newPage();
  const cdp = await ctx.newCDPSession(p);
  const consoleLines = [];
  cdp.on('Log.entryAdded', ({ entry }) => {
    if (CSP.test(entry.text)) consoleLines.push(entry.level + ' ' + entry.text);
  });
  cdp.on('Runtime.exceptionThrown', ({ exceptionDetails: d }) => {
    const text = d.exception && d.exception.description ? d.exception.description.split('\n')[0] : d.text;
    consoleLines.push('exception ' + text);
  });
  await cdp.send('Log.enable');
  await cdp.send('Runtime.enable');
  await p.goto(`${base}/${dirName}/`, { waitUntil: 'load' }).catch((e) => consoleLines.push('goto ' + e.message));
  await p.waitForFunction(() => typeof L !== 'undefined' && L[L.length - 1] === 'end', null, { timeout: 10000 }).catch(() => {});
  const log = await p.evaluate(() => (typeof L === 'undefined' ? '' : L.join('\n'))).catch((e) => 'evaluate failed');
  const requests = (await (await fetch(`${base}/__log/${dirName}`)).json()).slice(seen);
  out[c] = { log, console: consoleLines, requests };
  await ctx.close();
}
await b.close();
console.log(JSON.stringify(out, null, 1));

// Record how Chrome 153 reports a request the client blocked
// (Fetch.failRequest with BlockedByClient, which is what Playwright's
// route.abort('blockedbyclient') sends) on the surfaces the egress policy
// refuses on (ADR 0044): top-level navigation, fetch, XHR, image, script and
// worker fetch. Loopback only: the blocked host (blocked.test) is never
// looked up, the request is failed before the network.
//
// WebSocket: no CDP command blocks one, so its text is recorded for a host
// that does not resolve instead; the policy's text differs only in the code.
//
//   NPM_CONFIG_PREFIX=... CHROME=/path/to/chrome-headless-shell \
//     node chrome.mjs PORT > chrome.json
//
// PORT: a free loopback port for the page server.
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { execSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const { chromium } = require(path.join(execSync('npm root -g').toString().trim(), 'playwright'));
const port = Number(process.argv[2]);
const probes = fs.readFileSync(path.join(here, 'probes.html'));

const server = http.createServer((req, res) => {
  res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
  res.end(probes);
});
await new Promise((r) => server.listen(port, '127.0.0.1', r));

const browser = await chromium.launch({
  executablePath: process.env.CHROME,
  args: ['--disable-background-networking', '--no-proxy-server'],
});
const ctx = await browser.newContext();
await ctx.route(/^http:\/\/blocked\.test:8099\//, (route) => route.abort('blockedbyclient'));
const consoleMessages = [];
const out = { chrome: browser.version() };

// A blocked top-level navigation, as a client sees it: Playwright's message
// and the raw errorText of Page.navigate.
const page = await ctx.newPage();
const cdp = await ctx.newCDPSession(page);
const nav = await cdp.send('Page.navigate', { url: 'http://blocked.test:8099/' });
out.navigate = { errorText: nav.errorText };
out.playwright_goto = await page
  .goto('http://blocked.test:8099/')
  .then(() => 'resolved', (e) => e.message.split('\n')[0]);

// A page on loopback that probes every surface (a new page: the first one is
// on Chrome's error page now).
const probePage = await ctx.newPage();
probePage.on('console', (m) => consoleMessages.push({ type: m.type(), text: m.text() }));
await probePage.goto(`http://127.0.0.1:${port}/`);
await probePage.waitForFunction(() => window.R.done === true, null, { timeout: 15000 });
out.probes = await probePage.evaluate(() => {
  const r = { ...window.R };
  delete r.done;
  return r;
});
out.console = consoleMessages.map((m) => `${m.type}: ${m.text}`).sort();
await browser.close();
server.close();
console.log(JSON.stringify(out, null, 1));

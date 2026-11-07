// Content Security Policy over CDP (ADR 0036): rebrowser's "bypassCsp"
// check. A `<meta>` policy whose `script-src` does not list the script's
// host blocks it (no request, an `error` event and one violation), and
// Playwright's `bypassCSP` (CDP `Page.setBypassCSP`) lets it load, as in
// Chrome 153 (crates/lb-page/tests/csp/chrome.json, `rebrowser_bypass`).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, pages, requests } from './harness.mjs';

const policy =
  "default-src * data: 'unsafe-eval' 'unsafe-inline' blob:; script-src blob: 'self' 'unsafe-eval' 'unsafe-inline' https://static.cloudflareinsights.com https://cloudflareinsights.com;";

pages['/csp'] = `<!DOCTYPE html><html><head><meta http-equiv="Content-Security-Policy" content="${policy}"></head><body><script>
window.L = [];
document.addEventListener('securitypolicyviolation', (e) => L.push('violation ' + e.violatedDirective + ' ' + e.blockedURI.replace(/:\\d+/, ':PORT')));
const s = document.createElement('script');
s.src = 'http://localhost:' + location.port + '/csp-ext.js';
s.onload = () => L.push('load');
s.onerror = (e) => L.push('error ' + e.isTrusted);
document.head.appendChild(s);
</script></body></html>`;
pages['/csp-ext.js'] = (req, res) => {
  res.writeHead(200, { 'content-type': 'text/javascript' });
  res.end("L.push('ext ran');");
};

async function run(bypassCSP) {
  const { browser, base } = await env();
  const context = await browser.newContext({ viewport: null, bypassCSP });
  const page = await context.newPage();
  const before = requests.filter((r) => r.url === '/csp-ext.js').length;
  await page.goto(`${base}/csp`);
  await page.waitForFunction(() => L.length >= (window.__want || 2), null, { timeout: 5000 }).catch(() => {});
  const log = await page.evaluate(() => L.slice().sort());
  const fetched = requests.filter((r) => r.url === '/csp-ext.js').length - before;
  await context.close();
  return { log, fetched };
}

test('a script the meta policy does not allow is blocked: error event, violation, no request', async () => {
  const { log, fetched } = await run(false);
  assert.deepEqual(log, ['error true', 'violation script-src-elem http://localhost:PORT/csp-ext.js']);
  assert.equal(fetched, 0);
});

test('bypassCSP (Page.setBypassCSP) loads and runs it', async () => {
  const { log, fetched } = await run(true);
  assert.deepEqual(log, ['ext ran', 'load']);
  assert.equal(fetched, 1);
});

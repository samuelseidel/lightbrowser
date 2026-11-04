// Proxies per browser context (ADR 0027): Playwright's newContext({ proxy })
// sends Target.createBrowserContext { proxyServer, proxyBypassList:
// '<-loopback>' }, and httpCredentials answer the proxy's 407 through
// Fetch.authRequired / Fetch.continueWithAuth. The proxies are
// crates/lb-net/tests/proxy/proxy.py on loopback.

import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { env, pages, html } from './harness.mjs';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const procs = [];
after(() => procs.forEach((p) => p.kill()));

/** proxy.py with an HTTP and a SOCKS5 port; its log lines in `log`. */
function startProxy(auth) {
  const args = [path.join(repo, 'crates/lb-net/tests/proxy/proxy.py'), '--http', '0', '--socks', '0'];
  if (auth) args.push('--auth', auth);
  const proc = spawn('python3', args, { stdio: ['ignore', 'pipe', 'pipe'] });
  procs.push(proc);
  const log = [];
  let buf = '';
  proc.stdout.on('data', (c) => {
    buf += c;
    const lines = buf.split('\n');
    buf = lines.pop();
    for (const l of lines) if (l) log.push(JSON.parse(l));
  });
  return new Promise((resolve, reject) => {
    proc.stderr.once('data', (c) => {
      const [, http, socks] = String(c).trim().split(/\s+/);
      resolve({ http: Number(http), socks: Number(socks), log });
    });
    proc.on('exit', (code) => reject(new Error(`proxy.py exited ${code}`)));
  });
}

pages['/proxied'] = html(
  'Proxied',
  `<p id="out">waiting</p>
  <script>fetch('/proxied-api').then((r) => r.text()).then((t) => { document.getElementById('out').textContent = t; });</script>`,
);
pages['/proxied-api'] = (req, res) => {
  res.writeHead(200, { 'content-type': 'text/plain' });
  res.end('through ' + (req.headers['x-via'] || 'proxy'));
};

test('context proxies: HTTP with credentials via Fetch.authRequired, SOCKS5, bypass', async () => {
  const { browser, base } = await env();
  const port = new URL(base).port;

  // HTTP proxy that wants credentials; Playwright answers authRequired.
  const authed = await startProxy('u1:p1');
  const ctx = await browser.newContext({
    viewport: null,
    proxy: { server: `http://127.0.0.1:${authed.http}` },
    httpCredentials: { username: 'u1', password: 'p1' },
  });
  const page = await ctx.newPage();
  await page.goto(`${base}/proxied`);
  await page.waitForFunction(() => document.getElementById('out').textContent !== 'waiting');
  assert.equal(await page.textContent('#out'), 'through proxy');
  const lines = authed.log.filter((e) => e.proxy === 'http').map((e) => e.line);
  assert.deepEqual(lines.slice(0, 2), [
    `GET http://127.0.0.1:${port}/proxied HTTP/1.1`,
    `GET http://127.0.0.1:${port}/proxied HTTP/1.1`,
  ]);
  assert.ok(lines.includes(`GET http://127.0.0.1:${port}/proxied-api HTTP/1.1`), lines.join('\n'));
  const auth = (e) => e.headers.some(([n, v]) => n === 'Proxy-Authorization' && v === 'Basic dTE6cDE=');
  const http = authed.log.filter((e) => e.proxy === 'http');
  assert.ok(!auth(http[0]) && auth(http[1]) && auth(http[http.length - 1]));
  await ctx.close();

  // Without credentials: Chrome's error.
  const ctx2 = await browser.newContext({ viewport: null, proxy: { server: `http://127.0.0.1:${authed.http}` } });
  const page2 = await ctx2.newPage();
  await assert.rejects(page2.goto(`${base}/proxied`), /net::ERR_INVALID_AUTH_CREDENTIALS/);
  await ctx2.close();

  // SOCKS5 and a bypassed context.
  const open = await startProxy();
  const ctx3 = await browser.newContext({ viewport: null, proxy: { server: `socks5://127.0.0.1:${open.socks}` } });
  const page3 = await ctx3.newPage();
  await page3.goto(`${base}/proxied`);
  await page3.waitForFunction(() => document.getElementById('out').textContent !== 'waiting');
  const socks = open.log.filter((e) => e.proxy === 'socks' && e.request);
  assert.ok(socks.length >= 1);
  assert.equal(socks[0].host, '127.0.0.1');
  assert.deepEqual(socks[0].request, [5, 1, 0, 1]);
  await ctx3.close();

  const before = open.log.length;
  const ctx4 = await browser.newContext({
    viewport: null,
    proxy: { server: `http://127.0.0.1:${open.http}`, bypass: '127.0.0.1' },
  });
  const page4 = await ctx4.newPage();
  await page4.goto(`${base}/proxied`);
  assert.equal(open.log.length, before);
  await ctx4.close();
});

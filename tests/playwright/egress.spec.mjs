// The egress policy (ADR 0044) through stock Playwright: a `serve` started
// with `--egress loopback` fails a request to a host that is not allowed the
// way Chrome fails a request the client blocked, and sends nothing. The
// expected texts are Chrome 153's, recorded in
// crates/lb-page/tests/egress/chrome.json (Playwright's own `page.goto`
// message and the page-visible results of every subresource surface).
// Loopback only: `blocked.test` is never looked up.

import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { readFileSync, existsSync } from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from './harness.mjs';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const bin = process.env.LIGHTBROWSER_BIN || path.join(repo, 'target/debug/lightbrowser');
const chrome = JSON.parse(readFileSync(path.join(repo, 'crates/lb-page/tests/egress/chrome.json'), 'utf8'));
const probes = readFileSync(path.join(repo, 'crates/lb-page/tests/egress/probes.html'));

const cleanup = [];
after(async () => {
  for (const f of cleanup) await f();
});

/** `lightbrowser serve --egress loopback ...`, connected over CDP. */
async function start(extra = []) {
  assert.ok(existsSync(bin), `${bin} not found`);
  const proc = spawn(bin, ['serve', '--port', '0', '--egress', 'loopback', ...extra], {
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  cleanup.push(() => proc.kill());
  const ws = await new Promise((resolve, reject) => {
    let log = '';
    proc.stderr.on('data', (c) => {
      log += c;
      const m = log.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) resolve(m[1]);
    });
    proc.on('exit', (code) => reject(new Error(`serve exited ${code}:\n${log}`)));
  });
  const browser = await chromium.connectOverCDP(ws.replace(/^ws:\/\/([^/]+)\/.*$/, 'http://$1'));
  cleanup.push(() => browser.close().catch(() => {}));
  return browser;
}

const site = await new Promise((resolve) => {
  const s = http.createServer((req, res) => {
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    res.end(req.url === '/probes' ? probes : '<body>loopback ok</body>');
  });
  s.listen(0, '127.0.0.1', () => resolve(s));
});
cleanup.push(() => site.close());
const base = `http://127.0.0.1:${site.address().port}`;

test('egress: a refused navigation fails like a blocked request in Chrome', async () => {
  const browser = await start();
  const page = await (await browser.newContext()).newPage();
  const msg = await page.goto('http://blocked.test:8099/').then(
    () => 'resolved',
    (e) => e.message.split('\n')[0],
  );
  assert.equal(msg, chrome.playwright_goto);
  // Loopback still works.
  await page.goto(base + '/');
  assert.equal(await page.textContent('body'), 'loopback ok');
});

test('egress: every subresource surface fails as Chrome reports it', async () => {
  const browser = await start();
  const page = await (await browser.newContext()).newPage();
  await page.goto(base + '/probes');
  await page.waitForFunction(() => window.R.done === true, null, { timeout: 15000 });
  const got = await page.evaluate(() => {
    const r = { ...window.R };
    delete r.done;
    return r;
  });
  assert.deepEqual(got, chrome.probes);
});

test('egress: responses a CDP client supplies are not network', async () => {
  const browser = await start();
  const context = await browser.newContext();
  await context.route(/^http:\/\/blocked\.test:8099\//, (route) =>
    route.fulfill({
      status: 200,
      contentType: 'text/plain',
      headers: { 'access-control-allow-origin': '*' }, // cross-origin fetch
      body: 'fulfilled',
    }),
  );
  const page = await context.newPage();
  await page.goto(base + '/');
  const text = await page.evaluate(() => fetch('http://blocked.test:8099/x').then((r) => r.text()));
  assert.equal(text, 'fulfilled');
});

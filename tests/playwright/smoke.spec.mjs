// Playwright smoke test against `lightbrowser serve` (loopback only).
//
//   cargo build -p lightbrowser
//   node --test tests/playwright/
//
// Uses the globally installed `playwright` package (`npm root -g`), stock
// `chromium.connectOverCDP`. Set LIGHTBROWSER_BIN to test another binary.
// Until lb-page is wired in, `serve` runs the mock page host: it serves
// built-in pages for loopback URLs and never touches the network.

import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const require = createRequire(import.meta.url);
const globalRoot = execSync('npm root -g').toString().trim();
const { chromium } = require(path.join(globalRoot, 'playwright'));

const bin = process.env.LIGHTBROWSER_BIN || path.join(repo, 'target/debug/lightbrowser');
let server;
let endpoint;

before(async () => {
  assert.ok(existsSync(bin), `${bin} not found; run cargo build -p lightbrowser first`);
  server = spawn(bin, ['serve', '--port', '0'], { cwd: repo, stdio: ['ignore', 'ignore', 'pipe'] });
  endpoint = await new Promise((resolve, reject) => {
    let log = '';
    const timer = setTimeout(() => reject(new Error(`no DevTools line:\n${log}`)), 15000);
    server.stderr.on('data', (chunk) => {
      log += chunk;
      const m = log.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
    server.on('exit', (code) => reject(new Error(`serve exited with ${code}:\n${log}`)));
  });
});

after(() => {
  server?.kill();
});

test('connectOverCDP, newContext, newPage, goto, evaluate, content', async () => {
  // The http endpoint, like a user would pass it; Playwright reads
  // /json/version to find the WebSocket.
  const http = endpoint.replace(/^ws:\/\/([^/]+)\/.*$/, 'http://$1');
  const browser = await chromium.connectOverCDP(http);
  try {
    assert.match(browser.version(), /^153\./);
    // viewport: null keeps the persona's own window size; Playwright's
    // default 1280x720 screen would contradict the persona and be refused.
    const context = await browser.newContext({ viewport: null });
    const page = await context.newPage();
    const response = await page.goto('http://127.0.0.1:9/smoke');
    assert.equal(response.status(), 200);
    assert.equal(page.url(), 'http://127.0.0.1:9/smoke');
    assert.equal(await page.evaluate('1+1'), 2);
    assert.equal(await page.evaluate(() => 1 + 1), 2);
    assert.equal(await page.evaluate((a) => a * 2, 21), 42);
    assert.equal(await page.title(), 'LightBrowser mock');
    const html = await page.content();
    assert.match(html, /^<!DOCTYPE html><html><head><title>LightBrowser mock<\/title>/);
    assert.match(html, /<button id="go" type="button">Log in<\/button>/);
    // The persona is enforced: a contradicting viewport is an error.
    await assert.rejects(page.setViewportSize({ width: 5000, height: 5000 }), /contradicts the persona/);
    await context.close();
  } finally {
    await browser.close();
  }
});

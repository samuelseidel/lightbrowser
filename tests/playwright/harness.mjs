// Shared setup for the Playwright specs: one local site on 127.0.0.1 and
// one `lightbrowser serve`, started once per test process and stopped at
// the end. Loopback only.
//
// Uses stock `playwright` (`chromium.connectOverCDP`). package.json pins the
// version the suite is checked with (`npm install --prefix tests/playwright`).
// The package is looked up in this order: a local install (node_modules next
// to this file or above it), the directories of $NODE_PATH, then the global
// install (`npm root -g`). Set LIGHTBROWSER_BIN to test another binary.

import { after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');
const require = createRequire(import.meta.url);

/** The entry file of the `playwright` package, found as the header says. */
function findPlaywright() {
  // Node's own lookup covers the first two places, in that order: node_modules
  // directories from this file upwards, then $NODE_PATH.
  try {
    return require.resolve('playwright');
  } catch (e) {
    // Anything but "playwright itself is missing" is a broken install: say so.
    if (e?.code !== 'MODULE_NOT_FOUND' || !/Cannot find module 'playwright'/.test(e.message)) throw e;
  }
  let globalRoot = '';
  try {
    globalRoot = execSync('npm root -g', { stdio: ['ignore', 'pipe', 'ignore'] }).toString().trim();
  } catch {}
  if (globalRoot && existsSync(path.join(globalRoot, 'playwright'))) {
    return require.resolve(path.join(globalRoot, 'playwright'));
  }
  throw new Error(
    `Playwright was not found in node_modules next to or above ${here}, in NODE_PATH (${process.env.NODE_PATH || 'unset'}) ` +
      `or in the global install (${globalRoot || '`npm root -g` failed'}).\n` +
      'Install the pinned version: npm install --prefix tests/playwright',
  );
}

/** Path of the resolved `playwright` entry file; `playwright-core` sits next to it. */
export const playwrightEntry = findPlaywright();
export const { chromium } = require(playwrightEntry);

const bin = process.env.LIGHTBROWSER_BIN || path.join(repo, 'target/debug/lightbrowser');

/** Pages of the local site, by path. Values are HTML or a handler. */
export const pages = {};
/** Requests the site saw: `{ method, url, headers, body }`. */
export const requests = [];

const html = (title, body, head = '') =>
  `<!DOCTYPE html><html><head><meta charset="utf-8"><title>${title}</title>${head}</head><body>${body}</body></html>`;
export { html };

function serveSite() {
  const server = http.createServer((req, res) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      requests.push({ method: req.method, url: req.url, headers: req.headers, body });
      const { pathname } = new URL(req.url, 'http://x');
      const page = pages[pathname];
      if (typeof page === 'function') return page(req, res, body);
      if (page === undefined) {
        res.writeHead(404, { 'content-type': 'text/plain' });
        return res.end('not found');
      }
      res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
      res.end(page);
    });
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

function serveBrowser() {
  assert.ok(existsSync(bin), `${bin} not found; run cargo build -p lightbrowser first`);
  // LIGHTBROWSER_SERVE_ARGS: extra arguments for `serve` (space separated),
  // e.g. `--egress loopback` to run every suite under the egress policy.
  const extra = (process.env.LIGHTBROWSER_SERVE_ARGS || '').split(/\s+/).filter(Boolean);
  const proc = spawn(bin, ['serve', '--port', '0', ...extra], { cwd: repo, stdio: ['ignore', 'ignore', 'pipe'] });
  const endpoint = new Promise((resolve, reject) => {
    let log = '';
    const timer = setTimeout(() => reject(new Error(`no DevTools line:\n${log}`)), 15000);
    proc.stderr.on('data', (chunk) => {
      if (process.env.LB_LOG) process.stderr.write(chunk);
      log += chunk;
      const m = log.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
    proc.on('exit', (code) => reject(new Error(`serve exited with ${code}:\n${log}`)));
  });
  return { proc, endpoint };
}

let shared;

/** The site's base URL, the CDP endpoint and a connected browser. */
export function env() {
  if (!shared) {
    shared = (async () => {
      const t0 = performance.now();
      const site = await serveSite();
      const { proc, endpoint } = serveBrowser();
      const ws = await endpoint;
      // The http endpoint, like a user would pass it; Playwright reads
      // /json/version to find the WebSocket.
      const httpEndpoint = ws.replace(/^ws:\/\/([^/]+)\/.*$/, 'http://$1');
      const browser = await chromium.connectOverCDP(httpEndpoint);
      const base = `http://127.0.0.1:${site.address().port}`;
      return { site, proc, browser, base, ws, startedMs: performance.now() - t0 };
    })();
  }
  return shared;
}

// Registered at import (root level), so it runs once after every test.
after(async () => {
  if (!shared) return;
  const e = await shared;
  await e.browser.close().catch(() => {});
  e.proc.kill();
  e.site.close();
});

/** A fresh context (persona viewport) and page. */
export async function newPage() {
  const { browser } = await env();
  // viewport: null keeps the persona's own window size (Playwright's
  // default 1280x720 viewport also works: ADR 0009, revision 2).
  const context = await browser.newContext({ viewport: null });
  const page = await context.newPage();
  return { context, page };
}

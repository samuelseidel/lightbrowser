// Runs api.html in local Chrome for Testing and in LightBrowser (`serve`)
// on loopback and prints every value that differs.
// Usage: node api.mjs [outdir]   (LIGHTBROWSER_BIN, LB_PERSONA override)
import { spawn } from 'node:child_process';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../../../..');
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || '/opt/node22/lib/node_modules/playwright/index.mjs');
const out = process.argv[2] || fs.mkdtempSync('/tmp/lbscenes-');
const page = fs.readFileSync(path.join(here, 'api.html'));
const server = http.createServer((q, r) => { r.writeHead(q.url === '/' ? 200 : 404, { 'content-type': 'text/html' }); r.end(q.url === '/' ? page : ''); });
await new Promise((ok) => server.listen(0, '127.0.0.1', ok));
const url = `http://127.0.0.1:${server.address().port}/`;
async function run(browser) {
  const ctx = browser.contexts()[0] || await browser.newContext();
  const p = await ctx.newPage();
  await p.goto(url);
  await p.waitForFunction(() => window.OUT !== undefined, null, { timeout: 120000 });
  const r = await p.evaluate(() => window.OUT);
  await p.close();
  return r;
}
const chrome = await chromium.launch({ executablePath: process.env.CHROME || '/opt/pw-browsers/chromium', args: ['--no-proxy-server'] });
const c = await run(chrome);
await chrome.close();
const bin = process.env.LIGHTBROWSER_BIN || path.join(repo, 'target/debug/lightbrowser');
const persona = process.env.LB_PERSONA || path.join(repo, 'profiles/reference/chrome/chrome153_ubuntu24_intel_uhd630_x11_1080p.json');
const proc = spawn(bin, ['serve', '--port', '0', '--persona', persona], { stdio: ['ignore', 'ignore', 'pipe'] });
const ws = await new Promise((ok, no) => { let log = ''; proc.stderr.on('data', (d) => { log += d; const m = log.match(/DevTools listening on (ws:\/\/\S+)/); if (m) ok(m[1]); }); proc.on('exit', () => no(new Error(log))); });
const lbb = await chromium.connectOverCDP(ws);
const l = await run(lbb);
await lbb.close().catch(() => {});
proc.kill();
server.close();
fs.writeFileSync(path.join(out, 'api_chrome.json'), JSON.stringify(c, null, 1));
fs.writeFileSync(path.join(out, 'api_lightbrowser.json'), JSON.stringify(l, null, 1));
let diffs = 0, total = 0;
for (const g of Object.keys(c)) for (const k of Object.keys(c[g])) {
  total++;
  const a = JSON.stringify(c[g][k]), b = JSON.stringify(l[g]?.[k]);
  if (a !== b) { diffs++; console.log(`${g}.${k}\n  chrome:       ${a}\n  lightbrowser: ${b}`); }
}
console.log(`${total - diffs} of ${total} values equal`);

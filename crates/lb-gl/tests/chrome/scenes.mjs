// Renders scenes.html in local Chrome for Testing (SwiftShader) and in
// LightBrowser (`serve`, a reference persona) on loopback and compares
// the readPixels output per scene (plausibility, not equality).
// Usage: node scenes.mjs [outdir]   (LIGHTBROWSER_BIN, LB_PERSONA override)
import { spawn } from 'node:child_process';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../../../..');
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || '/opt/node22/lib/node_modules/playwright/index.mjs');
const out = process.argv[2] || fs.mkdtempSync('/tmp/lbscenes-');
const page = fs.readFileSync(path.join(here, 'scenes.html'));
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
fs.writeFileSync(path.join(out, 'chrome.json'), JSON.stringify(c));
fs.writeFileSync(path.join(out, 'lightbrowser.json'), JSON.stringify(l));
const rows = [];
for (const k of Object.keys(c)) {
  const a = c[k], b = l[k];
  if (!a?.b64 || !b?.b64) { rows.push({ scene: k, chrome: a?.error || 'ok', lightbrowser: b?.error || 'ok' }); continue; }
  const x = Buffer.from(a.b64, 'base64'), y = Buffer.from(b.b64, 'base64');
  let sum = 0, diff = 0, big = 0;
  for (let i = 0; i < x.length; i += 4) {
    let d = 0; for (let j = 0; j < 4; j++) { const e = Math.abs(x[i + j] - y[i + j]); d = Math.max(d, e); sum += e; }
    if (d > 2) diff++; if (d > 48) big++;
  }
  const n = x.length / 4;
  rows.push({ scene: k, size: `${a.w}x${a.h}`, meanAbs: +(sum / x.length).toFixed(2), pxDiffOver2: `${(100 * diff / n).toFixed(1)}%`, pxDiffOver48: `${(100 * big / n).toFixed(1)}%`, err: `${a.err}/${b.err}` });
}
console.table(rows);
console.log('raw output in', out);

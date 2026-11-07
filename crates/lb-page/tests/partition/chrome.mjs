// Run each partitioning case in local Chrome (loopback) and print the logs.
// Chrome is started directly (not by Playwright's launcher, whose default
// flags turn ThirdPartyStoragePartitioning off) and driven over CDP; each
// case gets a fresh browser context. A case's `steps` file lists its pages
// (`A index.html`: http://127.0.0.1:PORT/<case>/index.html; B: localhost),
// each opened in a new page of the same context once the last one logged
// 'end' (or after 15 s); the step logs are joined with '--'.
// Usage: CHROME=<chromium or chrome-headless-shell> node chrome.mjs PORT case...
// (PLAYWRIGHT_MJS: playwright-core's index.mjs; DEFAULT_CONTEXT=1 uses the
// browser's default context instead of new ones.)
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || 'playwright-core');
const here = dirname(fileURLToPath(import.meta.url));
const [port, ...cases] = process.argv.slice(2);
const dir = mkdtempSync(join(tmpdir(), 'lbpart-'));
const proc = spawn(process.env.CHROME || '/opt/pw-browsers/chromium', ['--headless', '--no-sandbox', `--user-data-dir=${dir}`,
  '--remote-debugging-port=0', '--disable-background-networking', '--no-proxy-server', '--no-first-run', 'about:blank'], { stdio: 'ignore' });
let ws;
for (let i = 0; i < 100 && !ws; i++) {
  await new Promise(r => setTimeout(r, 100));
  const f = join(dir, 'DevToolsActivePort');
  if (existsSync(f)) { const [p] = readFileSync(f, 'utf8').split('\n'); if (p) ws = `http://127.0.0.1:${p}`; }
}
const b = await chromium.connectOverCDP(ws);
console.error(b.version());
const out = {};
for (const c of cases) {
  const steps = readFileSync(join(here, 'cases', c, 'steps'), 'utf8').trim().split('\n').map(l => l.split(' '));
  const ctx = process.env.DEFAULT_CONTEXT ? b.contexts()[0] : await b.newContext();
  const logs = [];
  for (const [site, file] of steps) {
    const p = await ctx.newPage();
    await p.goto(`http://${site === 'B' ? 'localhost' : '127.0.0.1'}:${port}/${c}/${file}`, { waitUntil: 'load' });
    await p.waitForFunction(() => L[L.length - 1] === 'end', null, { timeout: 15000 }).catch(() => {});
    logs.push(await p.evaluate(() => L.join('\n')));
    await p.close();
  }
  out[c] = logs.join('\n--\n').split(port).join('PORT');
  if (!process.env.DEFAULT_CONTEXT) await ctx.close();
}
await b.close();
proc.kill();
console.log(JSON.stringify(out, null, 1));

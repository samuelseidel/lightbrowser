// Load each case in local Chrome 153 (loopback only) and print its log
// once the page logged 'end' (or after 40 s), plus the console messages
// Chrome's frame itself made ("<case>.console", one "level: text" per
// line; console.* calls of the page are left out). Cases with steps.json
// get those steps first (CDP commands, Playwright mouse/keyboard calls,
// waits, evaluations). Usage: node chrome.mjs BASE case...
// Chrome runs without Playwright's --disable-ipc-flooding-protection,
// which switches the history rate limit off. A case with an (empty) file
// `persistent` runs in a profile directory of its own, as a regular
// Chrome profile (newContext() is off the record: quota and storage
// differ).
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || '/opt/node22/lib/node_modules/playwright/index.mjs');
const here = path.dirname(fileURLToPath(import.meta.url));
const [base, ...cases] = process.argv.slice(2);
const b = await chromium.launch({
  executablePath: process.env.LB_CHROME || process.env.CHROME || '/opt/pw-browsers/chromium',
  args: ['--disable-background-networking', '--no-proxy-server'],
  ignoreDefaultArgs: ['--disable-ipc-flooding-protection'],
});
console.error(b.version());
const out = {};
for (const c of cases) {
  const dir = fs.existsSync(path.join(here, 'cases', c, 'persistent')) ? fs.mkdtempSync(path.join(os.tmpdir(), 'webplat-')) : null;
  const ctx = dir
    ? await chromium.launchPersistentContext(dir, { executablePath: process.env.LB_CHROME || process.env.CHROME || '/opt/pw-browsers/chromium', args: ['--disable-background-networking', '--no-proxy-server'], ignoreDefaultArgs: ['--disable-ipc-flooding-protection'] })
    : await b.newContext();
  const p = await ctx.newPage();
  const cdp = await ctx.newCDPSession(p);
  const browserLog = [];
  await cdp.send('Log.enable');
  cdp.on('Log.entryAdded', (e) => browserLog.push(`${e.entry.level}: ${e.entry.text}`));
  await p.goto(`${base}/${c}/`, { waitUntil: 'load' });
  const steps = path.join(here, 'cases', c, 'steps.json');
  if (fs.existsSync(steps)) {
    for (const s of JSON.parse(fs.readFileSync(steps, 'utf8'))) {
      if (s.wait) await new Promise((r) => setTimeout(r, s.wait));
      else if (s.burst) await Promise.all(s.burst.map((b) => cdp.send(b.method, b.params)));
      else if (s.eval) await p.evaluate(s.eval);
      else if (s.pw) await p.mouse[s.pw](...(s.args || []));
      else if (s.key) await p.keyboard[s.key](...(s.args || []));
      else await cdp.send(s.method, s.params);
    }
  }
  await p.waitForFunction(() => typeof L != 'undefined' && L[L.length - 1] === 'end', null, { timeout: 40000 }).catch(() => {});
  out[c] = await p.evaluate(() => (typeof L == 'undefined' ? '(no log on ' + location.href + ')' : L.join('\n')));
  out[c + '.console'] = browserLog.join('\n');
  await ctx.close();
  if (dir) fs.rmSync(dir, { recursive: true, force: true });
}
await b.close();
console.log(JSON.stringify(out, null, 1));

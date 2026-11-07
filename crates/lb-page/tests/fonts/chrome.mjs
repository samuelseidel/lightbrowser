// Records creep.html and cases.html in local Chrome on loopback and prints
// {browser, creep, cases} as JSON. Record each file on the OS its persona
// claims, with Chrome 153 headless shell:
// - chrome_macos26.json: macOS 26.4.1 (Chrome 156 gives the same except
//   that it also quotes generic family names in any case: "Serif");
// - chrome_ubuntu24.json: Ubuntu 24.04 with ubuntu-desktop-minimal (rtx-dev:
//   CHROME=$CHROME153 node crates/lb-page/tests/fonts/chrome.mjs 19142).
//   PLAYWRIGHT_CORE=/path/to/playwright-core/index.mjs CHROME=/path/to/chrome-headless-shell \
//     node crates/lb-page/tests/fonts/chrome.mjs 19142 > crates/lb-page/tests/fonts/chrome_macos26.json
import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const port = Number(process.argv[2] || 19142);
const srv = createServer((req, res) => {
  const name = req.url.slice(1).split('?')[0];
  if (!['creep.html', 'cases.html'].includes(name)) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
  res.end(readFileSync(join(here, name)));
}).listen(port, '127.0.0.1');
const { chromium } = await import(process.env.PLAYWRIGHT_CORE || 'playwright-core');
const exe = process.env.CHROME;
const shell = !exe || exe.includes('headless-shell');
const b = await chromium.launch({
  executablePath: exe,
  ignoreDefaultArgs: shell ? [] : ['--headless'],
  args: ['--disable-background-networking', '--no-proxy-server', ...(shell ? [] : ['--headless=new'])],
});
const out = { browser: b.version() };
for (const page of ['creep', 'cases']) {
  const p = await (await b.newContext()).newPage();
  await p.goto(`http://127.0.0.1:${port}/${page}.html`);
  await p.waitForFunction(() => window.R !== null, null, { timeout: 60000 });
  const r = await p.evaluate(() => window.R);
  out[page] = page === 'creep' ? JSON.parse(r) : r.split('\n');
}
await b.close();
srv.close();
console.log(JSON.stringify(out, null, 1));

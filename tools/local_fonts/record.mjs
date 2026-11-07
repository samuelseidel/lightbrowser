// Records which face names local Chrome resolves with FontFace local() on
// this machine (loopback only). Candidates: every family, full and
// PostScript name fontconfig lists for the installed fonts, plus the
// names CoreText lists (`ctnames.swift`: variable-font instances).
//
//   fc-list --format '%{file}\t%{family}\t%{fullname}\t%{postscriptname}\t%{style}\n' > /tmp/fc.tsv
//   swiftc -O -o /tmp/ctnames tools/local_fonts/ctnames.swift && /tmp/ctnames > /tmp/ct.txt
//   PLAYWRIGHT_CORE=/path/to/node_modules/playwright-core/index.mjs \
//     node tools/local_fonts/record.mjs --chrome "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
//       --fc-list /tmp/fc.tsv --names /tmp/ct.txt --port 19141 > /tmp/loaded.json
//   python3 tools/local_fonts/build.py --mac26 /tmp/loaded.json --fc-list /tmp/fc.tsv --apple DIR --ms DIR
import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const arg = (k, d) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : d; };
const chrome = arg('--chrome');
const port = Number(arg('--port', '19141'));
const names = new Set();
for (const line of readFileSync(arg('--fc-list'), 'utf8').split('\n')) {
  const c = line.split('\t');
  if (c.length < 4) continue;
  for (const n of [...c[1].split(','), ...c[2].split(','), c[3]]) if (n) names.add(n);
}
const extra = arg('--names');
if (extra) for (const n of readFileSync(extra, 'utf8').split('\n')) if (n) names.add(n);
const here = dirname(fileURLToPath(import.meta.url));
const page = readFileSync(join(here, 'probe.html'));
const list = JSON.stringify({ names: [...names].sort() });
const srv = createServer((req, res) => {
  const body = req.url.startsWith('/names.json') ? list : page;
  res.writeHead(200, { 'content-type': req.url.startsWith('/names.json') ? 'application/json' : 'text/html' });
  res.end(body);
}).listen(port, '127.0.0.1');
const { chromium } = await import(process.env.PLAYWRIGHT_CORE || 'playwright-core');
const headless = chrome.includes('headless-shell') ? [] : ['--headless=new'];
const b = await chromium.launch({
  executablePath: chrome,
  ignoreDefaultArgs: headless.length ? ['--headless'] : [],
  args: ['--disable-background-networking', '--no-proxy-server', ...headless],
});
console.error(b.version(), names.size, 'candidates');
const p = await (await b.newContext()).newPage();
await p.goto(`http://127.0.0.1:${port}/probe.html?list=/names.json`);
await p.waitForFunction(() => window.R !== null, null, { timeout: 300000 });
console.log(JSON.stringify(await p.evaluate(() => window.R), null, 1));
await b.close();
srv.close();

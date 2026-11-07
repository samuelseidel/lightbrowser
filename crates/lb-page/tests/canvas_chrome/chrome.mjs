// Records each case's log (the page's `L` array, up to 'end') in local
// Chrome for Testing 153, headless, on loopback. Serve this directory first:
//   python3 -m http.server 19130 --bind 127.0.0.1 &
//   CHROME=<chrome-headless-shell> node chrome.mjs http://127.0.0.1:19130 \
//     probe creep subpixel imagebitmap imagebitmap_errors > chrome_linux.json
// chrome_linux.json: Ubuntu 24.04 x86_64; chrome_mac.json: macOS 26 arm64.
// PLAYWRIGHT_MJS: playwright(-core) entry; CHROME: the browser binary.
const { chromium } = await import(process.env.PLAYWRIGHT_MJS || 'playwright-core');
const [base, ...cases] = process.argv.slice(2);
const b = await chromium.launch({
  executablePath: process.env.CHROME || `${process.env.HOME}/Library/Caches/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-mac-arm64/chrome-headless-shell`,
  args: ['--disable-background-networking', '--no-proxy-server'],
});
console.error(b.version());
const out = {};
for (const c of cases) {
  const ctx = await b.newContext();
  const p = await ctx.newPage();
  await p.goto(`${base}/${c}.html`, { waitUntil: 'load' });
  await p.waitForFunction(() => typeof L !== 'undefined' && L[L.length - 1] === 'end', null, { timeout: 20000 }).catch(() => {});
  out[c] = await p.evaluate(() => (typeof L === 'undefined' ? [] : L).join('\n'));
  await ctx.close();
}
await b.close();
console.log(JSON.stringify(out, null, 1));

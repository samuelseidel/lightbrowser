// Drive a local Chromium over raw CDP through the form-POST scenarios.
//
//   node drive.js H1_PORT H2_PORT [CHROMIUM]
//
// Raw CDP on purpose: Playwright's contexts set an Accept-Language override,
// which moves that header to the end of every request. Clicks are
// Input.dispatchMouseEvent (user activation, so Sec-Fetch-User: ?1);
// `auto=1` pages submit from a timer (no activation). Loopback only.
'use strict';
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const [h1Port, h2Port, exe = '/opt/pw-browsers/chromium'] = process.argv.slice(2);
const H1 = `http://127.0.0.1:${h1Port}`;
const H2 = `https://fptest.localhost:${h2Port}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'hdrcap-'));
const chrome = spawn(exe, [
  '--headless=new', '--no-sandbox', '--ignore-certificate-errors', '--no-proxy-server',
  `--host-resolver-rules=MAP fptest.localhost 127.0.0.1`,
  '--disable-background-networking', '--disable-component-update', '--disable-sync',
  '--no-first-run', `--user-data-dir=${dir}`, '--remote-debugging-port=9337', 'about:blank',
], { stdio: 'ignore' });

async function main() {
  let list;
  for (let i = 0; i < 50 && !list; i++) {
    try { list = await (await fetch('http://127.0.0.1:9337/json/list')).json(); } catch { await sleep(200); }
  }
  const ws = new WebSocket(list.find((t) => t.type === 'page').webSocketDebuggerUrl);
  await new Promise((r) => { ws.onopen = r; });
  let id = 0;
  const pending = new Map();
  ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id) pending.get(d.id)(d.result); };
  const send = (method, params = {}) => new Promise((r) => {
    pending.set(++id, r);
    ws.send(JSON.stringify({ id, method, params }));
  });
  await send('Page.enable');

  // Load `url`, then click #go (in the page, or in iframe #fr).
  const run = async (url, click, inFrame) => {
    await send('Page.navigate', { url });
    await sleep(900);
    if (click) {
      const expr = inFrame
        ? 'const f=document.getElementById("fr"),a=f.getBoundingClientRect(),b=f.contentDocument.getElementById("go").getBoundingClientRect();JSON.stringify({x:a.x+b.x+b.width/2,y:a.y+b.y+b.height/2})'
        : 'const b=document.getElementById("go").getBoundingClientRect();JSON.stringify({x:b.x+b.width/2,y:b.y+b.height/2})';
      const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true });
      const { x, y } = JSON.parse(r.result.value);
      for (const type of ['mousePressed', 'mouseReleased']) {
        await send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
      }
    }
    await sleep(900);
  };

  for (const base of [H1, H2]) {
    await run(`${base}/form?s=urlencoded-click`, true);
    await run(`${base}/form?s=urlencoded-script&auto=1`, false);
    await run(`${base}/form?s=multipart-click&enc=multipart/form-data`, true);
    await run(`${base}/form?s=text-plain-click&enc=text/plain`, true);
    await run(`${base}/form?s=no-redirect-click&action=/submit200`, true);
    await run(`${base}/form?s=307-click&action=/submit307`, true);
    await run(`${base}/outer?s=iframe-click`, true, true);
    await run(`${base}/outer?s=iframe-script&q=auto=1`, false);
  }
  await run(`${H1}/form?s=cross-site-click&action=http://localhost:${h1Port}/submit200`, true);
  ws.close();
}

main()
  .catch((e) => { console.error(e); process.exitCode = 1; })
  .finally(() => {
    chrome.kill();
    setTimeout(() => fs.rmSync(dir, { recursive: true, force: true }), 500);
  });

// Record when an idle service worker version is stopped and how it comes
// back (ADR 0029), over raw CDP, with and without a DevTools session on
// the worker. Loopback only.
//
//   node tests/playwright/workers/record_idle.mjs chrome            # local Chrome for Testing
//   node tests/playwright/workers/record_idle.mjs lightbrowser [BIN]
//
// The page posts `inc` to its controller; the worker answers with a
// global counter, so a restarted worker (fresh globals) answers 1 again.
// Each step prints the elapsed seconds, the answer and the worker's
// running status from `ServiceWorker.workerVersionUpdated` (Chrome) or
// from the target events.

import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const [mode, bin] = process.argv.slice(2);

const sw = `
self.n = 0;
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));
self.addEventListener('message', (e) => { self.n++; e.source.postMessage(self.n); });
self.addEventListener('fetch', (e) => {
  if (new URL(e.request.url).pathname === '/idle/ping') e.respondWith(new Response(String(++self.n)));
});
`;
const page = `<!DOCTYPE html><title>idle</title><script>
navigator.serviceWorker.register('/idle/sw.js');
window.ask = () => new Promise((r) => {
  navigator.serviceWorker.onmessage = (e) => r(e.data);
  navigator.serviceWorker.controller.postMessage('inc');
});
window.ping = () => fetch('/idle/ping').then((r) => r.text());
</script>`;
let scriptFetches = 0;
const site = await new Promise((resolve) => {
  const s = http.createServer((req, res) => {
    if (req.url === '/idle/sw.js') {
      scriptFetches++;
      res.writeHead(200, { 'content-type': 'text/javascript', 'cache-control': 'no-store' });
      return res.end(sw);
    }
    res.writeHead(200, { 'content-type': 'text/html', 'cache-control': 'no-store' });
    res.end(page);
  });
  s.listen(0, '127.0.0.1', () => resolve(s));
});
const base = `http://127.0.0.1:${site.address().port}`;

let dir;
let proc;
if (mode === 'chrome') {
  dir = mkdtempSync(path.join(os.tmpdir(), 'lb-idle-'));
  proc = spawn(process.env.CHROME || '/opt/pw-browsers/chromium', ['--headless=new', '--no-sandbox',
    '--remote-debugging-port=0', `--user-data-dir=${dir}`, '--no-proxy-server',
    '--disable-background-networking', '--disable-component-extensions-with-background-pages', '--no-first-run', 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe'] });
} else {
  proc = spawn(bin || path.join(repo, 'target/debug/lightbrowser'), ['serve', '--port', '0'], { cwd: repo, stdio: ['ignore', 'ignore', 'pipe'] });
}
const wsUrl = await new Promise((resolve, reject) => {
  let log = '';
  const t = setTimeout(() => reject(new Error(log)), 20000);
  proc.stderr.on('data', (c) => {
    log += c;
    const m = log.match(/DevTools listening on (ws:\/\/\S+)/);
    if (m) {
      clearTimeout(t);
      resolve(m[1]);
    }
  });
});

const ws = new WebSocket(wsUrl);
await new Promise((r) => ws.addEventListener('open', r));
let nextId = 1;
const waiting = new Map();
const t0 = Date.now();
const log = [];
const note = (s) => {
  const line = `${((Date.now() - t0) / 1000).toFixed(1)}s ${s}`;
  log.push(line);
  console.error(line);
};
const handlers = [];
ws.addEventListener('message', (e) => {
  const m = JSON.parse(e.data);
  if (m.id && waiting.has(m.id)) {
    waiting.get(m.id)(m);
    waiting.delete(m.id);
    return;
  }
  for (const h of handlers) h(m);
});
const send = (method, params = {}, sessionId) =>
  new Promise((resolve, reject) => {
    const id = nextId++;
    waiting.set(id, (m) => (m.error ? reject(new Error(`${method}: ${m.error.message}`)) : resolve(m.result)));
    ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
  });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Target events on the browser session (worker targets come and go).
handlers.push((m) => {
  if (m.sessionId) {
    if (m.method?.startsWith('Inspector.') || m.method === 'Runtime.executionContextCreated' || m.method === 'Runtime.executionContextDestroyed')
      note(`worker session: ${m.method}`);
    if (m.method === 'ServiceWorker.workerVersionUpdated')
      for (const v of m.params.versions) note(`version ${v.versionId} ${v.runningStatus} ${v.status}`);
    return;
  }
  if (m.method?.startsWith('Target.')) {
    const ti = m.params.targetInfo;
    if (ti && ti.type !== 'service_worker' && m.method !== 'Target.attachedToTarget') return;
    if (m.method === 'Target.attachedToTarget' && !ti) return;
    if (m.method === 'Target.attachedToTarget' && ti.type !== 'service_worker') {
      send('Runtime.runIfWaitingForDebugger', {}, m.params.sessionId).catch(() => {});
      return;
    }
    if (m.method === 'Target.attachedToTarget') {
      note(`attachedToTarget service_worker waiting=${m.params.waitingForDebugger}`);
      // Like Playwright: enable Runtime and let it run.
      send('Runtime.enable', {}, m.params.sessionId).catch(() => {});
      send('Runtime.runIfWaitingForDebugger', {}, m.params.sessionId).catch(() => {});
    } else if (ti) note(`${m.method} ${ti.type} attached=${ti.attached}`);
    else if (m.method !== 'Target.targetInfoChanged') note(`${m.method}`);
  }
});

async function run(label, attach) {
  note(`--- ${label} ---`);
  const { browserContextId } = await send('Target.createBrowserContext', { disposeOnDetach: true });
  await send('Target.setDiscoverTargets', { discover: true });
  if (attach) await send('Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
  const { targetId } = await send('Target.createTarget', { url: 'about:blank', browserContextId });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
  await send('ServiceWorker.enable', {}, sessionId).catch((e) => note(`ServiceWorker.enable: ${e.message}`));
  await send('Page.enable', {}, sessionId);
  await send('Page.navigate', { url: `${base}/idle/` }, sessionId);
  const ev = (expr) => send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }, sessionId).then((r) => r.result.value);
  for (let i = 0; i < 50 && !(await ev('!!navigator.serviceWorker.controller')); i++) await sleep(100);
  const start = Date.now();
  const at = (s) => sleep(Math.max(0, start + s * 1000 - Date.now()));
  note(`ask ${await ev('ask()')}`);
  await at(5);
  note(`ask ${await ev('ask()')}`);
  await at(28);
  note(`ask ${await ev('ask()')} (23 s after the last event)`);
  await at(70);
  note(`ask ${await ev('ask()')} (42 s idle)`);
  note(`ping ${await ev('ping()')}`);
  await at(115);
  note(`ping ${await ev('ping()')} (45 s idle, through fetch)`);
  note(`script fetches so far: ${scriptFetches}`);
  if (attach) await send('Target.setAutoAttach', { autoAttach: false, waitForDebuggerOnStart: false, flatten: true });
  await send('Target.disposeBrowserContext', { browserContextId });
}

try {
  await run('no DevTools session on the worker', false);
  await run('auto-attached to the worker (as Playwright)', true);
} finally {
  ws.close();
  proc.kill();
  site.close();
  if (dir) setTimeout(() => rmSync(dir, { recursive: true, force: true }), 500);
}
console.log(JSON.stringify({ mode, log }, null, 1));

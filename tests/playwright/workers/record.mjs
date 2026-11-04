// Record the CDP traffic between stock Playwright and a browser while
// `scenario.mjs` runs, for the worker targets (ADR 0029). Loopback only.
//
//   node tests/playwright/workers/record.mjs chrome > tests/playwright/workers/chrome.json
//   node tests/playwright/workers/record.mjs lightbrowser [BIN] > /tmp/lb.json
//   node tests/playwright/workers/record.mjs compare chrome.json lb.json
//
// `chrome` starts local Chrome for Testing (CHROME, default
// /opt/pw-browsers/chromium) headless with a fresh profile;
// `lightbrowser` starts `lightbrowser serve` (default target/debug). The
// output holds what Playwright saw (`results`) and the traffic of every
// worker-type session plus the target events, in order, without ids
// (`trace`).

import { spawn, execSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { workerPages, runScenario } from './scenario.mjs';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const [mode, arg1, arg2] = process.argv.slice(2);

if (mode === 'compare') {
  const a = JSON.parse(readFileSync(arg1, 'utf8'));
  const b = JSON.parse(readFileSync(arg2, 'utf8'));
  const show = (x) => JSON.stringify(x);
  let same = true;
  for (const k of new Set([...Object.keys(a.results), ...Object.keys(b.results)])) {
    if (show(a.results[k]) !== show(b.results[k])) {
      same = false;
      console.log(`result ${k}:\n  ${arg1}: ${show(a.results[k])}\n  ${arg2}: ${show(b.results[k])}`);
    }
  }
  const n = Math.max(a.trace.length, b.trace.length);
  for (let i = 0; i < n; i++) {
    const x = show(a.trace[i]);
    const y = show(b.trace[i]);
    console.log(`${x === y ? ' ' : '!'} ${String(i).padStart(3)} ${x ?? '-'}${x === y ? '' : `\n        ${y ?? '-'}`}`);
    if (x !== y) same = false;
  }
  console.log(same ? 'same' : 'different');
  process.exit(0);
}

// Capture Playwright's protocol log (it goes to stderr).
process.env.DEBUG = 'pw:protocol';
const lines = [];
const write = process.stderr.write.bind(process.stderr);
process.stderr.write = (chunk, ...rest) => {
  const s = String(chunk);
  if (s.includes('pw:protocol')) {
    lines.push(s);
    return true;
  }
  return write(chunk, ...rest);
};
const require = createRequire(import.meta.url);
const globalRoot = execSync('npm root -g').toString().trim();
const { chromium } = require(path.join(globalRoot, 'playwright'));

function serveSite() {
  const server = http.createServer((req, res) => {
    const { pathname } = new URL(req.url, 'http://x');
    const p = workerPages[pathname];
    if (typeof p === 'function') return p(req, res);
    if (p === undefined) {
      res.writeHead(404, { 'content-type': 'text/plain' });
      return res.end('not found');
    }
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' });
    res.end(p);
  });
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server)));
}

function startBrowser() {
  let cmd;
  let args;
  let dir;
  if (mode === 'chrome') {
    dir = mkdtempSync(path.join(os.tmpdir(), 'lb-rec-'));
    cmd = process.env.CHROME || '/opt/pw-browsers/chromium';
    args = ['--headless=new', '--no-sandbox', '--remote-debugging-port=0', `--user-data-dir=${dir}`,
      '--no-proxy-server', '--disable-background-networking', '--disable-component-extensions-with-background-pages', '--no-first-run', 'about:blank'];
  } else {
    cmd = arg1 || path.join(repo, 'target/debug/lightbrowser');
    args = ['serve', '--port', '0'];
  }
  const proc = spawn(cmd, args, { cwd: repo, stdio: ['ignore', 'ignore', 'pipe'] });
  const endpoint = new Promise((resolve, reject) => {
    let log = '';
    const timer = setTimeout(() => reject(new Error(`no DevTools line:\n${log}`)), 20000);
    proc.stderr.on('data', (c) => {
      log += c;
      const m = log.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
  });
  return { proc, endpoint, dir };
}

/** Turn the protocol log into an id-free trace of worker traffic. */
function normalize() {
  const msgs = [];
  for (const chunk of lines) {
    for (const l of chunk.split('\n')) {
      const m = l.match(/(SEND ►|◀ RECV) (\{.*\})\s*$/);
      if (!m) continue;
      try {
        msgs.push({ dir: m[1].startsWith('SEND') ? 'send' : 'recv', msg: JSON.parse(m[2]) });
      } catch {}
    }
  }
  const kind = new Map(); // sessionId -> target type
  const pending = new Map(); // `${session}:${id}` -> method
  const trace = [];
  for (const { dir, msg } of msgs) {
    const s = msg.sessionId ?? '';
    if (msg.method === 'Target.attachedToTarget') {
      const t = msg.params.targetInfo.type;
      kind.set(msg.params.sessionId, t);
      if (t !== 'page' && t !== 'browser')
        trace.push({ on: kind.get(s) ?? 'browser', ev: 'attachedToTarget', type: t, waiting: msg.params.waitingForDebugger, keys: Object.keys(msg.params.targetInfo).sort().join(',') });
      continue;
    }
    if (msg.method === 'Target.detachedFromTarget') {
      const t = kind.get(msg.params.sessionId);
      if (t && t !== 'page') trace.push({ on: kind.get(s) ?? 'browser', ev: 'detachedFromTarget', type: t });
      continue;
    }
    const k = kind.get(s);
    // Page sessions: only what a worker could also cause there.
    if (k === 'page' && dir === 'recv' && (msg.method === 'Runtime.exceptionThrown' || msg.method === 'Runtime.consoleAPICalled' || msg.method === 'Log.entryAdded')) {
      trace.push({ on: k, ev: msg.method, text: msg.params.exceptionDetails?.exception?.description?.split('\n')[0] ?? msg.params.type ?? msg.params.entry?.text });
      continue;
    }
    if (!k || k === 'page' || k === 'browser') continue;
    if (dir === 'send') {
      pending.set(`${s}:${msg.id}`, msg.method);
      trace.push({ on: k, send: msg.method });
    } else if (msg.id !== undefined) {
      const method = pending.get(`${s}:${msg.id}`);
      trace.push({ on: k, reply: method, ...(msg.error ? { error: msg.error.message } : {}) });
    } else {
      const e = { on: k, ev: msg.method };
      if (msg.method === 'Runtime.executionContextCreated') e.ctxKeys = Object.keys(msg.params.context).sort().join(',');
      if (msg.method === 'Runtime.consoleAPICalled') e.type = msg.params.type;
      if (msg.method === 'Network.requestWillBeSent') e.req = `${msg.params.type} ${new URL(msg.params.request.url).pathname}`;
      if (msg.method === 'Network.responseReceived') e.res = `${msg.params.type} ${new URL(msg.params.response.url).pathname} sw=${msg.params.response.fromServiceWorker}`;
      trace.push(e);
    }
  }
  return trace;
}

const site = await serveSite();
const base = `http://127.0.0.1:${site.address().port}`;
const b = startBrowser();
let out;
try {
  const ws = await b.endpoint;
  const browser = await chromium.connectOverCDP(ws.replace(/^ws:\/\/([^/]+)\/.*$/, 'http://$1'));
  const results = await runScenario(browser, base);
  await browser.close().catch(() => {});
  out = { browser: browser.version(), results, trace: normalize() };
} finally {
  b.proc.kill();
  site.close();
  if (b.dir) setTimeout(() => rmSync(b.dir, { recursive: true, force: true }), 500);
}
console.log(JSON.stringify(out, null, 1));

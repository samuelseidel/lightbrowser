#!/usr/bin/env node
// Accept-CH / Critical-CH header capture from the local Chromium, loopback only.
//
//   node tools/header_capture/accept_ch.js --out profiles/headers/captures/chrome_153_linux_accept_ch.jsonl
//   (--mode headed needs $DISPLAY: run under xvfb-run -a)
//
// Starts four servers on 127.0.0.1 (random ports): A (HTTP/1.1), B (HTTP/1.1,
// cross-origin, same site), C (HTTP/1.1, fresh origin) and H (HTTPS h2 with
// a throwaway self-signed certificate made by openssl in a temp dir). Every
// request is logged with its raw header names, case and order. Chromium
// (throwaway profile, no CDP, no automation flags; the throwaway key is
// trusted with --ignore-certificate-errors-spki-list) is pointed only at
// these servers.
//
// Run 1 (one launch, typed first navigation to A/start):
//   A/start      Accept-CH: 7 UA hints + Critical-CH: Sec-CH-UA-Platform-Version
//                -> Chrome restarts the navigation once with the hints.
//                Page: stylesheet (font, background image), classic and module
//                script, image, same-origin and cross-origin iframes, fetch/XHR
//                with the header sets of the base capture, beacon, favicon.
//   A/next       renderer-initiated navigation (hints, no Sec-Fetch-User).
//   A/redir      302 -> A/final (redirect hop with hints).
//   C/c-redir    302 carrying Accept-CH: Sec-CH-UA-Model, Sec-CH-UA-Arch,
//                Sec-CH-UA-Full-Version (deprecated, still honoured)
//                -> C/c-final (does a redirect's Accept-CH count?).
//   C/c-next     subset order (navigation and subresources).
//   H/start ...  the A flow again over h2.
// Run 2 (same profile, second launch, typed navigation to A/typed): stored
//   hints on a browser-initiated navigation.
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const http2 = require('http2');
const { spawn, execFileSync } = require('child_process');

const HINTS = ['Sec-CH-UA-Platform-Version', 'Sec-CH-UA-Arch', 'Sec-CH-UA-Bitness', 'Sec-CH-UA-Full-Version-List',
  'Sec-CH-UA-Model', 'Sec-CH-UA-WoW64', 'Sec-CH-UA-Form-Factors'];
const SUBSET = ['Sec-CH-UA-Model', 'Sec-CH-UA-Arch', 'Sec-CH-UA-Full-Version'];
// Page-set header combinations of the base capture (renderer_group.observed).
const COMBOS = [
  [], [['X-A', '1']], [['X-B', '1'], ['X-C', '1']], [['Accept', 'application/json']],
  [['Pragma', 'no-cache'], ['Cache-Control', 'no-cache']], [['X-Custom-Test', '1'], ['x-lower-test', '1']],
  [['X-D', '1'], ['X-E', '1'], ['X-F', '1'], ['X-G', '1'], ['X-H', '1']],
];
const PNG = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==', 'base64');

function parseArgs(argv) {
  const a = { out: null, mode: 'headless', chromium: '/opt/pw-browsers/chromium', timeout: 60000 };
  for (let i = 0; i < argv.length; i++) {
    const k = argv[i];
    const next = () => { if (i + 1 >= argv.length) throw new Error(`${k} needs a value`); return argv[++i]; };
    if (k === '--out') a.out = next();
    else if (k === '--mode') a.mode = next();
    else if (k === '--chromium') a.chromium = next();
    else if (k === '--timeout') a.timeout = Number(next());
    else if (k === '--cert-errors') a.certErrors = true;
    else throw new Error(`unknown argument ${k}`);
  }
  if (!a.out) throw new Error('--out is required');
  if (!['headed', 'headless'].includes(a.mode)) throw new Error('--mode must be headed or headless');
  if (a.mode === 'headed' && !process.env.DISPLAY) throw new Error('headed mode needs $DISPLAY (xvfb-run)');
  return a;
}

function pageHtml(o, self, step) {
  // `o` = origin URLs, `self` = this server's origin, `step` = which page.
  const nav = (to) => `<script>addEventListener('load',()=>setTimeout(()=>{location.href=${JSON.stringify(to)}},400))</script>`;
  if (step === 'start') {
    const fetches = COMBOS.map((c, i) => `await fetch('/api?combo${i}',{headers:${JSON.stringify(c)}});`).join('\n');
    const next = self === o.H ? `${o.H}/next` : `${o.A}/next`;
    return `<!doctype html><html><head><meta charset=utf-8><title>start</title>
<link rel=stylesheet href=/s.css><script src=/s.js></script><script type=module src=/m.js></script></head>
<body><img src=/i.png><iframe src=/frame.html></iframe><iframe src="${o.B}/frame.html"></iframe>
<script>
addEventListener('load', async () => {
  await fetch('/api?fetch-same');
  await fetch('/api?fetch-post',{method:'POST',headers:{'Content-Type':'text/plain'},body:'x'});
  await fetch('/api?fetch-nocors',{mode:'no-cors'});
  await new Promise(r=>{const x=new XMLHttpRequest();x.open('GET','/api?xhr-same');x.onloadend=r;x.send();});
  await new Promise(r=>{const x=new XMLHttpRequest();x.open('POST','/api?xhr-post');x.setRequestHeader('Content-Type','application/json');x.onloadend=r;x.send('{}');});
  ${fetches}
  await fetch('/api?fetch-auth',{method:'POST',headers:{'Authorization':'Basic eDp5','Content-Type':'text/plain'},body:'x'});
  await fetch('/api?fetch-pre',{method:'POST',headers:{'X-Pre':'1','Content-Type':'application/json'},body:'{}'});
  try { await fetch('${o.B}/api?fetch-cors'); } catch (e) {}
  navigator.sendBeacon('/api?beacon','x');
  setTimeout(()=>{location.href=${JSON.stringify(next)}},800);
});
</script></body></html>`;
  }
  if (step === 'frame') return '<!doctype html><title>f</title><link rel=stylesheet href=/f.css><img src=/fi.png><script>fetch("/api?frame-fetch")</script>';
  if (step === 'next') return `<!doctype html><title>next</title><img src=/n.png>${nav(self === o.H ? `${o.H}/redir` : `${o.A}/redir`)}`;
  if (step === 'final') return `<!doctype html><title>final</title>${nav(self === o.H ? `${o.H}/done` : `${o.C}/c-redir`)}`;
  if (step === 'c-final') return `<!doctype html><title>c-final</title>${nav(`${o.C}/c-next`)}`;
  if (step === 'c-next') {
    return `<!doctype html><title>c-next</title><link rel=stylesheet href=/s.css><img src=/i.png>
<script>addEventListener('load',async()=>{await fetch('/api?c-fetch');await fetch('/api?c-x-a',{headers:{'X-A':'1'}});
setTimeout(()=>{location.href=${JSON.stringify(`${o.H}/start`)}},400)})</script>`;
  }
  if (step === 'typed') return '<!doctype html><title>typed</title><img src=/t.png>';
  return '<!doctype html><title>x</title>';
}

function selfSigned(dir) {
  const key = path.join(dir, 'k.pem'), cert = path.join(dir, 'c.pem');
  execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', key, '-out', cert, '-days', '1',
    '-subj', '/CN=127.0.0.1', '-addext', 'subjectAltName=IP:127.0.0.1'], { stdio: 'ignore' });
  // SPKI hash for --ignore-certificate-errors-spki-list: Chromium then
  // trusts this key without a certificate error (with
  // --ignore-certificate-errors it ignores Accept-CH on the connection).
  const der = execFileSync('openssl', ['pkey', '-in', key, '-pubout', '-outform', 'der']);
  const spki = require('crypto').createHash('sha256').update(der).digest('base64');
  return { key: fs.readFileSync(key), cert: fs.readFileSync(cert), spki };
}

async function main() {
  const a = parseArgs(process.argv.slice(2));
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'achcap-'));
  const log = [];
  let run = 1;
  const waiters = [];
  const flags = { done: false, done2: false };
  const notify = () => { while (waiters.length) waiters.shift()(); };
  const until = (k, ms) => new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`timed out waiting for ${k}`)), ms);
    const check = () => { if (flags[k]) { clearTimeout(t); resolve(); } else waiters.push(check); };
    check();
  });
  const o = {};

  const handler = (label, proto) => (req, res) => {
    const body = [];
    req.on('data', (c) => body.push(c));
    req.on('end', () => {
      const raw = req.rawHeaders;
      const headers = [];
      for (let i = 0; i < raw.length; i += 2) headers.push([raw[i], raw[i + 1]]);
      const url = new URL(req.url, 'http://x');
      const p = url.pathname;
      if (process.env.ACH_DEBUG) console.error(label, req.method, req.url);
      log.push({ run, origin: label, proto, scheme: proto === 'h2' ? 'https' : 'http',
        request: proto === 'h2' ? `${req.method} ${req.url}` : `${req.method} ${req.url} HTTP/1.1`, headers });
      const self = o[label];
      const h = { 'Cache-Control': 'no-store' };
      const send = (code, type, data, extra = {}) => {
        res.writeHead(code, { ...h, 'Content-Type': type, ...extra });
        res.end(data);
      };
      const cors = { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Headers': '*' };
      const ach = { 'Accept-CH': HINTS.join(', ') };
      if (p === '/start') {
        return send(200, 'text/html; charset=utf-8', pageHtml(o, self, 'start'),
          { ...ach, 'Critical-CH': 'Sec-CH-UA-Platform-Version', 'Set-Cookie': 'sid=abc; Path=/; SameSite=Lax' });
      }
      if (p === '/next') return send(200, 'text/html; charset=utf-8', pageHtml(o, self, 'next'), ach);
      if (p === '/redir') return send(302, 'text/plain', '', { Location: '/final' });
      if (p === '/final') return send(200, 'text/html; charset=utf-8', pageHtml(o, self, 'final'), ach);
      if (p === '/c-redir') return send(302, 'text/plain', '', { Location: '/c-final', 'Accept-CH': SUBSET.join(', ') });
      if (p === '/c-final') return send(200, 'text/html; charset=utf-8', pageHtml(o, self, 'c-final'), { 'Accept-CH': SUBSET.join(', ') });
      if (p === '/c-next') return send(200, 'text/html; charset=utf-8', pageHtml(o, self, 'c-next'), { 'Accept-CH': SUBSET.join(', ') });
      if (p === '/typed') flags.done2 = true, notify();
      if (p === '/typed') return send(200, 'text/html; charset=utf-8', pageHtml(o, self, 'typed'), ach);
      if (p === '/frame.html') return send(200, 'text/html; charset=utf-8', pageHtml(o, self, 'frame'), { ...ach, ...cors });
      if (p === '/done') { flags.done = true; notify(); return send(200, 'text/html', '<!doctype html><title>done</title>'); }
      if (p.endsWith('.css')) {
        return send(200, 'text/css', p === '/s.css'
          ? '@font-face{font-family:F;src:url(/f.woff2)}body{font-family:F;background:url(/bg.png)}' : 'body{color:red}');
      }
      if (p.endsWith('.js')) return send(200, 'text/javascript', '1;', cors);
      if (p.endsWith('.png') || p === '/favicon.ico') return send(200, 'image/png', PNG);
      if (p.endsWith('.woff2')) return send(200, 'font/woff2', Buffer.alloc(0), cors);
      if (p === '/api') return send(200, 'text/plain', 'ok', cors);
      return send(404, 'text/plain', 'not found');
    });
  };

  const listen = (srv) => new Promise((r) => srv.listen(0, '127.0.0.1', () => r(srv.address().port)));
  const servers = [];
  for (const k of ['A', 'B', 'C']) {
    const s = http.createServer(handler(k, 'h1'));
    servers.push(s);
    o[k] = `http://127.0.0.1:${await listen(s)}`;
  }
  const tls = selfSigned(tmp);
  const hs = http2.createSecureServer({ key: tls.key, cert: tls.cert, allowHTTP1: false }, handler('H', 'h2'));
  servers.push(hs);
  o.H = `https://127.0.0.1:${await listen(hs)}`;

  const profile = path.join(tmp, 'profile');
  const chromeFlags = ['--no-sandbox', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check',
    '--password-store=basic', '--disable-component-update', '--disable-background-networking', '--disable-sync',
    '--no-proxy-server',
    // --cert-errors: the old way (certificate errors ignored). Chromium then
    // ignores Accept-CH on H; kept to compare header orders.
    a.certErrors ? '--ignore-certificate-errors' : `--ignore-certificate-errors-spki-list=${tls.spki}`];
  if (a.mode === 'headless') chromeFlags.push('--headless=new');
  // Chromium's singleton socket lives in $TMPDIR and must have a short path.
  const env = { ...process.env };
  delete env.TMPDIR;
  const launch = (url) => spawn(a.chromium, [...chromeFlags, url], { env, stdio: ['ignore', 'ignore', process.env.ACH_DEBUG ? 'inherit' : 'ignore'], detached: true });
  // Stop a launch and wait until its process is gone: a second launch on the
  // same profile would otherwise hand its URL to the exiting instance.
  const kill = (c) => new Promise((resolve) => {
    if (c.exitCode !== null || c.signalCode !== null) return resolve();
    const hard = setTimeout(() => { try { process.kill(-c.pid, 'SIGKILL'); } catch (_) {} }, 8000);
    c.once('exit', () => { clearTimeout(hard); resolve(); });
    try { process.kill(-c.pid, 'SIGTERM'); } catch (_) { clearTimeout(hard); resolve(); }
  });
  const settle = (ms) => new Promise((r) => setTimeout(r, ms));

  let incomplete = null;
  let chrome = launch(`${o.A}/start`);
  try {
    await until('done', a.timeout);
    await settle(1500); // favicon of the last page
  } catch (e) { incomplete = e.message; } finally { await kill(chrome); }
  if (process.env.ACH_DEBUG) console.error('run 1 finished', incomplete);
  await settle(1500);
  if (!incomplete) {
    // Mark the profile as cleanly closed so the second launch does not
    // restore the last session instead of loading A/typed.
    const prefs = path.join(profile, 'Default', 'Preferences');
    try {
      const j = JSON.parse(fs.readFileSync(prefs, 'utf8'));
      j.profile = { ...(j.profile || {}), exit_type: 'Normal', exited_cleanly: true };
      fs.writeFileSync(prefs, JSON.stringify(j));
    } catch (_) { /* no Preferences yet: nothing to restore */ }
    fs.rmSync(path.join(profile, 'Default', 'Sessions'), { recursive: true, force: true });
    run = 2;
    chrome = launch(`${o.A}/typed`);
    try {
      await until('done2', a.timeout);
      await settle(3000); // its image and favicon
    } catch (e) { incomplete = e.message; } finally { await kill(chrome); }
    await settle(500);
  }
  for (const s of servers) { s.close(); if (s.closeAllConnections) s.closeAllConnections(); }

  const version = (() => { try { return execFileSync(a.chromium, ['--version'], { encoding: 'utf8' }).trim(); } catch (_) { return null; } })();
  const meta = { _meta: { tool: 'tools/header_capture/accept_ch.js', browser: version, mode: a.mode === 'headed' ? 'headed (xvfb-run)' : '--headless=new',
    flags: chromeFlags.filter((f) => !f.startsWith('--user-data-dir')).map((f) => f.replace(/spki-list=.*/, 'spki-list=<throwaway key>')), origins: Object.fromEntries(Object.entries(o).map(([k, v]) => [k, v.replace(/:\d+$/, ':PORT')])),
    accept_ch: HINTS, critical_ch: ['Sec-CH-UA-Platform-Version'], subset: SUBSET, loopback_only: true, incomplete } };
  const lines = [JSON.stringify(meta), ...log.map((e) => JSON.stringify(e))];
  fs.writeFileSync(a.out, lines.join('\n') + '\n');
  fs.rmSync(tmp, { recursive: true, force: true });
  console.log(`${log.length} requests -> ${a.out}`);
  if (incomplete) { console.error(`incomplete: ${incomplete}`); process.exitCode = 1; }
  setTimeout(() => process.exit(process.exitCode || 0), 200);
}

main().catch((e) => { console.error(e); process.exit(1); });

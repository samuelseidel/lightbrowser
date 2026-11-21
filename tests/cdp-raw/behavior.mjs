#!/usr/bin/env node
// Behaviour probes against Chrome (ADR 0045, round 2): the cases around
// session history, referrers, offline emulation, blocked URLs, script
// disabling and redirect events that replay.mjs does not cover. Every probe
// drives one fresh page over raw CDP and returns plain data (URLs, events,
// request headers the test servers saw); the data of Chrome 153 is recorded
// in behavior.chrome.json and LightBrowser's must equal it, except the
// entries of KNOWN (each with its reason).
//
//   node tests/cdp-raw/behavior.mjs record  --launch chrome        # writes behavior.chrome.json
//   node tests/cdp-raw/behavior.mjs compare --launch lightbrowser  # compares with it
//   node tests/cdp-raw/behavior.mjs compare --launch lightbrowser --only histcap,offline
//
// Options: --launch chrome|lightbrowser, --bin PATH, --port N (default
// 19262), --only NAME[,NAME], --out FILE (record). Loopback only: the
// browser and two test servers listen on 127.0.0.1.

import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');
const argv = process.argv.slice(2);
const mode = argv[0] && !argv[0].startsWith('--') ? argv.shift() : 'compare';
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  if (i < 0) return dflt;
  return argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : true;
};
const kind = opt('launch', 'lightbrowser');
const port = String(opt('port', 19262));
const recorded = path.join(here, 'behavior.chrome.json');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- site
const seen = {}; // path -> list of headers
const site = { servers: [] };
await new Promise((res) => {
  const mk = () => {
    const s = http.createServer((req, rs) => {
      const u = new URL(req.url, 'http://x');
      (seen[u.pathname] ||= []).push({ ...req.headers });
      const page = (body, head = '', extra = {}) => {
        rs.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store', ...extra });
        rs.end(`<!DOCTYPE html><html><head><meta charset="utf-8"><title>${u.pathname}</title>${head}</head><body>${body}</body></html>`);
      };
      const logger = `<script>window.__log=[];for(const e of ['popstate','hashchange','pageshow','pagehide','online','offline','unload'])addEventListener(e,ev=>__log.push(e+':'+(ev.oldURL?ev.oldURL.split('/').pop()+'>'+ev.newURL.split('/').pop():'')+(e==='popstate'?JSON.stringify(ev.state):'')))</script>`;
      switch (u.pathname) {
        case '/p1': case '/p2': case '/p3': case '/p4':
          return page(`<div id=x>${u.pathname}</div>`, logger);
        case '/hdr': return page('hdr');
        case '/link': return page('<a id=l href="/hdr?l">l</a><form id=fm method=post action="/hdr?post"><input name=a value=1></form>');
        case '/rp-origin': return page('rp', '', { 'referrer-policy': 'origin' });
        case '/noref': return page('x', '<meta name=referrer content="no-referrer">');
        case '/img': return page('<img id=i src="/pix.png">');
        case '/pix.png':
          rs.writeHead(200, { 'content-type': 'image/png' });
          return rs.end(Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==', 'base64'));
        case '/r1': rs.writeHead(301, { location: '/hdr?r=1' }); return rs.end();
        case '/rdead': rs.writeHead(302, { location: `http://127.0.0.1:${site.dead}/x` }); return rs.end();
        case '/r307': rs.writeHead(307, { location: '/hdr?r=307' }); return rs.end();
        case '/rx': rs.writeHead(302, { location: `http://127.0.0.1:${site.pb}/hdr?x=1` }); return rs.end();
        case '/js': rs.writeHead(200, { 'content-type': 'text/javascript' }); return rs.end('window.__ext=(window.__ext||0)+1');
        case '/json': rs.writeHead(200, { 'content-type': 'application/json', 'access-control-allow-origin': '*' }); return rs.end('{"a":1}');
        case '/script-page':
          return page('<div id=a onclick="window.__clicked=1">a</div><noscript><p id=ns>noscript</p></noscript><button id=b onclick="window.__inl=1">b</button>',
            '<script>window.__inline=1</script><script src="/js"></script><script type=module>window.__mod=1</script>');
        case '/iframe': return page('<iframe id=f src="/p1"></iframe>', logger);
        default: rs.writeHead(404); return rs.end('nf');
      }
    });
    site.servers.push(s);
    return new Promise((r) => s.listen(0, '127.0.0.1', () => r(s.address().port)));
  };
  Promise.all([mk(), mk()]).then(([a, b]) => { site.pa = a; site.pb = b; res(); });
});
const O = `http://127.0.0.1:${site.pa}`;
const O2 = `http://127.0.0.1:${site.pb}`;
// A loopback port nothing listens on (connection refused).
site.dead = await new Promise((r) => {
  const s = http.createServer();
  s.listen(0, '127.0.0.1', () => {
    const p = s.address().port;
    s.close(() => r(p));
  });
});

// ---- browser
const dir = mkdtempSync(path.join(os.tmpdir(), 'cdp-beh-'));
const bin =
  opt('bin', null) ||
  (kind === 'chrome'
    ? process.env.LB_CHROME || process.env.CHROME_BIN || path.join(os.homedir(), '.cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell')
    : process.env.LIGHTBROWSER_BIN || path.join(repo, 'target/debug/lightbrowser'));
const args =
  kind === 'chrome'
    ? ['--no-sandbox', `--remote-debugging-port=${port}`, `--user-data-dir=${dir}`, '--no-first-run', '--disable-background-networking', '--no-proxy-server', 'about:blank']
    : ['serve', '--port', port];
const proc = spawn(bin, args, { stdio: ['ignore', 'ignore', 'pipe'] });
const wsUrl = await new Promise((resolve, reject) => {
  let log = '';
  setTimeout(() => reject(new Error('no ws')), 20000);
  proc.stderr.on('data', (c) => { log += c; const m = log.match(/DevTools listening on (ws:\/\/\S+)/); if (m) resolve(m[1]); });
});

class Conn {
  constructor() { this.id = 1; this.p = new Map(); this.ev = []; }
  open() { return new Promise((res) => { this.ws = new WebSocket(wsUrl); this.ws.onopen = res; this.ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id) { const f = this.p.get(d.id); this.p.delete(d.id); f?.(d); } else this.ev.push(d); }; }); }
  send(method, params, sessionId, to = 8000) {
    const id = this.id++;
    return new Promise((res) => {
      const t = setTimeout(() => { this.p.delete(id); res({ ok: false, error: { code: 'timeout' } }); }, to);
      this.p.set(id, (d) => { clearTimeout(t); res(d.error ? { ok: false, error: d.error } : { ok: true, result: d.result }); });
      this.ws.send(JSON.stringify({ id, method, params, sessionId }));
    });
  }
}

async function newPage(enable = ['Page', 'Network', 'Runtime', 'DOM']) {
  const c = new Conn(); await c.open();
  const t = await c.send('Target.createTarget', { url: 'about:blank' });
  const a = await c.send('Target.attachToTarget', { targetId: t.result.targetId, flatten: true });
  const sid = a.result.sessionId;
  const P = {
    c, sid,
    send: (m, p, to) => c.send(m, p, sid, to),
    async ev(expr) { const r = await P.send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true }); return r.ok ? (r.result.exceptionDetails ? 'EXC:' + (r.result.exceptionDetails.exception?.description || r.result.exceptionDetails.text) : r.result.result.value) : 'ERR:' + JSON.stringify(r.error); },
    mark() { P.m = c.ev.length; },
    events(filter) { return c.ev.slice(P.m || 0).filter((e) => e.sessionId === sid && (!filter || filter.includes(e.method))); },
    async nav(url, params = {}) {
      P.mark();
      const r = await P.send('Page.navigate', { url, ...params });
      await sleep(350);
      return r;
    },
  };
  for (const d of enable) await P.send(d + '.enable');
  await P.send('Page.setLifecycleEventsEnabled', { enabled: true });
  return P;
}
const hdr = (p, n = -1) => { const l = seen[p] || []; const h = l.at(n); return h ? { referer: h.referer ?? null, sfs: h['sec-fetch-site'] ?? null, sfm: h['sec-fetch-mode'] ?? null, sfd: h['sec-fetch-dest'] ?? null, sfu: h['sec-fetch-user'] ?? null, n: l.length } : null; };
const norm = (v) => JSON.parse(JSON.stringify(v ?? null).replaceAll(String(site.pa), 'A').replaceAll(String(site.pb), 'B').replaceAll(String(site.dead), 'D'));
const lc = (P) => P.events(['Page.frameNavigated', 'Page.navigatedWithinDocument', 'Page.frameStartedNavigating', 'Page.frameStartedLoading', 'Page.frameStoppedLoading', 'Page.loadEventFired', 'Page.domContentEventFired']).map((e) => e.method.replace('Page.', '') + (e.params.navigationType ? ':' + e.params.navigationType : ''));
const errOf = (r) => (r.ok ? { ok: true, keys: Object.keys(r.result || {}).sort() } : { ok: false, code: r.error.code, msg: norm(r.error.message) });

const probes = {};

probes.histcap = async () => {
  const P = await newPage();
  await P.nav(O + '/p1');
  const r = await P.ev(`(()=>{for(let i=0;i<60;i++)history.pushState(null,'','/p1?i='+i);return history.length})()`);
  const h = await P.send('Page.getNavigationHistory');
  return { len: r, entries: h.result?.entries?.length, cur: h.result?.currentIndex };
};
probes.histcap_nav = async () => {
  const P = await newPage();
  for (let i = 0; i < 55; i++) { await P.send('Page.navigate', { url: O + '/p1?n=' + i }); await sleep(20); }
  await sleep(300);
  const h = await P.send('Page.getNavigationHistory');
  return norm({ len: await P.ev('history.length'), entries: h.result?.entries?.length, cur: h.result?.currentIndex, first: h.result?.entries?.[0]?.url });
};
probes.same_hash = async () => {
  const P = await newPage();
  await P.nav(O + '/p1');
  const out = [];
  await P.ev(`location.hash='a'`); out.push(await P.ev('history.length'));
  await P.ev(`location.hash='a'`); out.push(await P.ev('history.length'));
  await P.ev(`location.hash='b'`); out.push(await P.ev('history.length'));
  await P.ev(`location.href=location.href`); await sleep(300); out.push(await P.ev('history.length'));
  await P.ev(`history.pushState(null,'',location.href)`); out.push(await P.ev('history.length'));
  await P.nav(O + '/p1#b'); out.push(await P.ev('history.length'));
  await P.nav(O + '/p1#c'); out.push(await P.ev('history.length'));
  await P.nav(O + '/p1#c'); out.push(await P.ev('history.length'));
  return out;
};
probes.back_events = async () => {
  const P = await newPage();
  await P.nav(O + '/p1'); await P.nav(O + '/p2'); await P.nav(O + '/p3');
  P.mark();
  await P.ev('history.back()'); await sleep(500);
  const e1 = lc(P);
  const log1 = await P.ev('JSON.stringify(window.__log)');
  const url1 = await P.ev('location.href');
  P.mark();
  await P.ev('history.forward()'); await sleep(500);
  return norm({ e1, log1, url1, e2: lc(P), url2: await P.ev('location.href') });
};
probes.double_back = async () => {
  const P = await newPage();
  await P.nav(O + '/p1'); await P.nav(O + '/p2'); await P.nav(O + '/p3');
  await P.ev('history.back();history.back()'); await sleep(700);
  const a = await P.ev('location.href');
  await P.ev('history.back();history.forward()'); await sleep(700);
  return norm({ a, b: await P.ev('location.href') });
};
probes.go_odd = async () => {
  const P = await newPage();
  await P.nav(O + '/p1'); await P.nav(O + '/p2');
  const out = {};
  for (const e of ['history.go(-5)', 'history.go(100000)', "history.go('abc')", 'history.go(NaN)', 'history.go(1e10)', 'history.go(-1.9)', 'history.go()', 'history.go(null)', 'history.go(-1)']) {
    out[e] = [await P.ev(e + ';1')]; await sleep(350);
    out[e].push(await P.ev('location.href'));
  }
  return norm(out);
};
probes.nth_errors = async () => {
  const P = await newPage();
  await P.nav(O + '/p1'); await P.nav(O + '/p2');
  const h = (await P.send('Page.getNavigationHistory')).result;
  const out = {};
  out.missing = errOf(await P.send('Page.navigateToHistoryEntry', {}));
  out.str = errOf(await P.send('Page.navigateToHistoryEntry', { entryId: 'x' }));
  out.neg = errOf(await P.send('Page.navigateToHistoryEntry', { entryId: -3 }));
  out.big = errOf(await P.send('Page.navigateToHistoryEntry', { entryId: 99999 }));
  out.float = errOf(await P.send('Page.navigateToHistoryEntry', { entryId: 1.5 }));
  P.mark();
  out.cur = errOf(await P.send('Page.navigateToHistoryEntry', { entryId: h.entries[h.currentIndex].id }));
  await sleep(400);
  out.curEv = lc(P);
  out.curHist = (await P.send('Page.getNavigationHistory')).result.entries.length;
  out.reset = errOf(await P.send('Page.resetNavigationHistory', {}));
  out.resetLen = [(await P.send('Page.getNavigationHistory')).result.entries.length, await P.ev('history.length')];
  out.resetExtra = errOf(await P.send('Page.resetNavigationHistory', { junk: 1 }));
  const h2 = (await P.send('Page.getNavigationHistory')).result;
  out.afterReset = { idx: h2.currentIndex };
  out.back_after_reset = await P.ev('history.back();1');
  return norm(out);
};
probes.ref_default = async () => {
  const P = await newPage();
  const out = {};
  const t = async (name, params, url) => {
    seen['/hdr'] = [];
    await P.nav(url || O + '/hdr', params);
    out[name] = { h: hdr('/hdr'), ref: await P.ev('document.referrer') };
  };
  await t('none', {});
  await t('same', { referrer: O + '/p1?q=1#frag' });
  await t('cross', { referrer: O2 + '/p1/deep?q=1#frag' });
  await t('cred', { referrer: `http://user:pw@127.0.0.1:${site.pb}/p?x` });
  await t('cross_noref', { referrer: O2 + '/p1', referrerPolicy: 'noReferrer' });
  await t('cross_origin', { referrer: O2 + '/p1/deep?q=1', referrerPolicy: 'origin' });
  await t('cross_soc', { referrer: O2 + '/p1/deep?q=1', referrerPolicy: 'strictOriginWhenCrossOrigin' });
  await t('cross_unsafe', { referrer: O2 + '/p1/deep?q=1#f', referrerPolicy: 'unsafeUrl' });
  await t('cross_sameorigin', { referrer: O2 + '/p1/deep?q=1', referrerPolicy: 'sameOrigin' });
  await t('same_sameorigin', { referrer: O + '/p1/deep?q=1', referrerPolicy: 'sameOrigin' });
  await t('cross_oowco', { referrer: O2 + '/p1/deep?q=1', referrerPolicy: 'originWhenCrossOrigin' });
  await t('cross_nrwd', { referrer: O2 + '/p1/deep?q=1', referrerPolicy: 'noReferrerWhenDowngrade' });
  await t('cross_sdc', { referrer: O2 + '/p1/deep?q=1', referrerPolicy: 'strictOrigin' });
  await t('empty_ref', { referrer: '' });
  await t('empty_ref_pol', { referrer: '', referrerPolicy: 'unsafeUrl' });
  await t('pol_only', { referrerPolicy: 'unsafeUrl' });
  await t('badref', { referrer: 'not a url' });
  await t('file_ref', { referrer: 'file:///etc/passwd', referrerPolicy: 'unsafeUrl' });
  await t('data_ref', { referrer: 'data:text/html,x', referrerPolicy: 'unsafeUrl' });
  await t('about_ref', { referrer: 'about:blank', referrerPolicy: 'unsafeUrl' });
  await t('https_ref', { referrer: 'https://example.invalid/x?y#z', referrerPolicy: 'unsafeUrl' });
  await t('https_ref_default', { referrer: 'https://example.invalid/x?y#z' });
  await t('policy_case', { referrer: O2 + '/x', referrerPolicy: 'unsafe-url' });
  out.badpol = errOf(await P.send('Page.navigate', { url: O + '/hdr', referrer: O + '/x', referrerPolicy: 'bogus' }));
  out.badpol2 = errOf(await P.send('Page.navigate', { url: O + '/hdr', referrerPolicy: '' }));
  out.badpol3 = errOf(await P.send('Page.navigate', { url: O + '/hdr', referrerPolicy: 5 }));
  out.badtrans = errOf(await P.send('Page.navigate', { url: O + '/hdr', transitionType: 'bogus' }));
  out.badtrans2 = errOf(await P.send('Page.navigate', { url: O + '/hdr', transitionType: 'reload' }));
  return norm(out);
};
probes.ref_redirect = async () => {
  const P = await newPage();
  const out = {};
  seen['/hdr'] = [];
  await P.nav(O + '/r1', { referrer: O2 + '/p/deep?q#f', referrerPolicy: 'unsafeUrl' });
  out.r301 = { h: hdr('/hdr'), ref: await P.ev('document.referrer'), url: await P.ev('location.href') };
  seen['/hdr'] = [];
  await P.nav(O + '/rx', { referrer: O + '/p/deep?q#f', referrerPolicy: 'sameOrigin' });
  out.rx = { h: hdr('/hdr'), ref: await P.ev('document.referrer') };
  await P.nav(O + '/rp-origin', { referrer: O + '/p/deep?q#f', referrerPolicy: 'unsafeUrl' });
  out.rp = { h: hdr('/rp-origin'), ref: await P.ev('document.referrer') };
  seen['/pix.png'] = [];
  await P.nav(O + '/img', { referrer: O2 + '/zzz/yyy', referrerPolicy: 'unsafeUrl' });
  out.img = hdr('/pix.png');
  seen['/hdr'] = [];
  await P.ev(`fetch('/hdr')`); await sleep(200);
  out.fetch = hdr('/hdr');
  seen['/hdr'] = [];
  await P.ev(`location.href='/hdr'`); await sleep(400);
  out.script_nav = hdr('/hdr');
  return norm(out);
};
probes.ref_history = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/hdr?one', { referrer: O2 + '/orig', referrerPolicy: 'unsafeUrl' });
  await P.nav(O + '/p2');
  seen['/hdr'] = [];
  await P.ev('history.back()'); await sleep(500);
  out.back = { h: hdr('/hdr'), ref: await P.ev('document.referrer'), loc: await P.ev('location.href') };
  out.perf = await P.ev(`performance.getEntriesByType('navigation')[0].type`);
  seen['/hdr'] = [];
  await P.send('Page.reload'); await sleep(500);
  out.reload = { h: hdr('/hdr'), ref: await P.ev('document.referrer'), perf: await P.ev(`performance.getEntriesByType('navigation')[0].type`) };
  await P.ev('history.forward()'); await sleep(400);
  seen['/hdr'] = [];
  const h = (await P.send('Page.getNavigationHistory')).result;
  await P.send('Page.navigateToHistoryEntry', { entryId: h.entries[0].id }); await sleep(500);
  out.nth = { h: hdr('/hdr'), perf: await P.ev(`performance.getEntriesByType('navigation')[0].type`) };
  return norm(out);
};
probes.script_disabled = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/script-page');
  await P.ev(`setTimeout(()=>{window.__timer=1},400);window.__t0=1`);
  out.set = errOf(await P.send('Emulation.setScriptExecutionDisabled', { value: true }));
  out.bad = errOf(await P.send('Emulation.setScriptExecutionDisabled', {}));
  out.bad2 = errOf(await P.send('Emulation.setScriptExecutionDisabled', { value: 'x' }));
  out.evalWhile = await P.ev('1+1');
  await sleep(600);
  out.timer = await P.ev('window.__timer');
  await P.nav(O + '/script-page');
  out.after_nav = await P.ev(`JSON.stringify({inl:window.__inline,ext:window.__ext,mod:window.__mod,ns:!!document.getElementById('ns'),nsKids:document.querySelector('noscript').children.length})`);
  out.dyn = await P.ev(`(()=>{const s=document.createElement('script');s.textContent='window.__dyn=1';document.body.appendChild(s);return window.__dyn})()`);
  out.dynsrc = await P.ev(`(()=>{const s=document.createElement('script');s.src='/js';document.body.appendChild(s);return 1})()`); await sleep(300);
  out.dynsrcv = await P.ev('window.__ext');
  out.jsurl = await P.ev(`(()=>{location.href='javascript:window.__jsurl=1';return 1})()`); await sleep(200);
  out.jsurlv = await P.ev('window.__jsurl');
  out.click = await P.ev(`document.getElementById('b').click();window.__inl`);
  out.fnCall = await P.ev(`typeof document.getElementById('b').onclick`);
  out.evalFn = await P.ev(`new Function('return 5')()`);
  out.promise = await P.ev(`Promise.resolve(3).then(x=>x+1)`);
  out.iframe = await P.ev(`(()=>{const f=document.createElement('iframe');f.srcdoc='<script>parent.__ifr=1<\\/script>';document.body.appendChild(f);return 1})()`); await sleep(300);
  out.iframev = await P.ev('window.__ifr');
  out.enable = errOf(await P.send('Emulation.setScriptExecutionDisabled', { value: false }));
  await P.nav(O + '/script-page');
  out.after_enable = await P.ev(`JSON.stringify({inl:window.__inline,ext:window.__ext,mod:window.__mod,ns:!!document.getElementById('ns')})`);
  return out;
};
probes.script_disabled_event = async () => {
  const P = await newPage();
  await P.nav(O + '/p1');
  await P.ev(`window.__q=[];document.addEventListener('click',()=>__q.push('c'));requestAnimationFrame(()=>__q.push('raf'));queueMicrotask(()=>__q.push('mt'))`);
  await P.send('Emulation.setScriptExecutionDisabled', { value: true });
  await P.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: 10, y: 10, button: 'left', clickCount: 1 });
  await P.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: 10, y: 10, button: 'left', clickCount: 1 });
  await sleep(300);
  return { q: await P.ev('JSON.stringify(__q)') };
};
probes.blocked = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/p1');
  out.setBad = errOf(await P.send('Network.setBlockedURLs', {}));
  out.setStr = errOf(await P.send('Network.setBlockedURLs', { urls: 'x' }));
  out.setNum = errOf(await P.send('Network.setBlockedURLs', { urls: [1] }));
  out.setPat = errOf(await P.send('Network.setBlockedURLs', { urlPatterns: [{ urlPattern: '*', block: true }] }));
  out.setPatBad = errOf(await P.send('Network.setBlockedURLs', { urlPatterns: [{ block: true }] }));
  await P.send('Network.setBlockedURLs', { urls: [] });
  const f = async (u) => P.ev(`fetch(${JSON.stringify(u)}).then(r=>r.status,e=>e.name+':'+e.message)`);
  await P.send('Network.setBlockedURLs', { urls: ['*/json'] });
  out.json = await f('/json');
  out.jsonq = await f('/json?x=1');
  out.hdr = await f('/hdr');
  await P.send('Network.setBlockedURLs', { urls: ['*/JSON'] });
  out.jsonCase = await f('/json');
  await P.send('Network.setBlockedURLs', { urls: ['*/json?*'] });
  out.q1 = await f('/json'); out.q2 = await f('/json?a');
  await P.send('Network.setBlockedURLs', { urls: ['http://127.0.0.1:' + site.pa + '/json'] });
  out.exact = await f('/json'); out.exactq = await f('/json?a');
  await P.send('Network.setBlockedURLs', { urls: ['*json'] });
  out.fragj = await f('/json#frag');
  await P.send('Network.setBlockedURLs', { urls: ['*/hdr'] });
  P.mark();
  out.nav = errOf(await P.send('Page.navigate', { url: O + '/hdr' }));
  await sleep(300);
  out.navEv = P.events(['Network.loadingFailed', 'Network.requestWillBeSent', 'Page.frameNavigated']).map((e) => e.method + (e.params.errorText ? ':' + e.params.errorText + ':' + e.params.blockedReason : ''));
  out.navUrl = await P.ev('location.href');
  P.mark();
  await P.send('Network.setBlockedURLs', { urls: ['*/json'] });
  out.f2 = await f('/json');
  out.fEv = P.events(['Network.loadingFailed', 'Network.requestWillBeSent', 'Network.responseReceived']).map((e) => e.method + (e.params.errorText ? ':' + e.params.errorText + ':' + e.params.blockedReason : ''));
  out.xhr = await P.ev(`new Promise(r=>{const x=new XMLHttpRequest();x.open('GET','/json');x.onerror=()=>r('error');x.onload=()=>r('load '+x.status);x.send()})`);
  out.img = await P.ev(`new Promise(r=>{const i=new Image();i.onerror=()=>r('error');i.onload=()=>r('load');i.src='/json?img'})`);
  await P.send('Network.setBlockedURLs', { urls: [] });
  out.unblocked = await f('/json');
  return norm(out);
};
probes.offline = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/p1');
  out.onl0 = await P.ev('navigator.onLine');
  out.bad = errOf(await P.send('Network.emulateNetworkConditions', {}));
  out.bad2 = errOf(await P.send('Network.emulateNetworkConditions', { offline: true }));
  out.bad3 = errOf(await P.send('Network.emulateNetworkConditions', { offline: 'x', latency: 0, downloadThroughput: -1, uploadThroughput: -1 }));
  out.ok = errOf(await P.send('Network.emulateNetworkConditions', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }));
  await sleep(100);
  out.onl1 = await P.ev('navigator.onLine');
  out.log = await P.ev('JSON.stringify(__log)');
  out.fetch = await P.ev(`fetch('/json').then(r=>r.status,e=>e.name+':'+e.message)`);
  P.mark();
  const n = await P.send('Page.navigate', { url: O + '/p2' });
  out.nav = errOf(n); out.navErr = n.result?.errorText;
  await sleep(300);
  out.navEv = P.events(['Network.loadingFailed', 'Page.frameNavigated']).map((e) => e.method + (e.params.errorText ? ':' + e.params.errorText : ''));
  out.ws = await P.ev(`new Promise(r=>{try{const w=new WebSocket('ws://127.0.0.1:${site.pa}/x');w.onerror=()=>r('error');w.onopen=()=>r('open')}catch(e){r('throw '+e.message)}})`);
  out.ok2 = errOf(await P.send('Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }));
  await sleep(100);
  out.onl2 = await P.ev('navigator.onLine');
  out.log2 = await P.ev('JSON.stringify(__log)');
  out.fetch2 = await P.ev(`fetch('/json').then(r=>r.status,e=>e.name+':'+e.message)`);
  out.over = errOf(await P.send('Network.overrideNetworkState', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }));
  out.fetch3 = await P.ev(`fetch('/json').then(r=>r.status,e=>e.name+':'+e.message)`);
  out.onl3 = await P.ev('navigator.onLine');
  return norm(out);
};
probes.offline_scope = async () => {
  const P = await newPage();
  await P.nav(O + '/p1');
  await P.send('Network.emulateNetworkConditions', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  const P2 = await newPage();
  await P2.nav(O + '/p1');
  const other = await P2.ev(`fetch('/json').then(r=>r.status,e=>e.name)`);
  const same = await P.ev(`fetch('/json').then(r=>r.status,e=>e.name)`);
  await P.send('Network.disable');
  const afterDisable = await P.ev(`fetch('/json').then(r=>r.status,e=>e.name)`);
  const online = await P.ev('navigator.onLine');
  return { other, same, afterDisable, online, log: await P.ev('JSON.stringify(__log)') };
};
probes.frame_owner = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/iframe');
  const tree = (await P.send('Page.getFrameTree')).result.frameTree;
  const main = tree.frame.id, child = tree.childFrames?.[0]?.frame.id;
  out.main = errOf(await P.send('DOM.getFrameOwner', { frameId: main }));
  out.noparam = errOf(await P.send('DOM.getFrameOwner', {}));
  out.bogus = errOf(await P.send('DOM.getFrameOwner', { frameId: 'nope' }));
  out.numeric = errOf(await P.send('DOM.getFrameOwner', { frameId: 5 }));
  out.childNoDoc = errOf(await P.send('DOM.getFrameOwner', { frameId: child }));
  await P.send('DOM.getDocument', { depth: 0 });
  const r = await P.send('DOM.getFrameOwner', { frameId: child });
  out.childDoc = errOf(r);
  if (r.ok) {
    const d = await P.send('DOM.describeNode', { backendNodeId: r.result.backendNodeId });
    out.desc = d.ok ? { name: d.result.node.nodeName } : errOf(d);
  }
  await P.ev(`document.getElementById('f').remove()`); await sleep(200);
  out.removed = errOf(await P.send('DOM.getFrameOwner', { frameId: child }));
  return norm(out);
};
probes.heap = async () => {
  const P = await newPage();
  const out = {};
  out.r = errOf(await P.send('Runtime.getHeapUsage'));
  const a = (await P.send('Runtime.getHeapUsage')).result;
  await P.ev(`window.__keep=[];for(let i=0;i<200000;i++)__keep.push({i,s:'x'+i});1`);
  const b = (await P.send('Runtime.getHeapUsage')).result;
  out.grows = b.usedSize > a.usedSize; out.typesOk = [typeof a.usedSize, typeof a.totalSize, typeof a.embedderHeapUsedSize, typeof a.backingStorageSize];
  out.totalGeUsed = b.totalSize >= b.usedSize;
  out.withParams = errOf(await P.send('Runtime.getHeapUsage', { x: 1 }));
  const P0 = await newPage([]);
  out.noEnable = errOf(await P0.send('Runtime.getHeapUsage'));
  return out;
};
probes.audits = async () => {
  const P = await newPage([]);
  const out = {};
  out.en = errOf(await P.send('Audits.enable'));
  out.en2 = errOf(await P.send('Audits.enable'));
  out.dis = errOf(await P.send('Audits.disable'));
  out.dis2 = errOf(await P.send('Audits.disable'));
  out.enParam = errOf(await P.send('Audits.enable', { x: 1 }));
  out.enc = errOf(await P.send('Audits.getEncodedResponse', {}));
  out.enc2 = errOf(await P.send('Audits.getEncodedResponse', { requestId: 'x', encoding: 'png' }));
  out.contrast = errOf(await P.send('Audits.checkContrast', {}));
  out.forms = errOf(await P.send('Audits.checkFormsIssues'));
  out.bypass = errOf(await P.send('Network.setBypassServiceWorker', {}));
  out.bypass2 = errOf(await P.send('Network.setBypassServiceWorker', { bypass: true }));
  out.bypass3 = errOf(await P.send('Network.setBypassServiceWorker', { bypass: 'y' }));
  out.csp = errOf(await P.send('Page.setBypassCSP', { enabled: true }));
  out.cspbad = errOf(await P.send('Page.setBypassCSP', {}));
  await P.send('Audits.enable');
  await P.nav(O + '/p1');
  await sleep(300);
  out.issues = P.events(['Audits.issueAdded']).map((e) => e.params.issue.code);
  return out;
};
probes.redirect_events = async () => {
  const P = await newPage();
  const out = {};
  P.mark();
  await P.nav(O + '/r1');
  const rs = P.events(['Network.requestWillBeSent', 'Network.responseReceived', 'Network.loadingFinished']);
  out.seq = rs.map((e) => e.method.replace('Network.', '') + (e.params.redirectResponse ? '(redir ' + e.params.redirectResponse.status + ')' : '') + ':' + (e.params.request?.url || e.params.response?.url || '').replace(/.*\//, ''));
  const ids = [...new Set(rs.filter((e) => e.method === 'Network.requestWillBeSent').map((e) => e.params.requestId))];
  out.sameId = ids.length;
  out.fields = rs.filter((e) => e.method === 'Network.requestWillBeSent').map((e) => ({ type: e.params.type, hasRR: !!e.params.redirectResponse, rrKeys: e.params.redirectResponse ? Object.keys(e.params.redirectResponse).sort().join() : null, hasInit: !!e.params.initiator, redirectHasExtraInfo: e.params.redirectHasExtraInfo, loader: !!e.params.loaderId }));
  P.mark();
  await P.ev(`fetch('/r307',{method:'POST',body:'x'}).then(r=>r.url)`);
  await sleep(200);
  const rs2 = P.events(['Network.requestWillBeSent']);
  out.fetchHops = rs2.map((e) => e.params.request.method + ' ' + e.params.request.url.replace(/.*\//, '') + ' ' + (e.params.redirectResponse?.status ?? '-') + ' ' + e.params.type + ' ' + (e.params.request.postData ?? 'nopost'));
  P.mark();
  await P.nav(O + '/rx');
  const rs3 = P.events(['Network.requestWillBeSent']);
  out.xhops = rs3.map((e) => e.params.request.url + ' ' + (e.params.redirectResponse?.status ?? '-'));
  return norm(out);
};
probes.reload_after_push = async () => {
  const P = await newPage();
  await P.nav(O + '/p1');
  await P.ev(`history.pushState({a:1},'','/p2')`);
  const out = {};
  P.mark();
  out.re = errOf(await P.send('Page.reload')); await sleep(500);
  out.url = await P.ev('location.href');
  out.state = await P.ev('JSON.stringify(history.state)');
  const h = (await P.send('Page.getNavigationHistory')).result;
  out.hist = h.entries.map((e) => e.url.replace(/.*\//, '') + ':' + e.transitionType + ':' + e.title);
  out.cur = h.currentIndex;
  out.ev = lc(P);
  out.re2 = errOf(await P.send('Page.reload', { ignoreCache: 'x' }));
  out.re3 = errOf(await P.send('Page.reload', { scriptToEvaluateOnLoad: 5 }));
  return norm(out);
};
probes.frame_history = async () => {
  const P = await newPage();
  await P.nav(O + '/iframe');
  const out = {};
  out.len0 = await P.ev('history.length');
  await P.ev(`document.getElementById('f').src='/p2'`); await sleep(400);
  out.len1 = await P.ev('history.length');
  const h = (await P.send('Page.getNavigationHistory')).result;
  out.entries = h.entries.map((e) => e.url.replace(/.*\//, ''));
  out.cur = h.currentIndex;
  await P.ev('history.back()'); await sleep(500);
  out.after_back = await P.ev('location.href');
  out.iframe_src = await P.ev(`document.getElementById('f')?.contentWindow.location.href`);
  return norm(out);
};
probes.popstate_state = async () => {
  const P = await newPage();
  await P.nav(O + '/p1');
  await P.ev(`history.replaceState({s:0},'')`);
  await P.ev(`history.pushState({s:1},'','/p1?a')`);
  await P.ev(`history.pushState({s:2},'','/p1#h')`);
  await P.ev(`location.hash='k'`);
  const out = {};
  out.state = await P.ev('JSON.stringify(history.state)');
  await P.ev('history.back()'); await sleep(300);
  out.s1 = [await P.ev('JSON.stringify(history.state)'), await P.ev('location.href')];
  await P.ev('history.go(-2)'); await sleep(300);
  out.s2 = [await P.ev('JSON.stringify(history.state)'), await P.ev('location.href')];
  await P.ev('history.go(3)'); await sleep(300);
  out.s3 = [await P.ev('JSON.stringify(history.state)'), await P.ev('location.href')];
  out.log = await P.ev('JSON.stringify(__log)');
  await P.nav(O + '/p2');
  await P.ev('history.back()'); await sleep(500);
  out.back_to_push = [await P.ev('JSON.stringify(history.state)'), await P.ev('location.href'), await P.ev('history.length')];
  await P.ev('history.back()'); await sleep(500);
  out.back2 = [await P.ev('JSON.stringify(history.state)'), await P.ev('location.href'), await P.ev('history.length')];
  return norm(out);
};
probes.nav_misc = async () => {
  const P = await newPage();
  const out = {};
  out.same = errOf(await P.send('Page.navigate', { url: O + '/p1' }));
  await sleep(300);
  const r = await P.send('Page.navigate', { url: O + '/p1#a' });
  out.frag = { keys: Object.keys(r.result || {}).sort(), err: r.error };
  const r2 = await P.send('Page.navigate', { url: O + '/p1#a' });
  out.frag2 = { keys: Object.keys(r2.result || {}).sort() };
  const r3 = await P.send('Page.navigate', { url: O + '/p1#b', referrer: O2 + '/x' });
  out.frag3 = { keys: Object.keys(r3.result || {}).sort() };
  out.len = await P.ev('history.length');
  const r4 = await P.send('Page.navigate', { url: 'about:blank#x' });
  out.blankfrag = { keys: Object.keys(r4.result || {}).sort(), err: r4.error?.message };
  await sleep(200);
  out.u = await P.ev('location.href');
  out.badframe = errOf(await P.send('Page.navigate', { url: O + '/p1', frameId: 'bogus' }));
  out.empty = errOf(await P.send('Page.navigate', { url: '' }));
  out.jsurl = errOf(await P.send('Page.navigate', { url: 'javascript:1' }));
  out.link = errOf(await P.send('Page.navigate', { url: O + '/p1', transitionType: 'link' })); await sleep(200);
  const h = (await P.send('Page.getNavigationHistory')).result;
  out.trans = h.entries.map((e) => e.transitionType).join(',');
  return norm(out);
};
// A navigation that fails commits Chrome's error page: a document at
// chrome-error://chromewebdata/ with an entry of the failed URL.
probes.fail_nav = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/p1'); await P.nav(O + '/p2');
  const dead = `http://127.0.0.1:${site.dead}/x`;
  P.mark();
  const r = await P.send('Page.navigate', { url: dead });
  out.result = errOf(r); out.errorText = r.result?.errorText;
  await sleep(400);
  const evs = P.events(['Page.frameNavigated', 'Page.frameStartedNavigating', 'Page.frameStartedLoading', 'Page.frameStoppedLoading', 'Page.loadEventFired', 'Page.domContentEventFired', 'Network.requestWillBeSent', 'Network.loadingFailed', 'Network.responseReceived']);
  out.events = evs.map((e) => e.method.replace(/^(Page|Network)\./, '') + (e.params.navigationType ? ':' + e.params.navigationType : '') + (e.params.errorText !== undefined ? ':' + e.params.errorText : '') + (e.method === 'Page.frameNavigated' ? ':' + e.params.frame.url + ':' + (e.params.frame.unreachableUrl ?? '-') + ':' + e.params.frame.loaderId.length : ''));
  const tree = (await P.send('Page.getFrameTree')).result.frameTree.frame;
  out.tree = { url: tree.url, unreachableUrl: tree.unreachableUrl ?? null, mimeType: tree.mimeType, secure: tree.secureContextType, origin: tree.securityOrigin };
  out.loc = await P.ev('location.href');
  out.origin = await P.ev('location.origin');
  out.title = await P.ev('document.title');
  out.html = await P.ev('document.documentElement.outerHTML.length > 100');
  out.ready = await P.ev('document.readyState');
  const h = (await P.send('Page.getNavigationHistory')).result;
  out.hist = h.entries.map((e) => [e.url.replace(/.*\//, ''), e.userTypedURL.replace(/.*\//, ''), e.title, e.transitionType]);
  out.cur = h.currentIndex;
  out.len = await P.ev('history.length');
  // A reload of the error page tries the failed URL again; going back
  // returns to the page before it.
  P.mark();
  const rl = await P.send('Page.reload');
  await sleep(400);
  out.reload = errOf(rl);
  out.reloadEv = P.events(['Page.frameNavigated', 'Network.loadingFailed']).map((e) => e.method.replace(/^(Page|Network)\./, ''));
  out.afterReload = [(await P.send('Page.getNavigationHistory')).result.entries.length, await P.ev('location.href')];
  await P.ev('history.back()'); await sleep(500);
  out.back = await P.ev('location.href');
  await P.ev('history.forward()'); await sleep(500);
  out.forward = await P.ev('location.href');
  // The same through a script navigation and after a successful one.
  await P.nav(O + '/p3');
  out.after = [await P.ev('location.href'), (await P.send('Page.getNavigationHistory')).result.entries.length];
  return norm(out);
};
// A script navigation (a link) that fails commits the error page too.
probes.fail_script_nav = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/p1');
  P.mark();
  await P.ev(`location.href='http://127.0.0.1:${site.dead}/y'`); await sleep(500);
  out.loc = await P.ev('location.href');
  out.events = P.events(['Page.frameNavigated', 'Network.loadingFailed']).map((e) => e.method.replace(/^(Page|Network)\./, ''));
  out.hist = (await P.send('Page.getNavigationHistory')).result.entries.map((e) => [e.url.replace(/.*\//, ''), e.transitionType]);
  await P.ev('history.back()'); await sleep(500);
  out.back = await P.ev('location.href');
  return norm(out);
};
// The newer `urlPatterns` form of Network.setBlockedURLs.
probes.url_patterns = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/p1');
  const f = async (u) => P.ev(`fetch(${JSON.stringify(u)}).then(r=>r.status,e=>e.name)`);
  const set = async (name, pats) => {
    const r = await P.send('Network.setBlockedURLs', { urlPatterns: pats });
    out[name] = { set: errOf(r), json: r.ok ? await f('/json') : null, hdr: r.ok ? await f('/hdr') : null };
  };
  await set('abs_port', [{ urlPattern: `http://127.0.0.1:${site.pa}/json`, block: true }]);
  await set('abs_wild', [{ urlPattern: 'http://127.0.0.1:*/json*', block: true }]);
  await set('star_scheme', [{ urlPattern: '*://127.0.0.1:*/json', block: true }]);
  await set('relative', [{ urlPattern: '/json', block: true }]);
  await set('no_block', [{ urlPattern: `http://127.0.0.1:${site.pa}/json`, block: false }]);
  await set('pathname_only', [{ urlPattern: 'http://*/json', block: true }]);
  await set('missing_block', [{ urlPattern: `http://127.0.0.1:${site.pa}/json` }]);
  await set('bad', [{ urlPattern: 'http://[', block: true }]);
  await set('empty', []);
  return norm(out);
};
// A redirect that ends in a refused connection: the events up to the failure.
probes.redirect_fail = async () => {
  const P = await newPage();
  const out = {};
  P.mark();
  const r = await P.send('Page.navigate', { url: O + '/rdead' });
  await sleep(400);
  out.nav = [errOf(r), r.result?.errorText];
  out.events = P.events(['Network.requestWillBeSent', 'Network.responseReceived', 'Network.loadingFailed', 'Page.frameNavigated']).map((e) => e.method.replace(/^(Page|Network)\./, '') + (e.params.redirectResponse ? '(redir ' + e.params.redirectResponse.status + ')' : '') + (e.params.request ? ':' + e.params.request.url : '') + (e.params.errorText !== undefined ? ':' + e.params.errorText : ''));
  out.loc = await P.ev('location.href');
  // The same for a fetch.
  await P.nav(O + '/p1');
  P.mark();
  out.fetch = await P.ev(`fetch('/rdead').then(r=>r.status,e=>e.name)`);
  out.fetchEvents = P.events(['Network.requestWillBeSent', 'Network.responseReceived', 'Network.loadingFailed']).map((e) => e.method.replace(/^Network\./, '') + (e.params.redirectResponse ? '(redir ' + e.params.redirectResponse.status + ')' : '') + (e.params.request ? ':' + e.params.request.url : '') + (e.params.errorText !== undefined ? ':' + e.params.errorText : ''));
  // A blocked URL at the end of a redirect chain.
  await P.send('Network.setBlockedURLs', { urls: ['*/hdr'] });
  P.mark();
  out.blockedHop = await P.ev(`fetch('/r1').then(r=>r.status,e=>e.name)`);
  out.blockedHopEvents = P.events(['Network.requestWillBeSent', 'Network.loadingFailed']).map((e) => e.method.replace(/^Network\./, '') + (e.params.request ? ':' + e.params.request.url : '') + (e.params.errorText !== undefined ? ':' + e.params.errorText + ':' + e.params.blockedReason : ''));
  return norm(out);
};
probes.fail_history = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/p1'); await P.nav(O + '/p2');
  const dead = await new Promise((r) => { const s = http.createServer(); s.listen(0, '127.0.0.1', () => { const p = s.address().port; s.close(() => r(p)); }); });
  const r = await P.send('Page.navigate', { url: `http://127.0.0.1:${dead}/x` });
  out.fail = r.result?.errorText;
  await sleep(300);
  const h = (await P.send('Page.getNavigationHistory')).result;
  out.hist = h.entries.map((e) => e.url.replace(/.*\//, '').replace(/^x$/, 'dead'));
  out.cur = h.currentIndex;
  out.len = await P.ev('history.length');
  out.loc = await P.ev('location.href');
  return norm(out);
};
probes.beforeunload_back = async () => {
  const P = await newPage();
  await P.nav(O + '/p1'); await P.nav(O + '/p2');
  const out = {};
  await P.ev(`window.onbeforeunload=()=>'x'; 1`);
  out.back = await P.ev('history.back();1'); await sleep(400);
  out.loc = await P.ev('location.href');
  out.dlg = P.events(['Page.javascriptDialogOpening']).length;
  return norm(out);
};


probes.link_ref = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/link?deep=1#f');
  seen['/hdr'] = [];
  await P.ev(`document.getElementById('l').click()`); await sleep(500);
  out.click = { h: hdr('/hdr'), ref: await P.ev('document.referrer') };
  seen['/hdr'] = [];
  await P.send('Page.reload'); await sleep(500);
  out.reload = { h: hdr('/hdr'), ref: await P.ev('document.referrer') };
  seen['/hdr'] = [];
  await P.ev('location.reload()'); await sleep(500);
  out.locreload = { h: hdr('/hdr'), ref: await P.ev('document.referrer') };
  await P.nav(O + '/p2');
  seen['/hdr'] = [];
  await P.ev('history.back()'); await sleep(500);
  out.back = { h: hdr('/hdr'), ref: await P.ev('document.referrer') };
  return norm(out);
};
probes.frag_events = async () => {
  const P = await newPage();
  await P.nav(O + '/p1');
  const out = {};
  P.mark();
  await P.ev(`location.hash='k'`); await sleep(200);
  out.hashset = [lc(P), await P.ev('JSON.stringify(__log)')];
  await P.ev('__log.length=0');
  P.mark();
  await P.send('Page.navigate', { url: O + '/p1#z' }); await sleep(300);
  out.cdp = [lc(P), await P.ev('JSON.stringify(__log)')];
  await P.ev('__log.length=0');
  P.mark();
  await P.ev(`history.pushState({a:1},'','/p1#q')`); await sleep(100);
  out.push = [lc(P), await P.ev('JSON.stringify(__log)')];
  await P.ev('__log.length=0');
  P.mark();
  await P.ev(`history.replaceState({a:1},'','/p1#qq')`); await sleep(100);
  out.repl = [lc(P), await P.ev('JSON.stringify(__log)')];
  return norm(out);
};
// Fragment navigations to the URL the page is on, from script and from
// Page.navigate: entries, transition types and the events each one fires.
probes.frag_same = async () => {
  const P = await newPage();
  await P.nav(O + '/p1');
  const out = {};
  const step = async (name, f) => {
    await P.ev('__log.length=0');
    P.mark();
    await f();
    await sleep(250);
    const h = (await P.send('Page.getNavigationHistory')).result;
    out[name] = { ev: lc(P), log: await P.ev('JSON.stringify(__log)'), len: await P.ev('history.length'), cur: h.currentIndex, trans: h.entries.map((e) => e.transitionType).join(',') };
  };
  await step('hash_a', () => P.ev(`location.hash='a'`));
  await step('hash_a_again', () => P.ev(`location.hash='a'`));
  await step('href_same', () => P.ev(`location.href=location.href`));
  await step('assign_other', () => P.ev(`location.assign('#b')`));
  await step('replace_c', () => P.ev(`location.replace('#c')`));
  await step('cdp_same', () => P.send('Page.navigate', { url: O + '/p1#c' }));
  await step('cdp_d', () => P.send('Page.navigate', { url: O + '/p1#d', transitionType: 'link' }));
  await step('cdp_nofrag', () => P.send('Page.navigate', { url: O + '/p1' }));
  return norm(out);
};
probes.double_back2 = async () => {
  const P = await newPage();
  await P.nav(O + '/p1'); await P.nav(O + '/p2'); await P.nav(O + '/p3');
  const out = {};
  await P.ev('history.back();history.back()'); await sleep(900);
  out.a = await P.ev('location.href');
  await P.ev('history.forward()'); await sleep(600);
  out.fwd = await P.ev('location.href');
  await P.ev('history.back();history.forward()'); await sleep(900);
  out.b = await P.ev('location.href');
  const h = (await P.send('Page.getNavigationHistory')).result;
  out.cur = h.currentIndex;
  await P.ev('history.go(-1);history.go(-1);history.go(1)'); await sleep(900);
  out.c = await P.ev('location.href');
  return norm(out);
};
probes.blocked_raw = async () => {
  const P = await newPage();
  await P.nav(O + '/p1');
  await P.send('Network.setBlockedURLs', { urls: ['*/json'] });
  P.mark();
  await P.ev(`fetch('/json').catch(()=>1)`); await sleep(200);
  const ev = P.events(['Network.loadingFailed', 'Network.requestWillBeSent', 'Network.requestWillBeSentExtraInfo']);
  // requestId and timestamp differ on every run (and between browsers): keep their types only.
  const stable = (p) => ({ ...p, requestId: typeof p.requestId, timestamp: typeof p.timestamp });
  return norm(ev.map((e) => ({ m: e.method, p: e.method.endsWith('Failed') ? stable(e.params) : Object.keys(e.params).sort() })));
};
probes.redirect_raw = async () => {
  const P = await newPage();
  P.mark();
  await P.nav(O + '/r1');
  const e = P.events(['Network.requestWillBeSent']);
  return norm(e.map((x) => ({ rr: x.params.redirectResponse && Object.fromEntries(Object.entries(x.params.redirectResponse).map(([k, v]) => [k, typeof v === 'object' ? Object.keys(v || {}).sort().join('|') : typeof v])), top: Object.keys(x.params).sort().join('|') })));
};
probes.long_ref = async () => {
  const P = await newPage();
  const out = {};
  seen['/hdr'] = [];
  await P.nav(O + '/hdr', { referrer: O2 + '/' + 'a'.repeat(5000) + '?q', referrerPolicy: 'unsafeUrl' });
  out.long = { h: hdr('/hdr'), ref: (await P.ev('document.referrer')).length };
  seen['/hdr'] = [];
  await P.nav(O + '/hdr', { referrer: O2 + '/' + 'a'.repeat(4000), referrerPolicy: 'unsafeUrl' });
  out.mid = { h: hdr('/hdr'), ref: (await P.ev('document.referrer')).length };
  return norm(out);
};
probes.frame_nav = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/iframe');
  const tree = (await P.send('Page.getFrameTree')).result.frameTree;
  const child = tree.childFrames[0].frame.id;
  seen['/hdr'] = [];
  P.mark();
  const r = await P.send('Page.navigate', { url: O + '/hdr', frameId: child, referrer: O2 + '/x', referrerPolicy: 'unsafeUrl' });
  out.r = errOf(r); await sleep(400);
  out.h = hdr('/hdr');
  out.len = await P.ev('history.length');
  out.ev = lc(P);
  const h = (await P.send('Page.getNavigationHistory')).result;
  out.entries = h.entries.map((e) => e.url.replace(/.*\//, ''));
  return norm(out);
};
probes.reload_older = async () => {
  const P = await newPage();
  const out = {};
  await P.nav(O + '/p1'); await P.nav(O + '/p2'); await P.nav(O + '/p3');
  await P.ev('history.back()'); await sleep(500);
  await P.send('Page.reload'); await sleep(500);
  const h = (await P.send('Page.getNavigationHistory')).result;
  out.cur = h.currentIndex; out.n = h.entries.length; out.loc = await P.ev('location.href');
  await P.ev('location.reload()'); await sleep(500);
  out.cur2 = (await P.send('Page.getNavigationHistory')).result.currentIndex;
  await P.ev('history.forward()'); await sleep(500);
  out.fw = await P.ev('location.href');
  return norm(out);
};
// (A Page.navigate followed at once by navigateToHistoryEntry is not
// probed: Chrome's answer to the second call differs from run to run.)
probes.glob_q = async () => {
  const P = await newPage();
  await P.nav(O + '/p1');
  const out = {};
  const f = async (u) => P.ev(`fetch(${JSON.stringify(u)}).then(r=>r.status,e=>e.name)`);
  await P.send('Network.setBlockedURLs', { urls: ['*/js?n'] });
  out.q = await f('/json'); out.q2 = await f('/json?n'); out.q3 = await f('/jsXn');
  await P.send('Network.setBlockedURLs', { urls: ['*/*son*a*a*a*a*a*a*a*b'] });
  out.many = await f('/json');
  await P.send('Network.setBlockedURLs', { urls: ['http://127.0.0.1:*/json'] });
  out.port = await f('/json');
  await P.send('Network.setBlockedURLs', { urls: ['json'] });
  out.bare = await f('/json');
  await P.send('Network.setBlockedURLs', { urls: ['/json'] });
  out.path = await f('/json');
  return out;
};

probes.stress = async () => {
  const out = {};
  const P = await newPage();
  await P.nav(O + '/p1'); await P.nav(O + '/p2'); await P.nav(O + '/p3');
  const h = (await P.send('Page.getNavigationHistory')).result;
  out.huge = errOf(await P.send('Page.navigateToHistoryEntry', { entryId: 9007199254740993 }));
  out.huge2 = errOf(await P.send('Page.navigateToHistoryEntry', { entryId: 1e300 }));
  out.neg0 = errOf(await P.send('Page.navigateToHistoryEntry', { entryId: -0 }));
  // two rapid traversals by id
  const a = P.send('Page.navigateToHistoryEntry', { entryId: h.entries[1].id });
  const b = P.send('Page.navigateToHistoryEntry', { entryId: h.entries[2].id });
  out.a = errOf(await a); out.b = errOf(await b);
  await sleep(600);
  out.loc = await P.ev('location.href');
  // close mid traversal
  const P2 = await newPage();
  await P2.nav(O + '/p1'); await P2.nav(O + '/p2');
  await P2.ev('history.back()');
  out.close = errOf(await P2.c.send('Target.closeTarget', { targetId: (await P2.c.send('Target.getTargets')).result.targetInfos.at(-1).targetId }));
  await sleep(300);
  // page still alive?
  const P3 = await newPage();
  await P3.nav(O + '/p1');
  out.alive = await P3.ev('1+1');
  // history.back in inline script during parse, and in unload
  const P4 = await newPage();
  await P4.nav(O + '/p1');
  await P4.ev(`addEventListener('unload',()=>history.back())`);
  await P4.nav(O + '/p2');
  out.unload_back = [await P4.ev('location.href'), await P4.ev('1+1')];
  // go(0) loop guard
  const P5 = await newPage();
  await P5.nav(O + '/p1');
  await P5.ev('history.go(0)'); await sleep(500);
  out.go0 = [await P5.ev('location.href'), (await P5.send('Page.getNavigationHistory')).result.entries.length];
  // many blocked patterns perf
  const P6 = await newPage();
  await P6.nav(O + '/p1');
  const pats = Array.from({ length: 5000 }, (_, i) => `*/never${i}/*a*b*c*d`);
  const t0 = Date.now();
  await P6.send('Network.setBlockedURLs', { urls: pats });
  await P6.ev(`Promise.all(Array.from({length:50},()=>fetch('/json'))).then(r=>r.length)`);
  out.perf_ms_lt = Date.now() - t0 < 5000;
  // script disabled + worker + module + dynamic import
  const P7 = await newPage();
  await P7.nav(O + '/p1');
  await P7.send('Emulation.setScriptExecutionDisabled', { value: true });
  out.disabled_nav = errOf(await P7.send('Page.navigate', { url: O + '/script-page' }));
  await sleep(300);
  out.disabled_eval = await P7.ev('document.title');
  out.disabled_worker = await P7.ev(`new Promise(r=>{try{const w=new Worker(URL.createObjectURL(new Blob(['postMessage(1)'])));w.onmessage=()=>r('msg');setTimeout(()=>r('none'),500)}catch(e){r('throw '+e.message)}})`);
  out.disabled_timer = await P7.ev(`new Promise(r=>{setTimeout(()=>r('fired'),50)})`);
  return out;
};


// ------------------------------------------------------------------ driver

// Differences from Chrome that are kept on purpose: a path prefix of
// `probe.key...` and the reason. A prefix that no longer differs fails the
// run until it is removed.
const KNOWN = {
  // Chrome's own behaviour that depends on its connection reuse, not on the
  // emulation: after going online again its next fetch still fails.
  'offline.fetch2': 'Chrome keeps failing the first fetch after the emulation ends',
};

function flatten(v, p, out) {
  if (v && typeof v === 'object') {
    const keys = Array.isArray(v) ? v.map((_, i) => String(i)) : Object.keys(v);
    if (!keys.length) out[p] = Array.isArray(v) ? '[]' : '{}';
    for (const k of keys) flatten(v[k], p ? `${p}.${k}` : k, out);
  } else out[p] = v;
  return out;
}

const names = typeof opt('only', null) === 'string' ? opt('only').split(',') : Object.keys(probes);
const results = {};
try {
  for (const n of names) {
    try {
      const r = await Promise.race([probes[n](), sleep(60000).then(() => 'PROBE TIMEOUT')]);
      results[n] = JSON.parse(JSON.stringify(norm(r)));
    } catch (e) {
      results[n] = 'THROW ' + (e.stack || e);
    }
    console.error('done', n);
  }
} finally {
  proc.kill('SIGKILL');
  try {
    rmSync(dir, { recursive: true, force: true });
  } catch {}
}
let code = 0;
if (mode === 'record') {
  const out = opt('out', recorded);
  const prev = (() => {
    try {
      return JSON.parse(readFileSync(out, 'utf8'));
    } catch {
      return {};
    }
  })();
  writeFileSync(out, JSON.stringify({ ...prev, ...results }, null, 1) + '\n');
  console.log(`recorded ${names.length} probes into ${out}`);
} else if (mode === 'compare') {
  const want = JSON.parse(readFileSync(recorded, 'utf8'));
  let diffs = 0;
  let known = 0;
  const seen = new Set();
  for (const n of names) {
    const a = flatten(want[n], n, {});
    const b = flatten(results[n], n, {});
    for (const k of new Set([...Object.keys(a), ...Object.keys(b)])) {
      if (JSON.stringify(a[k]) === JSON.stringify(b[k])) continue;
      const kn = Object.keys(KNOWN).find((p) => k === p || k.startsWith(p + '.'));
      if (kn) {
        known++;
        seen.add(kn);
        continue;
      }
      diffs++;
      console.log(`DIFF ${k}\n   chrome: ${JSON.stringify(a[k])?.slice(0, 300)}\n   ours:   ${JSON.stringify(b[k])?.slice(0, 300)}`);
    }
  }
  const stale = Object.keys(KNOWN).filter((k) => !seen.has(k) && names.some((n) => k.split('.')[0] === n) && !opt('only', null));
  for (const k of stale) console.log(`stale KNOWN entry (it matches now): ${k}`);
  console.log(`${names.length} probes compared with Chrome: ${diffs} differences, ${known} known, ${stale.length} stale`);
  code = diffs || stale.length ? 1 : 0;
} else {
  console.log(JSON.stringify(results, null, 1));
}
for (const s of site.servers) {
  s.closeAllConnections?.();
  s.close();
}
process.exit(code);

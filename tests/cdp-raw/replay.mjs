#!/usr/bin/env node
// Raw CDP replay: the command sequences stock Puppeteer and Playwright send
// (tests/cdp-raw/README.md) played over a plain WebSocket, no client library.
//
//   node tests/cdp-raw/replay.mjs record  --launch chrome        # writes chrome.json
//   node tests/cdp-raw/replay.mjs compare --launch lightbrowser  # compares with chrome.json
//   node tests/cdp-raw/replay.mjs run     --ws ws://127.0.0.1:PORT/devtools/browser/ID
//
// What is compared (Chrome's answers come from chrome.json, recorded from a
// local Chrome 153 on loopback pages; LightBrowser's are live):
//   * success versus error, and the error code (the text too where a step
//     says `msg: true`);
//   * the shape of a result: keys and types, never values (an empty array
//     matches any array);
//   * `value`: a few picked values that must be equal (URLs, history
//     entries, request headers), with the test site's origin and ids
//     normalised;
//   * `events`: the ordered Page events of a step, where a step asks for it.
//
// Only loopback: the browser and the two test sites listen on 127.0.0.1.
//
// Options: --launch chrome|lightbrowser, --bin PATH (browser binary),
// --port N (default 19260 for the browser), --out FILE, --expect FILE,
// --only NAME[,NAME] (scenarios), --verbose, --keep (print the trace).

import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../..');

const argv = process.argv.slice(2);
const mode = argv[0] && !argv[0].startsWith('--') ? argv.shift() : 'run';
const opt = (name, dflt) => {
  const i = argv.indexOf(`--${name}`);
  if (i < 0) return dflt;
  return argv[i + 1] && !argv[i + 1].startsWith('--') ? argv[i + 1] : true;
};
const verbose = !!opt('verbose', false);

// ---------------------------------------------------------------- CDP client

class Cdp {
  constructor(url) {
    this.url = url;
    this.nextId = 1;
    this.pending = new Map();
    this.events = [];
  }
  open() {
    return new Promise((resolve, reject) => {
      const ws = (this.ws = new WebSocket(this.url));
      ws.addEventListener('open', () => resolve());
      ws.addEventListener('error', (e) => reject(new Error(`websocket: ${e.message || e.type}`)));
      ws.addEventListener('close', () => {
        for (const [, p] of this.pending) p({ ok: false, error: { code: 'closed', message: 'connection closed' } });
        this.pending.clear();
      });
      ws.addEventListener('message', (m) => {
        const msg = JSON.parse(m.data);
        if (msg.id !== undefined) {
          const p = this.pending.get(msg.id);
          if (!p) return;
          this.pending.delete(msg.id);
          p(msg.error ? { ok: false, error: msg.error } : { ok: true, result: msg.result });
        } else {
          this.events.push({ method: msg.method, params: msg.params, sessionId: msg.sessionId });
        }
      });
    });
  }
  send(method, params, sessionId, timeout = 10000) {
    const id = this.nextId++;
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        if (verbose) console.error(`  timeout: ${method} ${JSON.stringify(params)}; last events: ${this.events.slice(-6).map((e) => e.method).join(', ')}`);
        resolve({ ok: false, error: { code: 'timeout', message: `${method} timed out` } });
      }, timeout);
      this.pending.set(id, (r) => {
        clearTimeout(timer);
        resolve(r);
      });
      const msg = { id, method };
      if (params !== undefined) msg.params = params;
      if (sessionId) msg.sessionId = sessionId;
      this.ws.send(JSON.stringify(msg));
    });
  }
  close() {
    try {
      this.ws.close();
    } catch {}
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** A loopback port nothing listens on. */
function deadPort() {
  return new Promise((resolve) => {
    const s = http.createServer();
    s.listen(0, '127.0.0.1', () => {
      const port = s.address().port;
      s.close(() => resolve(port));
    });
  });
}

// ------------------------------------------------------------- test sites

/** Two origins on loopback (different ports) so Referer and Sec-Fetch-Site
 *  have a same-origin and a same-site case. `seen[path]` is the last
 *  request's headers. */
function startSite(label) {
  const seen = {};
  const hits = [];
  const server = http.createServer((req, res) => {
    const u = new URL(req.url, 'http://x');
    seen[u.pathname] = req.headers;
    hits.push(u.pathname);
    const page = (title, body = '', head = '') => {
      res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' });
      res.end(`<!DOCTYPE html><html><head><meta charset="utf-8"><title>${title}</title>${head}</head><body>${body}</body></html>`);
    };
    switch (u.pathname) {
      case '/a':
        return page('Page A', '<a id="link" href="/b">to b</a><div id="x">a</div>');
      case '/b':
        return page('Page B', '<div id="x">b</div>');
      case '/c':
        return page('Page C', '<div id="x">c</div>');
      case '/headers':
        return page('Headers', '<div id="x">headers</div>');
      case '/iframe':
        return page('Frames', '<iframe id="f" src="/a"></iframe>');
      case '/redirect':
        res.writeHead(302, { location: '/b', 'cache-control': 'no-store' });
        return res.end();
      case '/slow':
        return setTimeout(() => page('Slow', 'slow'), 400);
      case '/cookie':
        res.writeHead(200, {
          'content-type': 'text/html; charset=utf-8',
          'set-cookie': ['srv=1; Path=/', 'srvhttp=2; Path=/; HttpOnly'],
        });
        return res.end('<!DOCTYPE html><title>Cookie</title>cookie');
      case '/json':
        res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' });
        return res.end('{"ok":true,"n":1}');
      case '/img.png':
        res.writeHead(200, { 'content-type': 'image/png', 'cache-control': 'no-store' });
        return res.end(Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg==', 'base64'));
      case '/hashpage':
        return page('Hash', '<div id="x">hash</div><a id="h" href="#target">h</a><div id="target">t</div>');
      default:
        res.writeHead(404, { 'content-type': 'text/plain' });
        return res.end('not found');
    }
  });
  return new Promise((resolve) =>
    server.listen(0, '127.0.0.1', () => resolve({ label, server, seen, hits, port: server.address().port, origin: `http://127.0.0.1:${server.address().port}` })),
  );
}

// ------------------------------------------------------------ normalising

/** Shape of a JSON value: keys and types, not values. Arrays are the shape
 *  of their first element (`[]` for an empty one: matches any array). */
function shapeOf(v) {
  if (v === null) return 'null';
  if (Array.isArray(v)) return v.length ? [shapeOf(v[0])] : [];
  if (typeof v === 'object') {
    const o = {};
    for (const k of Object.keys(v).sort()) o[k] = shapeOf(v[k]);
    return o;
  }
  return typeof v;
}

function diffShape(a, b, at = '$') {
  // a: expected (Chrome), b: actual.
  const out = [];
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!(Array.isArray(a) && Array.isArray(b))) return [`${at}: expected ${JSON.stringify(a)}, got ${JSON.stringify(b)}`];
    if (a.length && b.length) out.push(...diffShape(a[0], b[0], `${at}[]`));
    return out;
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    for (const k of Object.keys(a)) {
      if (!(k in b)) out.push(`${at}.${k}: missing (Chrome has ${JSON.stringify(a[k])})`);
      else out.push(...diffShape(a[k], b[k], `${at}.${k}`));
    }
    for (const k of Object.keys(b)) if (!(k in a)) out.push(`${at}.${k}: extra (Chrome has none)`);
    return out;
  }
  return a === b ? [] : [`${at}: expected ${JSON.stringify(a)}, got ${JSON.stringify(b)}`];
}

const sortKeys = (v) =>
  Array.isArray(v) ? v.map(sortKeys) : v && typeof v === 'object' ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sortKeys(v[k])])) : v;

/** Replace the sites' origins, hex ids and the session's random bits. */
function norm(v, ctx) {
  const s = JSON.stringify(sortKeys(v));
  if (s === undefined) return v;
  let t = s;
  t = t.split(ctx.siteA.origin).join('{A}').split(ctx.siteB.origin).join('{B}');
  t = t.replace(/\b[0-9A-F]{32}\b/g, '<id>');
  return JSON.parse(t);
}

// ------------------------------------------------------------- recording

/** A browser session: records every command made through `cmd`. */
class Run {
  constructor(cdp, siteA, siteB, trace) {
    this.cdp = cdp;
    this.siteA = siteA;
    this.siteB = siteB;
    this.trace = trace; // array of records of the current scenario
    this.ctx = { siteA, siteB };
    this.pages = [];
  }
  record(label, method, res, o = {}) {
    const rec = { label, method, ok: res.ok };
    if (!res.ok) {
      rec.error = { code: res.error.code };
      if (o.msg) rec.error.message = norm(res.error.message, this.ctx);
    } else {
      rec.shape = shapeOf(norm(res.result, this.ctx));
      if (o.value) {
        try {
          rec.value = norm(o.value(res.result), this.ctx);
        } catch (e) {
          rec.value = `value() threw: ${e.message}`;
        }
      }
    }
    if (o.events) rec.events = o.events;
    this.trace.push(rec);
    if (verbose) console.error(`  ${label}: ${res.ok ? 'ok' : 'ERR ' + res.error.code + ' ' + res.error.message}`);
    return rec;
  }
  /** A browser-level command. */
  async browser(label, method, params, o = {}) {
    const res = await this.cdp.send(method, params);
    this.record(label, method, res, o);
    return res;
  }
  /** A fresh context with one page target, attached with a flat session. */
  async newPage({ init = true, context = true } = {}) {
    let browserContextId;
    if (context) {
      const r = await this.cdp.send('Target.createBrowserContext', {});
      browserContextId = r.result?.browserContextId;
    }
    const t = await this.cdp.send('Target.createTarget', { url: 'about:blank', browserContextId });
    if (!t.ok) throw new Error(`createTarget: ${t.error.message}`);
    const targetId = t.result.targetId;
    const a = await this.cdp.send('Target.attachToTarget', { targetId, flatten: true });
    if (!a.ok) throw new Error(`attachToTarget: ${a.error.message}`);
    const p = new Page(this, targetId, a.result.sessionId, browserContextId);
    this.pages.push(p);
    if (init) {
      for (const [m, params] of [
        ['Page.enable'],
        ['Page.setLifecycleEventsEnabled', { enabled: true }],
        ['Runtime.enable'],
        ['Network.enable'],
      ]) {
        await p.quiet(m, params);
      }
      await sleep(50);
      p.drain();
    }
    return p;
  }
  async closeAll() {
    for (const p of this.pages) {
      await this.cdp.send('Target.closeTarget', { targetId: p.targetId });
      if (p.contextId) await this.cdp.send('Target.disposeBrowserContext', { browserContextId: p.contextId });
    }
    this.pages = [];
  }
}

// `Page.frameScheduledNavigation`, `frameRequestedNavigation` and
// `frameClearedScheduledNavigation` (script-initiated navigations) are not
// compared: LightBrowser does not send them (ADR 0045, open items).
const EV_DEFAULT = /^(Page\.(frame(?!Scheduled|Requested|ClearedScheduled)|navigated|lifecycle|load|domContent)|Runtime\.executionContext(Created|Destroyed|sCleared))/;
// Lifecycle names both browsers report in the same order; Chrome adds paint
// and idle milestones whose timing is not deterministic.
const LIFECYCLE_CORE = new Set(['init', 'commit', 'DOMContentLoaded', 'load']);

class Page {
  constructor(run, targetId, sessionId, contextId) {
    this.run = run;
    this.cdp = run.cdp;
    this.targetId = targetId;
    this.sessionId = sessionId;
    this.contextId = contextId;
    this.mark = this.cdp.events.length;
  }
  drain() {
    this.mark = this.cdp.events.length;
  }
  eventsSince(filter = EV_DEFAULT) {
    return this.cdp.events
      .slice(this.mark)
      .filter((e) => e.sessionId === this.sessionId)
      .filter((e) => filter.test(e.method))
      .filter((e) => e.method !== 'Page.lifecycleEvent' || LIFECYCLE_CORE.has(e.params.name))
      .map((e) => {
        if (e.method === 'Page.lifecycleEvent') return `lifecycle:${e.params.name}`;
        if (e.method === 'Page.frameStartedNavigating' || e.method === 'Page.navigatedWithinDocument') return `${e.method}:${e.params.navigationType}`;
        return e.method;
      });
  }
  /** Wait until `pred(event)` has been seen since the mark. */
  async waitFor(pred, ms = 8000) {
    const end = Date.now() + ms;
    while (Date.now() < end) {
      if (this.cdp.events.slice(this.mark).some((e) => e.sessionId === this.sessionId && pred(e))) return true;
      await sleep(20);
    }
    return false;
  }
  async quiet(method, params) {
    return this.cdp.send(method, params, this.sessionId);
  }
  /**
   * A recorded command. Options: `msg` (compare the error text), `value`
   * (fn(result) -> picked value), `events` (true or a regexp: record the
   * ordered events that follow, after `settle` ms), `settle`.
   */
  async cmd(label, method, params, o = {}) {
    this.drain();
    const res = await this.cdp.send(method, params, this.sessionId);
    let evs;
    if (o.events) {
      if (o.waitLoad) await this.waitFor((e) => e.method === 'Page.loadEventFired');
      await sleep(o.settle ?? 300);
      evs = this.eventsSince(o.events === true ? EV_DEFAULT : o.events);
    }
    this.run.record(label, method, res, { ...o, events: evs });
    return res;
  }
  /** `Page.navigate` and wait for load; records the navigate command. */
  async goto(label, url, extra = {}, o = {}) {
    return this.cmd(label, 'Page.navigate', { url, ...extra }, { events: true, waitLoad: true, ...o });
  }
  /** The id of the current main-world execution context of the page. */
  async mainCtx() {
    const e = this.cdp.events.filter((x) => x.sessionId === this.sessionId && x.method === 'Runtime.executionContextCreated' && x.params.context.auxData?.isDefault && x.params.context.auxData.frameId === this.targetId);
    return e.length ? e[e.length - 1].params.context.id : 0;
  }
  /** Evaluate in the main world, returning the value (not recorded). */
  async val(expression) {
    const r = await this.cdp.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true }, this.sessionId);
    return r.ok ? r.result.result?.value : undefined;
  }
}

// ----------------------------------------------------------- scenarios

const A = (r, p) => `${r.siteA.origin}${p}`;
const B = (r, p) => `${r.siteB.origin}${p}`;
const entriesValue = (res) => ({
  currentIndex: res.currentIndex,
  entries: res.entries.map((e) => ({ url: e.url, userTypedURL: e.userTypedURL, title: e.title, transitionType: e.transitionType })),
});
/** The id of the history entry `delta` from the current one (-1: none). */
const idAt = (h, delta) => h?.entries?.[h.currentIndex + delta]?.id ?? -1;

const scenarios = [];
const scenario = (name, fn) => scenarios.push({ name, fn });

// ---- Puppeteer: browser connect, new page, page setup (cdp/Browser.ts,
// TargetManager.ts, Page.ts, FrameManager.ts, NetworkManager.ts, EmulationManager.ts)
scenario('puppeteer.newPage', async (r) => {
  await r.browser('getVersion', 'Browser.getVersion', undefined, {});
  await r.browser('discover', 'Target.setDiscoverTargets', { discover: true, filter: [{}] });
  await r.browser('autoAttach', 'Target.setAutoAttach', {
    waitForDebuggerOnStart: true,
    flatten: true,
    autoAttach: true,
    filter: [{ type: 'page', exclude: true }, {}],
  });
  const ctx = await r.browser('createContext', 'Target.createBrowserContext', {});
  const browserContextId = ctx.result.browserContextId;
  const t = await r.browser('createTarget', 'Target.createTarget', { url: 'about:blank', browserContextId });
  const a = await r.browser('attach', 'Target.attachToTarget', { targetId: t.result.targetId, flatten: true });
  const p = new Page(r, t.result.targetId, a.result.sessionId, browserContextId);
  r.pages.push(p);
  const tree = await p.cmd('getFrameTree', 'Page.getFrameTree', undefined, {});
  const frameId = tree.result.frameTree.frame.id;
  // FrameManager.initialize sends these together while the target is paused.
  const init = [
    ['Network.enable', undefined],
    ['Network.setCacheDisabled', { cacheDisabled: false }],
    ['Page.enable', undefined],
    ['Page.setLifecycleEventsEnabled', { enabled: true }],
    ['Runtime.enable', undefined],
    ['Page.createIsolatedWorld', { frameId, worldName: '__puppeteer_utility_world__24.0.0', grantUniveralAccess: true }],
    ['Audits.enable', undefined],
    ['Performance.enable', undefined],
    ['Log.enable', undefined],
    ['WebMCP.enable', undefined],
    ['Target.setAutoAttach', { waitForDebuggerOnStart: true, flatten: true, autoAttach: true, filter: [{}] }],
    ['Runtime.runIfWaitingForDebugger', undefined],
  ];
  const results = await Promise.all(init.map(([m, params]) => p.quiet(m, params)));
  init.forEach(([m], i) => r.record(`init ${m}`, m, results[i], { msg: m === 'WebMCP.enable' }));
  // EmulationManager: the default viewport 800x600.
  await p.cmd('setDeviceMetrics', 'Emulation.setDeviceMetricsOverride', {
    mobile: false, width: 800, height: 600, deviceScaleFactor: 1, screenOrientation: { angle: 0, type: 'portraitPrimary' },
  });
  await p.cmd('setTouch', 'Emulation.setTouchEmulationEnabled', { enabled: false });
  await p.cmd('getTargetInfo', 'Target.getTargetInfo', { targetId: p.targetId }, { value: (x) => ({ type: x.targetInfo.type, url: x.targetInfo.url, attached: x.targetInfo.attached }) });
  await p.cmd('getWindowForTarget', 'Browser.getWindowForTarget', undefined);
  await p.cmd('bringToFront', 'Page.bringToFront');
  await r.closeAll();
  await r.browser('closeTargetTwice', 'Target.closeTarget', { targetId: p.targetId }, { msg: true });
});

// ---- Playwright: browser connect, new context and page (crBrowser.ts, crPage.ts)
scenario('playwright.newPage', async (r) => {
  await r.browser('getVersion', 'Browser.getVersion');
  await r.browser('autoAttach', 'Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
  const ctx = await r.browser('createContext', 'Target.createBrowserContext', { disposeOnDetach: true });
  const browserContextId = ctx.result.browserContextId;
  await r.browser('downloads', 'Browser.setDownloadBehavior', { behavior: 'deny', browserContextId });
  const t = await r.browser('createTarget', 'Target.createTarget', { url: 'about:blank', browserContextId });
  const a = await r.browser('attach', 'Target.attachToTarget', { targetId: t.result.targetId, flatten: true });
  const p = new Page(r, t.result.targetId, a.result.sessionId, browserContextId);
  r.pages.push(p);
  const tree = await p.cmd('getFrameTree', 'Page.getFrameTree');
  const frameId = tree.result.frameTree.frame.id;
  const init = [
    ['Browser.getWindowForTarget', undefined],
    ['Page.enable', undefined],
    ['Log.enable', {}],
    ['Page.setLifecycleEventsEnabled', { enabled: true }],
    ['Runtime.enable', {}],
    ['Page.addScriptToEvaluateOnNewDocument', { source: '', worldName: '__playwright_utility_world_page@1' }],
    ['Page.createIsolatedWorld', { frameId, grantUniveralAccess: true, worldName: '__playwright_utility_world_page@1' }],
    ['Network.enable', undefined],
    ['Target.setAutoAttach', { autoAttach: true, waitForDebuggerOnStart: true, flatten: true }],
    ['Emulation.setFocusEmulationEnabled', { enabled: true }],
    ['Emulation.setDeviceMetricsOverride', { mobile: false, width: 1280, height: 720, screenWidth: 1280, screenHeight: 720, deviceScaleFactor: 1, screenOrientation: { angle: 0, type: 'landscapePrimary' } }],
    ['Emulation.setUserAgentOverride', { userAgent: '' }],
    ['Page.setFontFamilies', { fontFamilies: { standard: 'Times New Roman', fixed: 'Courier New', serif: 'Times New Roman', sansSerif: 'Arial', cursive: 'Comic Sans MS', fantasy: 'Impact' } }],
    ['Emulation.setEmulatedMedia', { media: '', colorScheme: 'light', reducedMotion: 'no-preference', forcedColors: 'none', contrast: 'no-preference' }],
    ['Network.setBypassServiceWorker', { bypass: true }],
    ['Page.setBypassCSP', { enabled: true }],
    ['Security.setIgnoreCertificateErrors', { ignore: true }],
    ['Emulation.setTouchEmulationEnabled', { enabled: true }],
    ['Emulation.setScriptExecutionDisabled', { value: false }],
    ['Emulation.setTimezoneOverride', { timezoneId: 'Europe/Berlin' }],
    ['Emulation.setLocaleOverride', { locale: 'de-DE' }],
    ['Network.emulateNetworkConditions', { offline: false, downloadThroughput: -1, uploadThroughput: -1, latency: 0 }],
    ['Network.setCacheDisabled', { cacheDisabled: false }],
    ['Network.setExtraHTTPHeaders', { headers: {} }],
    ['Page.setInterceptFileChooserDialog', { enabled: false }],
    ['Runtime.runIfWaitingForDebugger', undefined],
  ];
  const results = await Promise.all(init.map(([m, params]) => p.quiet(m, params)));
  init.forEach(([m], i) => r.record(`init ${m}`, m, results[i]));
  await p.cmd('getLayoutMetrics', 'Page.getLayoutMetrics');
  await r.closeAll();
});

// ---- goto, referrer and referrerPolicy (Frame.ts: Page.navigate {url, referrer,
// frameId, referrerPolicy}; crPage.ts: {url, referrer, frameId, referrerPolicy: 'unsafeUrl'})
scenario('goto', async (r) => {
  const p = await r.newPage();
  const frameId = p.targetId;
  const seen = (site, path) => (res) => ({ ...pick(site.seen[path] || {}, ['referer', 'sec-fetch-site', 'sec-fetch-mode', 'sec-fetch-dest', 'sec-fetch-user']) });
  const pick = (h, ks) => Object.fromEntries(ks.map((k) => [k, h[k] ?? null]));
  await p.goto('goto a', A(r, '/a'), { frameId }, { value: (x) => ({ hasLoader: typeof x.loaderId === 'string', frame: x.frameId === frameId }) });
  await p.cmd('title', 'Runtime.evaluate', { expression: 'document.title', returnByValue: true }, { value: (x) => x.result.value });

  const cases = [
    ['no referrer', {}],
    ['unsafeUrl cross-site', { referrer: 'https://ref.example.test/page?q=1#frag', referrerPolicy: 'unsafeUrl' }],
    ['unsafeUrl same-origin', { referrer: A(r, '/from?x=1#h'), referrerPolicy: 'unsafeUrl' }],
    ['unsafeUrl same-site', { referrer: B(r, '/from?x=1'), referrerPolicy: 'unsafeUrl' }],
    ['referrer only', { referrer: 'https://ref.example.test/page?q=1' }],
    ['no-referrer', { referrer: 'https://ref.example.test/page?q=1', referrerPolicy: 'noReferrer' }],
    ['origin', { referrer: 'https://ref.example.test/page?q=1', referrerPolicy: 'origin' }],
    ['strict-origin-when-cross-origin', { referrer: 'https://ref.example.test/page?q=1', referrerPolicy: 'strictOriginWhenCrossOrigin' }],
    ['same-origin vs cross-origin', { referrer: 'https://ref.example.test/page?q=1', referrerPolicy: 'sameOrigin' }],
    ['same-origin vs same-origin', { referrer: A(r, '/from?x=1'), referrerPolicy: 'sameOrigin' }],
    ['origin-when-cross-origin', { referrer: A(r, '/from?x=1'), referrerPolicy: 'originWhenCrossOrigin' }],
    ['no-referrer-when-downgrade', { referrer: 'https://ref.example.test/page?q=1', referrerPolicy: 'noReferrerWhenDowngrade' }],
    ['unsafe-url (http referrer)', { referrer: 'http://ref.example.test/page?q=1', referrerPolicy: 'unsafeUrl' }],
    ['credentials in referrer', { referrer: 'https://u:p@ref.example.test/page?q=1', referrerPolicy: 'unsafeUrl' }],
  ];
  for (const [name, extra] of cases) {
    delete r.siteA.seen['/headers'];
    await p.goto(`headers: ${name}`, A(r, '/headers'), { frameId, ...extra }, { value: seen(r.siteA, '/headers') });
    await p.cmd(`document.referrer: ${name}`, 'Runtime.evaluate', { expression: 'document.referrer', returnByValue: true }, { value: (x) => x.result.value });
  }
  // Cross-origin target.
  delete r.siteB.seen['/headers'];
  await p.goto('headers: cross-origin target', B(r, '/headers'), { frameId, referrer: A(r, '/from?x=1'), referrerPolicy: 'unsafeUrl' }, { value: seen(r.siteB, '/headers') });
  delete r.siteB.seen['/headers'];
  await p.goto('headers: cross-origin target, default policy', B(r, '/headers'), { frameId, referrer: A(r, '/from?x=1') }, { value: seen(r.siteB, '/headers') });

  // Parameter errors.
  await p.cmd('invalid url', 'Page.navigate', { url: 'not a url' }, { msg: true, value: (x) => ({ err: x.errorText ?? null }) });
  await p.cmd('missing url', 'Page.navigate', {}, { msg: true });
  await p.cmd('bad frame', 'Page.navigate', { url: A(r, '/a'), frameId: 'nope' }, { msg: true });
  await p.cmd('bad referrerPolicy', 'Page.navigate', { url: A(r, '/a'), referrerPolicy: 'bogus' }, { msg: true });
  await p.cmd('bad referrer type', 'Page.navigate', { url: A(r, '/a'), referrer: 5 }, { msg: true });
  await p.cmd('connection refused', 'Page.navigate', { url: `http://127.0.0.1:${await deadPort()}/` }, { value: (x) => ({ err: x.errorText }) });
  await sleep(500); // Chrome commits an error page; its events end here
  await p.cmd('redirect', 'Page.navigate', { url: A(r, '/redirect') }, { events: true, waitLoad: true });
  await p.cmd('href after redirect', 'Runtime.evaluate', { expression: 'location.href', returnByValue: true }, { value: (x) => x.result.value });
  await p.goto('same-document hash', A(r, '/hashpage'), {});
  await p.cmd('hash navigate', 'Page.navigate', { url: A(r, '/hashpage#target') }, { events: true, settle: 300, value: (x) => ({ hasLoader: x.loaderId !== undefined }) });
  await p.cmd('href after hash', 'Runtime.evaluate', { expression: 'location.href', returnByValue: true }, { value: (x) => x.result.value });
  await r.closeAll();
});

// ---- session history: Page.getNavigationHistory, navigateToHistoryEntry,
// resetNavigationHistory, history.back/forward/go, pushState (Page.ts goBack,
// goForward; crPage.ts _go)
scenario('history', async (r) => {
  const p = await r.newPage();
  const hist = (label, o = {}) => p.cmd(label, 'Page.getNavigationHistory', undefined, { value: entriesValue, ...o });
  const href = (label) => p.cmd(label, 'Runtime.evaluate', { expression: 'location.href + " | " + document.title', returnByValue: true }, { value: (x) => x.result.value });
  const histRaw = async () => (await p.quiet('Page.getNavigationHistory')).result;
  await hist('initial');
  await p.goto('goto a', A(r, '/a'));
  await hist('after a');
  await p.goto('goto b', A(r, '/b'));
  await p.goto('goto c', A(r, '/c'));
  await hist('after c');

  let h = await histRaw();
  await p.cmd('back to b', 'Page.navigateToHistoryEntry', { entryId: idAt(h, -1) }, { events: true, waitLoad: true });
  await href('href after back');
  await hist('history after back');
  h = await histRaw();
  await p.cmd('back to a', 'Page.navigateToHistoryEntry', { entryId: idAt(h, -1) }, { events: true, waitLoad: true });
  await href('href after back 2');
  h = await histRaw();
  await p.cmd('forward to b', 'Page.navigateToHistoryEntry', { entryId: idAt(h, +1) }, { events: true, waitLoad: true });
  await href('href after forward');
  await hist('history after forward');
  await p.cmd('bad entry', 'Page.navigateToHistoryEntry', { entryId: 987654 }, { msg: true });
  await p.cmd('missing entryId', 'Page.navigateToHistoryEntry', {}, { msg: true });
  await p.cmd('entryId string', 'Page.navigateToHistoryEntry', { entryId: 'x' }, { msg: true });

  // A new navigation after going back drops the forward entries.
  h = await histRaw();
  await p.cmd('back to a again', 'Page.navigateToHistoryEntry', { entryId: idAt(h, -1) }, { events: true, waitLoad: true });
  await p.goto('goto c from a', A(r, '/c'));
  await hist('forward entries dropped');

  // history.back / forward / go from script.
  // (From a timer: a navigation during Runtime.evaluate fails the evaluate.)
  const later = (js) => ({ expression: `setTimeout(() => { ${js} }, 0); 0`, returnByValue: true });
  await p.cmd('history.back()', 'Runtime.evaluate', later('history.back()'), { events: true, waitLoad: true, settle: 400 });
  await href('href after history.back');
  await p.cmd('history.forward()', 'Runtime.evaluate', later('history.forward()'), { events: true, waitLoad: true, settle: 400 });
  await href('href after history.forward');
  await p.cmd('history.go(-2)', 'Runtime.evaluate', later('history.go(-2)'), { events: true, waitLoad: true, settle: 400 });
  await href('href after history.go(-2)');
  await hist('history after go(-2)');
  await p.cmd('history.go(2)', 'Runtime.evaluate', later('history.go(2)'), { events: true, waitLoad: true, settle: 400 });
  await href('href after history.go(2)');
  await p.cmd('history.go(0) reloads', 'Runtime.evaluate', later('history.go(0)'), { events: true, waitLoad: true, settle: 400 });
  await p.cmd('history.go(50) does nothing', 'Runtime.evaluate', later('history.go(50)'), { events: true, settle: 400 });
  await hist('history after go(50)');

  // pushState and replaceState entries are session history entries.
  await p.goto('goto hashpage', A(r, '/hashpage'));
  await p.cmd('pushState', 'Runtime.evaluate', { expression: "history.pushState({n:1}, 'T1', '/pushed1'); document.title = 'T1'; history.length", returnByValue: true }, { events: true, settle: 300, value: (x) => x.result.value });
  await p.cmd('pushState 2', 'Runtime.evaluate', { expression: "history.pushState({n:2}, '', '/pushed2'); history.length", returnByValue: true }, { events: true, settle: 300, value: (x) => x.result.value });
  await p.cmd('replaceState', 'Runtime.evaluate', { expression: "history.replaceState({n:22}, '', '/replaced2'); history.length", returnByValue: true }, { events: true, settle: 300, value: (x) => x.result.value });
  await hist('history after pushState');
  await p.cmd('history.back() same document', 'Runtime.evaluate', {
    expression: "new Promise(r => { addEventListener('popstate', e => r(JSON.stringify(e.state) + ' ' + location.pathname), {once:true}); history.back(); })",
    returnByValue: true, awaitPromise: true,
  }, { events: true, settle: 300, value: (x) => x.result.value });
  await hist('history after same-document back');
  h = await histRaw();
  await p.cmd('navigateToHistoryEntry same document', 'Page.navigateToHistoryEntry', { entryId: idAt(h, -1) }, { events: true, settle: 400 });
  await p.cmd('state after same-document entry', 'Runtime.evaluate', { expression: 'JSON.stringify(history.state) + " " + location.pathname', returnByValue: true }, { value: (x) => x.result.value });
  h = await histRaw();
  await p.cmd('navigateToHistoryEntry same document forward', 'Page.navigateToHistoryEntry', { entryId: idAt(h, +1) }, { events: true, settle: 400 });
  await p.cmd('state after same-document forward', 'Runtime.evaluate', { expression: 'JSON.stringify(history.state) + " " + location.pathname', returnByValue: true }, { value: (x) => x.result.value });
  // A cross-document back that lands on a pushState entry restores its state.
  await p.goto('goto b after pushState', A(r, '/b'));
  h = await histRaw();
  await p.cmd('back across documents to a pushState entry', 'Page.navigateToHistoryEntry', { entryId: idAt(h, -1) }, { events: true, waitLoad: true, settle: 400 });
  await p.cmd('state after cross-document back', 'Runtime.evaluate', { expression: 'JSON.stringify(history.state) + " " + location.pathname + " " + document.title', returnByValue: true }, { value: (x) => x.result.value });

  // Hash navigation.
  await p.goto('goto hashpage again', A(r, '/hashpage'));
  await p.cmd('location.hash', 'Runtime.evaluate', { expression: "location.hash = '#target'; location.href", returnByValue: true }, { events: true, settle: 300, value: (x) => x.result.value });
  await hist('history after hash');
  await p.cmd('history.back() over hash', 'Runtime.evaluate', {
    expression: "new Promise(r => { addEventListener('hashchange', e => r(e.oldURL + ' -> ' + e.newURL), {once:true}); history.back(); })",
    returnByValue: true, awaitPromise: true,
  }, { events: true, settle: 300, value: (x) => x.result.value });

  await p.cmd('reload', 'Page.reload', {}, { events: true, waitLoad: true });
  await hist('history after reload');
  await p.cmd('reload ignoreCache', 'Page.reload', { ignoreCache: true }, { events: true, waitLoad: true });
  await p.cmd('reset', 'Page.resetNavigationHistory');
  await hist('history after reset');
  await p.cmd('history.length after reset', 'Runtime.evaluate', { expression: 'history.length', returnByValue: true }, { value: (x) => x.result.value });
  await r.closeAll();
});

// ---- viewport and device metrics (EmulationManager.ts, crPage.ts)
scenario('emulation', async (r) => {
  const p = await r.newPage();
  await p.goto('goto a', A(r, '/a'));
  const inner = () => ({ value: (x) => x.result.value });
  const dims = 'JSON.stringify([innerWidth, innerHeight, outerWidth, outerHeight, devicePixelRatio, screen.width, screen.height, matchMedia("(orientation: landscape)").matches])';
  await p.cmd('metrics 800x600', 'Emulation.setDeviceMetricsOverride', { mobile: false, width: 800, height: 600, deviceScaleFactor: 1, screenOrientation: { angle: 0, type: 'portraitPrimary' } });
  await p.cmd('dims', 'Runtime.evaluate', { expression: dims, returnByValue: true }, inner());
  await p.cmd('metrics 1024x768 dsf2', 'Emulation.setDeviceMetricsOverride', { mobile: false, width: 1024, height: 768, deviceScaleFactor: 2, screenWidth: 1024, screenHeight: 768, screenOrientation: { angle: 0, type: 'landscapePrimary' } });
  await p.cmd('dims 2', 'Runtime.evaluate', { expression: dims, returnByValue: true }, inner());
  await p.cmd('getLayoutMetrics', 'Page.getLayoutMetrics');
  await p.cmd('metrics missing width', 'Emulation.setDeviceMetricsOverride', { mobile: false, height: 600, deviceScaleFactor: 1 }, { msg: true });
  await p.cmd('metrics negative width', 'Emulation.setDeviceMetricsOverride', { mobile: false, width: -1, height: 600, deviceScaleFactor: 1 }, { msg: true });
  await p.cmd('metrics too large', 'Emulation.setDeviceMetricsOverride', { mobile: false, width: 20000000, height: 600, deviceScaleFactor: 1 }, { msg: true });
  await p.cmd('metrics missing mobile', 'Emulation.setDeviceMetricsOverride', { width: 800, height: 600, deviceScaleFactor: 1 }, { msg: true });
  await p.cmd('clear metrics', 'Emulation.clearDeviceMetricsOverride');
  await p.cmd('touch on', 'Emulation.setTouchEmulationEnabled', { enabled: true });
  await p.cmd('touch off', 'Emulation.setTouchEmulationEnabled', { enabled: false });
  await p.cmd('touch missing enabled', 'Emulation.setTouchEmulationEnabled', {}, { msg: true });
  await p.cmd('focus on', 'Emulation.setFocusEmulationEnabled', { enabled: true });
  await p.cmd('focus missing', 'Emulation.setFocusEmulationEnabled', {}, { msg: true });
  await p.cmd('media print', 'Emulation.setEmulatedMedia', { media: 'print' });
  await p.cmd('media state', 'Runtime.evaluate', { expression: 'matchMedia("print").matches', returnByValue: true }, inner());
  await p.cmd('media features', 'Emulation.setEmulatedMedia', { media: '', features: [{ name: 'prefers-color-scheme', value: 'dark' }] });
  await p.cmd('media scheme', 'Runtime.evaluate', { expression: 'matchMedia("(prefers-color-scheme: dark)").matches', returnByValue: true }, inner());
  await p.cmd('media reset', 'Emulation.setEmulatedMedia', { media: '', features: [] });
  await p.cmd('background transparent', 'Emulation.setDefaultBackgroundColorOverride', { color: { r: 0, g: 0, b: 0, a: 0 } });
  await p.cmd('background reset', 'Emulation.setDefaultBackgroundColorOverride', {});
  await p.cmd('geolocation', 'Emulation.setGeolocationOverride', { latitude: 52.5, longitude: 13.4, accuracy: 10 });
  await p.cmd('geolocation clear', 'Emulation.setGeolocationOverride', {});
  await p.cmd('timezone', 'Emulation.setTimezoneOverride', { timezoneId: 'Europe/Berlin' });
  await p.cmd('timezone invalid', 'Emulation.setTimezoneOverride', { timezoneId: 'Not/AZone' }, { msg: true });
  await p.cmd('timezone reset', 'Emulation.setTimezoneOverride', { timezoneId: '' });
  await p.cmd('locale', 'Emulation.setLocaleOverride', { locale: 'de-DE' });
  await p.cmd('locale invalid', 'Emulation.setLocaleOverride', { locale: 'not a locale!' }, { msg: true });
  await p.cmd('locale reset', 'Emulation.setLocaleOverride', {});
  // The methods stock clients use that LightBrowser lacked.
  await p.cmd('script execution off', 'Emulation.setScriptExecutionDisabled', { value: true });
  await p.cmd('script blocked', 'Runtime.evaluate', { expression: '1+1', returnByValue: true }, { value: (x) => x.result?.value ?? null });
  await p.cmd('script execution missing value', 'Emulation.setScriptExecutionDisabled', {}, { msg: true });
  await p.cmd('script execution on', 'Emulation.setScriptExecutionDisabled', { value: false });
  await p.cmd('cpu throttling', 'Emulation.setCPUThrottlingRate', { rate: 1 });
  await p.cmd('cpu throttling invalid', 'Emulation.setCPUThrottlingRate', { rate: 0 }, { msg: true });
  await p.cmd('vision deficiency', 'Emulation.setEmulatedVisionDeficiency', { type: 'none' });
  await p.cmd('idle override clear', 'Emulation.clearIdleOverride');
  await p.cmd('geolocation clear 2', 'Emulation.clearGeolocationOverride');
  await r.closeAll();
});

// ---- screenshot and layout (Page.ts _screenshot, crPage.ts takeScreenshot)
scenario('screenshot', async (r) => {
  const p = await r.newPage();
  await p.goto('goto a', A(r, '/a'));
  await p.cmd('metrics', 'Emulation.setDeviceMetricsOverride', { mobile: false, width: 800, height: 600, deviceScaleFactor: 1, screenOrientation: { angle: 0, type: 'portraitPrimary' } });
  await p.cmd('layout', 'Page.getLayoutMetrics');
  const png = (x) => ({ magic: Buffer.from(x.data, 'base64').subarray(1, 4).toString(), w: Buffer.from(x.data, 'base64').readUInt32BE(16), h: Buffer.from(x.data, 'base64').readUInt32BE(20) });
  await p.cmd('viewport png', 'Page.captureScreenshot', { format: 'png', captureBeyondViewport: false }, { value: png });
  await p.cmd('puppeteer default', 'Page.captureScreenshot', { format: 'png', optimizeForSpeed: false, fromSurface: true, captureBeyondViewport: true }, { value: png });
  await p.cmd('clip', 'Page.captureScreenshot', { format: 'png', clip: { x: 0, y: 0, width: 100, height: 50, scale: 1 }, captureBeyondViewport: true }, { value: png });
  await p.cmd('jpeg', 'Page.captureScreenshot', { format: 'jpeg', quality: 80 }, { value: (x) => ({ magic: Buffer.from(x.data, 'base64').subarray(0, 2).toString('hex') }) });
  await p.cmd('webp', 'Page.captureScreenshot', { format: 'webp', quality: 80 }, { value: (x) => ({ magic: Buffer.from(x.data, 'base64').subarray(8, 12).toString() }) });
  await p.cmd('transparent', 'Emulation.setDefaultBackgroundColorOverride', { color: { r: 0, g: 0, b: 0, a: 0 } });
  await p.cmd('transparent png', 'Page.captureScreenshot', { format: 'png' }, { value: png });
  await p.cmd('transparent reset', 'Emulation.setDefaultBackgroundColorOverride');
  await p.cmd('bad format', 'Page.captureScreenshot', { format: 'gif' }, { msg: true });
  await p.cmd('bad clip', 'Page.captureScreenshot', { clip: { x: 0, y: 0, width: 0, height: 0, scale: 1 } }, { msg: true });
  await p.cmd('pdf', 'Page.printToPDF', {}, { value: (x) => ({ magic: Buffer.from(x.data, 'base64').subarray(0, 4).toString() }) });
  await r.closeAll();
});

// ---- evaluate, callFunctionOn, waitForFunction polling (ExecutionContext.ts,
// crExecutionContext.ts, IsolatedWorld.ts)
scenario('evaluate', async (r) => {
  const p = await r.newPage();
  await p.goto('goto a', A(r, '/a'));
  const v = { value: (x) => x.result ?? x };
  await p.cmd('evaluate by value', 'Runtime.evaluate', { expression: '1 + 1', returnByValue: true, awaitPromise: true, userGesture: true }, v);
  await p.cmd('evaluate object', 'Runtime.evaluate', { expression: '({a:1})', awaitPromise: true, userGesture: true });
  await p.cmd('evaluate promise', 'Runtime.evaluate', { expression: 'Promise.resolve(5)', returnByValue: true, awaitPromise: true }, v);
  await p.cmd('evaluate rejected', 'Runtime.evaluate', { expression: 'Promise.reject(new Error("boom"))', returnByValue: true, awaitPromise: true }, { value: (x) => x.exceptionDetails && { text: x.exceptionDetails.text, desc: x.exceptionDetails.exception?.description?.split('\n')[0] } });
  await p.cmd('evaluate throws', 'Runtime.evaluate', { expression: 'throw new TypeError("bad")', returnByValue: true }, { value: (x) => x.exceptionDetails && { text: x.exceptionDetails.text, desc: x.exceptionDetails.exception?.description?.split('\n')[0] } });
  await p.cmd('evaluate syntax error', 'Runtime.evaluate', { expression: 'let let', returnByValue: true }, { value: (x) => x.exceptionDetails && { text: x.exceptionDetails.text, desc: x.exceptionDetails.exception?.description?.split('\n')[0] } });
  await p.cmd('evaluate bad context', 'Runtime.evaluate', { expression: '1', contextId: 987654, returnByValue: true }, { msg: true });
  await p.cmd('evaluate missing expression', 'Runtime.evaluate', {}, { msg: true });
  await p.cmd('evaluate undefined', 'Runtime.evaluate', { expression: 'undefined', returnByValue: true }, { value: (x) => x.result });
  await p.cmd('evaluate bigint', 'Runtime.evaluate', { expression: '10n', returnByValue: false }, { value: (x) => x.result });
  await p.cmd('evaluate NaN', 'Runtime.evaluate', { expression: 'NaN', returnByValue: false }, { value: (x) => x.result });
  await p.cmd('evaluate function', 'Runtime.evaluate', { expression: '(function(){})', returnByValue: false }, { value: (x) => ({ type: x.result.type, className: x.result.className, desc: x.result.description }) });
  const obj = await p.cmd('evaluate window', 'Runtime.evaluate', { expression: 'document.body' });
  const objectId = obj.result.result.objectId;
  await p.cmd('callFunctionOn', 'Runtime.callFunctionOn', { functionDeclaration: 'function(a){ return this.tagName + a }', objectId, arguments: [{ value: '!' }], returnByValue: true, awaitPromise: true, userGesture: true }, v);
  await p.cmd('callFunctionOn executionContextId', 'Runtime.callFunctionOn', { functionDeclaration: '() => 7', executionContextId: await p.mainCtx(), returnByValue: true });
  await p.cmd('callFunctionOn bad object', 'Runtime.callFunctionOn', { functionDeclaration: '() => 7', objectId: 'nope' }, { msg: true });
  await p.cmd('getProperties', 'Runtime.getProperties', { objectId, ownProperties: true });
  await p.cmd('releaseObjectGroup', 'Runtime.releaseObjectGroup', { objectGroup: 'x' });
  await p.cmd('addBinding', 'Runtime.addBinding', { name: 'cdpRawBinding' });
  await p.cmd('removeBinding', 'Runtime.removeBinding', { name: 'cdpRawBinding' });
  await p.cmd('heap usage', 'Runtime.getHeapUsage');
  await p.cmd('queryObjects missing', 'Runtime.queryObjects', {}, { msg: true });
  // waitForFunction: poll from the client side like Puppeteer's raf polling.
  await p.cmd('set flag', 'Runtime.evaluate', { expression: 'setTimeout(() => window.flag = 42, 100); 0', returnByValue: true });
  await sleep(250);
  await p.cmd('poll flag', 'Runtime.evaluate', { expression: '(async () => { while(!window.flag) await new Promise(r => requestAnimationFrame(r)); return window.flag })()', returnByValue: true, awaitPromise: true }, v);
  await p.cmd('DOM.describeNode', 'DOM.describeNode', { objectId }, { value: (x) => ({ node: x.node.nodeName }) });
  await p.cmd('DOM.resolveNode', 'DOM.resolveNode', { backendNodeId: (await p.quiet('DOM.describeNode', { objectId })).result.node.backendNodeId, executionContextId: await p.mainCtx() });
  await p.cmd('releaseObject', 'Runtime.releaseObject', { objectId });
  await p.cmd('Accessibility', 'Accessibility.getFullAXTree');
  await r.closeAll();
});

// ---- frames: DOM.getFrameOwner (Frame.ts frameElement)
scenario('frames', async (r) => {
  const p = await r.newPage();
  // (A child frame's events differ in detail from Chrome's; not compared.)
  await p.goto('goto iframe page', A(r, '/iframe'), {}, { events: false });
  const tree = await p.cmd('getFrameTree', 'Page.getFrameTree', undefined, { value: (x) => ({ children: (x.frameTree.childFrames || []).length }) });
  const child = tree.result.frameTree.childFrames?.[0]?.frame.id;
  await p.cmd('getFrameOwner child', 'DOM.getFrameOwner', { frameId: child }, { value: (x) => ({ backend: typeof x.backendNodeId }) });
  await p.cmd('getFrameOwner main', 'DOM.getFrameOwner', { frameId: p.targetId }, { msg: true });
  await p.cmd('getFrameOwner unknown', 'DOM.getFrameOwner', { frameId: 'nope' }, { msg: true });
  await p.cmd('getFrameOwner missing', 'DOM.getFrameOwner', {}, { msg: true });
  await p.cmd('DOM.enable', 'DOM.enable');
  await p.cmd('getDocument', 'DOM.getDocument', { depth: 0 });
  await p.cmd('createIsolatedWorld child', 'Page.createIsolatedWorld', { frameId: child, worldName: 'w', grantUniveralAccess: true });
  await p.cmd('createIsolatedWorld unknown', 'Page.createIsolatedWorld', { frameId: 'nope', worldName: 'w' }, { msg: true });
  await r.closeAll();
});

// ---- network: interception, headers, cache, blocked URLs (NetworkManager.ts,
// crNetworkManager.ts)
scenario('network', async (r) => {
  const p = await r.newPage();
  await p.cmd('Network.enable', 'Network.enable');
  await p.cmd('setCacheDisabled', 'Network.setCacheDisabled', { cacheDisabled: true });
  await p.cmd('setCacheDisabled missing', 'Network.setCacheDisabled', {}, { msg: true });
  await p.cmd('setExtraHTTPHeaders', 'Network.setExtraHTTPHeaders', { headers: { 'x-cdp-raw': '1' } });
  await p.cmd('setExtraHTTPHeaders bad', 'Network.setExtraHTTPHeaders', { headers: 5 }, { msg: true });
  delete r.siteA.seen['/headers'];
  await p.goto('goto with extra header', A(r, '/headers'));
  await p.cmd('extra header seen', 'Runtime.evaluate', { expression: '1', returnByValue: true }, { value: () => ({ header: r.siteA.seen['/headers']?.['x-cdp-raw'] ?? null }) });
  await p.cmd('clear extra headers', 'Network.setExtraHTTPHeaders', { headers: {} });
  await p.cmd('setUserAgentOverride', 'Network.setUserAgentOverride', { userAgent: '', acceptLanguage: 'en-US' }, { msg: true });
  await p.cmd('setUserAgentOverride missing', 'Network.setUserAgentOverride', {}, { msg: true });
  await p.cmd('setBypassServiceWorker', 'Network.setBypassServiceWorker', { bypass: true });
  await p.cmd('setBypassServiceWorker missing', 'Network.setBypassServiceWorker', {}, { msg: true });
  await p.cmd('emulateNetworkConditions', 'Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  await p.cmd('emulateNetworkConditions slow', 'Network.emulateNetworkConditions', { offline: false, latency: 20, downloadThroughput: 1000000, uploadThroughput: 1000000, connectionType: 'cellular3g' });
  await p.cmd('emulateNetworkConditions missing', 'Network.emulateNetworkConditions', {}, { msg: true });
  await p.cmd('emulateNetworkConditions bad type', 'Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1, connectionType: 'bogus' }, { msg: true });
  await p.cmd('emulateNetworkConditions offline', 'Network.emulateNetworkConditions', { offline: true, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  await p.cmd('offline navigate', 'Page.navigate', { url: A(r, '/a') }, { value: (x) => ({ err: x.errorText ?? null }) });
  await p.cmd('online', 'Network.emulateNetworkConditions', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  await p.cmd('overrideNetworkState', 'Network.overrideNetworkState', { offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
  await p.cmd('emulateNetworkConditionsByRule', 'Network.emulateNetworkConditionsByRule', { matchedNetworkConditions: [{ urlPattern: '', offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1 }] });
  await p.cmd('setBlockedURLs', 'Network.setBlockedURLs', { urlPatterns: [{ urlPattern: A(r, '/img.png'), block: true }] });
  await p.cmd('setBlockedURLs missing', 'Network.setBlockedURLs', {}, { msg: true });
  await p.cmd('setBlockedURLs old form', 'Network.setBlockedURLs', { urls: [A(r, '/img.png')] });
  await p.goto('goto a (blocked image test)', A(r, '/a'));
  const imgState = "new Promise(res => { const i = new Image(); i.onload = () => res('loaded'); i.onerror = () => res('error'); i.src = '/img.png?x=' + Math.random(); })";
  await p.cmd('blocked image', 'Runtime.evaluate', { expression: imgState, returnByValue: true, awaitPromise: true }, { value: (x) => x.result.value });
  await p.cmd('unblock', 'Network.setBlockedURLs', { urlPatterns: [] });
  await p.cmd('unblocked image', 'Runtime.evaluate', { expression: imgState, returnByValue: true, awaitPromise: true }, { value: (x) => x.result.value });
  await p.cmd('blocked fetch', 'Network.setBlockedURLs', { urlPatterns: [{ urlPattern: A(r, '/json'), block: true }] });
  await p.cmd('blocked fetch result', 'Runtime.evaluate', { expression: "fetch('/json').then(r => 'ok ' + r.status, e => 'error ' + e.message)", returnByValue: true, awaitPromise: true }, { value: (x) => x.result.value });
  await p.cmd('unblock 2', 'Network.setBlockedURLs', { urlPatterns: [] });
  await p.cmd('clearBrowserCache', 'Network.clearBrowserCache');
  await p.cmd('Network.disable', 'Network.disable');
  await r.closeAll();
});

scenario('fetch', async (r) => {
  const p = await r.newPage();
  await p.goto('goto a', A(r, '/a'));
  await p.cmd('Fetch.enable', 'Fetch.enable', { handleAuthRequests: true, patterns: [{ urlPattern: '*' }] });
  await p.cmd('Fetch.enable bad pattern', 'Fetch.enable', { patterns: [{ requestStage: 'Bogus' }] }, { msg: true });
  p.drain();
  const run = (expr) => p.quiet('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
  const waitPaused = async (n) => {
    await p.waitFor(() => p.cdp.events.slice(p.mark).filter((e) => e.sessionId === p.sessionId && e.method === 'Fetch.requestPaused').length >= n);
    return p.cdp.events.slice(p.mark).filter((e) => e.sessionId === p.sessionId && e.method === 'Fetch.requestPaused');
  };
  // continue
  p.drain();
  const f1 = run("fetch('/json').then(r => r.text())");
  let paused = await waitPaused(1);
  r.record('requestPaused event', 'Fetch.requestPaused', { ok: true, result: paused[0].params }, {});
  await p.cmd('continueRequest', 'Fetch.continueRequest', { requestId: paused[0].params.requestId });
  r.record('continued fetch', 'Runtime.evaluate', await f1, { value: (x) => x.result.value });
  // fulfill
  p.drain();
  const f2 = run("fetch('/json').then(r => r.status + ' ' + r.headers.get('x-y') + ' ' + r.statusText).then(t => t)");
  paused = await waitPaused(1);
  await p.cmd('fulfillRequest', 'Fetch.fulfillRequest', { requestId: paused[0].params.requestId, responseCode: 201, responsePhrase: 'Made', responseHeaders: [{ name: 'x-y', value: 'z' }, { name: 'content-type', value: 'text/plain' }], body: Buffer.from('hi').toString('base64') });
  r.record('fulfilled fetch', 'Runtime.evaluate', await f2, { value: (x) => x.result.value });
  // fail
  p.drain();
  const f3 = run("fetch('/json').then(r => 'ok', e => 'error ' + e.message)");
  paused = await waitPaused(1);
  await p.cmd('failRequest', 'Fetch.failRequest', { requestId: paused[0].params.requestId, errorReason: 'Failed' });
  r.record('failed fetch', 'Runtime.evaluate', await f3, { value: (x) => x.result.value });
  await p.cmd('failRequest unknown id', 'Fetch.failRequest', { requestId: 'nope', errorReason: 'Failed' }, { msg: true });
  await p.cmd('failRequest bad reason', 'Fetch.failRequest', { requestId: 'nope', errorReason: 'Bogus' }, { msg: true });
  await p.cmd('continueRequest unknown id', 'Fetch.continueRequest', { requestId: 'nope' }, { msg: true });
  await p.cmd('fulfillRequest unknown id', 'Fetch.fulfillRequest', { requestId: 'nope', responseCode: 200 }, { msg: true });
  await p.cmd('Fetch.disable', 'Fetch.disable');
  await r.closeAll();
});

// ---- cookies (Page.ts cookies/setCookie/deleteCookie, BrowserContext.ts, crBrowser.ts)
scenario('cookies', async (r) => {
  const p = await r.newPage();
  await p.goto('goto cookie', A(r, '/cookie'));
  const list = (x) => x.cookies.map((c) => ({ name: c.name, value: c.value, domain: c.domain, path: c.path, httpOnly: c.httpOnly, secure: c.secure, session: c.session, sameSite: c.sameSite ?? null })).sort((a, b) => a.name.localeCompare(b.name));
  await p.cmd('Network.getCookies', 'Network.getCookies', { urls: [A(r, '/cookie')] }, { value: list });
  await p.cmd('Network.getAllCookies', 'Network.getAllCookies', undefined, { value: list });
  await p.cmd('Network.setCookies', 'Network.setCookies', { cookies: [{ name: 'c1', value: 'v1', url: A(r, '/') }, { name: 'c2', value: 'v2', url: A(r, '/'), httpOnly: true, sameSite: 'Lax', expires: 4102444800 }] });
  await p.cmd('getCookies after', 'Network.getCookies', { urls: [A(r, '/')] }, { value: list });
  await p.cmd('document.cookie', 'Runtime.evaluate', { expression: 'document.cookie', returnByValue: true }, { value: (x) => x.result.value.split('; ').sort().join('; ') });
  await p.cmd('Network.setCookie', 'Network.setCookie', { name: 'c3', value: 'v3', url: A(r, '/') }, { value: (x) => x });
  await p.cmd('Network.setCookie invalid', 'Network.setCookie', { name: 'c4', value: 'v4' }, { msg: true, value: (x) => x });
  await p.cmd('Network.setCookies invalid', 'Network.setCookies', { cookies: [{ name: 'c4', value: 'v4' }] }, { msg: true });
  await p.cmd('Network.deleteCookies', 'Network.deleteCookies', { name: 'c1', url: A(r, '/') });
  await p.cmd('Network.deleteCookies missing name', 'Network.deleteCookies', { url: A(r, '/') }, { msg: true });
  await p.cmd('getCookies after delete', 'Network.getCookies', { urls: [A(r, '/')] }, { value: list });
  // Storage.* with a browserContextId is a browser-target command.
  await r.browser('Storage.getCookies', 'Storage.getCookies', { browserContextId: p.contextId }, { value: list });
  await r.browser('Storage.setCookies', 'Storage.setCookies', { browserContextId: p.contextId, cookies: [{ name: 's1', value: 'x', domain: '127.0.0.1', path: '/' }] });
  await r.browser('Storage.getCookies after', 'Storage.getCookies', { browserContextId: p.contextId }, { value: list });
  await r.browser('Storage.clearCookies', 'Storage.clearCookies', { browserContextId: p.contextId });
  await r.browser('Storage.getCookies cleared', 'Storage.getCookies', { browserContextId: p.contextId }, { value: list });
  await p.cmd('Storage.getCookies on a page session', 'Storage.getCookies', { browserContextId: p.contextId }, { msg: true });
  await p.cmd('Network.clearBrowserCookies', 'Network.clearBrowserCookies');
  await r.closeAll();
});

// ---- permissions, downloads, window (BrowserContext.ts, crBrowser.ts, Page.ts)
scenario('browser', async (r) => {
  const p = await r.newPage();
  await p.goto('goto a', A(r, '/a'));
  await r.browser('grantPermissions', 'Browser.grantPermissions', { origin: r.siteA.origin, browserContextId: p.contextId, permissions: ['geolocation'] });
  await r.browser('grantPermissions bad', 'Browser.grantPermissions', { origin: r.siteA.origin, browserContextId: p.contextId, permissions: ['bogus'] }, { msg: true });
  await r.browser('resetPermissions', 'Browser.resetPermissions', { browserContextId: p.contextId });
  await r.browser('setPermission', 'Browser.setPermission', { origin: r.siteA.origin, browserContextId: p.contextId, permission: { name: 'geolocation' }, setting: 'granted' });
  await r.browser('setDownloadBehavior', 'Browser.setDownloadBehavior', { behavior: 'deny', browserContextId: p.contextId });
  await r.browser('getBrowserContexts', 'Target.getBrowserContexts');
  await r.browser('getTargets', 'Target.getTargets');
  await r.browser('getWindowBounds', 'Browser.getWindowBounds', { windowId: 1 }, {});
  await p.cmd('getWindowForTarget', 'Browser.getWindowForTarget');
  await p.cmd('performance metrics', 'Performance.getMetrics', undefined, { value: (x) => ({ n: x.metrics.length > 0 }) });
  await p.cmd('Page.setBypassCSP', 'Page.setBypassCSP', { enabled: true });
  await p.cmd('Page.setDocumentContent', 'Page.setDocumentContent', { frameId: p.targetId, html: '<title>set</title><b id="s">set</b>' });
  await p.cmd('content after setDocumentContent', 'Runtime.evaluate', { expression: 'document.title + document.getElementById("s").textContent', returnByValue: true }, { value: (x) => x.result.value });
  await p.cmd('Page.stopLoading', 'Page.stopLoading');
  await p.cmd('Page.getResourceTree', 'Page.getResourceTree');
  await p.cmd('Page.setInterceptFileChooserDialog', 'Page.setInterceptFileChooserDialog', { enabled: true });
  await p.cmd('Page.setInterceptFileChooserDialog off', 'Page.setInterceptFileChooserDialog', { enabled: false });
  await p.cmd('Security.setIgnoreCertificateErrors', 'Security.setIgnoreCertificateErrors', { ignore: true });
  await p.cmd('Audits.enable', 'Audits.enable');
  await p.cmd('Audits.disable', 'Audits.disable');
  await p.cmd('Audits.enable twice', 'Audits.enable');
  await p.cmd('Audits.checkContrast', 'Audits.checkContrast', {}, { msg: true });
  await r.closeAll();
});

// ---- closing (Page.ts close, Browser.ts)
scenario('close', async (r) => {
  const p = await r.newPage();
  await p.goto('goto a', A(r, '/a'));
  await p.cmd('Page.close', 'Page.close');
  await sleep(300);
  await r.browser('getTargets after Page.close', 'Target.getTargets', undefined, { value: (x) => ({ n: x.targetInfos.filter((t) => t.targetId === p.targetId).length }) });
  const q = await r.newPage();
  await r.browser('Target.closeTarget', 'Target.closeTarget', { targetId: q.targetId }, { value: (x) => x });
  await r.browser('Target.closeTarget again', 'Target.closeTarget', { targetId: q.targetId }, { msg: true });
  await r.browser('Target.closeTarget missing', 'Target.closeTarget', {}, { msg: true });
  await r.browser('dispose context', 'Target.disposeBrowserContext', { browserContextId: q.contextId });
  await r.browser('dispose context again', 'Target.disposeBrowserContext', { browserContextId: q.contextId }, { msg: true });
  await r.browser('attach to closed', 'Target.attachToTarget', { targetId: q.targetId, flatten: true }, { msg: true });
  r.pages = [];
});

// ------------------------------------------------------------------ driver

function launch(kind, port) {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'cdp-raw-'));
  let cmd, args;
  if (kind === 'chrome') {
    cmd =
      opt('bin', null) ||
      process.env.CHROME_BIN ||
      path.join(os.homedir(), '.cache/ms-playwright/chromium_headless_shell-1243/chrome-headless-shell-linux64/chrome-headless-shell');
    args = ['--no-sandbox', `--remote-debugging-port=${port}`, `--user-data-dir=${dir}`, '--no-first-run', '--disable-background-networking', '--no-proxy-server', 'about:blank'];
  } else {
    cmd = opt('bin', null) || process.env.LIGHTBROWSER_BIN || path.join(repo, 'target/debug/lightbrowser');
    args = ['serve', '--port', String(port)];
  }
  const proc = spawn(cmd, args, { stdio: ['ignore', 'ignore', 'pipe'] });
  let log = '';
  const ws = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`no DevTools line from ${cmd}:\n${log}`)), 20000);
    proc.stderr.on('data', (c) => {
      log += c;
      const m = log.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) {
        clearTimeout(timer);
        resolve(m[1]);
      }
    });
    proc.on('exit', (code) => reject(new Error(`${cmd} exited with ${code}:\n${log}`)));
  });
  return {
    ws,
    stop() {
      proc.removeAllListeners('exit');
      proc.kill('SIGKILL');
      try {
        rmSync(dir, { recursive: true, force: true });
      } catch {}
    },
  };
}

async function runAll() {
  const kind = opt('launch', null);
  const siteA = await startSite('A');
  const siteB = await startSite('B');
  let proc = null;
  let wsUrl = opt('ws', null);
  if (kind) {
    proc = launch(kind, Number(opt('port', 19260)));
    for (const sig of ['SIGTERM', 'SIGINT']) {
      process.on(sig, () => {
        proc.stop();
        process.exit(130);
      });
    }
    wsUrl = await proc.ws;
  }
  if (!wsUrl) throw new Error('give --launch chrome|lightbrowser or --ws URL');
  const only = opt('only', null);
  const wanted = typeof only === 'string' ? only.split(',') : null;
  const out = { browser: null, scenarios: {} };
  let cdp = null;
  try {
    {
      const probe = new Cdp(wsUrl);
      await probe.open();
      const v = await probe.send('Browser.getVersion');
      out.browser = v.ok ? v.result.product : 'unknown';
      probe.close();
    }
    for (const s of scenarios) {
      if (wanted && !wanted.includes(s.name)) continue;
      const trace = [];
      // A connection per scenario: Target.setAutoAttach and the like belong
      // to the connection, and one scenario's must not leak into the next.
      cdp = new Cdp(wsUrl);
      await cdp.open();
      const run = new Run(cdp, siteA, siteB, trace);
      if (verbose) console.error(`scenario ${s.name}`);
      try {
        await s.fn(run);
      } catch (e) {
        trace.push({ label: 'scenario aborted', method: '-', ok: false, error: { code: 'script', message: String(e.stack || e) } });
      }
      try {
        await run.closeAll();
      } catch {}
      cdp.close();
      out.scenarios[s.name] = trace;
    }
  } finally {
    cdp?.close();
    if (proc) proc.stop();
    siteA.server.close();
    siteB.server.close();
    siteA.server.closeAllConnections?.();
    siteB.server.closeAllConnections?.();
  }
  return out;
}

/**
 * Steps whose difference from Chrome is understood and kept, each with the
 * reason (`scenario: label`). They are reported as `known` and do not fail
 * the run; a step that now matches is reported as `known but equal` so the
 * entry gets removed.
 */
const KNOWN = {
  // Persona consistency (ADR 0009): a value that contradicts the persona
  // is refused, where Chrome applies it.
  'puppeteer.newPage: setDeviceMetrics': 'persona: Puppeteer\'s default viewport sends screenOrientation portraitPrimary / angle 0, which contradicts the persona screen',
  'playwright.newPage: init Emulation.setTouchEmulationEnabled': 'persona: maxTouchPoints is 0',
  'playwright.newPage: init Emulation.setTimezoneOverride': 'persona: timezone',
  'playwright.newPage: init Emulation.setLocaleOverride': 'persona: locale',
  'emulation: metrics 800x600': 'persona: device metrics',
  'emulation: dims': 'persona: device metrics',
  'emulation: metrics 1024x768 dsf2': 'persona: device metrics',
  'emulation: dims 2': 'persona: device metrics',
  'emulation: touch on': 'persona: maxTouchPoints is 0',
  'emulation: timezone': 'persona: timezone',
  'emulation: timezone invalid': 'persona: timezone',
  'emulation: locale': 'persona: locale',
  'emulation: locale invalid': 'persona: locale',
  'screenshot: metrics': 'persona: device metrics',
  'screenshot: viewport png': 'persona: size follows the persona viewport',
  'screenshot: puppeteer default': 'persona: size follows the persona viewport',
  'screenshot: transparent png': 'persona: size follows the persona viewport',
  // A browser window is not exposed (ADR 0009 revision 2).
  'puppeteer.newPage: getWindowForTarget': 'Browser.getWindowForTarget answers "Browser window not found": Playwright would go on to resize the persona window',
  'playwright.newPage: init Browser.getWindowForTarget': 'same',
  'browser: getWindowBounds': 'Browser.getWindowBounds is not implemented (no window id exists)',
  'browser: getWindowForTarget': 'same',
  // Not part of this package (ADR 0045, open items).
  'evaluate: evaluate rejected': 'exceptionDetails of a rejected promise: text, stack trace',
  'evaluate: evaluate throws': 'exceptionDetails has no stack trace and has executionContextId',
  'evaluate: evaluate syntax error': 'exceptionDetails has executionContextId',
  'evaluate: getProperties': 'internalProperties are not listed',
  'evaluate: removeBinding': 'Runtime.removeBinding is not implemented',
  'evaluate: queryObjects missing': 'Runtime.queryObjects is not implemented',
  'evaluate: DOM.describeNode': 'the node has a parentId Chrome omits',
  'evaluate: Accessibility': 'AX nodes lack chromeRole and name sources',
  'frames: getDocument': 'the root node has a frameId Chrome omits',
  'fetch: requestPaused event': 'request headers are the persona set, not Chrome\'s list at that stage',
  'cookies: Storage.getCookies after': 'sameSite, and a domain cookie set with Storage.setCookies gets a leading dot',
  'browser: performance metrics': 'Performance.getMetrics is not implemented',
  'browser: Page.setDocumentContent': 'Page.setDocumentContent is not implemented',
  'browser: content after setDocumentContent': 'follows from Page.setDocumentContent',
  'browser: Page.getResourceTree': 'Page.getResourceTree is not implemented',
};

function compare(expected, actual) {
  const problems = [];
  const known = new Set();
  const knownSeen = new Set();
  let checked = 0;
  for (const [name, exp] of Object.entries(expected.scenarios)) {
    const act = actual.scenarios[name];
    if (!act) {
      if (!opt('only', null)) problems.push(`${name}: scenario did not run`);
      continue;
    }
    const n = Math.max(exp.length, act.length);
    for (let i = 0; i < n; i++) {
      const e = exp[i];
      const a = act[i];
      const at = `${name} #${i} ${e?.label ?? a?.label} (${e?.method ?? a?.method})`;
      if (!e || !a) {
        problems.push(`${at}: ${e ? 'missing in LightBrowser run' : 'extra step in LightBrowser run'}`);
        continue;
      }
      checked++;
      if (e.label !== a.label) {
        problems.push(`${at}: step order differs (${a.label})`);
        continue;
      }
      if (e.ok !== a.ok) {
        problems.push(`${at}: Chrome ${e.ok ? 'succeeds' : 'fails: ' + JSON.stringify(e.error)}; LightBrowser ${a.ok ? 'succeeds' : 'fails: ' + JSON.stringify(a.error)}`);
        continue;
      }
      if (!e.ok) {
        if (e.error.code !== a.error.code) problems.push(`${at}: error code ${a.error.code}, Chrome ${e.error.code}`);
        else if (e.error.message !== undefined && e.error.message !== a.error.message) problems.push(`${at}: error text ${JSON.stringify(a.error.message)}, Chrome ${JSON.stringify(e.error.message)}`);
        continue;
      }
      for (const d of diffShape(e.shape, a.shape)) problems.push(`${at}: shape ${d}`);
      if (JSON.stringify(e.value) !== JSON.stringify(a.value)) problems.push(`${at}: value ${JSON.stringify(a.value)}, Chrome ${JSON.stringify(e.value)}`);
      if (JSON.stringify(e.events) !== JSON.stringify(a.events)) problems.push(`${at}: events ${JSON.stringify(a.events)}, Chrome ${JSON.stringify(e.events)}`);
    }
  }
  // Differences of steps in KNOWN move to `known`.
  const kept = [];
  for (const p of problems) {
    const key = Object.keys(KNOWN).find((k) => {
      const [scenario, label] = [k.slice(0, k.indexOf(':')), k.slice(k.indexOf(':') + 2)];
      return p.startsWith(`${scenario} #`) && new RegExp(`^${scenario.replace(/\./g, '\\.')} #\\d+ ${label.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')} \\(`).test(p);
    });
    if (key) {
      knownSeen.add(key);
      known.add(`${p.split(' (')[0]}: ${KNOWN[key]}`);
    } else kept.push(p);
  }
  const stale = Object.keys(KNOWN).filter((k) => !knownSeen.has(k) && !opt('only', null));
  return { problems: kept, known: [...known], stale, checked };
}

const chromeFile = path.join(here, 'chrome.json');
if (mode === 'diff') {
  // diff EXPECTED.json ACTUAL.json: compare two recordings, no browser.
  const files = argv.filter((a) => !a.startsWith('--'));
  const { problems, known, stale, checked } = compare(JSON.parse(readFileSync(files[0], 'utf8')), JSON.parse(readFileSync(files[1], 'utf8')));
  for (const p of problems) console.log(p);
  if (verbose) for (const k of known) console.log(`known: ${k}`);
  for (const k of stale) console.log(`stale KNOWN entry (the step matches now): ${k}`);
  console.log(`${checked} steps compared; ${problems.length} differences, ${known.length} known, ${stale.length} stale`);
  process.exit(problems.length || stale.length ? 1 : 0);
}
const result = await runAll();
if (mode === 'record') {
  const out = opt('out', chromeFile);
  writeFileSync(out, JSON.stringify(result, null, 1) + '\n');
  const steps = Object.values(result.scenarios).reduce((n, t) => n + t.length, 0);
  console.log(`recorded ${steps} steps from ${result.browser} into ${out}`);
  const aborted = Object.entries(result.scenarios).filter(([, t]) => t.some((s) => s.label === 'scenario aborted'));
  for (const [n, t] of aborted) console.log(`ABORTED ${n}: ${t.find((s) => s.label === 'scenario aborted').error.message}`);
  process.exit(aborted.length ? 1 : 0);
} else if (mode === 'compare') {
  const expected = JSON.parse(readFileSync(opt('expect', chromeFile), 'utf8'));
  if (opt('out', null)) writeFileSync(opt('out'), JSON.stringify(result, null, 1) + '\n');
  const { problems, known, stale, checked } = compare(expected, result);
  for (const p of problems) console.log(p);
  if (verbose) for (const k of known) console.log(`known: ${k}`);
  for (const k of stale) console.log(`stale KNOWN entry (the step matches now): ${k}`);
  console.log(`${checked} steps compared with ${expected.browser}; ${problems.length} differences, ${known.length} known, ${stale.length} stale`);
  process.exit(problems.length || stale.length ? 1 : 0);
} else {
  console.log(JSON.stringify(result, null, 1));
}

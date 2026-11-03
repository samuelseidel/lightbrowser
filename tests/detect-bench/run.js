#!/usr/bin/env node
// detect-bench scenario runner. Drives a browser against the local bench
// server, then asks the server for the session's bot score.
//
//   node run.js                                  # T-001 and T-003, headless Chromium
//   node run.js --scenario T-001 --save-fixtures fixtures
//   node run.js --scenario T-010 --cdp ws://127.0.0.1:9222/devtools/browser/... --expect '<=30'
//   node run.js --scenario T-010 --cdp ws://... --input human --expect 'beh<=30'
//   node run.js --scenario T-020 --trace trace.json --expect '<=40'
//
// --input playwright (default) drives T-010 with Playwright's own input
// (mouse.wheel, click, fill). --input human drives the same flow through
// LightBrowser.humanScroll/humanClick/humanType on a CDP session, with a
// reading pause per page from its text length (--seed N makes the pauses
// reproducible); it needs LightBrowser behind --cdp.
//
// Loopback only: any base URL, CDP URL or trace URL whose host is not
// 127.0.0.1, ::1 or localhost is refused.
'use strict';

const fs = require('fs');
const path = require('path');
const http = require('http');
const https = require('https');
const { chromium } = require('playwright');

const LOOPBACK = new Set(['127.0.0.1', 'localhost', '::1']);

function assertLoopback(u, what) {
  let host;
  try { host = new URL(u).hostname.replace(/^\[|\]$/g, ''); } catch (_) { host = null; }
  if (!host || !LOOPBACK.has(host)) {
    console.error(`refusing ${what} ${JSON.stringify(u)}: only 127.0.0.1, ::1 or localhost are allowed`);
    process.exit(2);
  }
}

function parseArgs(argv) {
  const a = { base: 'http://127.0.0.1:8765', scenarios: null, cdp: null, expect: null, trace: null,
    saveFixtures: null, json: false, wait: 3500, input: 'playwright', seed: 1 };
  for (let i = 0; i < argv.length; i++) {
    const k = argv[i];
    const next = () => {
      if (i + 1 >= argv.length) { console.error(`${k} needs a value`); process.exit(2); }
      return argv[++i];
    };
    if (k === '--base') a.base = next().replace(/\/$/, '');
    else if (k === '--scenario') a.scenarios = next().split(',').map((s) => s.trim().toUpperCase());
    else if (k === '--cdp') a.cdp = next();
    else if (k === '--expect') a.expect = next();
    else if (k === '--trace') a.trace = next();
    else if (k === '--save-fixtures') a.saveFixtures = next();
    else if (k === '--wait') a.wait = Number(next());
    else if (k === '--input') {
      a.input = next();
      if (!['playwright', 'human'].includes(a.input)) { console.error(`--input must be playwright or human, not ${JSON.stringify(a.input)}`); process.exit(2); }
    } else if (k === '--seed') a.seed = Number(next()) >>> 0;
    else if (k === '--json') a.json = true;
    else if (k === '-h' || k === '--help') {
      console.log(fs.readFileSync(__filename, 'utf8').split('\n').slice(1, 18).map((l) => l.replace(/^\/\/ ?/, '')).join('\n'));
      process.exit(0);
    } else { console.error(`unknown argument ${k}`); process.exit(2); }
  }
  return a;
}

function getJSON(url) {
  assertLoopback(url, 'API URL');
  const mod = url.startsWith('https:') ? https : http;
  return new Promise((resolve, reject) => {
    // The bench's TLS cert is self-signed and loopback-only.
    const req = mod.get(url, { rejectUnauthorized: false, agent: false }, (res) => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', (c) => { body += c; });
      res.on('end', () => {
        if (res.statusCode !== 200) return reject(new Error(`${url}: HTTP ${res.statusCode} ${body.slice(0, 200)}`));
        try { resolve(JSON.parse(body)); } catch (e) { reject(e); }
      });
    });
    req.on('error', reject);
    req.setTimeout(10000, () => req.destroy(new Error(`${url}: timeout`)));
  });
}

// Expectation strings: ">=90", "<=30", joined with "&". "fp>=70" checks the
// score from non-behaviour rules only (fingerprint, consistency, automation,
// server); "beh<=30" checks the score from behaviour rules only.
function parseExpect(s) {
  if (!s) return null;
  const out = { text: s };
  for (const part of s.split('&').map((p) => p.trim())) {
    const m = /^(fp|beh)?\s*(>=|<=)\s*(\d+(?:\.\d+)?)$/.exec(part);
    if (!m) { console.error(`bad --expect ${JSON.stringify(s)} (use ">=90", "<=30", ">=70&fp>=70", "beh<=30")`); process.exit(2); }
    out[(m[1] || 'score') + (m[2] === '>=' ? 'Min' : 'Max')] = Number(m[3]);
  }
  return out;
}

function partScore(r, behavior) {
  let keep = 1;
  for (const f of r.fired) if ((f.cat === 'behavior') === behavior) keep *= 1 - Math.min(f.weight, 99) / 100;
  return Math.round(1000 * (1 - keep)) / 10;
}

function check(exp, r) {
  if (!exp) return null;
  const got = { score: r.score, fp: partScore(r, false), beh: partScore(r, true) };
  for (const k of ['score', 'fp', 'beh']) {
    if (exp[k + 'Min'] !== undefined && got[k] < exp[k + 'Min']) return false;
    if (exp[k + 'Max'] !== undefined && got[k] > exp[k + 'Max']) return false;
  }
  return true;
}

const pause = (page, ms) => page.waitForTimeout(ms);

async function center(page, sel) {
  const b = await page.locator(sel).boundingBox();
  return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
}

// The common flow: shop, scroll, click a product, search, submit.
async function flowInstant(page, base) {
  await page.goto(`${base}/shop`);
  await page.mouse.wheel(0, 900);
  await page.click('#add-3');
  await page.fill('#q', 'jacket');
  await Promise.all([page.waitForURL(/\/search/), page.click('#search-btn')]);
}

async function flowScriptedSlow(page, base) {
  await page.goto(`${base}/shop`);
  await pause(page, 800);
  for (let i = 0; i < 6; i++) {
    await page.mouse.wheel(0, 120);
    await pause(page, 150);
  }
  await page.locator('#add-3').scrollIntoViewIfNeeded();
  const add = await center(page, '#add-3');
  await page.mouse.move(add.x - 140, add.y - 90, { steps: 10 });
  await page.mouse.move(add.x, add.y, { steps: 25 });
  await pause(page, 300);
  await page.click('#add-3', { delay: 80 });
  await pause(page, 500);
  const q = await center(page, '#q');
  await page.mouse.move(q.x, q.y, { steps: 20 });
  await page.click('#q', { delay: 60 });
  await page.locator('#q').pressSequentially('jacket', { delay: 120 });
  await pause(page, 400);
  await Promise.all([page.waitForURL(/\/search/), page.keyboard.press('Enter')]);
}

// Small seeded PRNG (mulberry32) for reproducible pauses.
function prng(seed) {
  let t = seed >>> 0;
  return () => {
    t = (t + 0x6d2b79f5) >>> 0;
    let r = Math.imul(t ^ (t >>> 15), 1 | t);
    r ^= r + Math.imul(r ^ (r >>> 7), 61 | r);
    return ((r ^ (r >>> 14)) >>> 0) / 4294967296;
  };
}

// Time spent looking at a page before acting on it. People skim a page at
// about 600-700 words per minute (Rayner et al. 2016) and look at part of
// it before acting: a third to two thirds of the words here, with a floor
// for orienting on a short page and a cap for a long one.
async function readingPause(page, rnd) {
  // innerText runs in Playwright's utility world, not the page's.
  const text = await page.locator('body').innerText().catch(() => '');
  const words = text.split(/\s+/).filter(Boolean).length;
  const wpm = 600 + 100 * rnd();
  const share = 0.33 + 0.33 * rnd();
  const ms = Math.min(6000, Math.max(1200, (words * share / wpm) * 60000));
  await pause(page, Math.round(ms));
}

// The common flow through LightBrowser's human input (M5). The CDP session
// is the page's own; selectors are resolved by LightBrowser, not by script.
async function flowHuman(page, base, a) {
  const rnd = prng(a.seed);
  await page.goto(`${base}/shop`);
  const cdp = await page.context().newCDPSession(page);
  const human = (m, p) => cdp.send(`LightBrowser.${m}`, p);
  await readingPause(page, rnd);
  await human('humanScroll', { deltaY: 900 });
  await human('humanClick', { selector: '#add-3' });
  await human('humanType', { selector: '#q', text: 'jacket' });
  await Promise.all([page.waitForURL(/\/search/), human('humanClick', { selector: '#search-btn' })]);
  await page.waitForLoadState('load');
  await readingPause(page, rnd);
  await cdp.detach().catch(() => {});
}

// Replays a trace from GET /api/trace/<sid> with its original timing.
async function flowReplay(page, base, tracePath) {
  const tr = JSON.parse(fs.readFileSync(tracePath, 'utf8'));
  const pages = tr.pages || [];
  if (!pages.length) throw new Error(`${tracePath}: no pages`);
  for (let i = 0; i < pages.length; i++) {
    const p = pages[i];
    const rel = new URL(p.url, base);
    const target = `${base}${rel.pathname}${rel.search}`;
    if (i === 0 || new URL(page.url()).pathname !== rel.pathname) await page.goto(target);
    let last = null;
    for (const e of p.events || []) {
      if (!e.tr) continue;
      if (last !== null) await pause(page, Math.max(0, Math.min(e.t - last, 5000)));
      last = e.t;
      if (e.type === 'mousemove') await page.mouse.move(e.x, e.y);
      else if (e.type === 'mousedown') await page.mouse.down({ button: ['left', 'middle', 'right'][e.b] || 'left' });
      else if (e.type === 'mouseup') await page.mouse.up({ button: ['left', 'middle', 'right'][e.b] || 'left' });
      else if (e.type === 'wheel') await page.mouse.wheel(e.dx || 0, e.dy || 0);
      // Password keys are recorded as 'x'; unknown key names are skipped.
      else if (e.type === 'keydown' && e.k) await page.keyboard.down(e.k).catch(() => {});
      else if (e.type === 'keyup' && e.k) await page.keyboard.up(e.k).catch(() => {});
    }
    await pause(page, 300); // a submit may still be navigating
    await page.waitForLoadState('load').catch(() => {});
  }
}

const SCENARIOS = {
  'T-001': { name: 'stock-chromium-headless', expect: '>=90', run: (page, a) => flowInstant(page, a.base) },
  'T-003': { name: 'scripted-slow', expect: '>=70&fp>=70', run: (page, a) => flowScriptedSlow(page, a.base) },
  'T-010': { name: 'cdp-endpoint', expect: null, cdp: true,
    run: (page, a) => (a.input === 'human' ? flowHuman(page, a.base, a) : flowInstant(page, a.base)) },
  'T-020': { name: 'human-replay', expect: null, run: (page, a) => flowReplay(page, a.base, a.trace) },
};

async function sessionId(context, page, base) {
  const cookies = await context.cookies(base);
  const c = cookies.find((x) => x.name === 'lb_sid');
  if (c) return c.value;
  return page.evaluate(() => JSON.parse(document.getElementById('lb-ctx').textContent).sid);
}

async function runScenario(id, a) {
  const sc = SCENARIOS[id];
  let browser, context, owned = true;
  if (sc.cdp || (id === 'T-020' && a.cdp)) {
    if (!a.cdp) return { id, name: sc.name, skipped: 'needs --cdp <ws url>' };
    assertLoopback(a.cdp, 'CDP URL');
    browser = await chromium.connectOverCDP(a.cdp);
    // viewport: null keeps the browser's own window size. Playwright's default
    // 1280x720 override would change screen and window sizes, and LightBrowser
    // refuses overrides that contradict its persona (ADR 0009).
    context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: null }).catch(() => browser.contexts()[0]);
    owned = false;
  } else {
    if (id === 'T-020' && !a.trace) return { id, name: sc.name, skipped: 'needs --trace <file>' };
    browser = await chromium.launch({ headless: true });
    context = await browser.newContext({ ignoreHTTPSErrors: true });
  }
  const page = await context.newPage();
  page.on('request', (r) => {
    let host;
    try { host = new URL(r.url()).hostname.replace(/^\[|\]$/g, ''); } catch (_) { return; }
    if (/^https?:/.test(r.url()) && !LOOPBACK.has(host)) console.error(`warning: page requested non-loopback ${r.url()}`);
  });
  const t0 = Date.now();
  let error = null;
  try {
    await sc.run(page, a);
    await pause(page, a.wait); // let the 3 s beacon of the last page go out
  } catch (e) { error = e; }
  let sid = null;
  try { sid = await sessionId(context, page, a.base); } catch (_) { /* reported below */ }
  await page.close({ runBeforeUnload: true }).catch(() => {});
  await new Promise((r) => setTimeout(r, 300)); // pagehide beacon
  await context.close().catch(() => {});
  if (owned) await browser.close();
  else await browser.close().catch(() => {});
  if (error) return { id, name: sc.name, error: String(error.message || error).split('\n')[0] };
  if (!sid) return { id, name: sc.name, error: 'no session cookie' };
  const r = await getJSON(`${a.base}/api/score/${sid}`);
  if (a.saveFixtures) {
    fs.mkdirSync(a.saveFixtures, { recursive: true });
    const s = await getJSON(`${a.base}/api/session/${sid}`);
    s.scenario = `${id} ${sc.name}`;
    fs.writeFileSync(path.join(a.saveFixtures, `${id}.json`), JSON.stringify(s, null, 1) + '\n');
  }
  // --expect applies to T-010/T-020, or to the only scenario selected.
  const useArg = a.expect && (id === 'T-010' || id === 'T-020' || (a.scenarios && a.scenarios.length === 1));
  const exp = parseExpect(useArg ? a.expect : sc.expect);
  return { id, name: sc.name, sid, ms: Date.now() - t0, input: sc.cdp ? a.input : 'playwright', result: r, expect: exp, pass: check(exp, r),
    fp: partScore(r, false), beh: partScore(r, true) };
}

function table(rows) {
  const lines = [['scenario', 'score', 'fp', 'beh', 'verdict', 'expect', 'result', 'top fired rules']];
  for (const r of rows) {
    if (r.skipped) { lines.push([`${r.id} ${r.name}`, '-', '-', '-', '-', '-', 'skip', r.skipped]); continue; }
    if (r.error) { lines.push([`${r.id} ${r.name}`, '-', '-', '-', '-', '-', 'FAIL', r.error]); continue; }
    const top = r.result.fired.slice(0, 6).map((f) => `${f.id}(${f.weight})`).join(' ');
    lines.push([`${r.id} ${r.name}`, String(r.result.score), String(r.fp), String(r.beh), r.result.verdict,
      r.expect ? r.expect.text : 'none', r.pass === null ? 'n/a' : r.pass ? 'pass' : 'FAIL', top]);
  }
  const w = lines[0].map((_, i) => Math.max(...lines.map((l) => l[i].length)));
  return lines.map((l) => l.map((c, i) => (i === l.length - 1 ? c : c.padEnd(w[i]))).join('  ')).join('\n');
}

async function main() {
  const a = parseArgs(process.argv.slice(2));
  assertLoopback(a.base, 'base URL');
  if (a.cdp) assertLoopback(a.cdp, 'CDP URL');
  parseExpect(a.expect); // validate early
  if (a.input === 'human' && !a.cdp) {
    console.error('--input human needs --cdp (LightBrowser.human* exists only in LightBrowser)');
    process.exit(2);
  }
  let ids = a.scenarios || ['T-001', 'T-003'];
  if (!a.scenarios && a.cdp) ids.push('T-010');
  if (!a.scenarios && a.trace) ids.push('T-020');
  for (const id of ids) {
    if (!SCENARIOS[id]) { console.error(`unknown scenario ${id} (have ${Object.keys(SCENARIOS).join(', ')})`); process.exit(2); }
  }
  await getJSON(`${a.base}/api/results`).catch((e) => {
    console.error(`bench server not reachable at ${a.base}: ${e.message}\nstart it with: python3 server.py`);
    process.exit(2);
  });
  const rows = [];
  for (const id of ids) rows.push(await runScenario(id, a));
  if (a.json) console.log(JSON.stringify(rows, null, 1));
  console.log(table(rows));
  const failed = rows.some((r) => r.error || r.pass === false);
  process.exit(failed ? 1 : 0);
}

main().catch((e) => { console.error(e); process.exit(1); });

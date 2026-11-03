#!/usr/bin/env node
// OS ground truth from the local Chromium, loopback only.
//
//   xvfb-run -a -s "-screen 0 1920x1080x24 -dpi 96" \
//     node tools/os_truth/capture.js --mode headed --out tests/fixtures/os/linux_x11_cft153.json
//   node tools/os_truth/capture.js --mode headless --out tests/fixtures/os/linux_x11_cft153_headless.json
//   ... --fontconf tools/os_truth/fontconf/ubuntu24 --gtk-font "Ubuntu Sans 11"
//
// Serves page.html on 127.0.0.1 (random port), starts Chromium on it with a
// throwaway profile and no automation flags (no CDP, no --enable-automation),
// waits for the page to POST its results, drives the wheel with XTest in
// headed mode (xtest.py), then writes one JSON file with the results and
// their provenance. Chromium is only ever pointed at this server.
'use strict';

const fs = require('fs');
const os = require('os');
const path = require('path');
const http = require('http');
const { spawn, execFileSync } = require('child_process');

const HERE = __dirname;

function parseArgs(argv) {
  const a = { mode: 'headless', out: null, label: null, fontconf: null, gtkFont: null,
    chromium: '/opt/pw-browsers/chromium', extra: [], timeout: 60000 };
  for (let i = 0; i < argv.length; i++) {
    const k = argv[i];
    const next = () => { if (i + 1 >= argv.length) throw new Error(`${k} needs a value`); return argv[++i]; };
    if (k === '--mode') a.mode = next();
    else if (k === '--out') a.out = next();
    else if (k === '--label') a.label = next();
    else if (k === '--fontconf') a.fontconf = path.resolve(next());
    else if (k === '--gtk-font') a.gtkFont = next();
    else if (k === '--chromium') a.chromium = next();
    else if (k === '--extra-arg') a.extra.push(next());
    else throw new Error(`unknown argument ${k}`);
  }
  if (!['headed', 'headless'].includes(a.mode)) throw new Error('--mode must be headed or headless');
  if (!a.out) throw new Error('--out is required');
  if (a.mode === 'headed' && !process.env.DISPLAY) throw new Error('headed mode needs $DISPLAY (run under xvfb-run)');
  if (a.extra.some((x) => /remote-debugging|enable-automation|proxy-server|host-resolver/.test(x))) {
    throw new Error('refusing extra args that add automation or change where the browser connects');
  }
  return a;
}

function sh(cmd, args) {
  try { return execFileSync(cmd, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim(); } catch (_) { return null; }
}

function osRelease() {
  try {
    const m = fs.readFileSync('/etc/os-release', 'utf8').match(/^PRETTY_NAME="?([^"\n]*)"?/m);
    return m ? m[1] : null;
  } catch (_) { return null; }
}

async function main() {
  const a = parseArgs(process.argv.slice(2));
  const page = fs.readFileSync(path.join(HERE, 'page.html'));
  let result = null, wheel = null, wheelDone = false;
  const waiters = [];
  const notify = () => { while (waiters.length) waiters.shift()(); };
  const until = (pred, ms, what) => new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`timed out waiting for ${what}`)), ms);
    const check = () => { if (pred()) { clearTimeout(t); resolve(); } else waiters.push(check); };
    check();
  });

  const server = http.createServer((req, res) => {
    const body = [];
    req.on('data', (c) => body.push(c));
    req.on('end', () => {
      const send = (code, type, data) => { res.writeHead(code, { 'Content-Type': type, 'Cache-Control': 'no-store' }); res.end(data); };
      if (req.method === 'GET' && (req.url === '/' || req.url.startsWith('/?'))) return send(200, 'text/html; charset=utf-8', page);
      if (req.method === 'GET' && req.url === '/wheel-done') return send(200, 'application/json', JSON.stringify({ done: wheelDone }));
      if (req.method === 'POST' && req.url === '/result') { result = JSON.parse(Buffer.concat(body)); notify(); return send(200, 'application/json', '{}'); }
      if (req.method === 'POST' && req.url === '/wheel') { wheel = JSON.parse(Buffer.concat(body)); notify(); return send(200, 'application/json', '{}'); }
      return send(404, 'text/plain', 'not found');
    });
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const url = `http://127.0.0.1:${server.address().port}/`;

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'lb-os-truth-'));
  const env = { ...process.env };
  if (a.fontconf) env.FONTCONFIG_FILE = path.join(a.fontconf, 'fonts.conf');
  if (a.gtkFont) {
    const dir = path.join(tmp, 'xdg', 'gtk-3.0');
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, 'settings.ini'), `[Settings]\ngtk-font-name=${a.gtkFont}\n`);
    fs.mkdirSync(path.join(tmp, 'xdg', 'gtk-4.0'), { recursive: true });
    fs.copyFileSync(path.join(dir, 'settings.ini'), path.join(tmp, 'xdg', 'gtk-4.0', 'settings.ini'));
    env.XDG_CONFIG_HOME = path.join(tmp, 'xdg');
  }
  const flags = ['--no-sandbox', `--user-data-dir=${path.join(tmp, 'profile')}`, '--no-first-run',
    '--no-default-browser-check', '--password-store=basic', '--disable-component-update', ...a.extra];
  if (a.mode === 'headless') flags.push('--headless=new');
  const chrome = spawn(a.chromium, [...flags, url], { env, stdio: ['ignore', 'ignore', 'pipe'], detached: true });
  // Chromium's helpers share its process group; signal the whole group.
  const killAll = (sig) => { try { process.kill(-chrome.pid, sig); } catch (_) { /* gone */ } };
  let stderr = '';
  chrome.stderr.on('data', (c) => { stderr = (stderr + c).slice(-4000); });

  try {
    await until(() => result, a.timeout, 'the page result');
    if (a.mode === 'headed' && result.wheelTarget) {
      const t = result.wheelTarget;
      execFileSync('python3', [path.join(HERE, 'xtest.py'), 'wheel', '--x', String(t.x), '--y', String(t.y),
        '--down', '4', '--up', '2', '--gap-ms', '350'], { stdio: 'inherit' });
      await new Promise((r) => setTimeout(r, 500));
    }
    wheelDone = true;
    await until(() => wheel, 20000, 'the wheel events');
  } catch (e) {
    console.error(e.message);
    console.error(stderr);
    process.exitCode = 1;
  } finally {
    killAll('SIGTERM');
    await new Promise((r) => { const t = setTimeout(r, 3000); chrome.on('exit', () => { clearTimeout(t); r(); }); });
    await new Promise((r) => setTimeout(r, 500));
    killAll('SIGKILL');
    server.close();
    fs.rmSync(tmp, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
  }
  if (!result) return;

  const limits = [
    'GPU: none in this container; WebGL/WebGPU show what Chromium does without a GPU (SwiftShader or nothing), not a desktop GPU.',
    'Audio: no PipeWire/PulseAudio server; AudioContext values are the no-device fallback.',
    'Fonts: the container font set (default sans-serif is Inter, not a stock distro font).',
    'Window manager: none under Xvfb, so avail* equals the screen and there is no panel or dock.',
    'Speech: no speech-dispatcher, so no local voices.',
  ];
  if (a.fontconf) limits.push(`fontconfig: ${path.relative(process.cwd(), a.fontconf)}/fonts.conf; distro fonts that are not installed here fall back to installed ones.`);
  const doc = {
    provenance: {
      captured: true,
      tool: 'tools/os_truth/capture.js',
      label: a.label || path.basename(a.out, '.json'),
      date: new Date().toISOString().slice(0, 10),
      browser: sh(a.chromium, ['--version']),
      mode: a.mode === 'headed' ? 'headed under xvfb-run (-screen 0 1920x1080x24 -dpi 96 unless noted)' : '--headless=new',
      flags: flags.filter((f) => !f.startsWith('--user-data-dir')),
      host: { os: osRelease(), kernel: sh('uname', ['-r']), arch: sh('uname', ['-m']) },
      fontconfig: a.fontconf ? path.relative(process.cwd(), a.fontconf) : 'system',
      gtkFont: a.gtkFont,
      loopbackOnly: true,
      limits,
    },
    result,
    wheel,
  };
  fs.mkdirSync(path.dirname(path.resolve(a.out)), { recursive: true });
  fs.writeFileSync(a.out, JSON.stringify(doc, null, 1) + '\n');
  console.log(`wrote ${a.out}`);
}

main().catch((e) => { console.error(e.message); process.exit(1); });

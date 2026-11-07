// Dump the JS surface of a dedicated, a shared and a service worker of the
// local Chromium (Chrome for Testing), on a secure loopback origin: own
// keys of the global in order with descriptor kinds, the global's
// prototype chain, every interface object (same entry format as
// tools/dump_surface.js), namespaces, WorkerNavigator and WorkerLocation
// values. Ground truth for the worker tables of lb-webidl-gen and for the
// worker realms of lb-webapi (ADR 0021, 0022).
//
//   node tools/dump_worker_surface.js tests/fixtures/chromium_153_worker_surface.json
//
// Loopback only: the page and the worker scripts are served by this script
// on 127.0.0.1; nothing else is loaded.
const http = require('http');
const fs = require('fs');
const { chromium } = require('playwright');

// Runs inside the worker. Kept as a plain function so its source can be
// put into a worker script.
function dumpScope() {
  const kind = (d) => d.get || d.set ? `${d.get ? 'g' : ''}${d.set ? 's' : ''}${d.enumerable ? 'e' : ''}${d.configurable ? 'c' : ''}`
    : `${typeof d.value === 'function' ? 'f' : 'v'}${d.writable ? 'w' : ''}${d.enumerable ? 'e' : ''}${d.configurable ? 'c' : ''}`;
  const props = (o) => Object.getOwnPropertyNames(o).map((n) => {
    try {
      const d = Object.getOwnPropertyDescriptor(o, n);
      return [n, kind(d), typeof d.value === 'function' ? d.value.length : null];
    } catch (e) { return [n, 'err', null]; }
  });
  const syms = (o) => Object.getOwnPropertySymbols(o).map((s) => s.toString());
  // The capture's own two functions are not part of the surface.
  const globalProps = props(self).filter(([n]) => n !== 'dumpScope' && n !== 'capture');
  const interfaces = {};
  const namespaces = {};
  for (const [name] of globalProps) {
    let v; try { v = self[name]; } catch (e) { continue; }
    if (!/^[A-Z]/.test(name)) continue;
    if (typeof v === 'object' && v !== null && name !== 'Infinity') {
      namespaces[name] = { props: props(v), symbols: syms(v), toStringTag: v[Symbol.toStringTag] || null };
      continue;
    }
    if (typeof v !== 'function' || !v.prototype) continue;
    const proto = v.prototype;
    const parent = Object.getPrototypeOf(proto);
    interfaces[name] = {
      length: v.length,
      static: props(v).filter(([n]) => !['length', 'name', 'prototype', 'arguments', 'caller'].includes(n)),
      proto: props(proto),
      protoSymbols: syms(proto),
      parent: parent && parent.constructor ? parent.constructor.name : null,
      toStringTag: (() => { try { return proto[Symbol.toStringTag] || null; } catch (e) { return null; } })(),
      ctorToString: Function.prototype.toString.call(v),
    };
  }
  const dump = (o) => {
    const r = {};
    for (const k in o) {
      try {
        const v = o[k];
        r[k] = typeof v === 'function' ? '[fn]' : (typeof v === 'object' && v !== null ? '[obj]' : v);
      } catch (e) { r[k] = '[err]'; }
    }
    return r;
  };
  const forIn = (o) => { const r = []; for (const k in o) r.push(k); return r; };
  const chain = [];
  for (let o = Object.getPrototypeOf(self); o; o = Object.getPrototypeOf(o)) {
    chain.push(Object.prototype.hasOwnProperty.call(o, Symbol.toStringTag) ? o[Symbol.toStringTag] : (o.constructor ? o.constructor.name : '?'));
  }
  const ua = navigator.userAgentData;
  return {
    globalProps,
    globalSymbols: syms(self),
    protoChain: chain,
    toString: Object.prototype.toString.call(self),
    interfaces,
    namespaces,
    navigator: {
      values: dump(navigator),
      forIn: forIn(navigator),
      languages: navigator.languages,
      uaData: ua ? { brands: ua.brands, mobile: ua.mobile, platform: ua.platform, json: ua.toJSON() } : null,
      gpu: 'gpu' in navigator ? typeof navigator.gpu : 'absent',
    },
    location: { values: dump(location), string: String(location) },
    isSecureContext: self.isSecureContext,
    origin: self.origin,
    name: self.name,
  };
}

async function capture(kind) {
  const hev = ['architecture', 'bitness', 'formFactors', 'fullVersionList', 'model', 'platformVersion', 'uaFullVersion', 'wow64'];
  const r = dumpScope();
  if (navigator.userAgentData) {
    try { r.navigator.uaData.high = await navigator.userAgentData.getHighEntropyValues(hev); } catch (e) { r.navigator.uaData.high = String(e); }
  }
  r.kind = kind;
  return r;
}

const workerSrc = (kind) => `${dumpScope.toString()}\n${capture.toString()}\n` + (kind === 'dedicated'
  ? `capture('dedicated').then((r) => postMessage(r), (e) => postMessage(String(e)));`
  : kind === 'shared'
    ? `onconnect = (e) => { const p = e.ports[0]; capture('shared').then((r) => p.postMessage(r), (err) => p.postMessage(String(err))); };`
    // A listener, not `onmessage`: setting a handler would not change the
    // own keys (they are accessors) but keeps the capture's footprint to
    // the two functions filtered out above.
    : `addEventListener('message', (e) => { e.waitUntil(capture('service').then((r) => e.source.postMessage(r), (err) => e.source.postMessage(String(err)))); });`);

const page = `<!doctype html><title>w</title><script>
window.run = () => Promise.all([
  new Promise((res) => { const w = new Worker('/dedicated.js'); w.onmessage = (e) => res(e.data); w.onerror = (e) => res('error ' + e.message); }),
  new Promise((res) => { const s = new SharedWorker('/shared.js'); s.port.onmessage = (e) => res(e.data); s.onerror = (e) => res('error'); }),
  navigator.serviceWorker.register('/service.js').then(() => navigator.serviceWorker.ready).then((reg) => new Promise((res) => {
    navigator.serviceWorker.onmessage = (e) => res(e.data);
    reg.active.postMessage('dump');
  })),
]);
</script>`;

(async () => {
  const out = process.argv[2];
  if (!out) throw new Error('usage: dump_worker_surface.js OUT.json');
  const server = http.createServer((req, res) => {
    const body = req.url === '/dedicated.js' ? workerSrc('dedicated') : req.url === '/shared.js' ? workerSrc('shared')
      : req.url === '/service.js' ? workerSrc('service') : page;
    res.writeHead(200, { 'Content-Type': req.url.endsWith('.js') ? 'text/javascript' : 'text/html', 'Cache-Control': 'no-store' });
    res.end(body);
  });
  await new Promise((r) => server.listen(0, '127.0.0.1', r));
  const base = `http://127.0.0.1:${server.address().port}`;
  const browser = await chromium.launch({
    executablePath: process.env.CHROME || '/opt/pw-browsers/chromium',
    args: ['--no-sandbox', '--disable-background-networking', '--no-proxy-server'],
  });
  const p = await browser.newPage();
  await p.goto(`${base}/`);
  const [dedicated, shared, service] = await p.evaluate(() => window.run());
  const version = browser.version();
  await browser.close();
  server.close();
  const data = {
    _meta: {
      source: `Chrome for Testing ${version}, Linux, headless (new), captured by tools/dump_worker_surface.js over Playwright on a secure loopback origin (http://127.0.0.1:PORT/, worker scripts at /dedicated.js, /shared.js and /service.js)`,
      note: 'UA says HeadlessChrome. Navigator values are the capture machine\'s, not persona ground truth; property order and descriptors are the fixture.',
    },
    dedicated: Object.assign(dedicated, { location: undefined, origin: undefined }),
    shared: Object.assign(shared, { location: undefined, origin: undefined }),
    service: typeof service === 'string' ? service : Object.assign(service, { location: undefined, origin: undefined }),
  };
  fs.writeFileSync(out, JSON.stringify(data, null, 1));
  for (const k of ['dedicated', 'shared', 'service']) {
    const d = data[k];
    console.log(k, typeof d === 'string' ? d : `global props ${d.globalProps.length}, interfaces ${Object.keys(d.interfaces).length}, chain ${d.protoChain.join(' > ')}`);
  }
})();

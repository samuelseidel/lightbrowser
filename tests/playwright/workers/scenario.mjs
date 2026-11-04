// Workers as CDP targets: the site and the Playwright scenario shared by
// `workers.spec.mjs` (LightBrowser) and `record.mjs` (Chrome or
// LightBrowser, with the CDP traffic recorded). Loopback only.

const html = (title, body) =>
  `<!DOCTYPE html><html><head><meta charset="utf-8"><title>${title}</title></head><body>${body}</body></html>`;

const js = (src) => (req, res) => {
  res.writeHead(200, { 'content-type': 'text/javascript', 'cache-control': 'no-store' });
  res.end(src);
};
const text = (body) => (req, res) => {
  res.writeHead(200, { 'content-type': 'text/plain', 'cache-control': 'no-store' });
  res.end(body);
};

/** Pages of the scenario, by path (merged into the harness's site). */
export const workerPages = {
  '/wk/': html(
    'Workers',
    `<script>
      var w = new Worker('/wk/worker.js', { name: 'dw' });
      var replies = [];
      w.onmessage = (e) => replies.push(e.data);
    </script>`,
  ),
  '/wk/worker.js': js(`
    console.log('hello from worker', 42);
    self.counter = 1;
    self.onmessage = (e) => {
      if (e.data === 'fetch') fetch('/wk/data.txt').then((r) => r.text()).then((t) => postMessage('got ' + t));
      if (e.data === 'throw') setTimeout(() => { throw new Error('boom in worker'); });
      if (e.data === 'log') console.warn('warned', { a: 1 });
    };
  `),
  '/wk/data.txt': text('worker data'),
  '/wk/shared/': html(
    'Shared',
    `<script>
      var s = new SharedWorker('/wk/shared.js', 'sw1');
      var sreply = '';
      s.port.onmessage = (e) => { sreply = e.data; };
      s.port.postMessage('hi');
    </script>`,
  ),
  '/wk/shared.js': js(`
    onconnect = (e) => { const p = e.ports[0]; p.onmessage = (m) => p.postMessage('shared got ' + m.data); };
  `),
  '/wk/sw/': html(
    'SW',
    `<p id="out">waiting</p>
    <script>
      const done = () => { document.getElementById('out').textContent = 'controlled'; };
      navigator.serviceWorker.oncontrollerchange = done;
      navigator.serviceWorker.register('/wk/sw/sw.js').then(() => navigator.serviceWorker.ready).then(() => {
        if (navigator.serviceWorker.controller) done();
      });
    </script>`,
  ),
  '/wk/sw/sw.js': js(`
    self.addEventListener('install', () => self.skipWaiting());
    self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));
    self.addEventListener('fetch', (e) => {
      const u = new URL(e.request.url);
      if (u.pathname === '/wk/sw/answered.txt') e.respondWith(new Response('from the service worker'));
      if (u.pathname === '/wk/sw/through.txt') e.respondWith(fetch(e.request));
    });
  `),
  '/wk/sw/through.txt': text('network through the worker'),
  '/wk/sw/direct.txt': text('network direct'),
};

const path = (u) => new URL(u).pathname;

/**
 * Run the scenario on a connected browser. Returns what Playwright saw,
 * as plain data (the same for Chrome and LightBrowser).
 */
export async function runScenario(browser, base) {
  const out = {};
  // --- dedicated worker ---
  {
    const context = await browser.newContext({ viewport: null });
    const page = await context.newPage();
    const consoles = [];
    page.on('console', (m) => consoles.push(`${m.type()}: ${m.text()}`));
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    const requests = [];
    page.on('request', (r) => requests.push(`${r.resourceType()} ${path(r.url())}`));
    const responses = [];
    page.on('response', (r) => responses.push(`${r.status()} ${path(r.url())}`));
    const [worker] = await Promise.all([page.waitForEvent('worker'), page.goto(`${base}/wk/`)]);
    out.workerUrl = path(worker.url());
    out.pageWorkers = page.workers().length;
    out.evaluate = await worker.evaluate(() => [
      self.constructor.name,
      typeof window,
      self.name,
      self.counter,
      typeof document,
    ]);
    out.evaluateArg = await worker.evaluate(([a, b]) => a + b, [40, 2]);
    const h = await worker.evaluateHandle(() => ({ x: [1, 2], y: 'z' }));
    out.handle = await h.jsonValue();
    out.handleProp = await (await h.getProperty('y')).jsonValue();
    await h.dispose();
    out.evaluateError = await worker.evaluate(() => { throw new Error('thrown'); }).then(
      () => 'no error',
      (e) => e.message.split('\n')[0],
    );
    out.evaluatePromise = await worker.evaluate(() => new Promise((r) => setTimeout(() => r('later'), 20)));
    out.userAgentMatches = (await worker.evaluate(() => navigator.userAgent)) === (await page.evaluate(() => navigator.userAgent));
    // Messages through the page; the worker's console, fetch and errors.
    await page.evaluate(() => w.postMessage('log'));
    await page.evaluate(() => w.postMessage('fetch'));
    await page.waitForFunction(() => replies.length > 0);
    out.replies = await page.evaluate(() => replies);
    await page.evaluate(() => w.postMessage('throw'));
    await page.waitForEvent('pageerror', { timeout: 5000 }).catch(() => {});
    out.consoles = consoles;
    out.errors = errors;
    out.workerRequests = requests.filter((r) => r.includes('data.txt'));
    out.workerResponses = responses.filter((r) => r.includes('data.txt'));
    // terminate() closes the target.
    const closed = new Promise((r) => worker.once('close', () => r('closed')));
    await page.evaluate(() => w.terminate());
    out.close = await closed;
    out.pageWorkersAfter = page.workers().length;
    out.evaluateAfterClose = await worker.evaluate(() => 1).then(
      () => 'no error',
      (e) => (e.message.includes('closed') ? 'closed' : e.message),
    );
    // Navigating away ends a new worker too.
    const [w2] = await Promise.all([page.waitForEvent('worker'), page.reload()]);
    const closed2 = new Promise((r) => w2.once('close', () => r('closed')));
    out.secondWorker = await w2.evaluate(() => self.counter);
    await page.goto(`${base}/wk/data.txt`);
    out.closeOnNavigation = await closed2;
    await context.close();
  }
  // --- shared worker: not a page worker in Playwright ---
  {
    const context = await browser.newContext({ viewport: null });
    const page = await context.newPage();
    let pageWorkerEvents = 0;
    page.on('worker', () => pageWorkerEvents++);
    await page.goto(`${base}/wk/shared/`);
    await page.waitForFunction(() => sreply !== '');
    out.sharedReply = await page.evaluate(() => sreply);
    out.sharedPageWorkers = page.workers().length + pageWorkerEvents;
    const cdp = await browser.newBrowserCDPSession();
    const { targetInfos } = await cdp.send('Target.getTargets');
    out.sharedTargets = targetInfos
      .filter((t) => t.type === 'shared_worker')
      .map((t) => ({ type: t.type, path: path(t.url), title: t.title === t.url ? 'url' : t.title, attached: t.attached, ctx: t.browserContextId === undefined ? 'none' : 'set' }));
    await cdp.detach();
    await context.close();
  }
  // --- service worker ---
  {
    const context = await browser.newContext({ viewport: null });
    const page = await context.newPage();
    const swRequests = [];
    context.on('request', (r) => {
      if (r.serviceWorker()) swRequests.push(`sw ${r.resourceType()} ${path(r.url())}`);
    });
    const [sw] = await Promise.all([context.waitForEvent('serviceworker'), page.goto(`${base}/wk/sw/`)]);
    out.swUrl = path(sw.url());
    await page.waitForFunction(() => document.getElementById('out').textContent !== 'waiting');
    out.swPage = await page.textContent('#out');
    out.swScope = await sw.evaluate(() => [self.constructor.name, new URL(self.registration.scope).pathname, typeof clients.claim]);
    out.contextServiceWorkers = context.serviceWorkers().length;
    const fromSw = {};
    page.on('response', (r) => {
      const p = path(r.url());
      if (p.endsWith('.txt')) fromSw[p] = `${r.status()} ${r.fromServiceWorker()}`;
    });
    const bodies = await page.evaluate(async () => {
      const t = (u) => fetch(u).then((r) => r.text());
      return [await t('/wk/sw/answered.txt'), await t('/wk/sw/through.txt'), await t('/wk/sw/direct.txt')];
    });
    out.swBodies = bodies;
    out.fromServiceWorker = Object.keys(fromSw).sort().map((k) => `${k} ${fromSw[k]}`);
    out.swRequests = [...new Set(swRequests)].sort();
    await context.close();
  }
  return out;
}

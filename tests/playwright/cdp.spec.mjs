// Raw CDP domains Playwright relies on less directly: DOM, Accessibility,
// Emulation, console and page errors, response bodies.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages, html } from './harness.mjs';

pages['/cdp'] = html(
  'CDP',
  `<main><h1>Title</h1><nav><a href="/x">Home</a></nav>
  <button id="b" style="width: 120px; height: 30px">Save</button>
  <div id="wide" style="width: 100%; height: 10px"></div>
  <input type="checkbox" id="c" aria-label="Remember me"></main>
  <script>console.log('hello', 42); setTimeout(() => { throw new Error('late failure'); }, 50);</script>`,
);

test('console events identify the isolated world that logged them', { timeout: 10_000 }, async () => {
  const { context, page } = await newPage();
  try {
    const session = await context.newCDPSession(page);
    await session.send('Runtime.enable');
    const { frameTree } = await session.send('Page.getFrameTree');
    const { executionContextId } = await session.send('Page.createIsolatedWorld', {
      frameId: frameTree.frame.id,
      worldName: 'console-test',
    });
    const event = new Promise((resolve) => session.once('Runtime.consoleAPICalled', resolve));
    await session.send('Runtime.evaluate', {
      expression: 'console.log("isolated world")',
      contextId: executionContextId,
    });
    const message = await event;
    assert.equal(message.args[0].value, 'isolated world');
    assert.equal(message.executionContextId, executionContextId);
  } finally {
    await context.close();
  }
});

pages['/same-document'] = html('History', '<iframe src="/same-document-child"></iframe>');
pages['/same-document-child'] = html('Child history', '');

test('CDP reports script-driven same-document navigations', { timeout: 10_000 }, async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  try {
    await page.goto(`${base}/same-document`);
    const session = await context.newCDPSession(page);
    await session.send('Page.enable');
    const { frameTree } = await session.send('Page.getFrameTree');
    const events = [];
    session.on('Page.navigatedWithinDocument', (event) => events.push(event));
    await page.evaluate(() => {
      window.navigationSentinel = 42;
      history.pushState({}, '', '/push');
      history.replaceState({}, '', '/replacement');
      location.hash = 'fragment';
    });
    await page.evaluate(() => new Promise((resolve) => {
      window.addEventListener('popstate', resolve, { once: true });
      history.back();
    }));
    const child = page.frames().find((frame) => frame.parentFrame() === page.mainFrame());
    await child.evaluate(() => history.pushState({}, '', '/child-push'));

    assert.equal(await page.evaluate(() => window.navigationSentinel), 42);
    assert.equal(page.url(), `${base}/replacement`);
    assert.equal(child.url(), `${base}/child-push`);
    const main = frameTree.frame.id;
    const nested = frameTree.childFrames[0].frame.id;
    assert.deepEqual(events.map(({ frameId, url, navigationType }) => [
      frameId, new URL(url).pathname + new URL(url).hash, navigationType,
    ]), [
      [main, '/push', 'historyApi'],
      [main, '/replacement', 'historyApi'],
      [main, '/replacement#fragment', 'fragment'],
      [main, '/replacement', 'fragment'],
      [nested, '/child-push', 'historyApi'],
    ]);
  } finally {
    await context.close();
  }
});

test('CDP fragment navigation preserves the document and loader', { timeout: 10_000 }, async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  try {
    const url = `${base}/same-document`;
    await page.goto(url);
    const session = await context.newCDPSession(page);
    const before = (await session.send('Page.getFrameTree')).frameTree.frame;
    await page.evaluate(() => {
      window.fragmentChanges = [];
      addEventListener('hashchange', (event) => fragmentChanges.push(event.newURL));
    });
    const result = await session.send('Page.navigate', { url: `${url}#raw` });
    assert.equal(result.loaderId, undefined);
    assert.equal(await page.goto(`${url}#playwright`), null);
    assert.equal(await page.goto(`${url}#playwright`), null);
    await page.waitForFunction(() => window.fragmentChanges?.length === 2);
    assert.deepEqual(await page.evaluate(() => fragmentChanges), [
      `${url}#raw`, `${url}#playwright`,
    ]);
    const after = (await session.send('Page.getFrameTree')).frameTree.frame;
    assert.equal(after.loaderId, before.loaderId);
    assert.equal(after.url + (after.urlFragment ?? ''), `${url}#playwright`);
  } finally {
    await context.close();
  }
});

pages['/reload-metadata'] = html(
  'Reload',
  '<script>window.initialNavigation = [performance.navigation.type, chrome.loadTimes().navigationType]</script>',
);

test('reload sets navigation metadata before scripts and preserves history length', { timeout: 10_000 }, async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  try {
    await page.goto(`${base}/reload-metadata`);
    assert.deepEqual(await page.evaluate(() => initialNavigation), [0, 'Other']);
    const entries = await page.evaluate(() => {
      history.pushState(null, '', '#entry');
      return history.length;
    });
    await page.reload();
    assert.deepEqual(await page.evaluate(() => [initialNavigation, history.length]), [[1, 'Reload'], entries]);
    await Promise.all([
      page.waitForNavigation(),
      page.evaluate(() => { location.reload(); }),
    ]);
    assert.deepEqual(await page.evaluate(() => [initialNavigation, history.length]), [[1, 'Reload'], entries]);
  } finally {
    await context.close();
  }
});

pages['/delayed-document'] = (_req, res) => {
  setTimeout(() => {
    res.writeHead(200, { 'content-type': 'text/html' });
    res.end(html('Delayed document', '<p>ready</p>'));
  }, 100);
};

test('navigation timing includes time spent waiting for the document', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  try {
    await page.goto(`${base}/delayed-document`);
    const timing = await page.evaluate(() => ({
      origin: performance.timeOrigin,
      elapsed: performance.now(),
      legacy: performance.timing.toJSON(),
      chrome: window.chrome.loadTimes(),
    }));
    const { navigationStart, responseEnd, domLoading, domContentLoadedEventStart } = timing.legacy;
    assert.ok(responseEnd - navigationStart >= 75, JSON.stringify(timing));
    assert.ok(responseEnd <= domLoading);
    assert.ok(domLoading <= domContentLoadedEventStart);
    assert.ok(Math.abs(timing.origin - navigationStart) <= 1);
    assert.ok(timing.elapsed >= 75);
    assert.ok(timing.chrome.commitLoadTime - timing.chrome.startLoadTime >= 0.075);
    assert.ok(timing.chrome.commitLoadTime <= timing.chrome.finishDocumentLoadTime);
  } finally {
    await context.close();
  }
});

test('DOM and Accessibility domains on the real document', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/cdp`);
  const s = await context.newCDPSession(page);
  const { root } = await s.send('DOM.getDocument', { depth: -1 });
  assert.equal(root.nodeName, '#document');
  assert.equal(root.documentURL, `${base}/cdp`);
  const { nodeId } = await s.send('DOM.querySelector', { nodeId: root.nodeId, selector: '#b' });
  assert.ok(nodeId > 0);
  const { node } = await s.send('DOM.describeNode', { nodeId });
  assert.equal(node.localName, 'button');
  const { model } = await s.send('DOM.getBoxModel', { nodeId });
  assert.equal(model.width, 120);
  assert.equal(model.height, 30);
  const { object } = await s.send('DOM.resolveNode', { nodeId });
  assert.equal(object.subtype, 'node');
  assert.equal(object.className, 'HTMLButtonElement');
  assert.equal(object.description, 'button#b');
  const back = await s.send('DOM.requestNode', { objectId: object.objectId });
  assert.equal(back.nodeId, nodeId);
  const { outerHTML } = await s.send('DOM.getOuterHTML', { nodeId });
  assert.equal(outerHTML, '<button id="b" style="width: 120px; height: 30px">Save</button>');
  const { nodes } = await s.send('Accessibility.getFullAXTree');
  const roles = nodes.map((n) => n.role?.value);
  for (const r of ['RootWebArea', 'heading', 'link', 'button', 'checkbox']) assert.ok(roles.includes(r), roles.join(','));
  const q = await s.send('Accessibility.queryAXTree', { backendNodeId: root.backendNodeId, role: 'checkbox' });
  assert.equal(q.nodes.length, 1);
  assert.equal(q.nodes[0].name.value, 'Remember me');
  await context.close();
});

test('Emulation viewport reaches layout; console and page errors are reported', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  const logs = [];
  page.on('console', (m) => logs.push(`${m.type()}: ${m.text()}`));
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  const resp = await page.goto(`${base}/cdp`);
  assert.match(await resp.text(), /<h1>Title<\/h1>/);
  await page.waitForTimeout(150);
  assert.ok(logs.some((l) => l.startsWith('log: hello 42')), logs.join('|'));
  assert.ok(errors.some((e) => e.includes('late failure')), errors.join('|'));
  const s = await context.newCDPSession(page);
  const full = await page.evaluate(() => [innerWidth, innerHeight]);
  await s.send('Emulation.setDeviceMetricsOverride', { width: 800, height: 600, deviceScaleFactor: 0, mobile: false });
  assert.deepEqual(await page.evaluate(() => [innerWidth, innerHeight]), [800, 600]);
  // 100% of the body: the viewport minus the default 8px body margins.
  assert.equal(await page.evaluate(() => document.getElementById('wide').getBoundingClientRect().width), 784);
  await s.send('Emulation.clearDeviceMetricsOverride');
  assert.deepEqual(await page.evaluate(() => [innerWidth, innerHeight]), full);
  await context.close();
});

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

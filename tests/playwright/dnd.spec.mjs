// Drag and drop through Playwright's dragAndDrop (Input.setInterceptDrags,
// Input.dragIntercepted, Input.dispatchDragEvent). The expected event logs
// were recorded in Chrome for Testing 1243 (headless shell) with this page.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages } from './harness.mjs';

pages['/dnd'] = `<!doctype html><style>div{width:80px;height:80px;margin:20px;display:inline-block}</style>
<div id=src draggable=true style="background:red">src</div><div id=zone style="background:blue">zone</div><div id=plain style="background:green">plain</div>
<a id=link href="/target?x=1" style="display:inline-block;width:60px;height:30px">link</a>
<script>
window.log = [];
for (const t of ['pointerdown','mousedown','pointermove','mousemove','dragstart','drag','dragenter','dragleave','dragover','drop','dragend','mouseup','pointerup','click','pointercancel'])
  document.addEventListener(t, (e) => {
    if ((t === 'pointermove' || t === 'mousemove' || t === 'drag' || t === 'dragover') && log.length && log[log.length-1].startsWith(t + ':' + e.target.id)) return;
    let extra = '';
    if (e.dataTransfer) extra = ' types=' + [...e.dataTransfer.types].join(',') + ' data=' + JSON.stringify(e.dataTransfer.getData('text/plain')) + ' eff=' + e.dataTransfer.effectAllowed + '/' + e.dataTransfer.dropEffect;
    log.push(t + ':' + (e.target.id || e.target.nodeName) + extra);
  }, true);
document.getElementById('src').addEventListener('dragstart', (e) => e.dataTransfer.setData('text/plain', 'payload'));
document.getElementById('zone').addEventListener('dragover', (e) => e.preventDefault());
document.getElementById('zone').addEventListener('drop', (e) => { e.preventDefault(); window.dropped = e.dataTransfer.getData('text/plain'); });
</script>`;

const chrome = {
  "accepted": {
    "log": [
      "pointermove:src",
      "mousemove:src",
      "pointerdown:src",
      "mousedown:src",
      "pointermove:zone",
      "mousemove:zone",
      "dragstart:src types= data=\"\" eff=uninitialized/none",
      "pointercancel:zone",
      "drag:src types=text/plain data=\"\" eff=uninitialized/none",
      "dragenter:zone types=text/plain data=\"\" eff=all/copy",
      "dragover:zone types=text/plain data=\"\" eff=all/copy",
      "drop:zone types=text/plain data=\"payload\" eff=all/copy",
      "dragend:src types=text/plain data=\"\" eff=uninitialized/copy"
    ],
    "dropped": "payload"
  },
  "rejected": {
    "log": [
      "pointermove:src",
      "mousemove:src",
      "pointerdown:src",
      "mousedown:src",
      "pointermove:plain",
      "mousemove:plain",
      "dragstart:src types= data=\"\" eff=uninitialized/none",
      "pointercancel:plain",
      "drag:src types=text/plain data=\"\" eff=uninitialized/none",
      "dragenter:plain types=text/plain data=\"\" eff=all/copy",
      "dragover:plain types=text/plain data=\"\" eff=all/copy",
      "drag:src types=text/plain data=\"\" eff=uninitialized/none",
      "dragleave:plain types=text/plain data=\"\" eff=all/none",
      "dragend:src types=text/plain data=\"\" eff=uninitialized/none"
    ]
  },
  "link": {
    "log": [
      "pointermove:link",
      "mousemove:link",
      "pointerdown:link",
      "mousedown:link",
      "pointermove:zone",
      "mousemove:zone",
      "dragstart:link types=text/uri-list,text/plain,text/html data=\"BASE/target?x=1\" eff=uninitialized/none",
      "pointercancel:zone",
      "drag:link types=text/uri-list,text/plain,text/html data=\"\" eff=uninitialized/none",
      "dragenter:zone types=text/plain,text/uri-list,text/html data=\"\" eff=all/copy",
      "dragover:zone types=text/plain,text/uri-list,text/html data=\"\" eff=all/copy",
      "drop:zone types=text/plain,text/uri-list,text/html data=\"BASE/target?x=1\" eff=all/copy",
      "dragend:link types=text/uri-list,text/plain,text/html data=\"\" eff=uninitialized/copy"
    ],
    "dropped": "BASE/target?x=1"
  }
};

test('dragAndDrop: accepted and rejected drops and a link, event for event as in Chrome', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/dnd`);
  const out = {};
  await page.dragAndDrop('#src', '#zone');
  out.accepted = await page.evaluate(() => ({ log: log.splice(0), dropped: window.dropped }));
  await page.dragAndDrop('#src', '#plain');
  out.rejected = await page.evaluate(() => ({ log: log.splice(0) }));
  await page.dragAndDrop('#link', '#zone');
  out.link = await page.evaluate(() => ({ log: log.splice(0), dropped: window.dropped }));
  const want = JSON.parse(JSON.stringify(chrome).replaceAll('BASE', base));
  assert.deepEqual(out, want);
  await context.close();
});

test('a drag also runs on plain mouse input when the client does not intercept it', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/dnd`);
  const s = await context.newCDPSession(page);
  const box = async (sel) => page.locator(sel).boundingBox();
  const a = await box('#src');
  const b = await box('#zone');
  const mouse = (type, x, y, extra = {}) => s.send('Input.dispatchMouseEvent', { type, x, y, button: 'left', ...extra });
  await mouse('mouseMoved', a.x + 10, a.y + 10, { button: 'none' });
  await mouse('mousePressed', a.x + 10, a.y + 10, { clickCount: 1, buttons: 1 });
  await mouse('mouseMoved', b.x + 10, b.y + 10, { buttons: 1 });
  await mouse('mouseMoved', b.x + 12, b.y + 12, { buttons: 1 });
  await mouse('mouseReleased', b.x + 12, b.y + 12, { clickCount: 1, buttons: 0 });
  const got = await page.evaluate(() => ({ events: log.map((l) => l.split(' ')[0]), dropped: window.dropped }));
  assert.equal(got.dropped, 'payload');
  for (const e of ['dragstart:src', 'dragenter:zone', 'dragover:zone', 'drop:zone', 'dragend:src']) assert.ok(got.events.includes(e), e);
  // No mouseup or click once the drag has started.
  assert.ok(!got.events.includes('mouseup:zone') && !got.events.includes('click:zone'));
  await context.close();
});

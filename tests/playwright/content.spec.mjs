// page.setContent (document.open/write/close and the end of parsing it
// runs again) and JavaScript dialogs (alert, confirm, prompt), as with
// Chrome over CDP.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages, html } from './harness.mjs';

pages['/content-start'] = html(
  'Start',
  `<p id="old">old</p>
  <script>window.addEventListener('load', () => { window.oldLoads = (window.oldLoads || 0) + 1; });</script>`,
);

test('setContent replaces the document and waits for its load', async () => {
  const { context, page } = await newPage();
  // On the initial about:blank and after a navigation.
  await page.setContent('<div id="d"><b>x</b></div>');
  assert.equal(await page.innerHTML('#d'), '<b>x</b>');
  const { base } = await env();
  await page.goto(`${base}/content-start`);
  await page.setContent(
    `<p id="new">new</p><script>
      window.events = [document.readyState];
      document.addEventListener('readystatechange', () => events.push('rs:' + document.readyState));
      document.addEventListener('DOMContentLoaded', () => events.push('dcl'));
      window.addEventListener('load', () => events.push('load'));
    </script>`,
  );
  assert.equal(await page.locator('#old').count(), 0);
  assert.equal(await page.textContent('#new'), 'new');
  // The old document's listeners were erased by document.open().
  assert.equal(await page.evaluate(() => window.oldLoads), 1);
  assert.deepEqual(await page.evaluate(() => window.events), [
    'loading',
    'rs:interactive',
    'dcl',
    'rs:complete',
    'load',
  ]);
  await page.setContent('<p id="early">early</p>', { waitUntil: 'domcontentloaded' });
  assert.equal(await page.textContent('#early'), 'early');
  // Actions on content set this way.
  await page.setContent('<button id="b" onclick="this.textContent = \'clicked\'">go</button>');
  await page.click('#b');
  assert.equal(await page.textContent('#b'), 'clicked');
  await context.close();
});

test('document.write after document.open builds the page', async () => {
  const { context, page } = await newPage();
  const out = await page.evaluate(() => {
    document.open();
    document.write('<p id="a">a</p>');
    document.writeln('<p id="b">b</p>');
    document.close();
    return [document.querySelectorAll('p').length, document.readyState];
  });
  // close() runs the end of parsing itself, as in Chrome.
  assert.deepEqual(out, [2, 'complete']);
  await context.close();
});

test('alert, confirm and prompt reach page.on("dialog")', async () => {
  const { context, page } = await newPage();
  const seen = [];
  page.on('dialog', async (d) => {
    seen.push([d.type(), d.message(), d.defaultValue()]);
    if (d.type() === 'prompt') await d.accept('answer!');
    else if (d.message() === 'no') await d.dismiss();
    else await d.accept();
  });
  assert.equal(await page.evaluate(() => alert('hi')), undefined);
  assert.equal(await page.evaluate(() => confirm('yes')), true);
  assert.equal(await page.evaluate(() => confirm('no')), false);
  assert.equal(await page.evaluate(() => prompt('question?', 'yes.')), 'answer!');
  assert.deepEqual(seen, [
    ['alert', 'hi', ''],
    ['confirm', 'yes', ''],
    ['confirm', 'no', ''],
    ['prompt', 'question?', 'yes.'],
  ]);
  // A dialog from a click handler: the click returns once it is closed.
  await page.setContent('<button id="b" onclick="this.dataset.r = confirm(\'sure?\')">x</button>');
  await page.click('#b');
  assert.equal(await page.getAttribute('#b', 'data-r'), 'true');
  await context.close();
});

test('dialogs without a listener are dismissed, as Playwright does with Chrome', async () => {
  const { context, page } = await newPage();
  assert.equal(await page.evaluate(() => confirm('x')), false);
  assert.equal(await page.evaluate(() => prompt('x', 'd')), null);
  await context.close();
});

test('closing a page with an open dialog', async () => {
  const { context, page } = await newPage();
  const opened = page.waitForEvent('dialog');
  page.evaluate(() => alert('stuck')).catch(() => {});
  await opened;
  await page.close();
  // The worker is free again: another page of the context works.
  const p2 = await context.newPage();
  await p2.setContent('<p id="p">ok</p>');
  assert.equal(await p2.textContent('#p'), 'ok');
  await context.close();
});

pages['/bu'] = html('bu', '<p style="height:300px">x</p><script>addEventListener("beforeunload", (e) => e.preventDefault())</script>');
pages['/bu-rv'] = html('rv', '<p style="height:300px">x</p><script>onbeforeunload = () => "leave?"</script>');
pages['/bu-next'] = html('next', '<p>next</p>');

test('beforeunload: a dialog only after a gesture; dismiss stays, accept leaves (as in Chrome)', async () => {
  const { base } = await env();
  const { context, page: first } = await newPage();
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  // No user gesture: the page closes without asking.
  await first.goto(`${base}/bu`);
  const none = [];
  first.on('dialog', (d) => none.push(d.type()));
  await first.close({ runBeforeUnload: true });
  await wait(200);
  assert.ok(first.isClosed());
  assert.deepEqual(none, []);

  // After a click: the first dialog is dismissed (the page stays), the
  // second accepted (it closes).
  const p = await context.newPage();
  await p.goto(`${base}/bu`);
  await p.click('p');
  let n = 0;
  const seen = [];
  p.on('dialog', async (d) => {
    seen.push([d.type(), d.message(), d.defaultValue()]);
    if (n++ === 0) await d.dismiss();
    else await d.accept();
  });
  await p.close({ runBeforeUnload: true });
  await wait(200);
  assert.equal(p.isClosed(), false);
  await p.close({ runBeforeUnload: true });
  await wait(200);
  assert.equal(p.isClosed(), true);
  assert.deepEqual(seen, [['beforeunload', '', ''], ['beforeunload', '', '']]);

  // Navigation: staying aborts it, leaving goes on.
  const q = await context.newPage();
  await q.goto(`${base}/bu-rv`);
  await q.click('p');
  let m = 0;
  q.on('dialog', async (d) => (m++ === 0 ? d.dismiss() : d.accept()));
  await assert.rejects(q.goto(`${base}/bu-next`), /net::ERR_ABORTED/);
  assert.equal(await q.title(), 'rv');
  await q.goto(`${base}/bu-next`);
  assert.equal(await q.title(), 'next');
  await context.close();
});

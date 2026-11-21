// File uploads: page.setInputFiles with in-memory files (File,
// DataTransfer and input.files in the page) and with local paths
// (DOM.setFileInputFiles), and the multipart body a form then posts.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { env, newPage, pages, requests, html } from './harness.mjs';

pages['/upload'] = html(
  'Upload',
  `<form id="f" action="/uploaded" method="post" enctype="multipart/form-data">
    <input type="text" name="title" value="t1">
    <input type="file" id="file" name="doc" multiple>
    <button id="send">Send</button>
  </form>
  <script>
    window.events = [];
    for (const t of ['input', 'change']) document.getElementById('file').addEventListener(t, (e) => events.push(t + ':' + e.bubbles));
  </script>`,
);
pages['/uploaded'] = (req, res) => {
  res.writeHead(200, { 'content-type': 'text/html' });
  res.end('<p id="ok">ok</p>');
};

test('File, FileList and DataTransfer in the page', async () => {
  const { context, page } = await newPage();
  const r = await page.evaluate(async () => {
    const f = new File(['ab', new Uint8Array([99])], 'n.txt', { type: 'Text/Plain', lastModified: 42 });
    const dt = new DataTransfer();
    dt.items.add(f);
    dt.items.add('hello', 'text/plain');
    dt.setData('text/uri-list', '#c\nhttps://a.test/');
    const input = document.createElement('input');
    input.type = 'file';
    const empty = input.files;
    input.files = dt.files;
    return {
      file: [f instanceof Blob, f.name, f.type, f.size, f.lastModified, await f.text(), Object.prototype.toString.call(f)],
      dt: [dt.items.length, dt.files.length, dt.files === dt.files, dt.items === dt.items, dt.types, dt.getData('url'), dt.items[0].kind, dt.items[1].kind],
      input: [empty.length, input.files.length, input.files[0].name, input.files.item(0) === input.files[0], input.value],
      errors: [
        (() => { try { new File(['x']); } catch (e) { return e.constructor.name; } })(),
        (() => { try { input.value = 'x'; } catch (e) { return e.name; } })(),
      ],
      text: document.createElement('input').files,
    };
  });
  assert.deepEqual(r.file, [true, 'n.txt', 'text/plain', 3, 42, 'abc', '[object File]']);
  // dt.files is the same object, dt.items a new one each time (Chrome).
  assert.deepEqual(r.dt, [3, 1, true, false, ['text/plain', 'text/uri-list', 'Files'], 'https://a.test/', 'file', 'string']);
  assert.deepEqual(r.input, [0, 1, 'n.txt', true, 'C:\\fakepath\\n.txt']);
  assert.deepEqual(r.errors, ['TypeError', 'InvalidStateError']);
  assert.equal(r.text, null);
  await context.close();
});

test('setInputFiles with buffers, then a multipart form post', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/upload`);
  await page.setInputFiles('#file', [
    { name: 'a.txt', mimeType: 'text/plain', buffer: Buffer.from('hello file') },
    { name: 'b.bin', mimeType: '', buffer: Buffer.from([0, 1, 2]) },
  ]);
  assert.deepEqual(await page.evaluate(() => window.events), ['input:true', 'change:true']);
  assert.equal(await page.inputValue('#file'), 'C:\\fakepath\\a.txt');
  assert.deepEqual(await page.$eval('#file', (i) => [...i.files].map((f) => [f.name, f.type, f.size])), [
    ['a.txt', 'text/plain', 10],
    // Playwright sends an empty mimeType as application/octet-stream.
    ['b.bin', 'application/octet-stream', 3],
  ]);
  const before = requests.length;
  await Promise.all([page.waitForURL('**/uploaded'), page.click('#send')]);
  const post = requests.slice(before).find((r) => r.url === '/uploaded');
  assert.ok(post, 'the form was posted');
  const boundary = post.headers['content-type'].match(/boundary=(\S+)/)[1];
  assert.match(boundary, /^----WebKitFormBoundary[A-Za-z0-9]{16}$/);
  assert.equal(
    post.body,
    `--${boundary}\r\nContent-Disposition: form-data; name="title"\r\n\r\nt1\r\n` +
      `--${boundary}\r\nContent-Disposition: form-data; name="doc"; filename="a.txt"\r\nContent-Type: text/plain\r\n\r\nhello file\r\n` +
      `--${boundary}\r\nContent-Disposition: form-data; name="doc"; filename="b.bin"\r\nContent-Type: application/octet-stream\r\n\r\n\u0000\u0001\u0002\r\n` +
      `--${boundary}--\r\n`,
  );
  await context.close();
});

test('setInputFiles with a local path (DOM.setFileInputFiles)', async () => {
  const { base } = await env();
  const dir = mkdtempSync(path.join(tmpdir(), 'lb-upload-'));
  const file = path.join(dir, 'report.json');
  writeFileSync(file, '{"a":1}');
  try {
    const { context, page } = await newPage();
    await page.goto(`${base}/upload`);
    await page.setInputFiles('#file', file);
    assert.deepEqual(await page.$eval('#file', (i) => [i.files.length, i.files[0].name, i.files[0].type, i.value]), [
      1,
      'report.json',
      'application/json',
      'C:\\fakepath\\report.json',
    ]);
    assert.equal(await page.$eval('#file', (i) => i.files[0].text()), '{"a":1}');
    assert.deepEqual(await page.evaluate(() => window.events), ['input:true', 'change:true']);
    // Clearing.
    await page.setInputFiles('#file', []);
    assert.equal(await page.$eval('#file', (i) => i.files.length), 0);
    await context.close();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

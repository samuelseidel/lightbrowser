// Page.captureScreenshot through stock Playwright: page and element
// screenshots painted from LightBrowser's layout (lb-paint), checked for
// size and a few pixel colors. Loopback only.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import zlib from 'node:zlib';
import { createRequire } from 'node:module';
import { execSync } from 'node:child_process';
import path from 'node:path';
import { env, newPage, pages, html } from './harness.mjs';

// ---- tiny PNG codec (8-bit RGB/RGBA, non-interlaced) ----

const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};

function encodePng(w, h, rgba) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(td));
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr.set([8, 6, 0, 0, 0], 8);
  const raw = Buffer.alloc((w * 4 + 1) * h);
  for (let y = 0; y < h; y++) rgba.copy(raw, y * (w * 4 + 1) + 1, y * w * 4, (y + 1) * w * 4);
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw)),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

function decodePng(buf) {
  assert.deepEqual([...buf.subarray(1, 4)], [80, 78, 71], 'PNG signature');
  let pos = 8;
  let w, h, ctype;
  const idat = [];
  while (pos < buf.length) {
    const len = buf.readUInt32BE(pos);
    const type = buf.toString('latin1', pos + 4, pos + 8);
    const data = buf.subarray(pos + 8, pos + 8 + len);
    if (type === 'IHDR') {
      w = data.readUInt32BE(0);
      h = data.readUInt32BE(4);
      assert.equal(data[8], 8, 'bit depth');
      ctype = data[9];
    } else if (type === 'IDAT') idat.push(data);
    pos += 12 + len;
  }
  const bpp = ctype === 6 ? 4 : ctype === 2 ? 3 : assert.fail(`color type ${ctype}`);
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = w * bpp;
  const out = Buffer.alloc(w * h * 4);
  let prev = Buffer.alloc(stride);
  for (let y = 0; y < h; y++) {
    const f = raw[y * (stride + 1)];
    const line = Buffer.from(raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1)));
    for (let i = 0; i < stride; i++) {
      const a = i >= bpp ? line[i - bpp] : 0;
      const b = prev[i];
      const c = i >= bpp ? prev[i - bpp] : 0;
      let p;
      if (f === 0) p = 0;
      else if (f === 1) p = a;
      else if (f === 2) p = b;
      else if (f === 3) p = (a + b) >> 1;
      else {
        const pa = Math.abs(b - c), pb = Math.abs(a - c), pc = Math.abs(a + b - 2 * c);
        p = pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      line[i] = (line[i] + p) & 0xff;
    }
    for (let x = 0; x < w; x++) {
      for (let k = 0; k < 3; k++) out[(y * w + x) * 4 + k] = line[x * bpp + k];
      out[(y * w + x) * 4 + 3] = bpp === 4 ? line[x * bpp + 3] : 255;
    }
    prev = line;
  }
  return { width: w, height: h, data: out };
}

// JPEG decoding uses the jpeg-js copy bundled with Playwright, when found.
function jpegDecoder() {
  try {
    const require = createRequire(import.meta.url);
    const root = execSync('npm root -g').toString().trim();
    const pkg = require.resolve('playwright-core/package.json', { paths: [path.join(root, 'playwright')] });
    return require(path.join(path.dirname(pkg), 'lib/utilsBundle.js')).jpegjs;
  } catch {
    return null;
  }
}

const px = (img, x, y) => [...img.data.subarray((y * img.width + x) * 4, (y * img.width + x) * 4 + 4)];
const near = (got, want, tol = 2) => got.slice(0, 3).every((v, i) => Math.abs(v - want[i]) <= tol);
const assertPx = (img, x, y, want, tol = 2) =>
  assert.ok(near(px(img, x, y), want, tol), `pixel (${x},${y}) = ${px(img, x, y)}, want ${want}`);

// ---- the page ----

const dot = (() => {
  const d = Buffer.alloc(4 * 4 * 4);
  for (let i = 0; i < 16; i++) d.set([255, 0, 255, 255], i * 4);
  return encodePng(4, 4, d);
})();
pages['/shot/dot.png'] = (req, res) => {
  res.writeHead(200, { 'content-type': 'image/png' });
  res.end(dot);
};
const abs = (l, t) => `position:absolute;left:${l}px;top:${t}px`;
pages['/shot'] = html(
  'Shot',
  `<div id="red" style="${abs(10, 10)};width:100px;height:50px;background:rgb(255,0,0)"></div>
  <div id="box" style="${abs(150, 10)};width:80px;height:40px;background:rgb(0,0,255);border:5px solid rgb(0,128,0)"></div>
  <div id="half" style="${abs(10, 100)};width:50px;height:50px;background:rgb(0,0,0);opacity:0.5"></div>
  <canvas id="cv" width="40" height="40" style="${abs(300, 10)}"></canvas>
  <img id="img" src="/shot/dot.png" style="${abs(400, 10)};width:20px;height:20px">
  <p id="text" style="${abs(10, 200)};margin:0;font:40px Arial;color:rgb(0,0,0)">Hello</p>
  <div id="clip" style="${abs(500, 10)};width:30px;height:30px;overflow:hidden"><div style="width:300px;height:30px;background:rgb(0,160,160)"></div></div>
  <input id="field" value="typed" style="${abs(10, 300)}">
  <div id="far" style="${abs(10, 3000)};width:60px;height:30px;background:rgb(255,255,0)"></div>
  <script>
    const c = document.getElementById('cv').getContext('2d');
    c.fillStyle = 'rgb(0,200,0)'; c.fillRect(0, 0, 40, 40);
    // 1px checkerboard: every pixel is an edge, so readback noise would touch it.
    for (let y = 0; y < 8; y++) for (let x = 0; x < 8; x++) {
      c.fillStyle = (x + y) % 2 ? 'rgb(10,20,30)' : 'rgb(200,100,50)'; c.fillRect(x, y, 1, 1);
    }
  </script>`,
  '<style>body { margin: 0; background: rgb(240,240,240) }</style>',
);
pages['/shot-bare'] = html('Bare', '<div style="width:10px;height:10px;background:rgb(255,0,0)"></div>', '<style>body{margin:0}</style>');

async function open(path = '/shot') {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}${path}`);
  await page.waitForFunction(() => document.getElementById('img')?.complete ?? true);
  const m = await page.evaluate(() => ({ w: innerWidth, h: innerHeight, dpr: devicePixelRatio }));
  return { context, page, ...m };
}

test('page.screenshot paints the viewport at the persona size', async () => {
  const { context, page, w, h, dpr } = await open();
  const img = decodePng(await page.screenshot());
  assert.equal(img.width, Math.round(w * dpr));
  assert.equal(img.height, Math.round(h * dpr));
  const at = (x, y, want, tol) => assertPx(img, Math.floor(x * dpr), Math.floor(y * dpr), want, tol);
  at(60, 35, [255, 0, 0]); // background
  at(152, 12, [0, 128, 0]); // border
  at(190, 30, [0, 0, 255]); // padding box
  at(35, 125, [120, 120, 120], 3); // opacity 0.5 black over rgb(240)
  at(800, 500, [240, 240, 240]); // body background on the canvas
  at(330, 30, [0, 200, 0]); // canvas bitmap
  at(410, 20, [255, 0, 255]); // image
  at(515, 25, [0, 160, 160]); // inside the overflow clip
  at(560, 25, [240, 240, 240]); // clipped away
  // Text ink in the paragraph, none just below it.
  const box = await page.locator('#text').boundingBox();
  let ink = 0;
  for (let y = Math.floor(box.y * dpr); y < (box.y + box.height) * dpr; y++)
    for (let x = Math.floor(box.x * dpr); x < (box.x + box.width) * dpr; x++) if (px(img, x, y)[0] < 80) ink++;
  assert.ok(ink > 150 * dpr * dpr, `text ink pixels: ${ink}`);
  // The input's value is drawn too.
  const field = await page.locator('#field').boundingBox();
  let fieldInk = 0;
  for (let y = Math.floor(field.y * dpr); y < (field.y + field.height) * dpr; y++)
    for (let x = Math.floor((field.x + 4) * dpr); x < (field.x + 40) * dpr; x++) if (px(img, x, y)[0] < 80) fieldInk++;
  assert.ok(fieldInk > 10, `input text pixels: ${fieldInk}`);
  await context.close();
});

test('element screenshots, scrolled into view, and full page', async () => {
  const { context, page, dpr } = await open();
  const box = decodePng(await page.locator('#box').screenshot());
  assert.equal(box.width, Math.round(90 * dpr));
  assert.equal(box.height, Math.round(50 * dpr));
  assertPx(box, 1, 1, [0, 128, 0]);
  assertPx(box, box.width >> 1, box.height >> 1, [0, 0, 255]);

  const far = decodePng(await page.locator('#far').screenshot());
  assert.equal(far.width, Math.round(60 * dpr));
  assert.equal(far.height, Math.round(30 * dpr));
  assertPx(far, far.width >> 1, far.height >> 1, [255, 255, 0]);
  assert.ok((await page.evaluate(() => scrollY)) > 0, 'the element was scrolled into view');

  const full = decodePng(await page.screenshot({ fullPage: true }));
  const docH = await page.evaluate(() => document.documentElement.scrollHeight);
  assert.equal(full.height, Math.round(docH * dpr));
  assertPx(full, Math.floor(40 * dpr), Math.floor(3015 * dpr), [255, 255, 0]);
  assertPx(full, Math.floor(60 * dpr), Math.floor(35 * dpr), [255, 0, 0]);
  await context.close();
});

test('jpeg with quality and clip; omitBackground', async () => {
  const { context, page, dpr } = await open();
  const jpg = await page.screenshot({ type: 'jpeg', quality: 50, clip: { x: 10, y: 10, width: 100, height: 50 } });
  assert.deepEqual([...jpg.subarray(0, 2)], [0xff, 0xd8]);
  const jpeg = jpegDecoder();
  if (jpeg) {
    const img = jpeg.decode(jpg, { useTArray: true, formatAsRGBA: true });
    assert.equal(img.width, Math.round(100 * dpr));
    assert.equal(img.height, Math.round(50 * dpr));
    assertPx({ ...img, data: Buffer.from(img.data) }, img.width >> 1, img.height >> 1, [255, 0, 0], 12);
  }
  const big = await page.screenshot({ type: 'jpeg', quality: 100, clip: { x: 10, y: 10, width: 100, height: 50 } });
  assert.ok(big.length > jpg.length, `quality 100 (${big.length} B) is larger than 50 (${jpg.length} B)`);
  await context.close();

  const bare = await open('/shot-bare');
  const t = decodePng(await bare.page.screenshot({ omitBackground: true, clip: { x: 0, y: 0, width: 20, height: 20 } }));
  assert.equal(px(t, Math.floor(5 * bare.dpr), Math.floor(5 * bare.dpr))[3], 255);
  assert.equal(px(t, Math.floor(15 * bare.dpr), Math.floor(15 * bare.dpr))[3], 0, 'transparent background');
  const opaque = decodePng(await bare.page.screenshot({ clip: { x: 0, y: 0, width: 20, height: 20 } }));
  assert.deepEqual(px(opaque, Math.floor(15 * bare.dpr), Math.floor(15 * bare.dpr)), [255, 255, 255, 255]);
  await bare.context.close();
});

test('raw CDP: clip scale, captureBeyondViewport, fromSurface, canvas never noised', async () => {
  const { context, page, dpr } = await open();
  const cdp = await context.newCDPSession(page);
  const shot = async (params) => decodePng(Buffer.from((await cdp.send('Page.captureScreenshot', params)).data, 'base64'));
  const scaled = await shot({ format: 'png', clip: { x: 150, y: 10, width: 90, height: 50, scale: 2 }, fromSurface: true });
  assert.equal(scaled.width, Math.round(180 * dpr));
  assert.equal(scaled.height, Math.round(100 * dpr));
  assertPx(scaled, 2, 2, [0, 128, 0]);

  // The far element lies outside the viewport (no scrolling here).
  assert.equal(await page.evaluate(() => scrollY), 0);
  const clip = { x: 10, y: 3000, width: 60, height: 30, scale: 1 };
  const hidden = await shot({ clip });
  assertPx(hidden, hidden.width >> 1, hidden.height >> 1, [240, 240, 240]);
  const beyond = await shot({ clip, captureBeyondViewport: true });
  assertPx(beyond, beyond.width >> 1, beyond.height >> 1, [255, 255, 0]);

  // Canvas pixels exactly as drawn (readback noise would flip low bits).
  const cv = await shot({ clip: { x: 300, y: 10, width: 8, height: 8, scale: 1 / dpr } });
  assert.equal(cv.width, 8);
  for (let y = 0; y < 8; y++)
    for (let x = 0; x < 8; x++)
      assert.deepEqual(px(cv, x, y), (x + y) % 2 ? [10, 20, 30, 255] : [200, 100, 50, 255], `canvas pixel ${x},${y}`);

  await assert.rejects(cdp.send('Page.captureScreenshot', { clip: { x: 0, y: 0, width: 0, height: 10 } }), /0 width/);
  const metrics = await cdp.send('Page.getLayoutMetrics');
  assert.ok(metrics.cssContentSize.height >= 3030, JSON.stringify(metrics.cssContentSize));
  await context.close();
});

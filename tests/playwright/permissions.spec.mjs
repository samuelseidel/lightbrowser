// context.grantPermissions / clearPermissions (Browser.grantPermissions,
// Browser.resetPermissions) and geolocation (Emulation.setGeolocationOverride),
// with the results recorded in Chrome for Testing 1243.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, pages, html } from './harness.mjs';

pages['/perm'] = html('Permissions', '<p>p</p>');

const states = (page) =>
  page.evaluate(async () => {
    const out = {};
    for (const name of ['geolocation', 'camera', 'microphone', 'notifications', 'clipboard-read', 'midi']) {
      out[name] = (await navigator.permissions.query({ name })).state;
    }
    out.Notification = Notification.permission;
    return out;
  });

const position = (page, timeout) =>
  page.evaluate(
    (timeout) =>
      new Promise((ok) => {
        navigator.geolocation.getCurrentPosition(
          (p) => ok(['ok', p.coords.latitude, p.coords.longitude, p.coords.accuracy, p.coords.altitude]),
          (e) => ok(['err', e.code, e.message]),
          timeout ? { timeout } : undefined,
        );
      }),
    timeout,
  );

test('grantPermissions grants for an origin and denies the rest; clearPermissions resets', async () => {
  const { browser, base } = await env();
  const context = await browser.newContext({ viewport: null });
  const page = await context.newPage();
  await page.goto(`${base}/perm`);
  const before = await states(page);
  await context.grantPermissions(['geolocation', 'camera'], { origin: base });
  assert.deepEqual(await states(page), {
    geolocation: 'granted',
    camera: 'granted',
    microphone: 'denied',
    notifications: 'denied',
    'clipboard-read': 'denied',
    midi: 'denied',
    Notification: 'denied',
  });
  // An all-origins grant does not replace this origin's.
  await context.grantPermissions(['clipboard-read']);
  assert.equal((await states(page))['clipboard-read'], 'denied');
  await context.clearPermissions();
  assert.deepEqual(await states(page), before);
  await context.close();
});

test('geolocation from context options, setGeolocation, watchPosition and the timeout', async () => {
  const { browser, base } = await env();
  const context = await browser.newContext({
    viewport: null,
    geolocation: { latitude: 52.52, longitude: 13.4, accuracy: 7 },
    permissions: ['geolocation'],
  });
  const page = await context.newPage();
  await page.goto(`${base}/perm`);
  assert.deepEqual(await position(page), ['ok', 52.52, 13.4, 7, null]);
  await context.setGeolocation({ latitude: -33.9, longitude: 151.2 });
  assert.deepEqual(await position(page), ['ok', -33.9, 151.2, 0, null]);
  const watch = await page.evaluate(
    () =>
      new Promise((ok) => {
        const id = navigator.geolocation.watchPosition((p) => {
          navigator.geolocation.clearWatch(id);
          ok([id > 0, Object.keys(p.toJSON()), JSON.stringify(p.coords)]);
        });
      }),
  );
  assert.deepEqual(watch, [
    true,
    ['timestamp', 'coords'],
    '{"accuracy":0,"latitude":-33.9,"longitude":151.2,"altitude":null,"altitudeAccuracy":null,"heading":null,"speed":null}',
  ]);
  await context.close();

  // Granted but no position: the request times out.
  const c2 = await browser.newContext({ viewport: null, permissions: ['geolocation'] });
  const p2 = await c2.newPage();
  await p2.goto(`${base}/perm`);
  assert.deepEqual(await position(p2, 200), ['err', 3, 'Timeout expired']);
  await c2.close();
});

pages['/perm-worker.js'] = (req, res) => {
  res.writeHead(200, { 'content-type': 'text/javascript' });
  res.end(`onmessage = async () => { const out = {}; for (const name of ['geolocation', 'camera']) out[name] = (await navigator.permissions.query({ name })).state; postMessage(out); };`);
};

test('watchPosition hears every new position until clearWatch; workers see granted permissions', async () => {
  const { browser, base } = await env();
  const context = await browser.newContext({ viewport: null, geolocation: { latitude: 1, longitude: 2 }, permissions: ['geolocation'] });
  const page = await context.newPage();
  await page.goto(`${base}/perm`);
  const wait = () => new Promise((r) => setTimeout(r, 200));
  await page.evaluate(() => {
    window.seen = [];
    window.wid = navigator.geolocation.watchPosition((p) => seen.push([p.coords.latitude, p.coords.longitude]));
  });
  await wait();
  await context.setGeolocation({ latitude: 3, longitude: 4 });
  await wait();
  await page.evaluate(() => navigator.geolocation.clearWatch(wid));
  await context.setGeolocation({ latitude: 5, longitude: 6 });
  await wait();
  assert.deepEqual(await page.evaluate(() => seen), [[1, 2], [3, 4]]);

  const states = await page.evaluate(
    () =>
      new Promise((ok) => {
        const w = new Worker('/perm-worker.js');
        w.onmessage = (e) => ok(e.data);
        w.postMessage(1);
      }),
  );
  assert.deepEqual(states, { geolocation: 'granted', camera: 'denied' });
  await context.close();
});

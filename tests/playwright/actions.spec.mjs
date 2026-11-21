// Input and locator actions through the real DOM event pipeline.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages, html } from './harness.mjs';

pages['/form'] = html(
  'Form',
  `<h1 id="title">Sign up</h1>
  <p class="intro">Welcome <b>back</b>, friend.</p>
  <form id="f" action="/done" method="get">
    <label for="name">Name</label> <input id="name" name="name" autocomplete="off">
    <label>Email <input id="email" name="email" type="email"></label>
    <textarea id="bio" name="bio"></textarea>
    <label><input id="terms" type="checkbox" name="terms"> I agree</label>
    <input type="radio" name="plan" id="free" value="free" checked><label for="free">Free</label>
    <input type="radio" name="plan" id="pro" value="pro"><label for="pro">Pro</label>
    <select id="color" name="color"><option value="r">Red</option><option value="g">Green</option><option value="b">Blue</option></select>
    <div id="editor" contenteditable="true" style="min-height: 20px"></div>
    <button id="submit" type="submit">Create account</button>
  </form>
  <button id="counter" type="button" data-n="0">Count 0</button>
  <a id="next" href="/next?from=form">Next page</a>
  <div id="late"></div>
  <script>
    window.log = [];
    const rec = (e) => window.log.push(e.type + ':' + e.isTrusted);
    for (const t of ['pointerdown', 'mousedown', 'pointerup', 'mouseup', 'click', 'dblclick', 'focus', 'keydown', 'keypress', 'beforeinput', 'input', 'keyup', 'change'])
      document.getElementById('counter').addEventListener(t, rec, true), document.getElementById('name').addEventListener(t, rec, true);
    document.getElementById('counter').addEventListener('click', (e) => {
      const b = e.currentTarget; b.dataset.n = +b.dataset.n + 1; b.textContent = 'Count ' + b.dataset.n;
      window.clickInfo = { x: e.clientX, y: e.clientY, sx: e.screenX, sy: e.screenY, detail: e.detail, buttons: e.buttons, stack: new Error().stack };
    });
    setTimeout(() => { document.getElementById('late').innerHTML = '<span class="ready">ready</span>'; }, 300);
  </script>`,
);
pages['/next'] = html('Next', '<p id="where">second document</p><input id="again"><button id="b2" onclick="this.textContent=\'pressed\'">Press</button>');
pages['/done'] = (req, res) => {
  const q = new URL(req.url, 'http://x').searchParams;
  res.writeHead(200, { 'content-type': 'text/html' });
  res.end(html('Done', `<pre id="q">${[...q].map(([k, v]) => `${k}=${v}`).join('&')}</pre>`));
};

test('click and dblclick are trusted, in Chrome order, at the right coordinates', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/form`);
  await page.click('#counter');
  assert.equal(await page.textContent('#counter'), 'Count 1');
  const log = await page.evaluate(() => window.log.splice(0));
  // A button takes focus on mousedown (Windows and Linux Chrome).
  assert.deepEqual(log, ['pointerdown:true', 'mousedown:true', 'focus:true', 'pointerup:true', 'mouseup:true', 'click:true']);
  const box = await page.locator('#counter').boundingBox();
  const info = await page.evaluate(() => window.clickInfo);
  assert.ok(Math.abs(info.x - (box.x + box.width / 2)) <= 1, `clientX ${info.x} vs ${JSON.stringify(box)}`);
  assert.ok(Math.abs(info.y - (box.y + box.height / 2)) <= 1);
  assert.ok(info.sx >= info.x && info.sy > info.y, 'screen coordinates include the window position');
  assert.equal(info.detail, 1);
  // The page's own stack: no automation frames.
  assert.doesNotMatch(info.stack, /playwright|UtilityScript|InjectedScript|__puppeteer/i);
  await page.dblclick('#counter');
  assert.equal(await page.textContent('#counter'), 'Count 3');
  const dbl = await page.evaluate(() => window.log.splice(0).filter((t) => /click/.test(t)));
  assert.deepEqual(dbl, ['click:true', 'click:true', 'dblclick:true']);
  await context.close();
});

test('fill, type, press and selection in text controls', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/form`);
  await page.fill('#name', 'Ada');
  assert.equal(await page.inputValue('#name'), 'Ada');
  await page.fill('#name', 'Grace Hopper');
  assert.equal(await page.inputValue('#name'), 'Grace Hopper');
  await page.fill('#name', '');
  assert.equal(await page.inputValue('#name'), '');
  await page.evaluate(() => (window.log.length = 0));
  await page.locator('#name').pressSequentially('hi');
  assert.equal(await page.inputValue('#name'), 'hi');
  const typed = await page.evaluate(() => window.log.splice(0));
  assert.deepEqual(typed.slice(0, 5), ['keydown:true', 'keypress:true', 'beforeinput:true', 'input:true', 'keyup:true']);
  await page.press('#name', 'Backspace');
  assert.equal(await page.inputValue('#name'), 'h');
  await page.keyboard.type('ey');
  assert.equal(await page.inputValue('#name'), 'hey');
  await page.press('#name', 'ArrowLeft');
  await page.keyboard.type('!');
  assert.equal(await page.inputValue('#name'), 'he!y');
  await page.type('#bio', 'line one');
  await page.press('#bio', 'Enter');
  await page.keyboard.insertText('line two');
  assert.equal(await page.inputValue('#bio'), 'line one\nline two');
  await page.click('#editor');
  await page.keyboard.type('rich');
  assert.equal(await page.textContent('#editor'), 'rich');
  await context.close();
});

test('check, radio, selectOption and label activation', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/form`);
  await page.check('#terms');
  assert.equal(await page.isChecked('#terms'), true);
  await page.uncheck('#terms');
  assert.equal(await page.isChecked('#terms'), false);
  // Clicking the wrapping label toggles the box.
  await page.click('text=I agree');
  assert.equal(await page.isChecked('#terms'), true);
  await page.check('#pro');
  assert.equal(await page.isChecked('#pro'), true);
  assert.equal(await page.isChecked('#free'), false);
  // A `for` label activates its radio.
  await page.click('label[for=free]');
  assert.equal(await page.isChecked('#free'), true);
  assert.deepEqual(await page.selectOption('#color', 'g'), ['g']);
  assert.equal(await page.inputValue('#color'), 'g');
  assert.deepEqual(await page.selectOption('#color', { label: 'Blue' }), ['b']);
  assert.equal(await page.evaluate(() => document.getElementById('color').selectedIndex), 2);
  await context.close();
});

test('getByRole, getByText, getByLabel and locator reads', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/form`);
  assert.equal(await page.getByRole('heading', { name: 'Sign up' }).textContent(), 'Sign up');
  assert.equal(await page.getByRole('button', { name: 'Create account' }).getAttribute('id'), 'submit');
  assert.equal(await page.getByRole('link', { name: 'Next page' }).getAttribute('href'), '/next?from=form');
  assert.equal(await page.getByRole('checkbox').count(), 1);
  assert.equal(await page.getByRole('radio').count(), 2);
  await page.getByLabel('Name').fill('Labelled');
  assert.equal(await page.inputValue('#name'), 'Labelled');
  await page.getByLabel('Email').fill('a@b.test');
  assert.equal(await page.inputValue('#email'), 'a@b.test');
  assert.equal(await page.getByText('Welcome').innerText(), 'Welcome back, friend.');
  assert.equal(await page.locator('.intro').textContent(), 'Welcome back, friend.');
  assert.equal(await page.locator('#counter').getAttribute('data-n'), '0');
  assert.equal(await page.locator('#title').innerHTML(), 'Sign up');
  const tree = await page.accessibility?.snapshot?.().catch(() => null);
  if (tree) assert.ok(JSON.stringify(tree).includes('Sign up'));
  await context.close();
});

test('waitForSelector and waitForFunction', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/form`);
  const el = await page.waitForSelector('.ready', { timeout: 5000 });
  assert.equal(await el.textContent(), 'ready');
  await page.evaluate(() => setTimeout(() => (window.flag = 42), 200));
  const v = await page.waitForFunction(() => window.flag, null, { timeout: 5000 });
  assert.equal(await v.jsonValue(), 42);
  await page.waitForSelector('#late .ready', { state: 'visible' });
  await page.evaluate(() => document.getElementById('late').remove());
  await page.waitForSelector('#late', { state: 'detached', timeout: 5000 });
  await context.close();
});

test('link navigation, form submit and a second document', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/form`);
  await page.click('#next');
  await page.waitForURL(`${base}/next?from=form`);
  assert.equal(await page.title(), 'Next');
  // The new document works: evaluate, actions, locators.
  assert.equal(await page.textContent('#where'), 'second document');
  await page.fill('#again', 'still works');
  assert.equal(await page.inputValue('#again'), 'still works');
  await page.click('#b2');
  assert.equal(await page.textContent('#b2'), 'pressed');
  // Back to the form document, as a real history navigation.
  await page.goBack();
  assert.equal(page.url(), `${base}/form`);
  assert.equal(await page.title(), 'Form');
  await page.goto(`${base}/form`);
  await page.fill('#name', 'Ada');
  await page.fill('#email', 'ada@example.test');
  await page.check('#terms');
  await page.selectOption('#color', 'b');
  await Promise.all([page.waitForURL(/\/done\?/), page.click('#submit')]);
  assert.equal(await page.textContent('#q'), 'name=Ada&email=ada@example.test&bio=&terms=on&plan=free&color=b');
  // Enter in a text field submits too.
  await page.goto(`${base}/form`);
  await page.fill('#name', 'Enter');
  await Promise.all([page.waitForURL(/name=Enter/), page.press('#name', 'Enter')]);
  assert.equal(await page.title(), 'Done');
  await context.close();
});

test('number inputs sanitize exposed values and preserve in-progress exponents', async () => {
  const { context, page } = await newPage();
  try {
    await page.evaluate(() => {
      document.body.innerHTML = '<input id="number" type="number" value="+2">';
    });
    const number = page.locator('#number');
    assert.equal(await number.inputValue(), '');
    assert.deepEqual(await number.evaluate((input) => {
      return ['+2', '1.', '1e', '1e309', ' 1', 'Infinity', '-.5', '1e+2'].map((value) => {
        input.value = value;
        return [input.value, Number.isNaN(input.valueAsNumber) ? null : input.valueAsNumber];
      });
    }), [
      ['', null], ['', null], ['', null], ['', null], ['', null], ['', null],
      ['-.5', -0.5], ['1e+2', 100],
    ]);
    await number.evaluate((input) => { input.value = ''; input.focus(); });
    await page.keyboard.type('1e');
    assert.equal(await number.inputValue(), '');
    await page.keyboard.type('2');
    assert.equal(await number.inputValue(), '1e2');
  } finally {
    await context.close();
  }
});

test('number input filters keystrokes and pasted text after beforeinput', async () => {
  const { context, page } = await newPage();
  try {
    await page.evaluate(() => {
      document.body.innerHTML = '<input type="number">';
      const input = document.querySelector('input');
      window.numberEvents = [];
      for (const type of ['beforeinput', 'input']) {
        input.addEventListener(type, (event) => numberEvents.push([type, event.data]));
      }
      input.focus();
    });
    const number = page.locator('input');
    await page.keyboard.type('1x..2ee+-3');
    assert.equal(await number.inputValue(), '1.2e+3');
    assert.deepEqual(
      (await page.evaluate(() => numberEvents)).filter(([type]) => type === 'input').map(([, data]) => data),
      ['1', '.', '2', 'e', '+', '3'],
    );
    await number.evaluate((input) => { input.value = ''; numberEvents.length = 0; });
    await page.keyboard.insertText('1abc2');
    assert.equal(await number.inputValue(), '12');
    assert.deepEqual(await page.evaluate(() => numberEvents), [['beforeinput', '1abc2'], ['input', '12']]);
  } finally {
    await context.close();
  }
});

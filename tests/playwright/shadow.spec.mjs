// Shadow DOM and custom elements through Playwright: selectors pierce open
// shadow roots, clicks and typing reach elements inside them (ADR 0026).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { env, newPage, pages, html } from './harness.mjs';

pages['/shadow'] = html(
  'Shadow',
  `<x-card id="card"><span slot="title">Hello</span></x-card>
  <div id="host"></div>
  <script>
    customElements.define('x-card', class extends HTMLElement {
      constructor() {
        super();
        const r = this.attachShadow({ mode: 'open' });
        r.innerHTML = '<h2><slot name="title"></slot></h2>'
          + '<input id="who" placeholder="name">'
          + '<button id="go" type="button">Greet</button>'
          + '<p id="out"></p>';
        r.getElementById('go').addEventListener('click', (e) => {
          r.getElementById('out').textContent = 'Hi ' + r.getElementById('who').value;
        });
      }
    });
    // Seen from the document, the click is retargeted to the host.
    document.addEventListener('click', (e) => { window.clickTarget = e.composedPath()[0].id + ' ' + e.target.id; });
    const s = document.getElementById('host').attachShadow({ mode: 'closed' });
    s.innerHTML = '<button id="hidden">secret</button>';
  </script>`,
);

test('click and fill inside an open shadow root', async () => {
  const { base } = await env();
  const { context, page } = await newPage();
  await page.goto(`${base}/shadow`);
  // CSS and text selectors pierce open shadow roots.
  await page.fill('#who', 'Ada');
  await page.click('text=Greet');
  assert.equal(await page.textContent('#out'), 'Hi Ada');
  assert.equal(await page.evaluate(() => window.clickTarget), 'go card');
  assert.equal(await page.locator('x-card >> #go').getAttribute('type'), 'button');
  assert.equal(await page.getByRole('button', { name: 'Greet' }).count(), 1);
  // Closed shadow roots stay closed to selectors.
  assert.equal(await page.locator('#hidden').count(), 0);
  // The slotted light DOM child is visible through its slot.
  assert.equal(await page.isVisible('span[slot=title]'), true);
  // CDP sees both roots, children only when piercing.
  const cdp = await context.newCDPSession(page);
  const { root } = await cdp.send('DOM.getDocument', { depth: -1, pierce: true });
  const find = (n, id) => (n.attributes || []).join(' ').includes(`id ${id}`) ? n : [...(n.children || []), ...(n.shadowRoots || [])].map((c) => find(c, id)).find(Boolean);
  const card = find(root, 'card');
  assert.equal(card.shadowRoots[0].shadowRootType, 'open');
  assert.ok(find(card.shadowRoots[0], 'go'));
  assert.equal(find(root, 'host').shadowRoots[0].shadowRootType, 'closed');
  const flat = await cdp.send('DOM.getDocument', { depth: -1 });
  assert.equal(find(flat.root, 'card').shadowRoots[0].children, undefined);
  await context.close();
});

// page.pdf: Page.printToPDF with transferMode ReturnAsStream, read through
// IO.read / IO.close. Pages are laid out at the paper's width and embedded
// as images (no selectable text); the page does not notice the print.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { newPage } from './harness.mjs';

/** Page boxes ("/MediaBox [0 0 w h]") of a PDF. */
const boxes = (pdf) => [...pdf.toString('latin1').matchAll(/\/MediaBox \[0 0 ([\d.]+) ([\d.]+)\]/g)].map((m) => [Number(m[1]), Number(m[2])]);

test('page.pdf: pages, paper size, landscape, page ranges; the page is unchanged', async () => {
  const { context, page } = await newPage();
  await page.setContent(
    `<h1>Report</h1>${'<p>Paragraph text, quite a lot of it to fill pages.</p>'.repeat(120)}
    <script>window.resizes = 0; addEventListener('resize', () => resizes++);</script>`,
  );
  const before = await page.evaluate(() => [innerWidth, document.documentElement.scrollWidth]);
  const pdf = await page.pdf();
  assert.equal(pdf.subarray(0, 8).toString(), '%PDF-1.4');
  assert.ok(pdf.toString('latin1').trimEnd().endsWith('%%EOF'));
  const letter = boxes(pdf);
  assert.ok(letter.length >= 4 && letter.length <= 6, `${letter.length} pages`);
  assert.deepEqual(letter[0], [612, 792]);

  const a4 = boxes(await page.pdf({ format: 'A4', landscape: true }));
  // Playwright's A4 is 8.27 x 11.7 in.
  assert.deepEqual(a4[0], [842.4, 595.44]);
  assert.ok(a4.length > letter.length, 'landscape pages hold fewer lines');

  assert.equal(boxes(await page.pdf({ pageRanges: '2-3' })).length, 2);
  await assert.rejects(page.pdf({ pageRanges: '99' }), /Page range exceeds page count/);

  assert.deepEqual(await page.evaluate(() => [innerWidth, document.documentElement.scrollWidth]), before);
  assert.equal(await page.evaluate(() => window.resizes), 0);
  await context.close();
});

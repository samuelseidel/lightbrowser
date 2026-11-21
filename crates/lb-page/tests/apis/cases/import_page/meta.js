export const keys = Object.keys(import.meta).join(',') + ' ' + typeof import.meta.resolve + ' ' + Object.getPrototypeOf(import.meta);
export const r1 = import.meta.resolve('./x.js').replace(/^http:\/\/127\.0\.0\.1:\d+/, '');
export const r2 = import.meta.resolve('https://example.test/a/../b');
let r3; try { import.meta.resolve('bare'); } catch (e) { r3 = e.name + ': ' + e.message; }
export { r3 };

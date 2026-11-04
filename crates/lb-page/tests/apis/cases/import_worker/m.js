import { v } from './lib.js';
const o = ['static ' + v];
const m = await import('./lib.js');
o.push('same ' + (m.v === v));
const d = await import('data:text/javascript,export default 7');
o.push('data ' + d.default);
try { await import('./syntax.js'); } catch (e) { o.push(e.name); }
postMessage(o.join(' | '));

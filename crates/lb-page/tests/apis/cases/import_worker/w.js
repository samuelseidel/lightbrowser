(async () => {
  var o = [];
  try { var m = await import('./lib.js'); o.push(m.v + ' ' + m.meta); } catch (e) { o.push('err ' + e.name + ': ' + e.message); }
  try { await import('./missing.js'); } catch (e) { o.push(e.name + ': ' + e.message.replace(/127\.0\.0\.1:\d+/, 'HOST')); }
  try { await import('bare'); } catch (e) { o.push(e.name + ': ' + e.message); }
  postMessage(o.join(' | '));
})();

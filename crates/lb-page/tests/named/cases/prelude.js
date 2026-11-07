// Shared by every case: `log(label, value)` appends a line to `L`
// (`note` a raw string); `fmt` names a value without touching anything a
// test looks at.
const L = [];
const WP = Object.getPrototypeOf(Window.prototype);
function fmt(v) {
  try {
    if (v === undefined) return 'undefined';
    if (v === null) return 'null';
    if (typeof v === 'function') return 'function ' + v.name;
    if (typeof v !== 'object') return typeof v + ' ' + String(v);
    if (v === window) return 'window';
    for (let i = 0; i < frames.length; i++) if (frames[i] === v) return 'frames[' + i + ']';
    if (v === WP) return 'WP';
    if (v.nodeType === 1) {
      const n = v.getAttribute('name');
      return '<' + v.localName + (v.id ? '#' + v.id : '') + (n !== null ? ' name=' + n : '') +
        (v.ownerDocument === document ? '' : ' other-doc') + '>';
    }
    const tag = Object.prototype.toString.call(v);
    if (typeof v.length === 'number' && typeof v.item === 'function')
      return tag + '(' + Array.prototype.map.call(v, fmt).join(',') + ')';
    return tag;
  } catch (e) {
    return 'fmt threw ' + e.name;
  }
}
function desc(d) {
  if (d === undefined) return 'undefined';
  const parts = [];
  if ('value' in d) parts.push('value=' + fmt(d.value));
  if ('get' in d) parts.push('get=' + fmt(d.get));
  if ('set' in d) parts.push('set=' + fmt(d.set));
  for (const k of ['writable', 'enumerable', 'configurable']) if (k in d) parts.push(k + '=' + d[k]);
  return '{' + parts.join(' ') + '}';
}
function log(label, v) {
  L.push(label + ': ' + fmt(v));
}
function note(label, s) {
  L.push(label + ': ' + s);
}
function attempt(label, f) {
  try {
    const r = f();
    note(label, 'ok ' + fmt(r));
  } catch (e) {
    note(label, 'threw ' + e.name + ': ' + e.message);
  }
}

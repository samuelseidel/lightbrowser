// Selection.modify battery: from every caret position in #t (and a few
// ranges), every alter x direction x granularity, and walks of repeated
// moves. A state is "A-Fd" (anchor and focus as indices into R.table of
// "path:offset" positions, path = child indices from the document; one
// index for a caret) plus the first letter of `direction`.
function path(n) {
  var p = [];
  while (n && n.parentNode) { p.unshift(Array.prototype.indexOf.call(n.parentNode.childNodes, n)); n = n.parentNode; }
  return p.join('.');
}
function node(p) {
  var n = document;
  if (p !== '') p.split('.').forEach(function (i) { n = n.childNodes[+i]; });
  return n;
}
var T = [], TI = {};
function idx(n, o) {
  if (!n) return -1;
  var k = path(n) + ':' + o;
  if (!(k in TI)) { TI[k] = T.length; T.push(k); }
  return TI[k];
}
function len(n) { return n.nodeType === 3 || n.nodeType === 8 ? n.data.length : n.childNodes.length; }
var root = document.getElementById('t');
var starts = [];
(function walk(n) {
  for (var i = 0; i <= len(n); i++) starts.push([n, i]);
  if (n.nodeType === 1) for (var c = n.firstChild; c; c = c.nextSibling) walk(c);
})(root);
var s = getSelection();
var ALTER = ['move', 'extend'], DIR = ['forward', 'backward', 'left', 'right'];
var GRAN = ['character', 'word', 'sentence', 'line', 'paragraph', 'lineboundary', 'sentenceboundary', 'paragraphboundary', 'documentboundary'];
function state() {
  var a = idx(s.anchorNode, s.anchorOffset), f = idx(s.focusNode, s.focusOffset);
  var d = s.direction[0];
  return (a === f ? f : a + '-' + f) + d;
}
var R = { starts: [], canon: [], res: [] };
starts.forEach(function (st) {
  var n = st[0], o = st[1];
  R.starts.push(idx(n, o));
  // canonical: extend by 0 (a character forward then back) from the caret
  s.collapse(n, o); s.modify('extend', 'forward', 'character'); s.modify('extend', 'backward', 'character');
  R.canon.push(state());
  var row = [];
  ALTER.forEach(function (al) { DIR.forEach(function (di) { GRAN.forEach(function (g) {
    s.collapse(n, o); s.modify(al, di, g); row.push(state());
  }); }); });
  R.res.push(row);
});
// Ranges: forward and backward selections between some text positions.
var texts = [];
(function walk(n) { if (n.nodeType === 3 && n.data.trim()) texts.push(n); for (var c = n.firstChild; c; c = c.nextSibling) walk(c); })(root);
R.ranges = [];
for (var i = 0; i + 1 < texts.length; i += 2) {
  var a = texts[i], b = texts[i + 1];
  [[a, 1, b, 1], [b, 1, a, 1], [a, 0, a, a.data.length]].forEach(function (q) {
    var row = [idx(q[0], q[1]), idx(q[2], q[3])];
    ALTER.forEach(function (al) { DIR.forEach(function (di) { GRAN.forEach(function (g) {
      s.setBaseAndExtent(q[0], q[1], q[2], q[3]); s.modify(al, di, g); row.push(state());
    }); }); });
    R.ranges.push(row);
  });
}
// Repeated moves keep the line x position.
R.walks = [];
GRAN.forEach(function (g) {
  ['forward', 'backward'].forEach(function (di) {
    var w = [];
    if (di === 'forward') s.collapse(root, 0); else s.collapse(root, root.childNodes.length);
    for (var k = 0; k < 400; k++) {
      var before = state();
      s.modify('move', di, g);
      w.push(state());
      if (state() === before) break;
    }
    R.walks.push(w);
  });
});
R.table = T;
R.desc = T.map(function (k) {
  var i = k.lastIndexOf(':'), n = node(k.slice(0, i)), o = +k.slice(i + 1);
  if (n.nodeType === 3) return JSON.stringify(n.data.slice(0, o) + '|' + n.data.slice(o));
  var c = n.childNodes[o];
  return '<' + n.nodeName + '>[' + o + (c ? ' before ' + (c.nodeType === 3 ? JSON.stringify(c.data.slice(0, 8)) : c.nodeName) : ' end') + ']';
});
window.R = R;
document.title = 'done';

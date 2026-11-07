/* shared helpers, loaded synchronously in <head> */
(function (g) {
  'use strict';
  function el(tag, attrs, children) {
    var n = document.createElement(tag);
    if (attrs) for (var k in attrs) {
      if (k === 'text') n.textContent = attrs[k];
      else if (k === 'class') n.className = attrs[k];
      else n.setAttribute(k, attrs[k]);
    }
    if (children) for (var i = 0; i < children.length; i++) n.appendChild(children[i]);
    return n;
  }
  function slug(s) { return String(s).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, ''); }
  function money(n) { return '$' + (Math.round(n * 100) / 100).toFixed(2); }
  function mark(n) { document.documentElement.setAttribute('data-bench-rendered', String(n)); }
  g.bench = { el: el, slug: slug, money: money, mark: mark, ready: [] };
})(window);

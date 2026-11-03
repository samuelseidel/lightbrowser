// Page-observable JS surface, as text (tools/surface_diff, ADR 0016).
// Runs on `load` and writes into #o; `fetch --dump html` and CDP read it.
(() => {
  const fts = Function.prototype.toString;
  const err = (f) => { try { const r = f(); return 'ok:' + (typeof r); } catch (e) { return 'throw:' + (e && e.constructor && e.constructor.name) + ':' + (e && e.message); } };
  const keys = (o) => Reflect.ownKeys(o).map(String).join(',');
  function surface(G, tag, out) {
    const seen = new Set();
    const label = (o) => o === null ? 'null'
      : (typeof o === 'function' ? 'fn:' + o.name : 'obj:' + (G.Object.prototype.toString.call(o)));
    function walk(o, path, depth) {
      if (o === null || (typeof o !== 'object' && typeof o !== 'function') || seen.has(o)) return;
      seen.add(o);
      out.push(path + ' __proto__=' + label(Object.getPrototypeOf(o)) + ' ext=' + Object.isExtensible(o));
      for (const k of Reflect.ownKeys(o)) {
        const d = Object.getOwnPropertyDescriptor(o, k);
        let line = path + '.' + String(k) + ' ';
        if (d.get || d.set) {
          line += 'a';
          if (d.get) line += ' g[' + d.get.name + '/' + d.get.length + ' ' + fts.call(d.get) + ' ' + (path === tag ? '' : err(() => d.get.call({}))) + ']';
          if (d.set) line += ' s[' + d.set.name + '/' + d.set.length + ' ' + fts.call(d.set) + ']';
        } else {
          const v = d.value;
          line += typeof v;
          if (typeof v === 'function') {
            line += ':' + v.name + '/' + v.length + ' ' + fts.call(v);
            if (path !== tag && !new RegExp('^' + tag + '\\.(Function|Object|Array|Promise|Reflect|Proxy|eval|Atomics)').test(path)) line += ' ' + err(() => v.call({}));
            if (path === tag && /^[A-Z]/.test(String(k)) && v.prototype && Object.getOwnPropertyDescriptor(v, 'prototype') && !Object.getOwnPropertyDescriptor(v, 'prototype').writable) line += ' callnonew=' + err(() => v());
          } else if (v === null || typeof v !== 'object') line += '=' + String(v);
          if (d.writable) line += 'w';
        }
        if (d.enumerable) line += 'e';
        if (d.configurable) line += 'c';
        out.push(line);
        if (depth < 2 && d.value && (typeof d.value === 'function' || typeof d.value === 'object')) {
          walk(d.value, path + '.' + String(k), depth + 1);
          if (typeof d.value === 'function' && d.value.prototype) walk(d.value.prototype, path + '.' + String(k) + '.prototype', depth + 1);
        }
      }
    }
    walk(G, tag, 0);
  }
  function run() {
    const out = [];
    const f = document.getElementById('f1').contentWindow;
    const g = document.getElementById('f2').contentWindow;
    const h = document.getElementById('f3');
    out.push('KEYS0 ' + keys(window));
    out.push('FKEYS0 ' + keys(f));
    // Cross-realm, before anything in the child realms was touched: the
    // first touch of a lazy interface object comes from this realm.
    const X = [];
    const B = f.Blob;
    X.push(B !== Blob, B === f.eval('Blob'), Object.getPrototypeOf(B.prototype) === f.Object.prototype, B.prototype.constructor === B,
      new B(['x']) instanceof f.Blob, new B(['x']) instanceof Blob, fts.call(B), B.name, B.length);
    const D = f.HTMLDivElement;
    X.push(Object.getPrototypeOf(D) === f.HTMLElement, Object.getPrototypeOf(D.prototype) === f.HTMLElement.prototype,
      f.document.createElement('div') instanceof D, f.document.createElement('div') instanceof HTMLDivElement,
      f.document.createElement('div').constructor === D, Object.getPrototypeOf(f.document.body) === f.HTMLBodyElement.prototype);
    try { f.Node.prototype.appendChild.call({}); } catch (e) { X.push('appendChild', e instanceof f.TypeError, e instanceof TypeError, e.message); }
    try { Node.prototype.appendChild.call(f.document.body, 1); } catch (e) { X.push('appendChild2', e instanceof f.TypeError, e instanceof TypeError, e.message); }
    try { new f.Event(); } catch (e) { X.push('Event', e instanceof f.TypeError, e.message); }
    try { f.Event('x'); } catch (e) { X.push('Event()', e instanceof f.TypeError, e.message); }
    // Not dumped: a method or getter of one realm called on a node of
    // another document resolves the node in the wrong document (open bug,
    // the same in M7; see docs/ROADMAP.md).
    X.push(f.document.body instanceof f.Node, f.document.body instanceof Node, Node[Symbol.hasInstance].call(f.Node, document.body));
    class Sub extends f.Blob {}
    X.push(new Sub([]) instanceof f.Blob, Object.getPrototypeOf(Sub) === f.Blob, fts.call(f.URL), f.webkitURL === f.URL, f.webkitURL.name);
    X.push(typeof f.DOMException, Object.getPrototypeOf(f.DOMException.prototype) === f.Error.prototype, Object.getPrototypeOf(f.DOMException) === f.Function.prototype);
    X.push(String(Object.getOwnPropertyDescriptor(f, 'Text') && JSON.stringify(Object.keys(Object.getOwnPropertyDescriptor(f, 'Text')))));
    out.push('XREALM ' + X.join(','));
    out.push('FKEYS1 ' + keys(f));
    // Changes made by a page to lazy interface objects (in a second child).
    const Y = [];
    Y.push(g.eval('delete Blob; typeof Blob'), g.eval('XMLHttpRequest = 5; XMLHttpRequest'), g.eval('window.Text = 7; Text'));
    Object.defineProperty(g, 'Comment', { get() { return 9; }, configurable: true });
    Y.push(g.Comment, g.eval('typeof Comment'), g.eval('Object.getOwnPropertyNames(window).indexOf("Blob")'), g.eval('"Blob" in window'));
    Y.push(g.eval('var Range = 3; Range'), g.eval('function Attr() { return 1 } typeof Attr'));
    Y.push(g.eval('Object.freeze(window.Node); Object.isFrozen(Node)'), g.eval('Object.getOwnPropertyNames(window).length'));
    out.push('MUT ' + Y.join(','));
    out.push('GKEYS ' + keys(g));
    // A detached realm: first touch after the frame is gone.
    const w = h.contentWindow;
    h.remove();
    const Z = [];
    Z.push(err(() => w.Blob), err(() => w.URL && w.URL.name), err(() => typeof w.HTMLElement), err(() => Object.getOwnPropertyNames(w).length));
    out.push('DETACHED ' + Z.join(','));
    surface(window, 'g', out);
    surface(f, 'f', out);
    out.push('KEYS2 ' + keys(window));
    out.push('FKEYS2 ' + keys(f));
    out.push('MISC ' + [webkitURL === URL, URL.name, webkitURL.name, Object.getPrototypeOf(DOMException.prototype) === Error.prototype,
      Object.getPrototypeOf(HTMLDivElement) === HTMLElement, document.createElement('div').constructor === HTMLDivElement,
      Image.prototype === HTMLImageElement.prototype, typeof Option, String(Object.getOwnPropertyDescriptor(window, 'Node').writable)].join(','));
    document.getElementById('o').textContent = out.join('\n');
    document.documentElement.setAttribute('data-done', '1');
  }
  window.addEventListener('load', () => { try { run(); } catch (e) { document.getElementById('o').textContent = 'DUMP FAILED ' + e + '\n' + e.stack; document.documentElement.setAttribute('data-done', '1'); } });
})();

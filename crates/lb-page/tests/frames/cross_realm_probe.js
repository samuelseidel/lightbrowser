// Cross-realm DOM calls (frames, ADR 0012): a method or getter of one
// realm called on a node of another document. Each line was checked
// against Chrome 153 (Chrome for Testing 153.0.8010.12, loopback).
(() => {
  const err = (f) => { try { const r = f(); return 'ok:' + (typeof r === 'object' || typeof r === 'function' ? typeof r : JSON.stringify(r)); } catch (e) { return 'throw:' + (e && e.constructor && e.constructor.name) + ':' + (e && e.message); } };
  const out = [];
  const T = (lab, fn) => { try { fn(); } catch (e) { out.push(lab + ' = PROBE-THROW ' + e); } };
  const P = (k, ...v) => out.push(k + ' = ' + v.map(String).join(','));
    const f = document.getElementById('f1').contentWindow;
    const fd = f.document;
    // (a) cross-realm
    T('a1 nodeName', () => { P('a1 nodeName', err(() => Object.getOwnPropertyDescriptor(f.Node.prototype, 'nodeName').get.call(document.body))); });
    const i = document.createElement('i');
    T('a2 appendChild', () => { P('a2 appendChild', err(() => f.Node.prototype.appendChild.call(document.body, i) === i), document.body.lastChild === i); });
    const fi = fd.createElement('i');
    T('a3 main appendChild frame', () => { P('a3 main appendChild frame', err(() => Node.prototype.appendChild.call(fd.body, fi) === fi), fd.body.lastChild === fi, fi.ownerDocument === fd); });
    const s = fd.createElement('span'); s.id = 'sp'; s.textContent = 'x';
    let clicks = 0; s.addEventListener('click', () => clicks++);
    T('a4 adopt via appendChild', () => { P('a4 adopt via appendChild', document.body.appendChild(s) === s, s.ownerDocument === document, Object.getPrototypeOf(s) === f.HTMLSpanElement.prototype, Object.getPrototypeOf(s) === HTMLSpanElement.prototype, document.getElementById('sp') === s, s.parentNode === document.body); });
    T('s.click(); s.dispatc', () => { s.click(); s.dispatchEvent(new Event('click')); P('a5 listeners after adopt', clicks); });
    const t = fd.createElement('b'); fd.body.appendChild(t);
    T('a6 adoptNode', () => { P('a6 adoptNode', document.adoptNode(t) === t, t.parentNode, t.ownerDocument === document, fd.body.contains(t)); });
    T('a7 cloneNode xrealm', () => { P('a7 cloneNode xrealm', err(() => Object.getPrototypeOf(f.Node.prototype.cloneNode.call(document.body)) === HTMLBodyElement.prototype), err(() => Object.getPrototypeOf(f.Node.prototype.cloneNode.call(document.body)) === f.HTMLBodyElement.prototype)); });
    T('a8 firstChild id', () => { P('a8 firstChild id', err(() => Object.getOwnPropertyDescriptor(f.Node.prototype, 'firstChild').get.call(document.body) === document.body.firstChild)); });
    T('try { f.Node.prototy', () => { try { f.Node.prototype.appendChild.call(document.body, document.documentElement); } catch (e) { P('a9 err realm', e instanceof f.DOMException, e instanceof DOMException, e.name, e.message); } });
    T('a10 rect', () => { P('a10 rect', err(() => f.Element.prototype.getBoundingClientRect.call(document.body) instanceof DOMRect), err(() => f.Element.prototype.getBoundingClientRect.call(document.body) instanceof f.DOMRect)); });
    T('a11 childNodes', () => { P('a11 childNodes', err(() => Object.getOwnPropertyDescriptor(f.Node.prototype, 'childNodes').get.call(document.body) === document.body.childNodes), err(() => Object.getOwnPropertyDescriptor(f.Node.prototype, 'childNodes').get.call(document.body) instanceof NodeList)); });
    T('a12 classList', () => { P('a12 classList', err(() => { const c = Object.getOwnPropertyDescriptor(f.Element.prototype, 'classList').get.call(document.body); f.DOMTokenList.prototype.add.call(c, 'zz'); return [c === document.body.classList, document.body.className, c instanceof DOMTokenList].join('/'); })); });
    T('a13 ownerDocument', () => { P('a13 ownerDocument', err(() => Object.getOwnPropertyDescriptor(f.Node.prototype, 'ownerDocument').get.call(document.body) === document)); });
    T('a14 createElement xrealm', () => { P('a14 createElement xrealm', err(() => { const e = f.Document.prototype.createElement.call(document, 'div'); return [e.ownerDocument === document, Object.getPrototypeOf(e) === HTMLDivElement.prototype, Object.getPrototypeOf(e) === f.HTMLDivElement.prototype].join('/'); })); });
    T('a15 querySelector xrealm', () => { P('a15 querySelector xrealm', err(() => f.Document.prototype.querySelector.call(document, 'body') === document.body)); });
    T('a16 insertBefore foreign', () => { P('a16 insertBefore foreign', err(() => { const x = fd.createElement('u'); const y = fd.createElement('em'); x.appendChild(y); document.body.insertBefore(x, document.body.firstChild); return [x.ownerDocument === document, y.ownerDocument === document, x.firstChild === y, Object.getPrototypeOf(y) === f.HTMLElement.prototype].join('/'); })); });
    T('a17 replaceChildren foreign', () => { P('a17 replaceChildren foreign', err(() => { const d = document.createElement('div'); const x = fd.createElement('u'); d.replaceChildren(x, 'q'); return [d.childNodes.length, x.ownerDocument === document].join('/'); })); });
    T('a18 importNode', () => { P('a18 importNode', err(() => { const x = fd.body; const c = document.importNode(x, true); return [c.ownerDocument === document, Object.getPrototypeOf(c) === HTMLBodyElement.prototype, x.ownerDocument === fd].join('/'); })); });
    T('a19 append dom parser doc node', () => { P('a19 append dom parser doc node', err(() => { const pd = new DOMParser().parseFromString('<p id=dp>hi</p>', 'text/html'); const p = pd.getElementById('dp'); document.body.append(p); return [p.ownerDocument === document, document.getElementById('dp') === p, pd.getElementById('dp')].join('/'); })); });
    T('a20 frame innerHTML on top', () => { P('a20 frame innerHTML on top', err(() => { const d = document.createElement('div'); Object.getOwnPropertyDescriptor(f.Element.prototype, 'innerHTML').set.call(d, '<b>1</b>'); return [d.firstChild.ownerDocument === document, Object.getPrototypeOf(d.firstChild) === HTMLElement.prototype].join('/'); })); });
    T('a21 frame textContent of top body', () => { P('a21 frame textContent of top body', err(() => typeof Object.getOwnPropertyDescriptor(f.Node.prototype, 'textContent').get.call(document.getElementById('sp')))); });
    T('a22 compareDocumentPosition', () => { P('a22 compareDocumentPosition', err(() => document.body.compareDocumentPosition(fd.body))); });
    T('a23 contains', () => { P('a23 contains', err(() => document.body.contains(fd.body))); });
    T('a24 isEqualNode', () => { P('a24 isEqualNode', err(() => document.createElement('p').isEqualNode(fd.createElement('p')))); });
    T('a25 adopt document', () => { P('a25 adopt document', err(() => document.adoptNode(fd))); });
    T('a26 adopt html', () => { P('a26 adopt html', err(() => { const h = fd.documentElement; document.adoptNode(h); return [h.ownerDocument === document, fd.documentElement].join('/'); })); });
  return out.join('\n');
})()

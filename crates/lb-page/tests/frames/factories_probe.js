// Legacy factory functions (Image, Audio, Option) as in Chrome 153
// (checked against Chrome for Testing 153.0.8010.12, loopback).
(() => { const out = []; const fts = Function.prototype.toString;
const err = (f) => { try { const r = f(); return 'ok:' + (typeof r === 'object' || typeof r === 'function' ? typeof r : JSON.stringify(r)); } catch (e) { return 'throw:' + (e && e.constructor && e.constructor.name) + ':' + (e && e.message); } };
const f = document.getElementById('f1').contentWindow; const fd=f.document;
    const C = [];
    for (const [n, I] of [['Image', 'HTMLImageElement'], ['Audio', 'HTMLAudioElement'], ['Option', 'HTMLOptionElement']]) {
      const F = window[n];
      C.push(n + ':' + [F.prototype === window[I].prototype, F.length, F.name, fts.call(F), Object.getPrototypeOf(F) === Function.prototype, Object.getPrototypeOf(F) === window[I], JSON.stringify(Object.getOwnPropertyDescriptor(F, 'prototype') && { w: Object.getOwnPropertyDescriptor(F, 'prototype').writable, e: Object.getOwnPropertyDescriptor(F, 'prototype').enumerable, c: Object.getOwnPropertyDescriptor(F, 'prototype').configurable }),
        Reflect.ownKeys(F).join(';'), err(() => F()), new F() instanceof F, new F() instanceof window[I], new F().constructor === window[I], F.prototype.constructor === window[I],
        err(() => { function G() {} G.prototype = Object.create(window[I].prototype); const o = Reflect.construct(F, [], G); return Object.getPrototypeOf(o) === G.prototype; }),
        err(() => { class X extends F {} const o = new X(); return [o instanceof X, o instanceof F, Object.getPrototypeOf(X) === F].join('/'); }),
        JSON.stringify(Object.getOwnPropertyDescriptor(window, n)), err(() => F.call(new F())), err(() => new F(1, 2, 3, 4).outerHTML)].join('|'));
    }
    out.push('c1 = ' + C.join('\n   '));
    out.push('c2 = ' + [new Image(5, 7).width, new Image(5, 7).height, new Image(5).outerHTML, new Option('t', 'v', true, true).outerHTML, new Audio('a.mp3').outerHTML, new Option().selected, new Option('a','b',false,true).selected].join(','));
    out.push('c3 frame Image = ' + [f.Image.prototype === f.HTMLImageElement.prototype, f.Image !== Image, new f.Image().ownerDocument === fd].join(','));
    out.push('c4 keys = ' + [Object.getOwnPropertyNames(window).filter(k => k === 'Image' || k === 'Audio' || k === 'Option').join(','), Object.getOwnPropertyNames(window).indexOf('Image') - Object.getOwnPropertyNames(window).indexOf('HTMLImageElement')].join(','));
return out.join('\n'); })()

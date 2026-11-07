// A removed iframe's window (frames, ADR 0012), as in Chrome 153: checked
// against Chrome for Testing 153.0.8010.12 on the same page (loopback).
// Persona values and globals LightBrowser does not implement are left out.
(() => {
  const out = [];
  const err = (f) => { try { const r = f(); return 'ok:' + (r === undefined ? 'U' : r === null ? 'N' : typeof r === 'object' || typeof r === 'function' ? typeof r : JSON.stringify(r)); } catch (e) { return 'throw:' + e.name + ':' + e.message; } };
  const h = document.getElementById('f3');
  const w = h.contentWindow;
  const pre = w.Blob, d = w.document, nav = w.navigator, scr = w.screen, hist = w.history, loc = w.location;
  h.remove();
  const keys = ['Blob', 'URL', 'HTMLElement', 'Node', 'Image', 'Option', 'Audio', 'webkitURL', 'DOMException', 'TextEncoder', 'XMLHttpRequest', 'Event',
    'Object', 'Array', 'Function', 'eval', 'Math', 'JSON', 'Promise', 'WebAssembly', 'Intl', 'console', 'Atomics', 'escape', 'Iterator', 'SharedArrayBuffer',
    'navigator', 'document', 'location', 'parent', 'top', 'opener', 'frameElement', 'closed', 'length', 'window', 'self', 'frames', 'globalThis', 'name',
    'history', 'screen', 'setTimeout', 'alert', 'fetch', 'performance', 'chrome', 'localStorage', 'sessionStorage', 'innerWidth', 'outerWidth',
    'devicePixelRatio', 'origin', 'isSecureContext', 'onload', 'event', 'scrollX', 'screenX', 'status', 'clientInformation', 'crossOriginIsolated'];
  for (const k of keys) out.push(k + ' ' + err(() => w[k]));
  out.push('pre-touched ' + (w.Blob === pre) + ' ' + err(() => new pre(['x']).size));
  out.push('own ' + Object.getOwnPropertyNames(w).includes('URL') + ' ' + ('URL' in w) + ' ' + JSON.stringify(Object.getOwnPropertyDescriptor(w, 'URL')));
  out.push('doc ' + [d === w.document, err(() => d.body.nodeName), err(() => d.createElement('p').nodeName), err(() => w.eval('1+1')), err(() => w.Function('return 1')()), err(() => w.eval('typeof Node')),
    err(() => d.readyState), err(() => d.defaultView), err(() => d.visibilityState), err(() => d.hidden), err(() => d.hasFocus())].join(' '));
  for (const k of ['userAgent', 'appVersion', 'vendor', 'cookieEnabled', 'onLine', 'product', 'appName', 'maxTouchPoints']) out.push('nav.' + k + ' ' + err(() => nav[k]));
  out.push('nav.same ' + (nav === w.navigator));
  for (const k of ['width', 'height', 'availWidth', 'availHeight', 'availLeft', 'availTop', 'colorDepth', 'pixelDepth', 'orientation', 'isExtended']) out.push('scr.' + k + ' ' + err(() => scr[k]));
  for (const k of ['length', 'state', 'scrollRestoration']) out.push('hist.' + k + ' ' + err(() => hist[k]));
  out.push('loc ' + err(() => loc.href) + ' ' + (loc === w.location));
  out.push('cs ' + err(() => { const s = w.getComputedStyle(d.body); return [s.length, typeof s.display, s.display, s.cssText, s.getPropertyValue('display')].join('|'); }));
  out.push('mm ' + err(() => { const m = w.matchMedia('(min-width: 1px)'); return [m.matches, m.media].join('|'); }));
  out.push('timers ' + [err(() => w.setTimeout(() => {}, 1)), err(() => w.setInterval(() => {}, 1)), err(() => w.requestAnimationFrame(() => {})), err(() => w.requestIdleCallback(() => {})), err(() => w.queueMicrotask(() => {}))].join(' '));
  out.push('geometry ' + [err(() => w.innerHeight), err(() => w.outerHeight), err(() => w.scrollY), err(() => w.screenY), err(() => w.screenLeft), err(() => w.pageXOffset)].join(' '));
  out.push('misc ' + [err(() => w.atob('YQ==')), err(() => w.btoa('a')), err(() => w.postMessage('x', '*')), err(() => w.location.origin), err(() => w.performance.now() >= 0)].join(' '));
  const h4 = document.getElementById('f4'); const w4 = h4.contentWindow; w4.eval('Node; URL;'); h4.remove();
  out.push('touched ' + [typeof w4.Node, typeof w4.URL, typeof w4.Blob, w4.eval('typeof Blob')].join(' '));
  // The test server's port varies.
  return out.join('\n').split(location.origin).join('ORIGIN');
})()

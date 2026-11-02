// Dump the JS surface of the local Chromium: interface list and property order per prototype,
// descriptor kinds, navigator/screen values, engine quirks. Ground truth for lb-webidl-gen and M4.
const { chromium } = require('playwright');
const fs = require('fs');
(async () => {
  const out = process.argv[2];
  const browser = await chromium.launch({ channel: 'chromium', headless: true, args: ['--no-sandbox'] });
  const page = await browser.newPage();
  await page.goto('about:blank');
  const data = await page.evaluate(() => {
    const kind = (d) => d.get || d.set ? `${d.get ? 'g' : ''}${d.set ? 's' : ''}${d.enumerable ? 'e' : ''}${d.configurable ? 'c' : ''}`
      : `${typeof d.value === 'function' ? 'f' : 'v'}${d.writable ? 'w' : ''}${d.enumerable ? 'e' : ''}${d.configurable ? 'c' : ''}`;
    const props = (o) => Object.getOwnPropertyNames(o).map(n => { try { return [n, kind(Object.getOwnPropertyDescriptor(o, n)), (typeof Object.getOwnPropertyDescriptor(o, n).value === 'function' ? Object.getOwnPropertyDescriptor(o, n).value.length : null)]; } catch (e) { return [n, 'err', null]; } });
    const syms = (o) => Object.getOwnPropertySymbols(o).map(s => s.toString());
    const windowProps = props(window);
    const interfaces = {};
    for (const [name] of windowProps) {
      let v; try { v = window[name]; } catch (e) { continue; }
      if (typeof v !== 'function' || !v.prototype) continue;
      if (!/^[A-Z]/.test(name)) continue;
      const proto = v.prototype;
      const parent = Object.getPrototypeOf(proto);
      const parentName = parent && parent.constructor ? parent.constructor.name : null;
      interfaces[name] = {
        length: v.length,
        static: props(v).filter(([n]) => !['length', 'name', 'prototype', 'arguments', 'caller'].includes(n)),
        proto: props(proto),
        protoSymbols: syms(proto),
        parent: parentName,
        toStringTag: (() => { try { return proto[Symbol.toStringTag] || null; } catch (e) { return null; } })(),
        ctorToString: Function.prototype.toString.call(v),
      };
    }
    const dump = (o) => { const r = {}; for (const k in o) { try { const v = o[k]; r[k] = typeof v === 'function' ? '[fn]' : (typeof v === 'object' && v !== null ? '[obj]' : v); } catch (e) { r[k] = '[err]'; } } return r; };
    const quirks = {};
    try { null.x } catch (e) { quirks.nullProp = [e.message, e.stack.split('\n')[0]]; }
    try { undefinedFn() } catch (e) { quirks.refErr = [e.message, e.stack.split('\n')[0]]; }
    try { (1).toFixed(200) } catch (e) { quirks.rangeErr = e.message; }
    quirks.fnToString = [Function.prototype.toString.call(Math.max), Function.prototype.toString.call(Object.getOwnPropertyDescriptor(Navigator.prototype, 'userAgent').get), (function foo(a,b){ return a+b }).toString()];
    quirks.mathTan = Math.tan(-1e300); quirks.mathSin = Math.sin(1e300); quirks.expm1 = Math.expm1(1); quirks.pow = Math.pow(Math.PI, -100);
    quirks.stackDepth = (() => { let i = 0; const f = () => { i++; f(); }; try { f(); } catch (e) {} return i; })();
    quirks.errorStackFormat = new Error('x').stack;
    quirks.errorProps = Object.getOwnPropertyNames(new Error('x'));
    quirks.v8Version = (() => { try { return /Chrome\/(\S+)/.exec(navigator.userAgent)[1]; } catch (e) { return null; } })();
    return {
      ua: navigator.userAgent,
      windowProps, windowSymbols: syms(window),
      windowProtoChain: (() => { const r = []; let o = Object.getPrototypeOf(window); while (o) { r.push(o.constructor ? o.constructor.name : '?'); o = Object.getPrototypeOf(o); } return r; })(),
      interfaces,
      navigator: { props: props(Navigator.prototype), values: dump(navigator), forIn: (() => { const r = []; for (const k in navigator) r.push(k); return r; })(),
        uaData: navigator.userAgentData ? { brands: navigator.userAgentData.brands, mobile: navigator.userAgentData.mobile, platform: navigator.userAgentData.platform } : null,
        plugins: Array.from(navigator.plugins).map(p => [p.name, p.filename, p.description, Array.from(p).map(m => [m.type, m.suffixes, m.description])]),
        languages: navigator.languages, hardwareConcurrency: navigator.hardwareConcurrency, deviceMemory: navigator.deviceMemory },
      screen: dump(screen), screenProps: props(Screen.prototype),
      windowSizes: { innerWidth, innerHeight, outerWidth, outerHeight, devicePixelRatio, screenX, screenY },
      documentProps: props(Document.prototype).length, htmlElementProps: props(HTMLElement.prototype).length, elementProps: props(Element.prototype).length,
      cssProps: Object.getOwnPropertyNames(document.body.style).length,
      intl: { tz: Intl.DateTimeFormat().resolvedOptions().timeZone, locale: Intl.DateTimeFormat().resolvedOptions().locale, tzOffset: new Date().getTimezoneOffset() },
      performanceNowSamples: (() => { const r = []; let last = performance.now(); for (let i = 0; i < 2000; i++) { const n = performance.now(); if (n !== last) { r.push(+(n - last).toFixed(6)); last = n; } if (r.length > 20) break; } return r; })(),
      quirks,
      webgl: (() => { try { const c = document.createElement('canvas'); const gl = c.getContext('webgl'); const d = gl.getExtension('WEBGL_debug_renderer_info'); return { vendor: gl.getParameter(gl.VENDOR), renderer: gl.getParameter(gl.RENDERER), unmaskedVendor: gl.getParameter(d.UNMASKED_VENDOR_WEBGL), unmaskedRenderer: gl.getParameter(d.UNMASKED_RENDERER_WEBGL), version: gl.getParameter(gl.VERSION), sl: gl.getParameter(gl.SHADING_LANGUAGE_VERSION), extensions: gl.getSupportedExtensions(), params: Object.fromEntries(['MAX_TEXTURE_SIZE','MAX_VIEWPORT_DIMS','MAX_RENDERBUFFER_SIZE','MAX_VERTEX_ATTRIBS','MAX_VERTEX_UNIFORM_VECTORS','MAX_FRAGMENT_UNIFORM_VECTORS','MAX_VARYING_VECTORS','MAX_TEXTURE_IMAGE_UNITS','MAX_COMBINED_TEXTURE_IMAGE_UNITS','MAX_VERTEX_TEXTURE_IMAGE_UNITS','MAX_CUBE_MAP_TEXTURE_SIZE','ALIASED_LINE_WIDTH_RANGE','ALIASED_POINT_SIZE_RANGE','RED_BITS','GREEN_BITS','BLUE_BITS','ALPHA_BITS','DEPTH_BITS','STENCIL_BITS','SUBPIXEL_BITS','SAMPLES','SAMPLE_BUFFERS'].map(k => { const v = gl.getParameter(gl[k]); return [k, ArrayBuffer.isView(v) ? Array.from(v) : v]; })), precision: ['VERTEX_SHADER','FRAGMENT_SHADER'].flatMap(s => ['LOW_FLOAT','MEDIUM_FLOAT','HIGH_FLOAT','LOW_INT','MEDIUM_INT','HIGH_INT'].map(p => { const f = gl.getShaderPrecisionFormat(gl[s], gl[p]); return [s, p, f.rangeMin, f.rangeMax, f.precision]; })) }; } catch (e) { return String(e); } })(),
    };
  });
  const workerProps = await page.evaluate(() => new Promise(res => {
    const b = new Blob([`postMessage({self: Object.getOwnPropertyNames(self), nav: Object.getOwnPropertyNames(WorkerNavigator.prototype)})`], { type: 'text/javascript' });
    const w = new Worker(URL.createObjectURL(b)); w.onmessage = e => res(e.data); w.onerror = e => res(String(e.message));
  }));
  data.worker = workerProps;
  fs.writeFileSync(out, JSON.stringify(data, null, 1));
  console.log('interfaces:', Object.keys(data.interfaces).length, 'window props:', data.windowProps.length, 'ua:', data.ua);
  await browser.close();
})();

// detect-bench collector: client-side probes and an input-event trace.
//
// Every probe returns {id, cat, value, suspicious, weight}. `suspicious` is
// set only on positive evidence; scoring.py owns the final weights (the
// weight here is the default for probes it does not list).
// Beacons go to /api/report on load, 3 s after load, and on pagehide.
// No globals are defined: everything lives in this closure.
(() => {
  'use strict';

  const ctxEl = document.getElementById('lb-ctx');
  let ctx = {};
  try { ctx = JSON.parse(ctxEl ? ctxEl.textContent : '{}'); } catch (_) { /* keep {} */ }
  if (!ctx.sid) return;

  const now = () => performance.now();
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const r1 = (n) => Math.round(n * 10) / 10;
  const pageId = Math.random().toString(16).slice(2, 10) + Date.now().toString(16).slice(-6);
  const UA = navigator.userAgent;
  const family = /Firefox\//.test(UA) ? 'firefox'
    : /(Headless)?Chrome\//.test(UA) ? 'chrome'
      : /Safari\//.test(UA) ? 'safari' : 'other';
  const uaOs = /Windows NT/.test(UA) ? 'windows'
    : /Android/.test(UA) ? 'android'
      : /CrOS/.test(UA) ? 'chromeos'
        : /Mac OS X|Macintosh/.test(UA) ? 'mac'
          : /Linux|X11/.test(UA) ? 'linux' : 'other';

  // cyrb53: small, fast, stable string hash (hex).
  function hash(str) {
    let h1 = 0xdeadbeef, h2 = 0x41c6ce57;
    for (let i = 0; i < str.length; i++) {
      const ch = str.charCodeAt(i);
      h1 = Math.imul(h1 ^ ch, 2654435761);
      h2 = Math.imul(h2 ^ ch, 1597334677);
    }
    h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
    h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
    return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(16);
  }
  const median = (a) => {
    if (!a.length) return null;
    const s = a.slice().sort((x, y) => x - y);
    const m = s.length >> 1;
    return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
  };
  const mean = (a) => a.reduce((x, y) => x + y, 0) / (a.length || 1);
  // Robust spread: median absolute deviation over the median.
  const rcv = (a) => {
    const m = median(a);
    if (m === null || a.length < 2 || m === 0) return null;
    return median(a.map((x) => Math.abs(x - m))) / m;
  };
  const cv = (a) => {
    if (a.length < 2) return null;
    const m = mean(a);
    if (m === 0) return 0;
    const v = mean(a.map((x) => (x - m) ** 2));
    return Math.sqrt(v) / m;
  };

  // ---- input-event trace ------------------------------------------------

  const trace = [];
  const MAX_TRACE = 20000;
  const TYPES = ['mousemove', 'mousedown', 'mouseup', 'click', 'keydown', 'keyup', 'wheel',
    'scroll', 'input', 'touchstart', 'touchmove', 'touchend', 'focus', 'blur', 'visibilitychange'];
  const TEXT_INPUT = /^(text|search|email|password|tel|url|number|textarea)$/;

  function targetInfo(t) {
    if (!t || t.nodeType !== 1) return {};
    const el = t.closest ? (t.closest('button,a,input,textarea,select,label,[role=button]') || t) : t;
    return { el, tg: el.id ? '#' + el.id : el.tagName.toLowerCase() };
  }

  function record(e) {
    if (trace.length >= MAX_TRACE) return;
    // e.timeStamp is when the input happened, not when the handler ran.
    const ts = e.timeStamp > 0 && e.timeStamp < now() + 1000 ? e.timeStamp : now();
    const ev = { t: r1(ts), type: e.type, tr: e.isTrusted ? 1 : 0 };
    if (e.type === 'mousemove' || e.type === 'mousedown' || e.type === 'mouseup' || e.type === 'click') {
      ev.x = r1(e.clientX);
      ev.y = r1(e.clientY);
      if (e.type === 'mousemove') {
        ev.mx = e.movementX;
        ev.my = e.movementY;
      } else {
        ev.b = e.button;
        ev.d = e.detail;
        const { el, tg } = targetInfo(e.target);
        ev.tg = tg;
        if (el && el.getBoundingClientRect) {
          const r = el.getBoundingClientRect();
          ev.cx = r1(e.clientX - (r.left + r.width / 2));
          ev.cy = r1(e.clientY - (r.top + r.height / 2));
          ev.w = r1(r.width);
          ev.h = r1(r.height);
        }
        ev.f = document.hasFocus() ? 1 : 0;
      }
    } else if (e.type === 'keydown' || e.type === 'keyup') {
      const pw = e.target && e.target.type === 'password';
      ev.k = pw && e.key.length === 1 ? 'x' : e.key;
      ev.rp = e.repeat ? 1 : 0;
      ev.f = document.hasFocus() ? 1 : 0;
      ev.tg = targetInfo(e.target).tg;
    } else if (e.type === 'wheel') {
      ev.x = r1(e.clientX);
      ev.y = r1(e.clientY);
      ev.dx = r1(e.deltaX);
      ev.dy = r1(e.deltaY);
      ev.dm = e.deltaMode;
    } else if (e.type === 'scroll') {
      if (e.target !== document) return;
      ev.sy = Math.round(window.scrollY);
    } else if (e.type === 'input') {
      const t = e.target;
      if (!t || !TEXT_INPUT.test(t.type || t.tagName.toLowerCase())) return;
      ev.it = e.inputType || '';
      ev.dl = typeof e.data === 'string' ? e.data.length : 0;
      ev.tg = targetInfo(t).tg;
    } else if (e.type === 'touchstart' || e.type === 'touchmove' || e.type === 'touchend') {
      const p = e.changedTouches && e.changedTouches[0];
      if (p) { ev.x = r1(p.clientX); ev.y = r1(p.clientY); }
    } else if (e.type === 'focus' || e.type === 'blur') {
      if (e.target !== window) return;
    } else if (e.type === 'visibilitychange') {
      ev.v = document.visibilityState;
    }
    trace.push(ev);
  }
  for (const t of TYPES) {
    const target = t === 'visibilitychange' ? document : window;
    target.addEventListener(t, record, { capture: true, passive: true });
  }

  // ---- helpers used by probes ---------------------------------------------

  // A clean about:blank realm: its Function.prototype.toString and global
  // property list are not touched by scripts patching the top window.
  let frame = null;
  function cleanFrame() {
    if (frame) return frame;
    try {
      const f = document.createElement('iframe');
      f.style.cssText = 'position:absolute;width:1px;height:1px;left:-9999px;border:0';
      f.setAttribute('aria-hidden', 'true');
      f.tabIndex = -1;
      document.documentElement.appendChild(f);
      frame = f;
    } catch (_) { frame = null; }
    return frame;
  }
  function cleanToString() {
    const f = cleanFrame();
    try { return f.contentWindow.Function.prototype.toString; } catch (_) { return Function.prototype.toString; }
  }
  const NATIVE_RE = /^function ([\w$ ]*)\(\) \{\s*\[native code\]\s*\}$/;
  function nativeCheck(fn, name) {
    if (typeof fn !== 'function') return 'not a function';
    let s;
    try { s = cleanToString().call(fn); } catch (e) { return 'toString threw'; }
    const m = NATIVE_RE.exec(s);
    if (!m) return 'source visible';
    if (name && m[1] !== name) return `name ${m[1]}`;
    if (Object.prototype.hasOwnProperty.call(fn, 'prototype')) return 'has prototype';
    // The top-window toString must agree with the clean one.
    try { if (Function.prototype.toString.call(fn) !== s) return 'toString differs'; } catch (_) { return 'toString threw'; }
    return null;
  }
  // Call a native getter/method with a bad receiver. The first stack frame
  // must be ours; a JS wrapper or Proxy trap adds frames above it.
  function lbIllegalCall(fn) {
    try { fn.call(Object.create(null)); } catch (e) { return String((e && e.stack) || ''); }
    return null;
  }
  function stackWrapped(fn) {
    if (family !== 'chrome') return null;
    const st = lbIllegalCall(fn);
    if (st === null) return null; // did not throw: not a receiver-checked native
    const first = st.split('\n').find((l) => /^\s+at /.test(l));
    if (first && !/lbIllegalCall/.test(first)) return 'extra stack frame';
    return null;
  }
  function getter(obj, key) {
    const d = obj && Object.getOwnPropertyDescriptor(obj, key);
    return d && d.get;
  }

  // ---- fingerprint probes -------------------------------------------------

  const fp = [];
  const P = [];
  function probe(id, cat, weight, fn) {
    P.push((async () => {
      let value = null, suspicious = false;
      try {
        const out = await Promise.race([fn(), sleep(2500).then(() => [{ timeout: true }, false])]);
        [value, suspicious] = out;
      } catch (e) {
        value = { error: String(e && e.message || e).slice(0, 200) };
      }
      fp.push({ id, cat, value, suspicious: !!suspicious, weight });
    })());
  }

  probe('webdriver', 'automation', 90, () => {
    const v = navigator.webdriver;
    return [v === undefined ? 'undefined' : v, v === true];
  });

  probe('webdriver_tamper', 'automation', 40, () => {
    if (family !== 'chrome' && family !== 'firefox') return [null, false];
    const reasons = [];
    const d = Object.getOwnPropertyDescriptor(Navigator.prototype, 'webdriver');
    if (!d) reasons.push('missing on Navigator.prototype');
    if (Object.getOwnPropertyDescriptor(navigator, 'webdriver')) reasons.push('own property on navigator');
    const names = Object.getOwnPropertyNames(Navigator.prototype);
    const idx = names.indexOf('webdriver');
    if (d && family === 'chrome' && idx !== names.indexOf('onLine') + 1) reasons.push('moved in property order');
    if (d && d.get) {
      const n = nativeCheck(d.get, 'get webdriver');
      if (n) reasons.push('getter ' + n);
    }
    return [{ reasons, index: idx }, reasons.length > 0];
  });

  probe('cdp_runtime', 'automation', 80, async () => {
    // With Runtime.enable on, the inspector serialises console arguments
    // and touches these getters. Nothing does that without a CDP client.
    let stackHit = false, idHit = false;
    const err = new Error('lb');
    Object.defineProperty(err, 'stack', { configurable: true, get() { stackHit = true; return 'Error: lb'; } });
    console.debug(err);
    const div = document.createElement('div');
    Object.defineProperty(div, 'id', { configurable: true, get() { idHit = true; return 'lb'; } });
    console.debug(div);
    await sleep(0);
    return [{ errorStackGetter: stackHit, nodeIdGetter: idHit }, stackHit || idHit];
  });

  probe('ua_headless', 'automation', 95, () => {
    const hit = /Headless/i.test(UA) || /Headless/i.test(navigator.appVersion || '');
    return [UA, hit];
  });

  probe('automation_keys', 'automation', 90, () => {
    const re = /^(\$?cdc_|\$wdc_|__webdriver|__selenium|_selenium|__driver|__fxdriver|__nightmare|_phantom|callPhantom|__phantomas|domAutomation|__playwright|__pw_|__pwInitScripts|__puppeteer|puppeteer_|_Selenium_IDE|__lastWatirAlert|_WEBDRIVER_ELEM_CACHE|ChromeDriverw|__webdriverFunc|__\$webdriverAsyncExecutor|wdioElectron)/;
    const hits = [];
    for (const k of Object.getOwnPropertyNames(window)) if (re.test(k)) hits.push('window.' + k);
    for (const k of Object.getOwnPropertyNames(document)) if (re.test(k) || /\$cdc_|webdriver/i.test(k)) hits.push('document.' + k);
    for (const a of document.documentElement.getAttributeNames()) {
      if (/webdriver|selenium|driver/i.test(a)) hits.push('html@' + a);
    }
    return [hits, hits.length > 0];
  });

  probe('extra_globals', 'automation', 30, () => {
    const f = cleanFrame();
    if (!f || !f.contentWindow) return [null, false];
    const clean = new Set(Object.getOwnPropertyNames(f.contentWindow));
    // Index keys ("0", "1") are child frames, including our own probe frame.
    const extra = Object.getOwnPropertyNames(window).filter((k) => !clean.has(k) && !/^\d+$/.test(k));
    return [extra.slice(0, 40), extra.length > 0];
  });

  probe('chrome_object', 'consistency', 40, () => {
    const c = window.chrome;
    const v = { present: !!c, keys: c ? Object.keys(c).sort() : null, runtime: !!(c && c.runtime) };
    if (family !== 'chrome') return [v, family === 'firefox' && !!c];
    return [v, !c || !('loadTimes' in c) || !('csi' in c) || !('app' in c)];
  });

  probe('plugins', 'consistency', 45, () => {
    const pl = navigator.plugins, mt = navigator.mimeTypes;
    const names = Array.from(pl || []).map((p) => p.name);
    const shape = !!pl && pl instanceof PluginArray && !!mt && mt instanceof MimeTypeArray &&
      Object.getPrototypeOf(pl) === PluginArray.prototype &&
      (pl.length === 0 || (pl[0] instanceof Plugin && pl[0][0] instanceof MimeType)) &&
      nativeCheck(PluginArray.prototype.item, 'item') === null;
    const v = { plugins: pl ? pl.length : null, mimeTypes: mt ? mt.length : null, names, shape,
      pdfViewerEnabled: navigator.pdfViewerEnabled };
    const want = ['PDF Viewer', 'Chrome PDF Viewer', 'Chromium PDF Viewer', 'Microsoft Edge PDF Viewer', 'WebKit built-in PDF'];
    let bad = !shape;
    if (family === 'chrome' || family === 'firefox') {
      bad = bad || pl.length !== 5 || mt.length !== 2 || names.join('|') !== want.join('|');
    }
    return [v, bad];
  });

  probe('languages', 'consistency', 40, () => {
    const langs = Array.from(navigator.languages || []);
    const al = (ctx.acceptLanguage || '').split(',').map((s) => s.split(';')[0].trim()).filter(Boolean);
    const reasons = [];
    if (!langs.length) reasons.push('navigator.languages empty');
    if (langs.length && navigator.language !== langs[0]) reasons.push('language != languages[0]');
    if (!al.length) reasons.push('no Accept-Language header');
    if (al.length && langs.length && al[0] !== langs[0]) reasons.push('Accept-Language first entry differs');
    const base = new Set(langs.map((l) => l.split('-')[0]));
    const unknown = al.filter((l) => !langs.includes(l) && !base.has(l));
    if (unknown.length) reasons.push('Accept-Language has ' + unknown.join(','));
    return [{ languages: langs, acceptLanguage: ctx.acceptLanguage, reasons }, reasons.length > 0];
  });

  probe('notification_permission', 'consistency', 50, async () => {
    if (!('Notification' in window) || !navigator.permissions) return [null, false];
    const st = (await navigator.permissions.query({ name: 'notifications' })).state;
    const np = Notification.permission;
    const map = { default: 'prompt', granted: 'granted', denied: 'denied' };
    return [{ notification: np, permissions: st }, map[np] !== st];
  });

  probe('outer_size_zero', 'automation', 60, () => {
    const v = { outerWidth, outerHeight, innerWidth, innerHeight };
    return [v, outerWidth === 0 || outerHeight === 0];
  });

  probe('window_frame_absent', 'consistency', 25, () => {
    // A desktop window has a tab strip and toolbar: outerHeight > innerHeight.
    const v = { dw: outerWidth - innerWidth, dh: outerHeight - innerHeight };
    return [v, outerWidth > 0 && v.dh === 0 && v.dw === 0 && uaOs !== 'android'];
  });

  probe('screen_sanity', 'consistency', 35, () => {
    const s = screen;
    const v = { width: s.width, height: s.height, availWidth: s.availWidth, availHeight: s.availHeight,
      colorDepth: s.colorDepth, dpr: devicePixelRatio, innerWidth, innerHeight };
    const reasons = [];
    if (innerWidth > s.width || innerHeight > s.height) reasons.push('viewport larger than screen');
    if (innerWidth === s.width && innerHeight === s.height) reasons.push('viewport equals screen');
    if (s.width === 800 && s.height === 600) reasons.push('800x600 default screen');
    if (![24, 30, 32, 48].includes(s.colorDepth)) reasons.push('colorDepth ' + s.colorDepth);
    if (!(devicePixelRatio > 0 && devicePixelRatio <= 5)) reasons.push('devicePixelRatio ' + devicePixelRatio);
    if (s.availWidth > s.width || s.availHeight > s.height) reasons.push('avail larger than screen');
    v.reasons = reasons;
    return [v, reasons.length > 0];
  });

  probe('screen_no_taskbar', 'consistency', 10, () => {
    const s = screen;
    const hit = s.availWidth === s.width && s.availHeight === s.height && (uaOs === 'windows' || uaOs === 'mac');
    return [{ availHeight: s.availHeight, height: s.height }, hit];
  });

  function parseSecChUa(h) {
    const out = [];
    const re = /"([^"]*)";\s*v="([^"]*)"/g;
    let m;
    while ((m = re.exec(h || ''))) out.push(m[1] + ';' + m[2]);
    return out;
  }

  probe('ua_ch_headless', 'automation', 90, () => {
    const d = navigator.userAgentData;
    const brands = d ? d.brands.map((b) => b.brand) : [];
    const hdr = parseSecChUa(ctx.secChUa).map((b) => b.split(';')[0]);
    const hit = brands.concat(hdr).some((b) => /Headless/i.test(b));
    return [{ brands, header: hdr }, hit];
  });

  probe('ua_ch_mismatch', 'consistency', 50, () => {
    const d = navigator.userAgentData;
    if (family === 'firefox' || family === 'safari') return [{ present: !!d }, !!d];
    if (family !== 'chrome') return [null, false];
    if (!d) return [{ present: false }, isSecureContext];
    const major = (UA.match(/Chrome\/(\d+)/) || [])[1];
    const brands = d.brands.map((b) => b.brand + ';' + b.version);
    const reasons = [];
    if (!d.brands.some((b) => /^(Google Chrome|Chromium|Microsoft Edge|Opera|Brave)$/.test(b.brand) && b.version === major)) {
      reasons.push('no brand with UA major ' + major);
    }
    if (!d.brands.some((b) => /Not.?A.?Brand/i.test(b.brand))) reasons.push('no GREASE brand');
    const plat = { windows: 'Windows', mac: 'macOS', linux: 'Linux', android: 'Android', chromeos: 'Chrome OS' }[uaOs];
    if (plat && d.platform !== plat) reasons.push('platform ' + d.platform + ' vs UA ' + plat);
    if (d.mobile !== /Mobile/.test(UA)) reasons.push('mobile flag');
    const hdr = parseSecChUa(ctx.secChUa);
    if (hdr.length && hdr.slice().sort().join() !== brands.slice().sort().join()) reasons.push('sec-ch-ua header differs from JS brands');
    if (ctx.secChUaPlatform && JSON.parse(ctx.secChUaPlatform || '""') !== d.platform) reasons.push('sec-ch-ua-platform differs');
    return [{ brands, platform: d.platform, mobile: d.mobile, reasons }, reasons.length > 0];
  });

  function webglInfo() {
    const c = document.createElement('canvas');
    const gl = c.getContext('webgl') || c.getContext('experimental-webgl');
    if (!gl) return null;
    const dbg = gl.getExtension('WEBGL_debug_renderer_info');
    const P = ['VERSION', 'SHADING_LANGUAGE_VERSION', 'VENDOR', 'RENDERER', 'MAX_TEXTURE_SIZE',
      'MAX_CUBE_MAP_TEXTURE_SIZE', 'MAX_RENDERBUFFER_SIZE', 'MAX_VIEWPORT_DIMS', 'MAX_VERTEX_ATTRIBS',
      'MAX_VERTEX_UNIFORM_VECTORS', 'MAX_FRAGMENT_UNIFORM_VECTORS', 'MAX_VARYING_VECTORS',
      'MAX_TEXTURE_IMAGE_UNITS', 'MAX_VERTEX_TEXTURE_IMAGE_UNITS', 'MAX_COMBINED_TEXTURE_IMAGE_UNITS',
      'ALIASED_LINE_WIDTH_RANGE', 'ALIASED_POINT_SIZE_RANGE', 'RED_BITS', 'GREEN_BITS', 'BLUE_BITS',
      'ALPHA_BITS', 'DEPTH_BITS', 'STENCIL_BITS', 'SUBPIXEL_BITS', 'SAMPLES', 'SAMPLE_BUFFERS'];
    const params = P.map((p) => {
      const v = gl.getParameter(gl[p]);
      return p + '=' + (v && v.length !== undefined && typeof v !== 'string' ? Array.from(v).join(',') : v);
    });
    const prec = [];
    for (const sh of ['VERTEX_SHADER', 'FRAGMENT_SHADER']) {
      for (const pr of ['HIGH_FLOAT', 'MEDIUM_FLOAT', 'LOW_FLOAT', 'HIGH_INT']) {
        const f = gl.getShaderPrecisionFormat(gl[sh], gl[pr]);
        prec.push(f ? `${f.rangeMin},${f.rangeMax},${f.precision}` : 'null');
      }
    }
    const ext = (gl.getSupportedExtensions() || []).slice().sort();
    return {
      vendor: dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) : null,
      renderer: dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : null,
      paramHash: hash(params.join(';') + '|' + prec.join(';')),
      extHash: hash(ext.join(',')),
      extensions: ext.length,
    };
  }
  let glCache;
  const gl = () => (glCache === undefined ? (glCache = webglInfo()) : glCache);

  probe('webgl_params', 'fingerprint', 0, () => [gl(), false]);

  probe('webgl_software', 'fingerprint', 45, () => {
    const g = gl();
    if (!g) return [{ supported: false }, family === 'chrome' || family === 'firefox'];
    const s = `${g.vendor} ${g.renderer}`;
    const hit = /SwiftShader|llvmpipe|softpipe|Software Rasterizer|Microsoft Basic Render|Mesa OffScreen/i.test(s) ||
      g.vendor === 'Google Inc. (Google)';
    return [{ vendor: g.vendor, renderer: g.renderer }, hit];
  });

  probe('webgl_os_mismatch', 'consistency', 50, () => {
    const g = gl();
    if (!g || !g.renderer) return [null, false];
    const r = g.renderer;
    const reasons = [];
    if (/Direct3D|D3D11|D3D9/i.test(r) && uaOs !== 'windows') reasons.push('Direct3D renderer on ' + uaOs);
    if (/Metal|Apple M\d|Apple GPU/i.test(r) && uaOs !== 'mac') reasons.push('Apple renderer on ' + uaOs);
    if (uaOs === 'mac' && /NVIDIA|GeForce/i.test(r) && !/OpenGL/.test(r)) reasons.push('NVIDIA on modern mac');
    if (uaOs === 'windows' && /Mesa|llvmpipe|Vulkan/i.test(r)) reasons.push('Linux driver on Windows');
    return [{ renderer: r, os: uaOs, reasons }, reasons.length > 0];
  });

  function canvasHash() {
    const c = document.createElement('canvas');
    c.width = 240; c.height = 60;
    const x = c.getContext('2d');
    x.textBaseline = 'top';
    x.font = '14px Arial';
    x.fillStyle = '#f60';
    x.fillRect(100, 1, 62, 20);
    x.fillStyle = '#069';
    x.fillText('Northwind, 0.1 \u{1F600} éß', 2, 15);
    x.fillStyle = 'rgba(102, 204, 0, 0.7)';
    x.fillText('Northwind, 0.1 \u{1F600} éß', 4, 17);
    const g = x.createLinearGradient(0, 0, 240, 0);
    g.addColorStop(0, '#d1495b');
    g.addColorStop(1, '#00798c');
    x.fillStyle = g;
    x.beginPath();
    x.arc(200, 40, 16, 0, Math.PI * 2);
    x.fill();
    return hash(c.toDataURL());
  }

  probe('canvas', 'fingerprint', 30, () => {
    const a = canvasHash();
    const b = canvasHash();
    const c = document.createElement('canvas');
    c.width = c.height = 4;
    const x = c.getContext('2d');
    x.fillStyle = 'rgb(10,20,30)';
    x.fillRect(0, 0, 4, 4);
    const d = x.getImageData(0, 0, 4, 4).data;
    let solid = true;
    for (let i = 0; i < d.length; i += 4) {
      if (d[i] !== 10 || d[i + 1] !== 20 || d[i + 2] !== 30 || d[i + 3] !== 255) solid = false;
    }
    return [{ hash: a, stable: a === b, solidExact: solid }, a !== b || !solid];
  });

  async function audioOnce() {
    const Ctx = window.OfflineAudioContext || window.webkitOfflineAudioContext;
    const ac = new Ctx(1, 5000, 44100);
    const osc = ac.createOscillator();
    osc.type = 'triangle';
    osc.frequency.value = 10000;
    const comp = ac.createDynamicsCompressor();
    comp.threshold.value = -50;
    comp.knee.value = 40;
    comp.ratio.value = 12;
    comp.attack.value = 0;
    comp.release.value = 0.25;
    osc.connect(comp);
    comp.connect(ac.destination);
    osc.start(0);
    const buf = await ac.startRendering();
    const d = buf.getChannelData(0);
    let s = 0;
    for (let i = 4500; i < 5000; i++) s += Math.abs(d[i]);
    return s;
  }

  probe('audio', 'fingerprint', 25, async () => {
    if (!(window.OfflineAudioContext || window.webkitOfflineAudioContext)) {
      return [{ supported: false }, family === 'chrome' || family === 'firefox'];
    }
    const a = await audioOnce();
    const b = await audioOnce();
    return [{ sum: a, hash: hash(String(a)), stable: a === b }, a !== b];
  });

  const FONTS = {
    windows: ['Segoe UI', 'Calibri', 'Cambria', 'Consolas', 'Tahoma', 'MS Gothic', 'Franklin Gothic Medium', 'Segoe UI Emoji'],
    mac: ['Helvetica Neue', 'Menlo', 'Monaco', 'Geneva', 'Lucida Grande', 'Avenir', 'Apple Color Emoji', 'Gill Sans'],
    linux: ['DejaVu Sans', 'Liberation Sans', 'Ubuntu', 'Cantarell', 'Noto Sans', 'FreeSans', 'Nimbus Sans', 'Noto Color Emoji'],
    common: ['Arial', 'Times New Roman', 'Courier New', 'Verdana', 'Georgia', 'Trebuchet MS'],
  };

  probe('fonts', 'consistency', 35, () => {
    const span = document.createElement('span');
    span.style.cssText = 'position:absolute;left:-9999px;top:0;font-size:72px;white-space:nowrap';
    span.textContent = 'mmmmmmmmmmlli1WQ@#é';
    document.body.appendChild(span);
    const bases = ['monospace', 'serif', 'sans-serif'];
    const base = {};
    for (const b of bases) {
      span.style.fontFamily = b;
      base[b] = [span.offsetWidth, span.offsetHeight];
    }
    const detected = {};
    for (const [os, list] of Object.entries(FONTS)) {
      detected[os] = list.filter((f) => bases.some((b) => {
        span.style.fontFamily = `"${f}", ${b}`;
        return span.offsetWidth !== base[b][0] || span.offsetHeight !== base[b][1];
      }));
    }
    span.remove();
    const own = (detected[uaOs] || []).length;
    const foreign = Object.entries(detected)
      .filter(([os]) => os !== uaOs && os !== 'common' && FONTS[uaOs])
      .reduce((n, [, l]) => n + l.length, 0);
    const all = Object.values(detected).reduce((n, l) => n + l.length, 0);
    // Linux often has metric-compatible stand-ins (Carlito for Calibri,
    // Caladea for Cambria), so foreign fonts only count against Windows/macOS.
    const reasons = [];
    if (all === 0) reasons.push('no fonts detected');
    if (FONTS[uaOs] && own === 0) reasons.push('no ' + uaOs + ' fonts');
    if ((uaOs === 'windows' || uaOs === 'mac') && foreign > own) reasons.push('more foreign-OS fonts than ' + uaOs + ' fonts');
    return [{ os: uaOs, detected, hash: hash(JSON.stringify(detected)), reasons }, reasons.length > 0];
  });

  probe('hardware', 'consistency', 20, () => {
    const hc = navigator.hardwareConcurrency;
    const dm = navigator.deviceMemory;
    const reasons = [];
    if (!(Number.isInteger(hc) && hc >= 1 && hc <= 256)) reasons.push('hardwareConcurrency ' + hc);
    if (family === 'chrome' && isSecureContext) {
      if (dm === undefined) reasons.push('deviceMemory missing');
      else if (![0.25, 0.5, 1, 2, 4, 8, 16, 32].includes(dm)) reasons.push('deviceMemory ' + dm);
    }
    if (family === 'firefox' && dm !== undefined) reasons.push('deviceMemory on Firefox');
    return [{ hardwareConcurrency: hc, deviceMemory: dm, reasons }, reasons.length > 0];
  });

  function tzInfo() {
    const d = new Date();
    const ro = Intl.DateTimeFormat().resolvedOptions();
    const parts = new Intl.DateTimeFormat('en-US', { timeZone: ro.timeZone, hourCycle: 'h23', year: 'numeric',
      month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' }).formatToParts(d);
    const g = (t) => +parts.find((p) => p.type === t).value;
    const asUtc = Date.UTC(g('year'), g('month') - 1, g('day'), g('hour') % 24, g('minute'), g('second'));
    const intlOffset = Math.round((asUtc - Math.floor(d.getTime() / 1000) * 1000) / 60000);
    return { tz: ro.timeZone, locale: ro.locale, intlOffset, dateOffset: -d.getTimezoneOffset() };
  }

  probe('timezone_mismatch', 'consistency', 40, () => {
    const t = tzInfo();
    return [t, Math.abs(t.intlOffset - t.dateOffset) > 1];
  });

  probe('timezone_utc', 'consistency', 10, () => {
    const t = tzInfo();
    return [t.tz, /^(UTC|Etc\/UTC|Etc\/GMT|GMT|Universal|Etc\/Universal)$/.test(t.tz)];
  });

  probe('locale_mismatch', 'consistency', 15, () => {
    const t = tzInfo();
    const a = (t.locale || '').split('-')[0];
    const b = (navigator.language || '').split('-')[0];
    return [{ intl: t.locale, navigator: navigator.language }, a !== b];
  });

  probe('perf_now_resolution', 'consistency', 15, () => {
    let min = Infinity, last = performance.now();
    const end = last + 5;
    let n = 0;
    while (n++ < 200000) {
      const t = performance.now();
      if (t !== last) { min = Math.min(min, t - last); last = t; }
      if (t > end) break;
    }
    const step = Math.round(min * 1e4) / 1e4;
    let bad = false;
    if (family === 'chrome') bad = crossOriginIsolated ? step < 0.004 : step < 0.09 || step > 0.5;
    if (family === 'firefox') bad = !crossOriginIsolated && step < 0.015;
    return [{ stepMs: step, crossOriginIsolated }, bad];
  });

  probe('native_functions', 'automation', 50, () => {
    const N = Navigator.prototype;
    const items = [];
    for (const k of ['webdriver', 'userAgent', 'languages', 'language', 'plugins', 'mimeTypes', 'platform',
      'hardwareConcurrency', 'deviceMemory', 'vendor', 'userAgentData', 'connection']) {
      const g = getter(N, k);
      if (g) items.push([`navigator.${k}`, g, 'get ' + k, true]);
    }
    for (const k of ['width', 'height', 'availWidth', 'availHeight', 'colorDepth']) {
      items.push([`screen.${k}`, getter(Screen.prototype, k), 'get ' + k, true]);
    }
    for (const k of ['outerWidth', 'outerHeight', 'innerWidth', 'devicePixelRatio']) {
      const g = getter(window, k);
      if (g) items.push([`window.${k}`, g, 'get ' + k, false]);
    }
    const methods = [
      ['Function.prototype.toString', Function.prototype.toString, 'toString'],
      ['HTMLCanvasElement.toDataURL', HTMLCanvasElement.prototype.toDataURL, 'toDataURL'],
      ['HTMLCanvasElement.getContext', HTMLCanvasElement.prototype.getContext, 'getContext'],
      ['HTMLCanvasElement.toBlob', HTMLCanvasElement.prototype.toBlob, 'toBlob'],
      ['CanvasRenderingContext2D.getImageData', CanvasRenderingContext2D.prototype.getImageData, 'getImageData'],
      ['CanvasRenderingContext2D.measureText', CanvasRenderingContext2D.prototype.measureText, 'measureText'],
      ['WebGLRenderingContext.getParameter', window.WebGLRenderingContext && WebGLRenderingContext.prototype.getParameter, 'getParameter'],
      ['WebGLRenderingContext.getExtension', window.WebGLRenderingContext && WebGLRenderingContext.prototype.getExtension, 'getExtension'],
      ['Permissions.query', window.Permissions && Permissions.prototype.query, 'query'],
      ['Date.getTimezoneOffset', Date.prototype.getTimezoneOffset, 'getTimezoneOffset'],
      ['Intl.DateTimeFormat.resolvedOptions', Intl.DateTimeFormat.prototype.resolvedOptions, 'resolvedOptions'],
      ['AudioBuffer.getChannelData', window.AudioBuffer && AudioBuffer.prototype.getChannelData, 'getChannelData'],
      ['Element.getBoundingClientRect', Element.prototype.getBoundingClientRect, 'getBoundingClientRect'],
      ['Document.hasFocus', Document.prototype.hasFocus, 'hasFocus'],
      ['Object.getOwnPropertyDescriptor', Object.getOwnPropertyDescriptor, 'getOwnPropertyDescriptor'],
      ['Object.getOwnPropertyNames', Object.getOwnPropertyNames, 'getOwnPropertyNames'],
      ['Navigator.getBattery', N.getBattery, 'getBattery'],
      ['Notification.permission', getter(window.Notification, 'permission'), 'get permission'],
    ];
    const bad = [];
    for (const [label, fn, name, illegal] of items) {
      const r = nativeCheck(fn, name) || (illegal && stackWrapped(fn));
      if (r) bad.push(`${label}: ${r}`);
    }
    for (const [label, fn, name] of methods) {
      if (fn === undefined) continue;
      const r = nativeCheck(fn, name);
      if (r) bad.push(`${label}: ${r}`);
    }
    if (Object.getOwnPropertyNames(navigator).length) {
      bad.push('navigator own props: ' + Object.getOwnPropertyNames(navigator).join(','));
    }
    return [{ checked: items.length + methods.length, bad }, bad.length > 0];
  });

  probe('property_order', 'automation', 30, () => {
    const forIn = [];
    for (const k in navigator) forIn.push(k);
    const proto = new Set();
    for (let o = Object.getPrototypeOf(navigator); o; o = Object.getPrototypeOf(o)) {
      for (const k of Object.getOwnPropertyNames(o)) proto.add(k);
    }
    const strays = forIn.filter((k) => !proto.has(k));
    const win = Object.getOwnPropertyNames(window);
    return [{ navigatorHash: hash(forIn.join(',')), windowHash: hash(win.join(',')), windowCount: win.length,
      strays }, strays.length > 0];
  });

  probe('has_focus', 'automation', 10, () => {
    const v = { hasFocus: document.hasFocus(), visibility: document.visibilityState };
    return [v, !v.hasFocus && v.visibility === 'visible'];
  });

  probe('iframe_mismatch', 'automation', 45, () => {
    const f = cleanFrame();
    if (!f) return [{ created: false }, false];
    const w = f.contentWindow;
    const reasons = [];
    if (!w) return [{ contentWindow: null }, true];
    if (w === window) reasons.push('contentWindow is window');
    if (w.self !== w) reasons.push('self mismatch');
    const n = w.navigator;
    if (n.webdriver !== navigator.webdriver) reasons.push('webdriver');
    if (n.userAgent !== navigator.userAgent) reasons.push('userAgent');
    if (n.plugins.length !== navigator.plugins.length) reasons.push('plugins');
    if (n.hardwareConcurrency !== navigator.hardwareConcurrency) reasons.push('hardwareConcurrency');
    if (n.languages.join() !== navigator.languages.join()) reasons.push('languages');
    if (n.platform !== navigator.platform) reasons.push('platform');
    if (!!w.chrome !== !!window.chrome) reasons.push('window.chrome');
    if (w.screen.width !== screen.width) reasons.push('screen');
    return [{ reasons }, reasons.length > 0];
  });

  probe('worker_mismatch', 'automation', 45, () => new Promise((resolve) => {
    if (!window.Worker || !window.Blob) { resolve([{ supported: false }, false]); return; }
    const src = 'postMessage({ua:navigator.userAgent,hc:navigator.hardwareConcurrency,' +
      'langs:(navigator.languages||[]).join(),platform:navigator.platform,dm:navigator.deviceMemory,' +
      'tz:Intl.DateTimeFormat().resolvedOptions().timeZone})';
    let url, wk;
    try {
      url = URL.createObjectURL(new Blob([src], { type: 'text/javascript' }));
      wk = new Worker(url);
    } catch (e) { resolve([{ error: String(e.message) }, false]); return; }
    const done = (v) => { try { wk.terminate(); URL.revokeObjectURL(url); } catch (_) { /* ignore */ } resolve(v); };
    const timer = setTimeout(() => done([{ timeout: true }, false]), 1500);
    wk.onmessage = (e) => {
      clearTimeout(timer);
      const d = e.data;
      const reasons = [];
      if (d.ua !== navigator.userAgent) reasons.push('userAgent');
      if (d.hc !== navigator.hardwareConcurrency) reasons.push('hardwareConcurrency');
      if (d.langs !== Array.from(navigator.languages || []).join()) reasons.push('languages');
      if (d.platform !== navigator.platform) reasons.push('platform');
      if (d.dm !== navigator.deviceMemory) reasons.push('deviceMemory');
      if (d.tz !== Intl.DateTimeFormat().resolvedOptions().timeZone) reasons.push('timezone');
      done([{ reasons }, reasons.length > 0]);
    };
    wk.onerror = () => { clearTimeout(timer); done([{ error: 'worker error' }, false]); };
  }));

  function lbStackSample() { return new Error('lb').stack || ''; }

  probe('stack_engine', 'consistency', 60, () => {
    const st = lbStackSample();
    const v8 = /\n\s+at lbStackSample/.test(st);
    const sm = /^lbStackSample@/m.test(st);
    const engine = v8 ? 'v8' : sm ? 'spidermonkey' : 'unknown';
    const bad = (family === 'chrome' && engine !== 'v8') || (family === 'firefox' && engine !== 'spidermonkey');
    return [{ engine, sample: st.split('\n').slice(0, 2).join(' | ').slice(0, 160) }, bad];
  });

  probe('stack_hooked', 'automation', 25, () => {
    const reasons = [];
    if (Error.prepareStackTrace !== undefined) reasons.push('Error.prepareStackTrace set');
    if (family === 'chrome' && Error.stackTraceLimit !== 10) reasons.push('stackTraceLimit ' + Error.stackTraceLimit);
    return [{ reasons }, reasons.length > 0];
  });

  probe('connection', 'consistency', 10, () => {
    const c = navigator.connection;
    const v = c ? { effectiveType: c.effectiveType, rtt: c.rtt, downlink: c.downlink, saveData: c.saveData } : null;
    if (family === 'chrome') return [v, !c];
    if (family === 'firefox') return [v, !!c];
    return [v, false];
  });

  probe('battery', 'consistency', 10, async () => {
    if (!navigator.getBattery) return [{ supported: false }, family === 'chrome'];
    const b = await navigator.getBattery();
    return [{ charging: b.charging, level: b.level, chargingTime: b.chargingTime,
      dischargingTime: b.dischargingTime }, false];
  });

  // ---- behavioural probes (recomputed for every beacon) -------------------

  function behaviorProbes() {
    const out = [];
    const add = (id, weight, value, suspicious) => out.push({ id, cat: 'behavior', value, suspicious: !!suspicious, weight });
    const ev = trace;
    const tr = ev.filter((e) => e.tr);
    const counts = {};
    let untrusted = 0;
    for (const e of ev) {
      counts[e.type] = (counts[e.type] || 0) + 1;
      if (!e.tr && e.type !== 'scroll' && e.type !== 'focus' && e.type !== 'blur' && e.type !== 'visibilitychange') untrusted++;
    }
    add('beh_counts', 0, { counts, untrusted, dwellMs: Math.round(now()) }, false);
    add('beh_untrusted', 70, untrusted, untrusted > 0);

    const of = (t) => tr.filter((e) => e.type === t);
    const moves = of('mousemove');
    const downs = of('mousedown');
    const ups = of('mouseup');
    const clicks = of('click').filter((c) => c.d > 0); // detail 0 = keyboard activation
    const keydowns = of('keydown').filter((k) => !k.rp);
    const keyups = of('keyup');
    const wheels = of('wheel');
    const touches = of('touchstart');

    // Time to first deliberate interaction. Bots act the moment `load`
    // fires; people either act during loading or need a reaction time.
    const firstAct = tr.find((e) => ['mousedown', 'keydown', 'wheel', 'touchstart'].includes(e.type));
    const ttfi = firstAct ? firstAct.t : null;
    const nav = performance.getEntriesByType && performance.getEntriesByType('navigation')[0];
    const loadEnd = nav && nav.loadEventEnd ? r1(nav.loadEventEnd) : null;
    const afterLoad = ttfi !== null && loadEnd !== null ? r1(ttfi - loadEnd) : null;
    add('beh_ttfi', 25, { ttfi, afterLoad }, ttfi !== null && (ttfi < 300 || (afterLoad !== null && afterLoad >= 0 && afterLoad < 150)));

    // mousedown -> mouseup hold time.
    const holds = [];
    for (const d of downs) {
      const u = ups.find((x) => x.t >= d.t);
      if (u) holds.push(u.t - d.t);
    }
    const holdMed = median(holds);
    add('beh_click_hold', 30, { median: holdMed, n: holds.length }, holdMed !== null && holdMed < 8);

    // Clicks at the exact centre of the target.
    const sized = clicks.filter((c) => c.w >= 10 && c.h >= 10);
    const centred = sized.filter((c) => Math.abs(c.cx) < 1 && Math.abs(c.cy) < 1);
    add('beh_click_center', 40, { centred: centred.length, clicks: sized.length },
      sized.length > 0 && centred.length / sized.length >= 0.5);

    // Clicks with (almost) no pointer path before them.
    let prevT = -Infinity;
    let bare = 0;
    for (const c of clicks) {
      const d = downs.filter((x) => x.t <= c.t).pop();
      const at = d ? d.t : c.t;
      const touched = touches.some((x) => x.t <= at && at - x.t < 800);
      const n = moves.filter((m) => m.t > prevT && m.t <= at && at - m.t < 1500).length;
      if (!touched && n < 3) bare++;
      prevT = c.t;
    }
    add('beh_click_no_path', 35, { bare, clicks: clicks.length }, clicks.length > 0 && bare / clicks.length >= 0.5);

    // Mouse path shape. Split moves into strokes at pauses > 250 ms, button
    // presses and turns > 25 degrees, then look at strokes >= 50 px long.
    const strokes = [];
    let cur = [];
    let lastDir = null;
    for (const m of moves) {
      const last = cur[cur.length - 1];
      if (last) {
        const pressed = downs.some((d) => d.t > last.t && d.t <= m.t);
        if (m.t - last.t > 250 || pressed) {
          strokes.push(cur); cur = []; lastDir = null;
        } else if (Math.hypot(m.x - last.x, m.y - last.y) >= 2) {
          const dir = Math.atan2(m.y - last.y, m.x - last.x);
          if (lastDir !== null) {
            let d = Math.abs(dir - lastDir);
            if (d > Math.PI) d = 2 * Math.PI - d;
            if (d > (25 * Math.PI) / 180) { strokes.push(cur); cur = [last]; }
          }
          lastDir = dir;
        }
      }
      cur.push(m);
    }
    if (cur.length) strokes.push(cur);
    const shaped = [];
    for (const r of strokes) {
      if (r.length < 5) continue;
      const a = r[0], b = r[r.length - 1];
      if (Math.hypot(b.x - a.x, b.y - a.y) < 50) continue;
      // Speed profile: people accelerate then brake, so the middle of a
      // stroke is faster than its ends. Interpolated moves are flat.
      const v = [];
      for (let i = 1; i < r.length; i++) {
        const dt = r[i].t - r[i - 1].t;
        if (dt > 0) v.push(Math.hypot(r[i].x - r[i - 1].x, r[i].y - r[i - 1].y) / dt);
      }
      if (v.length < 8) continue;
      const q = Math.floor(v.length / 4);
      const ends = mean(v.slice(0, q).concat(v.slice(v.length - q)));
      const mid = mean(v.slice(q, v.length - q));
      if (ends > 0) shaped.push(Math.round((mid / ends) * 100) / 100);
    }

    // Straightness: cut the path (at pauses and presses only) into the
    // longest pieces whose points stay within 1 px of the piece's chord.
    // Interpolated moves are almost all long straight pieces.
    const dist = (p, q2) => Math.hypot(q2.x - p.x, q2.y - p.y);
    const fits = (r, i, j) => {
      const a = r[i], b = r[j];
      const c = dist(a, b);
      if (c < 1) return true;
      for (let k = i + 1; k < j; k++) {
        const p = r[k];
        const perp = Math.abs((b.x - a.x) * (a.y - p.y) - (a.x - p.x) * (b.y - a.y)) / c;
        const along = ((p.x - a.x) * (b.x - a.x) + (p.y - a.y) * (b.y - a.y)) / c;
        if (perp > 1 || along < -1 || along > c + 1) return false;
      }
      return true;
    };
    let pathPx = 0, straightPx = 0;
    const runs = [];
    let run = [];
    for (const m of moves.slice(-3000)) {
      const last = run[run.length - 1];
      if (last && (m.t - last.t > 250 || downs.some((d) => d.t > last.t && d.t <= m.t))) { runs.push(run); run = []; }
      run.push(m);
    }
    if (run.length) runs.push(run);
    for (const r of runs) {
      for (let k = 1; k < r.length; k++) pathPx += dist(r[k - 1], r[k]);
      let i = 0;
      while (i < r.length - 1) {
        let j = i + 1;
        while (j + 1 < r.length && fits(r, i, j + 1)) j++;
        if (j - i + 1 >= 6 && dist(r[i], r[j]) >= 50) straightPx += dist(r[i], r[j]);
        i = j;
      }
    }
    const ratio = pathPx > 0 ? Math.round((straightPx / pathPx) * 100) / 100 : null;
    add('beh_mouse_linear', 35, { pathPx: Math.round(pathPx), straightPx: Math.round(straightPx), ratio },
      pathPx >= 200 && ratio >= 0.85);
    add('beh_mouse_velocity', 25, shaped.slice(0, 8), shaped.length > 0 && shaped.every((b) => b < 1.15));

    // movementX/Y: real devices report deltas; injected events often do not.
    let moved = 0, zeroDelta = 0;
    for (let i = 1; i < moves.length; i++) {
      if (moves[i].x !== moves[i - 1].x || moves[i].y !== moves[i - 1].y) {
        moved++;
        if (!moves[i].mx && !moves[i].my) zeroDelta++;
      }
    }
    add('beh_movement_zero', 30, { moved, zeroDelta }, moved >= 5 && zeroDelta === moved);

    // Wheel: one huge synthetic delta, or a perfectly regular stream.
    const big = wheels.filter((w) => w.dm === 0 && Math.abs(w.dy) > 500).length;
    const gaps = [];
    for (let i = 1; i < wheels.length; i++) gaps.push(wheels[i].t - wheels[i - 1].t);
    const sameDelta = wheels.length >= 4 && wheels.every((w) => w.dy === wheels[0].dy);
    const gapCv = cv(gaps);
    add('beh_wheel_pattern', 20, { n: wheels.length, big, sameDelta, gapCv: gapCv === null ? null : r1(gapCv * 100) / 100 },
      big > 0 || (sameDelta && gapCv !== null && gapCv < 0.15));

    // Scrolls with no wheel, key, touch or pointer press just before them.
    const inputs = tr.filter((e) => ['wheel', 'keydown', 'touchmove', 'touchstart', 'mousedown'].includes(e.type));
    let lastY = 0, orphan = 0;
    for (const s of ev.filter((e) => e.type === 'scroll')) {
      if (s.sy === lastY) continue;
      lastY = s.sy;
      if (!inputs.some((i) => i.t <= s.t && s.t - i.t < 700)) orphan++;
    }
    add('beh_scroll_no_input', 25, orphan, orphan > 0);

    // Text entered with no key events (insertText from CDP / fill()).
    let noKey = 0, maxChunk = 0;
    for (const i of tr.filter((e) => e.type === 'input' && /^insert(Text|CompositionText)?$/.test(e.it))) {
      maxChunk = Math.max(maxChunk, i.dl);
      if (!keydowns.some((k) => k.t <= i.t && i.t - k.t < 150)) noKey++;
    }
    add('beh_typing_no_keys', 45, { inputs: noKey, maxChunk }, noKey > 0);

    // Key rhythm: inter-key intervals and key hold (dwell) times.
    const iki = [];
    for (let i = 1; i < keydowns.length; i++) {
      const g = keydowns[i].t - keydowns[i - 1].t;
      if (g < 2000) iki.push(g);
    }
    const ikiSpread = rcv(iki);
    add('beh_key_rhythm', 35, { n: iki.length, median: median(iki), spread: ikiSpread === null ? null : r1(ikiSpread * 100) / 100 },
      iki.length >= 5 && ikiSpread < 0.1);
    const dwell = [];
    for (const d of keydowns) {
      const u = keyups.find((x) => x.k === d.k && x.t >= d.t);
      if (u) dwell.push(u.t - d.t);
    }
    const dwellMed = median(dwell);
    const dwellSpread = rcv(dwell);
    add('beh_key_dwell', 35, { n: dwell.length, median: dwellMed, spread: dwellSpread === null ? null : r1(dwellSpread * 100) / 100 },
      (dwell.length >= 3 && dwellMed < 10) || (dwell.length >= 5 && dwellSpread < 0.08));

    // Key or button input while the document claims not to have focus.
    const unfocused = tr.filter((e) => (e.type === 'keydown' || e.type === 'mousedown') && e.f === 0).length;
    add('beh_input_without_focus', 30, unfocused, unfocused > 0);

    return out;
  }

  // ---- beacons ------------------------------------------------------------

  let seq = 0;
  let sentTrace = 0;
  let fpSent = false;
  let loadedAt = null;

  function send(reason, viaBeacon) {
    const mark = trace.length;
    const fpN = fp.length;
    const body = JSON.stringify({
      sid: ctx.sid,
      page: { id: pageId, url: location.pathname + location.search, seq: ++seq, reason, t: Math.round(now()) },
      probes: (fpSent ? [] : fp.slice()).concat(behaviorProbes()),
      traceFrom: sentTrace,
      trace: trace.slice(sentTrace, mark),
    });
    const ok = () => {
      sentTrace = Math.max(sentTrace, mark);
      if (fpN === P.length) fpSent = true;
    };
    if (viaBeacon && navigator.sendBeacon && body.length < 60000) {
      if (navigator.sendBeacon('/api/report', new Blob([body], { type: 'application/json' }))) ok();
      return;
    }
    fetch('/api/report', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body,
      keepalive: body.length < 60000, credentials: 'same-origin' })
      .then((r) => { if (r.ok) ok(); }).catch(() => {});
  }

  function start() {
    loadedAt = now();
    Promise.all(P).then(() => {
      if (frame) { frame.remove(); frame = null; }
      send('load', false);
      setTimeout(() => send('t3', false), Math.max(0, 3000 - (now() - loadedAt)));
    });
  }
  addEventListener('pagehide', () => send('pagehide', true));
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'hidden') send('hidden', true);
  });
  // Flush long traces so no single beacon gets too large.
  setInterval(() => { if (trace.length - sentTrace > 1500) send('flush', false); }, 1000);

  if (document.readyState === 'complete') start();
  else addEventListener('load', start, { once: true });
})();

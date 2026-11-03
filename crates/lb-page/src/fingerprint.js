// Fingerprint surface collector for `LightBrowser.fingerprintReport`.
// Runs in a throwaway isolated world of the page (never the main world),
// with CFG (persona-derived names to probe) defined before it. Resolves to
// a JSON string. Every probe is guarded: a missing surface becomes
// {"__error": "..."} instead of failing the report.
(async () => {
  const out = {};
  const probe = async (key, f) => {
    try {
      out[key] = await f();
    } catch (e) {
      out[key] = { __error: String((e && e.message) || e) };
    }
  };
  const n = navigator;
  const fnv = (s) => {
    let h = 0x811c9dc5;
    for (let i = 0; i < s.length; i++) {
      h ^= s.charCodeAt(i);
      h = Math.imul(h, 16777619) >>> 0;
    }
    return h.toString(16).padStart(8, '0');
  };
  const plain = (v) => (v && typeof v === 'object' && typeof v.length === 'number' ? Array.from(v) : v);

  await probe('navigator', () => ({
    userAgent: n.userAgent,
    appVersion: n.appVersion,
    appName: n.appName,
    appCodeName: n.appCodeName,
    platform: n.platform,
    product: n.product,
    productSub: n.productSub,
    vendor: n.vendor,
    vendorSub: n.vendorSub,
    language: n.language,
    languages: Array.from(n.languages || []),
    hardwareConcurrency: n.hardwareConcurrency,
    deviceMemory: n.deviceMemory === undefined ? null : n.deviceMemory,
    maxTouchPoints: n.maxTouchPoints,
    cookieEnabled: n.cookieEnabled,
    pdfViewerEnabled: n.pdfViewerEnabled === undefined ? null : n.pdfViewerEnabled,
    doNotTrack: n.doNotTrack === undefined ? null : n.doNotTrack,
    webdriver: n.webdriver,
    webdriverOwn: Object.getOwnPropertyNames(n).includes('webdriver'),
    plugins: Array.from(n.plugins || []).map((p) => p.name),
    mimeTypes: Array.from(n.mimeTypes || []).map((m) => m.type),
  }));
  await probe('uaData', () => {
    const d = n.userAgentData;
    if (!d) return null;
    return {
      brands: d.brands.map((b) => ({ brand: b.brand, version: b.version })),
      mobile: d.mobile,
      platform: d.platform,
    };
  });
  await probe('uaHigh', async () => {
    const d = n.userAgentData;
    if (!d) return null;
    const v = await d.getHighEntropyValues([
      'architecture', 'bitness', 'model', 'platformVersion', 'uaFullVersion', 'fullVersionList', 'wow64',
      'formFactors',
    ]);
    return {
      architecture: v.architecture,
      bitness: v.bitness,
      model: v.model,
      platformVersion: v.platformVersion,
      uaFullVersion: v.uaFullVersion,
      wow64: v.wow64,
      formFactors: v.formFactors,
      fullVersionList: (v.fullVersionList || []).map((b) => ({ brand: b.brand, version: b.version })),
    };
  });
  await probe('connection', () => {
    const c = n.connection;
    if (!c) return null;
    return { effectiveType: c.effectiveType, rtt: c.rtt, downlink: c.downlink, saveData: c.saveData };
  });
  await probe('screen', () => ({
    width: screen.width,
    height: screen.height,
    availWidth: screen.availWidth,
    availHeight: screen.availHeight,
    availTop: screen.availTop,
    availLeft: screen.availLeft,
    colorDepth: screen.colorDepth,
    pixelDepth: screen.pixelDepth,
    devicePixelRatio: devicePixelRatio,
  }));
  await probe('window', () => ({ innerWidth, innerHeight, outerWidth, outerHeight, screenX, screenY }));
  // Scrollbar width of an overflow:scroll box.
  await probe('layout', () => {
    const d = document.createElement('div');
    d.style.cssText = 'position:absolute;left:-9999px;top:0;width:100px;height:100px;overflow:scroll';
    (document.body || document.documentElement).appendChild(d);
    const w = d.offsetWidth - d.clientWidth;
    d.remove();
    return { scrollbarWidth: w };
  });
  // Generic families: the width of each keyword equals that of the
  // persona's default family.
  await probe('genericFonts', () => {
    const x = document.createElement('canvas').getContext('2d');
    const text = 'mmmmmmmmmmlli1|WwQq@#';
    const width = (f) => {
      x.font = `72px ${f}`;
      return x.measureText(text).width;
    };
    const r = {};
    for (const [g, fam] of Object.entries(CFG.genericFonts)) {
      r[g] = width(g) === width(`"${fam}"`);
    }
    return r;
  });
  // OS-gated APIs, codecs and WebGPU.
  await probe('osApis', () => ({
    share: 'share' in n,
    canShare: 'canShare' in n,
    bluetooth: 'bluetooth' in n,
    Bluetooth: typeof Bluetooth !== 'undefined',
  }));
  await probe('codecs', async () => {
    const v = document.createElement('video');
    const h264 = 'video/mp4; codecs="avc1.640028"';
    const hevc = 'video/mp4; codecs="hvc1.1.6.L93.B0"';
    const av1 = 'video/mp4; codecs="av01.0.05M.08"';
    const r = { hevc: v.canPlayType(hevc) === 'probably' };
    if (n.mediaCapabilities) {
      const info = (contentType) => n.mediaCapabilities.decodingInfo({
        type: 'file', video: { contentType, width: 1920, height: 1080, bitrate: 5000000, framerate: 30 },
      });
      r.h264PowerEfficient = (await info(h264)).powerEfficient;
      r.av1PowerEfficient = (await info(av1)).powerEfficient;
    }
    return r;
  });
  await probe('webgpu', async () => {
    if (!n.gpu) return null;
    const a = await n.gpu.requestAdapter();
    return a ? { vendor: a.info.vendor, architecture: a.info.architecture } : null;
  });
  await probe('locale', () => {
    const o = Intl.DateTimeFormat().resolvedOptions();
    return { timeZone: o.timeZone, locale: o.locale, offset: new Date().getTimezoneOffset() };
  });
  await probe('webgl', () => {
    const gl = document.createElement('canvas').getContext('webgl2');
    if (!gl) return null;
    const dbg = gl.getExtension('WEBGL_debug_renderer_info');
    const params = {};
    for (const name of CFG.webglParams) {
      const e = gl[name];
      params[name] = e === undefined ? { __error: 'unknown enum' } : plain(gl.getParameter(e));
    }
    const precision = {};
    for (const key of CFG.shaderPrecision) {
      const [shader, kind] = key.split('.');
      const f = gl.getShaderPrecisionFormat(gl[shader], gl[kind]);
      precision[key.replace('.', ':')] = f ? { range_min: f.rangeMin, range_max: f.rangeMax, precision: f.precision } : null;
    }
    // Seeded readback: a cleared buffer with one triangle, read twice.
    const pixels = () => {
      const c = document.createElement('canvas');
      c.width = 64;
      c.height = 64;
      const g = c.getContext('webgl');
      if (!g) return null;
      const vs = g.createShader(g.VERTEX_SHADER);
      g.shaderSource(vs, 'attribute vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }');
      g.compileShader(vs);
      const fs = g.createShader(g.FRAGMENT_SHADER);
      g.shaderSource(fs, 'precision mediump float; uniform vec4 c; void main() { gl_FragColor = c; }');
      g.compileShader(fs);
      const pr = g.createProgram();
      g.attachShader(pr, vs);
      g.attachShader(pr, fs);
      g.linkProgram(pr);
      g.useProgram(pr);
      g.bindBuffer(g.ARRAY_BUFFER, g.createBuffer());
      g.bufferData(g.ARRAY_BUFFER, new Float32Array([-0.8, -0.7, 0.9, -0.6, 0.1, 0.85]), g.STATIC_DRAW);
      const loc = g.getAttribLocation(pr, 'p');
      g.enableVertexAttribArray(loc);
      g.vertexAttribPointer(loc, 2, g.FLOAT, false, 0, 0);
      g.uniform4f(g.getUniformLocation(pr, 'c'), 0.2, 0.6, 0.9, 1);
      g.clearColor(0.95, 0.4, 0.1, 1);
      g.clear(g.COLOR_BUFFER_BIT);
      g.drawArrays(g.TRIANGLES, 0, 3);
      const px = new Uint8Array(64 * 64 * 4);
      g.readPixels(0, 0, 64, 64, g.RGBA, g.UNSIGNED_BYTE, px);
      return fnv(Array.from(px).join(',')) + ':' + fnv(c.toDataURL());
    };
    const p1 = pixels();
    const p2 = pixels();
    return {
      pixelHash: p1,
      pixelStable: p1 === p2,
      vendor: gl.getParameter(gl.VENDOR),
      renderer: gl.getParameter(gl.RENDERER),
      version: gl.getParameter(gl.VERSION),
      glsl: gl.getParameter(gl.SHADING_LANGUAGE_VERSION),
      unmaskedVendor: dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) : null,
      unmaskedRenderer: dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : null,
      extensions: gl.getSupportedExtensions(),
      params,
      precision,
    };
  });
  await probe('canvas', () => {
    const draw = () => {
      const c = document.createElement('canvas');
      c.width = 220;
      c.height = 40;
      const x = c.getContext('2d');
      x.textBaseline = 'top';
      x.font = '16px Arial';
      x.fillStyle = '#f60';
      x.fillRect(125, 1, 62, 20);
      x.fillStyle = '#069';
      x.fillText('Cwm fjordbank glyphs vext quiz', 2, 15);
      x.fillStyle = 'rgba(102, 204, 0, 0.7)';
      x.fillText('Cwm fjordbank glyphs vext quiz', 4, 17);
      return c.toDataURL();
    };
    const a = draw();
    const b = draw();
    const s = document.createElement('canvas');
    s.width = 4;
    s.height = 4;
    const sx = s.getContext('2d');
    sx.fillStyle = 'rgb(10, 20, 30)';
    sx.fillRect(0, 0, 4, 4);
    const d = sx.getImageData(0, 0, 4, 4).data;
    let solid = true;
    for (let i = 0; i < d.length; i += 4) {
      if (d[i] !== 10 || d[i + 1] !== 20 || d[i + 2] !== 30 || d[i + 3] !== 255) solid = false;
    }
    return { hash: fnv(a), stable: a === b, solidExact: solid };
  });
  await probe('audio', async () => {
    if (typeof OfflineAudioContext === 'undefined') return null;
    const ctx = new OfflineAudioContext(1, 5000, 44100);
    const osc = ctx.createOscillator();
    osc.type = 'triangle';
    osc.frequency.value = 10000;
    const comp = ctx.createDynamicsCompressor();
    comp.threshold.value = -50;
    comp.knee.value = 40;
    comp.ratio.value = 12;
    comp.attack.value = 0;
    comp.release.value = 0.25;
    osc.connect(comp);
    comp.connect(ctx.destination);
    osc.start(0);
    const buf = await ctx.startRendering();
    const data = buf.getChannelData(0);
    let sum = 0;
    for (let i = 4500; i < 5000; i++) sum += Math.abs(data[i]);
    const hash = fnv(Array.from(data).join(','));
    let live = null;
    if (typeof AudioContext !== 'undefined') {
      const a = new AudioContext();
      live = { sampleRate: a.sampleRate, baseLatency: a.baseLatency, maxChannelCount: a.destination.maxChannelCount };
      // outputLatency is 0 until the context runs.
      if (a.resume) await a.resume();
      live.outputLatency = a.state === 'running' ? a.outputLatency : null;
      if (a.close) a.close();
    }
    return { sum: Math.round(sum * 1e6) / 1e6, hash, live };
  });
  await probe('fonts', () => {
    const x = document.createElement('canvas').getContext('2d');
    const text = 'mmmmmmmmmmlli1|WwQq@#';
    const bases = ['monospace', 'sans-serif', 'serif'];
    const base = {};
    for (const b of bases) {
      x.font = `72px ${b}`;
      base[b] = x.measureText(text).width;
    }
    const has = (f) =>
      bases.some((b) => {
        x.font = `72px "${f}", ${b}`;
        return x.measureText(text).width !== base[b];
      });
    // Widths of every persona font against each base, as one hash.
    const widths = CFG.fonts.map((f) =>
      bases.map((b) => {
        x.font = `72px "${f}", ${b}`;
        return x.measureText(text).width;
      }).join('/'),
    );
    return {
      hash: fnv(widths.join(',')),
      missing: CFG.fonts.filter((f) => !has(f)),
      unexpected: CFG.absentFonts.filter((f) => has(f)),
      checked: CFG.fonts.length,
    };
  });
  await probe('permissions', async () => {
    const r = {};
    for (const name of CFG.permissions) {
      try {
        r[name] = (await n.permissions.query({ name })).state;
      } catch (e) {
        r[name] = { __error: String(e.message || e) };
      }
    }
    return r;
  });
  await probe('notification', () => (typeof Notification === 'undefined' ? null : Notification.permission));
  await probe('battery', async () => {
    if (!n.getBattery) return null;
    const b = await n.getBattery();
    return { level: b.level, charging: b.charging };
  });
  await probe('mediaDevices', async () => {
    if (!n.mediaDevices) return null;
    const c = {};
    for (const d of await n.mediaDevices.enumerateDevices()) c[d.kind] = (c[d.kind] || 0) + 1;
    return c;
  });
  await probe('voices', () => (typeof speechSynthesis === 'undefined' ? null : speechSynthesis.getVoices().map((v) => v.name)));
  await probe('media', () => ({
    dark: matchMedia('(prefers-color-scheme: dark)').matches,
    reducedMotion: matchMedia('(prefers-reduced-motion: reduce)').matches,
    p3: matchMedia('(color-gamut: p3)').matches,
  }));
  await probe('performance', () => {
    let tenth = true;
    for (let i = 0; i < 200; i++) {
      const t = performance.now() * 10;
      if (Math.abs(t - Math.round(t)) > 1e-6) tenth = false;
    }
    return { tenthMs: tenth };
  });
  return JSON.stringify(out);
})()

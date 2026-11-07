// Record Chrome's time zone, locale, acceptLanguage and geolocation
// overrides (ADR 0039) on serve.py's pages, as Playwright sets them.
// Usage: python3 serve.py 19173 & CHROME=/path/to/chrome-headless-shell \
//   node chrome.mjs http://127.0.0.1:19173 > chrome.json
// With CDP=ws://127.0.0.1:PORT/devtools/browser/ID it drives that browser
// over connectOverCDP instead (LightBrowser: the cases needing
// Browser.grantPermissions are skipped; use a persona that grants
// geolocation), for comparing by hand.
const { chromium } = await import('playwright');
const base = process.argv[2];
const b = process.env.CDP
  ? await chromium.connectOverCDP(process.env.CDP)
  : await chromium.launch({ executablePath: process.env.CHROME, args: ['--disable-background-networking', '--no-proxy-server'] });
const lb = !!process.env.CDP;
// LightBrowser keeps the persona's window (ADR 0009: viewport null).
const newContext = (o = {}) => b.newContext(lb ? { ...o, viewport: null } : o);
const out = { version: b.version() };
const ev = (p, f, a) => p.evaluate(f, a).catch(e => ({ thrown: e.message }));
const send = async (s, m, p) => { try { return { ok: await s.send(m, p) }; } catch (e) { return { err: e.message.replace(/^.*?: Protocol error \([^)]*\): /, '') }; } };
const all = async (p) => ({
  main: await ev(p, () => probe()),
  frame: await ev(p, () => framed()),
  blank: await ev(p, () => blankFrame()),
  worker: await ev(p, () => ask(dw)),
  newWorker: await ev(p, () => newWorker()),
  shared: await ev(p, () => askShared()),
  sw: await ev(p, () => askSW()),
  acceptLanguage: await ev(p, () => acceptLanguage()),
});

// 1. newContext({timezoneId, locale}): set before the page loads.
{
  const ctx = await newContext({ timezoneId: 'Asia/Tokyo', locale: 'de-DE' });
  const p = await ctx.newPage();
  await p.goto(base + '/');
  out.context = await all(p);
  await ctx.close();
}

// 2. Raw CDP after the page and its workers run: every realm follows.
{
  const ctx = await newContext();
  const p = await ctx.newPage();
  await p.goto(base + '/');
  await ev(p, () => Promise.all([ask(dw), askShared(), askSW()]));
  const s = await ctx.newCDPSession(p);
  await s.send('Emulation.setTimezoneOverride', { timezoneId: 'America/Sao_Paulo' });
  await s.send('Emulation.setLocaleOverride', { locale: 'fr-FR' });
  out.late = await all(p);
  await p.reload();
  out.reload = { main: await ev(p, () => probe()), worker: await ev(p, () => ask(dw)) };
  // Zone ids as ICU canonicalizes them, locales as Blink passes them.
  out.ids = {};
  for (const id of ['UTC', 'Etc/UTC', 'GMT', 'Etc/GMT-14', 'EST', 'EST5EDT', 'US/Pacific', 'Asia/Calcutta', 'GMT+5', '+05:00', 'Europe/Kiev', 'Factory']) {
    out.ids[id] = { r: await send(s, 'Emulation.setTimezoneOverride', { timezoneId: id }), v: await ev(p, () => { const x = probe(); return [x.tz, x.off, x.str]; }) };
  }
  out.locales = {};
  for (const l of ['de_DE', 'EN-gb', 'zh-Hant-TW', 'iw', 'en-US-u-ca-buddhist', 'ja', 'xx', 'C']) {
    out.locales[l] = { r: await send(s, 'Emulation.setLocaleOverride', { locale: l }), v: await ev(p, () => { const x = probe(); return [x.loc, x.nloc, x.ls, x.num, x.lang]; }) };
  }
  await send(s, 'Emulation.setLocaleOverride', {});
  out.cleared = await ev(p, () => [probe().loc === Intl.DateTimeFormat().resolvedOptions().locale, probe().tz]);
  // acceptLanguage: navigator.languages (window, not workers) and the header.
  await s.send('Emulation.setUserAgentOverride', { userAgent: '', acceptLanguage: 'fr-CH, fr;q=0.9,en_GB' });
  out.acceptLanguage = { langs: await ev(p, () => [navigator.language, navigator.languages, navigator.languages === navigator.languages]), worker: await ev(p, () => newWorker().then(x => x.langs)), header: await ev(p, () => acceptLanguage()) };
  await ctx.close();
}

// 3. Geolocation.
{
  const G = out.geo = {};
  const opts = { geolocation: { latitude: 52.52, longitude: 13.405, accuracy: 20 } };
  const ctx = await newContext(lb ? opts : { ...opts, permissions: ['geolocation'] });
  const p = await ctx.newPage();
  await p.goto(base + '/geo.html');
  G.surface = await ev(p, () => surface());
  G.granted = await ev(p, () => one());
  const s = await ctx.newCDPSession(p);
  await s.send('Emulation.setGeolocationOverride', { latitude: -33.8688, longitude: 151.2093, accuracy: 5, altitude: 58, altitudeAccuracy: 3, heading: 90, speed: 1.5 });
  G.full = await ev(p, () => one());
  await s.send('Emulation.setGeolocationOverride', { latitude: 90, longitude: -180, accuracy: 0, heading: 400, speed: -1, altitudeAccuracy: -1 });
  G.edge = await ev(p, () => one());
  await s.send('Emulation.setGeolocationOverride', { latitude: 1, longitude: 2 });
  G.noAccuracy = await ev(p, () => one());
  await s.send('Emulation.setGeolocationOverride', {});
  G.empty = await ev(p, () => one());
  await s.send('Emulation.setGeolocationOverride', { latitude: 1, longitude: 2, accuracy: 3 });
  G.timeout0 = await ev(p, () => one({ timeout: 0 }));
  await s.send('Emulation.clearGeolocationOverride');
  G.cleared = await ev(p, () => one({ timeout: 300 }));
  // A request waiting without an override gets the next one.
  const pending = ev(p, () => one({ timeout: 5000 }));
  await p.waitForTimeout(300);
  await s.send('Emulation.setGeolocationOverride', { latitude: 7, longitude: 8, accuracy: 9 });
  G.pendingThenSet = await pending;
  await ctx.close();
  if (!lb) {
    const c2 = await newContext(opts);
    const p2 = await c2.newPage();
    await p2.goto(base + '/geo.html');
    G.prompt = await ev(p2, () => one());
    await c2.close();
  }
}
await b.close();
console.log(JSON.stringify(out, null, 1));

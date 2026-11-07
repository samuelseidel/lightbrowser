// Record Chrome's replies to the locale override commands (ADR 0039) on an
// about:blank page with two sessions. Writes JSON to stdout: the browser
// version and, per step, the session (1 or 2), method, params and reply
// (`result`, or `error` with code, message and data without Chrome's
// " at position N"). A step with method "detach" detaches that session.
// Usage: CHROME=/path/to/chrome-headless-shell node record.mjs > chrome.json
import { spawn } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const chrome = process.env.CHROME;
const port = process.env.PORT || '19172';
const dir = mkdtempSync(join(tmpdir(), 'lb-emu-'));
const proc = spawn(chrome, ['--headless', '--no-sandbox', `--user-data-dir=${dir}`, `--remote-debugging-port=${port}`,
  '--remote-debugging-address=127.0.0.1', '--disable-background-networking', '--no-proxy-server', 'about:blank'], { stdio: 'ignore' });
let version;
for (let i = 0; i < 100 && !version; i++) {
  await new Promise(r => setTimeout(r, 100));
  version = await fetch(`http://127.0.0.1:${port}/json/version`).then(r => r.json()).catch(() => null);
}
const sock = new WebSocket(version.webSocketDebuggerUrl);
let id = 0; const pending = new Map();
sock.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); } };
await new Promise(r => sock.onopen = r);
const send = (method, params, sessionId) => new Promise(r => { const i = ++id; pending.set(i, r); sock.send(JSON.stringify({ id: i, method, params, sessionId })); });
const { targetId } = (await send('Target.createTarget', { url: 'about:blank' })).result;
const sessions = {};
for (const n of [1, 2]) sessions[n] = (await send('Target.attachToTarget', { targetId, flatten: true })).result.sessionId;
const T = 'Emulation.setTimezoneOverride', L = 'Emulation.setLocaleOverride', G = 'Emulation.setGeolocationOverride';
const steps = [
  [1, T, {}], [1, T, { timezoneId: 5 }], [1, T, { timezoneId: 'Mars/Olympus' }], [1, T, { timezoneId: 'europe/berlin' }],
  [1, T, { timezoneId: 'Europe/Berlin ' }], [1, T, { timezoneId: 'Local' }], [1, T, { timezoneId: 'utc' }],
  [1, T, { timezoneId: '' }], [1, T, { timezoneId: 'Asia/Tokyo' }], [2, T, { timezoneId: 'Asia/Tokyo' }],
  [2, T, { timezoneId: 'Europe/London' }], [2, T, { timezoneId: '' }], [1, T, { timezoneId: 'Mars/Olympus' }],
  [1, T, { timezoneId: 'Europe/London' }], [2, T, { timezoneId: 'Asia/Tokyo' }], [1, T, { timezoneId: '' }],
  [2, T, { timezoneId: 'Asia/Tokyo' }], [2, T, { timezoneId: '' }],
  [1, L, { locale: 5 }], [1, L, { locale: 'not a locale' }], [1, L, { locale: 'root' }], [1, L, { locale: 'ROOT' }],
  [1, L, { locale: 'abcdefgh' }], [1, L, { locale: 'abcdefghi' }], [1, L, { locale: 'abcdefghijk' }], [1, L, { locale: 'abcdefghijkl' }], [1, L, { locale: 'ab cd' }], [1, L, { locale: 'ab1' }], [1, L, { locale: 'X-foo' }], [1, L, { locale: 'I-klingon' }], [1, L, { locale: 'i' }], [1, L, { locale: 'x' }],
  [1, L, { locale: 'i-klingon' }], [1, L, { locale: 'x-foo' }], [1, L, { locale: '@x' }], [1, L, { locale: '.x' }],
  [1, L, { locale: '-de' }], [1, L, { locale: '_DE' }], [1, L, { locale: 'de@calendar=x' }], [1, L, { locale: 'de.UTF-8' }],
  [1, L, {}], [1, L, { locale: 'de-DE' }], [2, L, { locale: 'de-DE' }], [2, L, {}], [2, L, { locale: 'fr-FR' }],
  [1, L, { locale: 'fr-FR' }], [1, L, {}], [2, L, { locale: 'fr-FR' }], [1, L, { locale: 'de-DE' }], [2, L, {}],
  [1, G, { latitude: 91, longitude: 0, accuracy: 1 }], [1, G, { latitude: 'x' }], [1, G, { latitude: 0, longitude: 181, accuracy: 1 }],
  [1, G, { latitude: 1, longitude: 2, accuracy: -1 }], [1, G, { latitude: 0, longitude: 0, accuracy: 1, heading: 400, speed: -1 }],
  [1, G, {}], [1, G, { latitude: 1, longitude: 2 }], [1, 'Emulation.clearGeolocationOverride', {}],
  [1, 'Emulation.setUserAgentOverride', { userAgent: '', acceptLanguage: 'de-DE' }],
  [1, T, { timezoneId: 'Asia/Tokyo' }], [1, L, { locale: 'de-DE' }],
  [2, T, { timezoneId: 'Europe/London' }], [2, L, { locale: 'fr-FR' }],
  [1, 'detach', {}],
  [2, T, { timezoneId: 'Europe/London' }], [2, L, { locale: 'fr-FR' }],
];
const out = { version: version.Browser, steps: [] };
for (const [n, method, params] of steps) {
  if (method === 'detach') {
    await send('Target.detachFromTarget', { sessionId: sessions[n] });
    out.steps.push({ session: n, method, params, reply: {} });
    continue;
  }
  const r = await send(method, params, sessions[n]);
  const reply = r.error ? { error: { ...r.error } } : { result: r.result };
  if (reply.error && reply.error.data) reply.error.data = reply.error.data.replace(/ at position \d+$/, '');
  out.steps.push({ session: n, method, params, reply });
}
console.log(JSON.stringify(out, null, 1));
sock.close();
proc.kill();

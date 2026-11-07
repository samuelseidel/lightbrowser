//! The navigator / window fingerprint surface from the persona, on real
//! pages served from loopback. Expected shapes were taken from the local
//! Chromium 153 (same expressions evaluated on a loopback page).
//!
//! One test drives both reference personas in turn: ICU's time zone is
//! process-wide, so pages with different zones must not run in parallel.
#![cfg(feature = "chrome")]

mod common;

use std::sync::Arc;

use common::{Resource, browser, open, serve};
use lb_js::JsValue;
use lb_page::{BrowserContext, WaitUntil, context_options_for};
use lb_persona::PersonaProfile;
use serde_json::Value;

const PROBE: &str = r#"<!DOCTYPE html><body><script>
(async () => {
  const r = {};
  const n = navigator, pl = n.plugins, mt = n.mimeTypes, u = n.userAgentData;
  r.ua = n.userAgent; r.platform = n.platform; r.navOwn = Object.getOwnPropertyNames(n);
  r.plugins = Array.from(pl).map((p) => p.name);
  r.pluginKeys = Object.keys(pl);
  r.pluginNames = Object.getOwnPropertyNames(pl);
  r.pluginForIn = []; for (const k in pl) r.pluginForIn.push(k);
  r.mimeJson = JSON.stringify(mt);
  r.pluginsJson = JSON.stringify(pl);
  r.ident = [pl === n.plugins, pl[0] === pl[0], pl[0][0] === pl[0][0], mt[0] === mt.item(0),
    mt[0].enabledPlugin === pl[0], pl[1][1].enabledPlugin === pl[0], pl.item(4294967296) === pl[0],
    pl.namedItem('Chrome PDF Viewer') === pl[1], pl['WebKit built-in PDF'] === pl[4], pl.item(5), pl[5]];
  r.pluginDesc = JSON.stringify([Object.getOwnPropertyDescriptor(pl, '0').writable,
    Object.getOwnPropertyDescriptor(pl, '0').enumerable, Object.getOwnPropertyDescriptor(pl, 'PDF Viewer').enumerable]);
  r.tags = [pl, pl[0], mt[0], u, n.connection, n.permissions, n.mediaDevices, speechSynthesis,
    screen.orientation, performance.memory, performance.timing].map((o) => Object.prototype.toString.call(o));
  r.uaSame = [u === n.userAgentData, u.brands === u.brands, Object.isFrozen(u.brands)];
  r.brands = u.brands; r.uaPlatform = u.platform; r.mobile = u.mobile;
  r.he = await u.getHighEntropyValues(['wow64', 'model', 'architecture', 'formFactors', 'uaFullVersion',
    'bitness', 'platformVersion', 'fullVersionList', 'bogus']);
  r.heKeys = Object.keys(r.he);
  r.heErr = await u.getHighEntropyValues('x').then(() => 'ok', (e) => e.message);
  r.conn = [n.connection.effectiveType, n.connection.rtt, n.connection.downlink, n.connection.saveData];
  const q = async (d) => { try { const s = await n.permissions.query(d); return [s.name, s.state]; } catch (e) { return e.name + ': ' + e.message; } };
  r.perm = [await q({ name: 'camera' }), await q({ name: 'notifications' }), await q({ name: 'clipboard-write' }),
    await q({ name: 'bogus' }), await q({ name: 'push' }), await q({ name: 'nfc' })];
  r.notif = Notification.permission;
  const b = await n.getBattery();
  r.battery = [b.charging, b.level, b.chargingTime, b.dischargingTime === Infinity, n.getBattery() === n.getBattery(), b === await n.getBattery()];
  r.devices = (await n.mediaDevices.enumerateDevices()).map((d) => [d.kind, d.deviceId, d.label, d.groupId, Object.prototype.toString.call(d)]);
  const v = speechSynthesis.getVoices();
  r.voices = [v.length, v.filter((x) => x.default).map((x) => x.name), v.length && v[0] === speechSynthesis.getVoices()[0]];
  r.tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
  r.locale = Intl.DateTimeFormat().resolvedOptions().locale;
  r.offset = [new Date(2026, 0, 15).getTimezoneOffset(), new Date(2026, 6, 15).getTimezoneOffset()];
  const win = Object.getOwnPropertyNames(window);
  r.chrome = [win.indexOf('chrome') - win.indexOf('WebAssembly'), Object.keys(chrome), String(chrome.csi),
    String(chrome.app.getDetails), 'runtime' in chrome, typeof new chrome.loadTimes(), Object.keys(chrome.loadTimes()).length,
    JSON.stringify(Object.getOwnPropertyDescriptor(window, 'chrome').enumerable)];
  r.screen = [screen.width, screen.height, screen.availHeight, screen.orientation.type, devicePixelRatio,
    outerWidth, outerHeight, innerWidth, innerHeight];
  r.mm = ['(prefers-color-scheme: light)', '(color-gamut: srgb)', '(color-gamut: p3)', '(inverted-colors: none)',
    '(dynamic-range: standard)'].map((x) => matchMedia(x).matches);
  const m = performance.memory;
  r.memory = [m === performance.memory, m.jsHeapSizeLimit, m.usedJSHeapSize > 0, m.totalJSHeapSize >= m.usedJSHeapSize,
    Object.getOwnPropertyNames(Object.getPrototypeOf(m)), String(Object.getOwnPropertyDescriptor(Object.getPrototypeOf(m), 'usedJSHeapSize').get)];
  let ticks = true; for (let i = 0; i < 2000; i++) { const t = performance.now() * 10; if (Math.abs(t - Math.round(t)) > 1e-6) ticks = false; }
  r.perfTicks = ticks;
  r.timing = [performance.timing.navigationStart === Math.floor(performance.timeOrigin), performance.navigation.type];
  r.rtc = await new Promise((res) => {
    const pc = new RTCPeerConnection(); const c = [];
    pc.createDataChannel('x');
    pc.onicecandidate = (e) => { if (e.candidate) c.push([e.candidate.type, e.candidate.address]); else res(c); };
    pc.createOffer().then((o) => pc.setLocalDescription(o));
    setTimeout(() => res('timeout'), 2000);
  });
  r.focus = [document.hasFocus(), document.visibilityState];
  window.result = JSON.stringify(r);
})();
</script></body>"#;

fn persona(name: &str) -> Arc<PersonaProfile> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../profiles");
    Arc::new(PersonaProfile::from_path_checked(format!("{dir}/reference/chrome/{name}")).unwrap())
}

async fn probe(url: &str, p: &Arc<PersonaProfile>) -> Value {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../profiles/tls");
    let ctx = BrowserContext::new(
        context_options_for(p, std::path::Path::new(dir), false, true).unwrap(),
    )
    .unwrap();
    let b = browser();
    let page = open(&b, &ctx, url, WaitUntil::Load).await;
    for _ in 0..200 {
        if let JsValue::String(s) = page.evaluate("window.result").await.unwrap() {
            return serde_json::from_str(&s).unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("probe did not finish");
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn navigator_surface_follows_the_persona() {
    let s = serve(vec![("/", Resource::html(PROBE))]).await;
    let url = s.url("/");

    // Windows 10 reference persona.
    let p = persona("chrome153_win10_rtx3060_1080p.json");
    let r = probe(&url, &p).await;
    assert_eq!(r["ua"], p.browser.user_agent.as_str());
    assert_eq!(r["platform"], "Win32");
    assert_eq!(r["navOwn"], serde_json::json!([]));
    assert_eq!(
        strs(&r["plugins"]),
        [
            "PDF Viewer",
            "Chrome PDF Viewer",
            "Chromium PDF Viewer",
            "Microsoft Edge PDF Viewer",
            "WebKit built-in PDF"
        ]
    );
    assert_eq!(strs(&r["pluginKeys"]), ["0", "1", "2", "3", "4"]);
    assert_eq!(
        r["pluginNames"].as_array().unwrap().len(),
        10,
        "{}",
        r["pluginNames"]
    );
    assert_eq!(
        strs(&r["pluginForIn"]),
        [
            "0",
            "1",
            "2",
            "3",
            "4",
            "length",
            "item",
            "namedItem",
            "refresh"
        ]
    );
    assert_eq!(r["mimeJson"], r#"{"0":{},"1":{}}"#);
    assert_eq!(
        r["pluginsJson"],
        r#"{"0":{"0":{},"1":{}},"1":{"0":{},"1":{}},"2":{"0":{},"1":{}},"3":{"0":{},"1":{}},"4":{"0":{},"1":{}}}"#
    );
    assert_eq!(
        r["ident"],
        serde_json::json!([
            true, true, false, true, true, true, true, true, true, null, null
        ])
    );
    assert_eq!(r["pluginDesc"], "[false,true,false]");
    assert_eq!(
        strs(&r["tags"]),
        [
            "[object PluginArray]",
            "[object Plugin]",
            "[object MimeType]",
            "[object NavigatorUAData]",
            "[object NetworkInformation]",
            "[object Permissions]",
            "[object MediaDevices]",
            "[object SpeechSynthesis]",
            "[object ScreenOrientation]",
            "[object MemoryInfo]",
            "[object PerformanceTiming]"
        ]
    );
    assert_eq!(r["uaSame"], serde_json::json!([false, false, true]));
    let brands: Vec<String> = r["brands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            format!(
                "{};{}",
                b["brand"].as_str().unwrap(),
                b["version"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        brands,
        ["Google Chrome;153", "Not_A Brand;8", "Chromium;153"]
    );
    assert_eq!(r["uaPlatform"], "Windows");
    assert_eq!(r["mobile"], false);
    assert_eq!(
        strs(&r["heKeys"]),
        [
            "architecture",
            "bitness",
            "brands",
            "formFactors",
            "fullVersionList",
            "mobile",
            "model",
            "platform",
            "platformVersion",
            "uaFullVersion",
            "wow64"
        ]
    );
    assert_eq!(r["he"]["architecture"], "x86");
    assert_eq!(r["he"]["platformVersion"], "10.0.0");
    assert_eq!(r["he"]["uaFullVersion"], "153.0.8010.12");
    assert_eq!(r["he"]["formFactors"], serde_json::json!(["Desktop"]));
    assert_eq!(
        r["heErr"],
        "Failed to execute 'getHighEntropyValues' on 'NavigatorUAData': The provided value cannot be converted to a sequence."
    );
    assert_eq!(r["conn"], serde_json::json!(["4g", 50, 10, false]));
    assert_eq!(
        r["perm"],
        serde_json::json!([
            ["video_capture", "prompt"],
            ["notifications", "prompt"],
            ["clipboard_write", "granted"],
            "TypeError: Failed to execute 'query' on 'Permissions': Failed to read the 'name' property from 'PermissionDescriptor': The provided value 'bogus' is not a valid enum value of type PermissionName.",
            "NotSupportedError: Failed to execute 'query' on 'Permissions': Push Permission without userVisibleOnly:true isn't supported yet.",
            "TypeError: Failed to execute 'query' on 'Permissions': Web NFC is not enabled."
        ])
    );
    assert_eq!(r["notif"], "default");
    assert_eq!(
        r["battery"],
        serde_json::json!([true, 1, 0, true, true, true])
    );
    assert_eq!(
        r["devices"],
        serde_json::json!([
            ["audioinput", "", "", "", "[object InputDeviceInfo]"],
            ["videoinput", "", "", "", "[object InputDeviceInfo]"],
            ["audiooutput", "", "", "", "[object MediaDeviceInfo]"]
        ])
    );
    assert_eq!(
        r["voices"],
        serde_json::json!([22, ["Microsoft David - English (United States)"], true])
    );
    assert_eq!(r["tz"], "America/New_York");
    assert_eq!(r["locale"], "en-US");
    assert_eq!(r["offset"], serde_json::json!([300, 240]));
    assert_eq!(
        r["chrome"],
        serde_json::json!([
            1,
            ["loadTimes", "csi", "app"],
            "function () { [native code] }",
            "function getDetails() { [native code] }",
            false,
            "object",
            13,
            "true"
        ])
    );
    assert_eq!(
        r["screen"],
        serde_json::json!([
            1920,
            1080,
            1040,
            "landscape-primary",
            1,
            1920,
            1040,
            1920,
            937
        ])
    );
    assert_eq!(r["mm"], serde_json::json!([true, true, false, false, true]));
    assert_eq!(r["memory"][0], false);
    assert_eq!(r["memory"][1], 4_395_630_592u64);
    assert_eq!(r["memory"][2], true);
    assert_eq!(r["memory"][3], true);
    assert_eq!(
        r["memory"][4],
        serde_json::json!(["totalJSHeapSize", "usedJSHeapSize", "jsHeapSizeLimit"])
    );
    assert_eq!(
        r["memory"][5],
        "function get usedJSHeapSize() { [native code] }"
    );
    assert_eq!(r["perfTicks"], true);
    assert_eq!(r["timing"], serde_json::json!([true, 0]));
    let rtc = r["rtc"]
        .as_array()
        .unwrap_or_else(|| panic!("{}", r["rtc"]));
    assert_eq!(rtc.len(), 1, "{rtc:?}");
    assert_eq!(rtc[0][0], "host");
    assert!(rtc[0][1].as_str().unwrap().ends_with(".local"), "{rtc:?}");
    assert_eq!(r["focus"], serde_json::json!([true, "visible"]));

    // macOS reference persona: another zone, DPR 2, a P3 screen.
    let p = persona("chrome153_macos15_apple_m2_retina.json");
    let r = probe(&url, &p).await;
    assert_eq!(r["platform"], "MacIntel");
    assert_eq!(r["uaPlatform"], "macOS");
    assert_eq!(r["tz"], "America/Los_Angeles");
    assert_eq!(r["offset"], serde_json::json!([480, 420]));
    assert_eq!(r["screen"][4], 2);
    assert_eq!(r["mm"], serde_json::json!([true, true, true, false, true]));
    assert_eq!(r["voices"][1], serde_json::json!(["Samantha"]));
    assert_eq!(
        r["he"]["architecture"],
        p.client_hints.as_ref().unwrap().architecture.as_str()
    );
}

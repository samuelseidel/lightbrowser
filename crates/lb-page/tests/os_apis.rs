//! OS-dependent web APIs on real loopback pages (M7 WP-D): Web Share and
//! Web Bluetooth only for Windows and macOS personas, media type answers,
//! `navigator.keyboard`, `navigator.gpu`, `screenX/Y` and Web Audio
//! defaults per OS. Linux values were compared with the local Chrome for
//! Testing 153 (`tests/fixtures/os/`); Windows and macOS are the M7 surface
//! reference.
//!
//! One test drives the personas in turn: ICU's time zone is process-wide.
#![cfg(feature = "chrome")]

mod common;

use std::sync::Arc;

use common::{Resource, browser, open, serve};
use lb_js::JsValue;
use lb_page::{BrowserContext, WaitUntil, context_options_for};
use lb_persona::PersonaProfile;
use serde_json::{Value, json};

const PROBE: &str = r#"<!DOCTYPE html><body><script>
(async () => {
  const r = {};
  const n = navigator;
  const err = (p) => p.then(() => 'ok', (e) => e.name);
  r.apis = ['share' in n, 'canShare' in n, 'bluetooth' in n, typeof Bluetooth, typeof BluetoothUUID];
  r.navNames = Object.getOwnPropertyNames(Navigator.prototype);
  if (n.canShare) {
    r.canShare = [n.canShare(), n.canShare({}), n.canShare({ url: 'https://a.test/' }), n.canShare({ title: 'x' })];
    r.share = [await err(n.share({ title: 'x' })), await err(n.share({}))];
  }
  if (n.bluetooth) {
    r.bt = [await n.bluetooth.getAvailability(), await err(n.bluetooth.requestDevice({ acceptAllDevices: true })),
      Object.prototype.toString.call(n.bluetooth), n.bluetooth === n.bluetooth];
  }
  const km = await n.keyboard.getLayoutMap();
  r.kb = [km.size, km.get('KeyQ'), km.get('Semicolon'), Object.prototype.toString.call(km), [...km.keys()].length,
    await err(n.keyboard.lock())];
  const v = document.createElement('video');
  r.canPlay = [v.canPlayType('video/mp4; codecs="avc1.42E01E"'), v.canPlayType('video/mp4; codecs="hvc1.1.6.L93.B0"'),
    v.canPlayType('video/webm'), v.canPlayType('foo')];
  r.mse = [MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01E"'), MediaSource.isTypeSupported('audio/flac')];
  r.rec = [MediaRecorder.isTypeSupported('video/webm;codecs=vp9'), MediaRecorder.isTypeSupported('video/mp4;codecs=hvc1')];
  const di = await n.mediaCapabilities.decodingInfo({ type: 'file',
    video: { contentType: 'video/mp4; codecs="avc1.640028"', width: 1920, height: 1080, bitrate: 5e6, framerate: 30 } });
  r.di = [di.supported, di.smooth, di.powerEfficient, Object.keys(di).join()];
  r.diErr = [await err(n.mediaCapabilities.decodingInfo({})), await err(n.mediaCapabilities.decodingInfo())];
  const a = await n.gpu.requestAdapter();
  r.gpu = [n.gpu.getPreferredCanvasFormat(), n.gpu.wgslLanguageFeatures.size,
    a && a.info.vendor, a && a.info.architecture, a && a.limits.maxTextureDimension2D,
    a && a.features.has('texture-compression-bc'), a && a.info === a.info];
  r.screenXY = [screenX, screenY, screenLeft, screenTop];
  const ac = new AudioContext();
  r.audio = [ac.sampleRate, ac.baseLatency];
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

/// Members of `list` among `names`, in order.
fn pick<'a>(list: &'a Value, names: &[&str]) -> Vec<&'a str> {
    list.as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|n| names.contains(n))
        .collect()
}

#[tokio::test]
async fn os_apis_follow_the_persona() {
    let s = serve(vec![("/", Resource::html(PROBE))]).await;
    let url = s.url("/");

    // Windows 10: Web Share and Web Bluetooth, hardware decode, an adapter.
    let r = probe(&url, &persona("chrome153_win10_rtx3060_1080p.json")).await;
    assert_eq!(r["apis"], json!([true, true, true, "function", "function"]));
    assert_eq!(
        pick(
            &r["navNames"],
            &[
                "protectedAudience",
                "bluetooth",
                "clipboard",
                "canLoadAdAuctionFencedFrame",
                "canShare",
                "share",
                "clearAppBadge"
            ]
        ),
        [
            "protectedAudience",
            "bluetooth",
            "clipboard",
            "canLoadAdAuctionFencedFrame",
            "canShare",
            "share",
            "clearAppBadge"
        ]
    );
    assert_eq!(r["canShare"], json!([false, false, true, true]));
    // No user gesture: NotAllowedError; nothing to share: TypeError.
    assert_eq!(r["share"], json!(["NotAllowedError", "TypeError"]));
    assert_eq!(
        r["bt"],
        json!([true, "SecurityError", "[object Bluetooth]", true])
    );
    assert_eq!(
        r["kb"],
        json!([48, "q", ";", "[object KeyboardLayoutMap]", 48, "ok"])
    );
    assert_eq!(r["canPlay"], json!(["probably", "probably", "maybe", ""]));
    assert_eq!(r["mse"], json!([true, false]));
    assert_eq!(r["rec"], json!([true, false]));
    assert_eq!(
        r["di"],
        json!([
            true,
            true,
            true,
            "powerEfficient,smooth,supported,configuration,keySystemAccess"
        ])
    );
    assert_eq!(r["diErr"], json!(["TypeError", "TypeError"]));
    assert_eq!(
        r["gpu"],
        json!(["bgra8unorm", 4, "nvidia", "ampere", 16384, true, true])
    );
    assert_eq!(r["screenXY"], json!([0, 0, 0, 0]));
    assert_eq!(r["audio"], json!([48000, 0.01]));

    // Ubuntu 24.04 (captured): none of the gated APIs, no HEVC, no
    // adapter, rgba8unorm, the window at the work-area origin.
    let r = probe(
        &url,
        &persona("chrome153_ubuntu24_intel_uhd630_x11_1080p.json"),
    )
    .await;
    assert_eq!(
        r["apis"],
        json!([false, false, false, "undefined", "undefined"])
    );
    assert!(r.get("canShare").is_none() && r.get("bt").is_none());
    assert_eq!(r["canPlay"], json!(["probably", "", "maybe", ""]));
    assert_eq!(
        r["di"],
        json!([
            true,
            true,
            false,
            "powerEfficient,smooth,supported,configuration,keySystemAccess"
        ])
    );
    assert_eq!(
        r["gpu"],
        json!(["rgba8unorm", 4, null, null, null, null, null])
    );
    assert_eq!(r["kb"][0], json!(48));
    assert_eq!(r["screenXY"], json!([64, 32, 64, 32]));
    assert_eq!(r["audio"], json!([48000, 0.01]));

    // Fedora 42 on Wayland: screenX/Y are 0.
    let r = probe(
        &url,
        &persona("chrome153_fedora42_amd_rx6600_wayland_1440p.json"),
    )
    .await;
    assert_eq!(r["screenXY"], json!([0, 0, 0, 0]));
    assert_eq!(r["apis"][0], json!(false));

    // macOS 15 on Apple M2: share and Bluetooth, an Apple adapter, the
    // window below the menu bar, 44.1 kHz.
    let r = probe(&url, &persona("chrome153_macos15_apple_m2_retina.json")).await;
    assert_eq!(r["apis"], json!([true, true, true, "function", "function"]));
    assert_eq!(r["gpu"][0], json!("bgra8unorm"));
    assert_eq!(r["gpu"][2], json!("apple"));
    assert_eq!(r["gpu"][5], json!(true));
    assert_eq!(r["screenXY"][1], json!(37));
    assert_eq!(r["audio"], json!([44100, 0.01]));
}

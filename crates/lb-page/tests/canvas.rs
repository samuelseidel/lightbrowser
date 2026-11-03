//! Canvas 2D and image loading through real pages on a loopback server:
//! seeded readback noise is stable per persona seed, and `<img>` loads go
//! through the network with load/error events, sizes and `drawImage`.
#![cfg(feature = "chrome")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Resource, browser, context, open, serve};
use lb_js::JsValue;
use lb_net::ClientHelloProfile;
use lb_page::{BrowserContext, ContextOptions, WaitUntil};
use lb_persona::{Persona, PersonaProfile};

/// A 3x2 RGBA PNG: red, green, blue / yellow, cyan, half-transparent magenta.
const PNG: [u8; 83] = [
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 3, 0, 0, 0, 2, 8, 6, 0,
    0, 0, 157, 116, 102, 26, 0, 0, 0, 26, 73, 68, 65, 84, 120, 156, 99, 249, 207, 192, 240, 159,
    17, 72, 48, 0, 9, 22, 6, 16, 3, 72, 0, 169, 70, 0, 103, 245, 7, 133, 143, 206, 47, 66, 0, 0, 0,
    0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// The detect-bench canvas scene plus a few more primitives; returns an
/// FNV-1a hash of `toDataURL()`, the solid-fill check and a pixel row.
const SCENE: &str = r#"<!DOCTYPE html><body><script>
function fnv(s) { let h = 0x811c9dc5; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619) >>> 0; } return h.toString(16); }
function scene() {
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
  x.globalCompositeOperation = 'multiply';
  x.strokeStyle = 'rgb(255,0,255)';
  x.lineWidth = 3;
  x.setLineDash([4, 2]);
  x.strokeRect(60, 30, 50, 20);
  return c;
}
const a = scene(), b = scene();
const stable = a.toDataURL() === b.toDataURL();
const s = document.createElement('canvas');
s.width = s.height = 4;
const sx = s.getContext('2d');
sx.fillStyle = 'rgb(10,20,30)';
sx.fillRect(0, 0, 4, 4);
const d = sx.getImageData(0, 0, 4, 4).data;
let solid = true;
for (let i = 0; i < d.length; i += 4) if (d[i] !== 10 || d[i+1] !== 20 || d[i+2] !== 30 || d[i+3] !== 255) solid = false;
// Readback is idempotent on opaque pixels: put a noised read back and
// read it again.
const o = document.createElement('canvas');
o.width = 120; o.height = 40;
const ox = o.getContext('2d');
ox.fillStyle = '#fff'; ox.fillRect(0, 0, 120, 40);
ox.drawImage(a, 0, 0);
const r1 = ox.getImageData(0, 0, 120, 40);
ox.putImageData(r1, 0, 0);
const r2 = ox.getImageData(0, 0, 120, 40);
let same = true;
for (let i = 0; i < r1.data.length; i++) if (r1.data[i] !== r2.data[i]) { same = false; break; }
const ax = a.getContext('2d');
window.result = JSON.stringify({
  hash: fnv(scene().toDataURL()), stable, solid, same,
  type: Object.prototype.toString.call(r1.data), w: ax.getImageData(0, 0, 240, 1).width,
  m: ax.measureText('Hello').width,
});
</script></body>"#;

fn persona_ctx(seed: Option<&str>) -> BrowserContext {
    let persona = Arc::new(Persona::default_chrome());
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../profiles");
    let tls = Arc::new(
        ClientHelloProfile::load(std::path::Path::new(&format!(
            "{dir}/tls/{}.json",
            persona.tls_profile_id
        )))
        .unwrap(),
    );
    let profile = seed.map(|s| {
        let mut p = PersonaProfile::from_path(format!(
            "{dir}/reference/chrome/chrome153_win10_rtx3060_1080p.json"
        ))
        .unwrap();
        p.fingerprint_seeds.canvas_seed = s.to_owned();
        Arc::new(p)
    });
    BrowserContext::new(ContextOptions {
        persona,
        profile,
        tls_profile: tls,
        accept_invalid_certs: false,
        virtual_time: true,
    })
    .unwrap()
}

async fn scene_result(url: &str, seed: Option<&str>) -> serde_json::Value {
    let b = browser();
    let ctx = persona_ctx(seed);
    let page = open(&b, &ctx, url, WaitUntil::Load).await;
    match page.evaluate("window.result").await.unwrap() {
        JsValue::String(s) => serde_json::from_str(&s).unwrap(),
        v => panic!("{v:?}"),
    }
}

#[tokio::test]
async fn canvas_hash_is_stable_per_seed() {
    let s = serve(vec![("/", Resource::html(SCENE))]).await;
    let url = s.url("/");
    const A: &str = "0123456789abcdef0123456789abcdef";
    const B: &str = "fedcba9876543210fedcba9876543210";
    let a1 = scene_result(&url, Some(A)).await;
    let a2 = scene_result(&url, Some(A)).await;
    let b1 = scene_result(&url, Some(B)).await;
    let plain = scene_result(&url, None).await;
    for r in [&a1, &a2, &b1, &plain] {
        assert_eq!(r["stable"], true, "{r}");
        assert_eq!(r["solid"], true, "{r}");
        assert_eq!(r["same"], true, "{r}");
        assert_eq!(r["type"], "[object Uint8ClampedArray]", "{r}");
        assert_eq!(r["w"], 240, "{r}");
    }
    assert_eq!(a1["hash"], a2["hash"], "same seed, same output");
    assert_ne!(a1["hash"], b1["hash"], "different seed, different output");
    assert_ne!(a1["hash"], plain["hash"], "noise applied");
    // Arial 14px "Hello": (1479 + 1139 + 455 * 2 + 1139) * 14 / 2048.
    let m = a1["m"].as_f64().unwrap();
    assert!((m - 31.9).abs() < 0.1, "{m}");
    eprintln!(
        "hashes: a={} b={} none={}",
        a1["hash"], b1["hash"], plain["hash"]
    );
}

#[tokio::test]
async fn images_load_over_the_network() {
    let page_html = r#"<!DOCTYPE html><body>
<img id=a src="/i.png">
<img id=s src="/p.svg" alt="svg">
<img id=bad src="/missing.png">
<img id=lazy loading=lazy src="/far.png" style="margin-top: 20000px">
<script>
const a = document.getElementById('a'), s = document.getElementById('s'), bad = document.getElementById('bad');
window.ev = [];
for (const id of ['a', 's', 'bad']) {
  const el = document.getElementById(id);
  el.onload = () => ev.push(id + ':load');
  el.onerror = () => ev.push(id + ':error');
}
const dyn = new Image();
dyn.onload = () => {
  const c = document.createElement('canvas');
  c.width = 6; c.height = 4;
  const x = c.getContext('2d');
  x.drawImage(dyn, 0, 0, 6, 4);
  window.px = Array.from(x.getImageData(0, 0, 1, 1).data).join(',');
  window.dynDone = dyn.complete + ' ' + dyn.naturalWidth + 'x' + dyn.naturalHeight;
};
dyn.src = '/i.png?dyn';
window.before = dyn.complete;
const data = new Image();
data.src = 'data:image/svg+xml,' + encodeURIComponent("<svg xmlns='http://www.w3.org/2000/svg' width='7' height='9'/>");
data.decode().then(() => { window.dataSize = data.naturalWidth + 'x' + data.naturalHeight; });
document.getElementById('bad').decode().catch(e => { window.decodeErr = e.name; });
</script></body>"#;
    let s = serve(vec![
        ("/", Resource::html(page_html)),
        ("/i.png", Resource::new("image/png", PNG.to_vec())),
        (
            "/p.svg",
            Resource::new(
                "image/svg+xml",
                "<svg xmlns='http://www.w3.org/2000/svg' width='320' height='220'><rect width='320' height='220'/></svg>",
            ),
        ),
        ("/far.png", Resource::new("image/png", PNG.to_vec())),
    ])
    .await;
    let (b, ctx) = (browser(), context(true));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let get = |e: &'static str| {
        let page = &page;
        async move {
            match page.evaluate(e).await.unwrap() {
                JsValue::String(s) => s,
                v => format!("{v:?}"),
            }
        }
    };
    let ev = get("ev.sort().join()").await;
    assert_eq!(ev, "a:load,bad:error,s:load");
    assert_eq!(
        get("[a.complete, a.naturalWidth, a.naturalHeight, a.width, a.height, a.currentSrc.endsWith('/i.png')].join()").await,
        "true,3,2,3,2,true"
    );
    assert_eq!(
        get("[s.naturalWidth, s.naturalHeight].join()").await,
        "320,220"
    );
    assert_eq!(
        get("[bad.complete, bad.naturalWidth].join()").await,
        "true,0"
    );
    assert_eq!(get("String(before)").await, "false");
    assert_eq!(get("dynDone").await, "true 3x2");
    // Top-left pixel of the scaled image is red (nearest or bilinear).
    assert_eq!(get("px").await, "255,0,0,255");
    assert_eq!(get("dataSize").await, "7x9");
    assert_eq!(get("decodeErr").await, "EncodingError");
    // Layout uses the natural size.
    assert_eq!(get("String(a.getBoundingClientRect().width)").await, "3");
    // The lazy image far below the viewport was not requested.
    assert_eq!(
        get("String(document.getElementById('lazy').complete)").await,
        "false"
    );
    // Drawing a broken image throws.
    assert_eq!(
        get("(() => { try { document.createElement('canvas').getContext('2d').drawImage(bad, 0, 0); return 'no'; } catch (e) { return e.name; } })()").await,
        "InvalidStateError"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    let reqs: Vec<String> = s.requests().into_iter().map(|r| r.1).collect();
    assert!(reqs.iter().any(|r| r == "/i.png"), "{reqs:?}");
    assert!(reqs.iter().any(|r| r == "/i.png?dyn"), "{reqs:?}");
    assert!(reqs.iter().any(|r| r == "/missing.png"), "{reqs:?}");
    assert!(!reqs.iter().any(|r| r == "/far.png"), "{reqs:?}");
    let img_req = s.requests().into_iter().find(|r| r.1 == "/i.png").unwrap();
    let dest = img_req
        .2
        .iter()
        .find(|(k, _)| k == "sec-fetch-dest")
        .map(|(_, v)| v.clone());
    assert_eq!(dest.as_deref(), Some("image"), "{:?}", img_req.2);
}

#[tokio::test]
async fn canvas_api_surface() {
    let s = serve(vec![(
        "/",
        Resource::html(
            r#"<body><canvas id=c></canvas><script>
const c = document.getElementById('c');
const x = c.getContext('2d');
const out = [];
out.push(c.getContext('2d') === x, c.getContext('webgl') === null, x.canvas === c);
out.push(x.fillStyle, x.font, x.textAlign, x.globalCompositeOperation, x.lineWidth);
x.fillStyle = 'nope'; out.push(x.fillStyle);
x.fillStyle = 'rgba(0,0,255,0.5)'; out.push(x.fillStyle);
const g = x.createLinearGradient(0, 0, 1, 1); x.fillStyle = g; out.push(x.fillStyle === g);
x.font = 'bold 12px "Times New Roman", serif'; out.push(x.font);
x.setLineDash([1, 2, 3]); out.push(x.getLineDash().join(' '));
x.translate(5, 6); x.scale(2, 3); const m = x.getTransform(); out.push(m.a, m.d, m.e, m.f, m.is2D, String(m));
try { x.arc(0, 0, -1, 0, 1); } catch (e) { out.push(e.name); }
try { g.addColorStop(2, 'red'); } catch (e) { out.push(e.name); }
try { x.getImageData(0, 0, 0, 1); } catch (e) { out.push(e.name); }
c.width = 10; out.push(x.getTransform().e, x.fillStyle, c.toDataURL().slice(0, 22));
out.push(c.toDataURL('image/jpeg').slice(0, 23), c.toDataURL('image/webp').slice(0, 22));
const p = new Path2D('M0 0 L10 0 L10 10 Z'); out.push(x.isPointInPath(p, 8, 2), x.isPointInPath(p, 2, 8));
const id = new ImageData(2, 3); out.push(id.width, id.height, id.data.length);
const tm = x.measureText('abc'); out.push(tm.width > 0, tm.fontBoundingBoxAscent > 0);
const oc = new OffscreenCanvas(8, 8); const ox = oc.getContext('2d'); ox.fillRect(0, 0, 8, 8);
out.push(Array.from(ox.getImageData(1, 1, 1, 1).data).join(' '));
c.toBlob(b => { window.blob = b.type + ' ' + (b.size > 0); });
oc.convertToBlob().then(b => { window.oblob = b.type; });
window.result = out.join('|');
</script></body>"#,
        ),
    )])
    .await;
    let (b, ctx) = (browser(), context(true));
    let page = open(&b, &ctx, &s.url("/"), WaitUntil::Load).await;
    let get = |e: &'static str| {
        let page = &page;
        async move {
            match page.evaluate(e).await.unwrap() {
                JsValue::String(s) => s,
                v => format!("{v:?}"),
            }
        }
    };
    assert_eq!(
        get("result").await,
        "true|true|true|#000000|10px sans-serif|start|source-over|1|#000000|rgba(0, 0, 255, 0.5)|true|\
bold 12px \"Times New Roman\", serif|1 2 3 1 2 3|2|3|5|6|true|matrix(2, 0, 0, 3, 5, 6)|\
IndexSizeError|IndexSizeError|IndexSizeError|0|#000000|data:image/png;base64,|\
data:image/jpeg;base64,|data:image/webp;base64|true|false|2|3|24|true|true|0 0 0 255"
    );
    assert_eq!(get("blob").await, "image/png true");
    assert_eq!(get("oblob").await, "image/png");
}

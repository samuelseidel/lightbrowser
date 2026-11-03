//! WebGL, Web Audio and font surfaces through real pages on a loopback
//! server: answers come from the persona, and canvas, WebGL and audio
//! fingerprints are stable per seed and change with it.
#![cfg(feature = "chrome")]

mod common;

use std::sync::Arc;

use common::{Resource, browser, open, serve};
use lb_js::JsValue;
use lb_net::ClientHelloProfile;
use lb_page::{BrowserContext, ContextOptions, WaitUntil};
use lb_persona::{Persona, PersonaProfile};

/// Canvas, WebGL and audio fingerprints as the common scripts compute
/// them, plus surface checks. Sets `window.result` (JSON) when done.
const PAGE: &str = r#"<!DOCTYPE html><body><script>
function fnv(s) { let h = 0x811c9dc5; for (let i = 0; i < s.length; i++) { h ^= s.charCodeAt(i); h = Math.imul(h, 16777619) >>> 0; } return h.toString(16); }
const R = {};
// Canvas 2D.
{
  const c = document.createElement('canvas'); c.width = 200; c.height = 40;
  const x = c.getContext('2d');
  x.font = '16px Arial'; x.fillStyle = '#069'; x.fillText('Cwm fjordbank glyphs', 4, 24);
  x.fillStyle = 'rgba(102,204,0,0.7)'; x.fillRect(120, 5, 40, 20);
  R.canvas = fnv(c.toDataURL());
}
// WebGL.
function glScene(gl) {
  const vs = gl.createShader(gl.VERTEX_SHADER);
  gl.shaderSource(vs, 'attribute vec2 p; uniform vec2 off; void main(){ gl_Position = vec4(p + off, 0.0, 1.0); }');
  gl.compileShader(vs);
  const fs = gl.createShader(gl.FRAGMENT_SHADER);
  gl.shaderSource(fs, 'precision mediump float; void main(){ gl_FragColor = vec4(0.8, 0.2, 0.4, 1.0); }');
  gl.compileShader(fs);
  const pr = gl.createProgram(); gl.attachShader(pr, vs); gl.attachShader(pr, fs); gl.linkProgram(pr); gl.useProgram(pr);
  const b = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, b);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-0.8, -0.7, 0.9, -0.5, 0.1, 0.85]), gl.STATIC_DRAW);
  const loc = gl.getAttribLocation(pr, 'p');
  gl.enableVertexAttribArray(loc); gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
  gl.clearColor(0.1, 0.2, 0.3, 1); gl.clear(gl.COLOR_BUFFER_BIT);
  gl.drawArrays(gl.TRIANGLES, 0, 3);
  return { pr, loc, uoff: gl.getUniformLocation(pr, 'off') };
}
{
  const c = document.createElement('canvas'); c.width = 64; c.height = 64;
  const gl = c.getContext('webgl');
  const s = glScene(gl);
  const px = new Uint8Array(64 * 64 * 4);
  gl.readPixels(0, 0, 64, 64, gl.RGBA, gl.UNSIGNED_BYTE, px);
  const one = new Uint8Array(4);
  gl.readPixels(32, 30, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, one);
  const corner = new Uint8Array(4);
  gl.readPixels(0, 0, 1, 1, gl.RGBA, gl.UNSIGNED_BYTE, corner);
  const masked = gl.getParameter(0x9246), maskedErr = gl.getError();
  const dbg = gl.getExtension('WEBGL_debug_renderer_info');
  const hp = gl.getShaderPrecisionFormat(gl.FRAGMENT_SHADER, gl.HIGH_FLOAT);
  // toDataURL agrees with readPixels: copy into a 2D canvas and compare.
  const c2 = document.createElement('canvas'); c2.width = 64; c2.height = 64;
  const x2 = c2.getContext('2d'); x2.drawImage(c, 0, 0);
  const id = x2.getImageData(0, 0, 64, 64).data;
  // The 2D readback adds its own canvas noise (at most 1 per channel).
  let maxDiff = 0;
  for (let y = 0; y < 64; y++) for (let xx = 0; xx < 64; xx++) {
    const a = ((63 - y) * 64 + xx) * 4, b = (y * 64 + xx) * 4;
    for (let k = 0; k < 4; k++) maxDiff = Math.max(maxDiff, Math.abs(px[a + k] - id[b + k]));
  }
  const agree = maxDiff <= 1;
  R.gl = {
    hash: fnv(Array.from(px).join(',')), url: fnv(c.toDataURL()), agree,
    center: Array.from(one).join(','), corner: Array.from(corner).join(','),
    vendor: gl.getParameter(gl.VENDOR), renderer: gl.getParameter(gl.RENDERER),
    version: gl.getParameter(gl.VERSION), glsl: gl.getParameter(gl.SHADING_LANGUAGE_VERSION),
    maskedBefore: masked, maskedErr,
    uvendor: gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL), urenderer: gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL),
    dbgTag: Object.prototype.toString.call(dbg), dbgSame: gl.getExtension('webgl_debug_renderer_info') === dbg,
    maxTex: gl.getParameter(gl.MAX_TEXTURE_SIZE), dims: Array.from(gl.getParameter(gl.MAX_VIEWPORT_DIMS)).join(','),
    dimsType: Object.prototype.toString.call(gl.getParameter(gl.MAX_VIEWPORT_DIMS)),
    points: Array.from(gl.getParameter(gl.ALIASED_POINT_SIZE_RANGE)).join(','),
    vuv: gl.getParameter(gl.MAX_VERTEX_UNIFORM_VECTORS), varying: gl.getParameter(gl.MAX_VARYING_VECTORS),
    exts: gl.getSupportedExtensions(), prec: [hp.rangeMin, hp.rangeMax, hp.precision].join(','),
    attrs: JSON.stringify(gl.getContextAttributes()), lost: gl.isContextLost(), err: gl.getError(),
    link: gl.getProgramParameter(s.pr, gl.LINK_STATUS), loc: s.loc,
    uloc: Object.prototype.toString.call(s.uoff), nouloc: gl.getUniformLocation(s.pr, 'nope'),
    active: gl.getProgramParameter(s.pr, gl.ACTIVE_UNIFORMS),
    constProto: WebGLRenderingContext.prototype.COLOR_BUFFER_BIT, constIface: WebGLRenderingContext.DEPTH_TEST,
    tag: Object.prototype.toString.call(gl), bad: gl.getParameter(0x8073), badErr: gl.getError(),
  };
  const g2 = document.createElement('canvas').getContext('webgl2');
  R.gl2 = {
    version: g2.getParameter(g2.VERSION), glsl: g2.getParameter(g2.SHADING_LANGUAGE_VERSION),
    tex3d: g2.getParameter(g2.MAX_3D_TEXTURE_SIZE), varyingComps: g2.getParameter(g2.MAX_VARYING_COMPONENTS),
    exts: g2.getSupportedExtensions().length, tag: Object.prototype.toString.call(g2),
  };
}
// Fonts by measurement.
{
  const span = document.createElement('span');
  span.style.cssText = 'position:absolute;left:-9999px;top:0;font-size:72px;white-space:nowrap';
  span.textContent = 'mmmmmmmmmmlli1WQ@#';
  document.body.appendChild(span);
  const w = (f) => { span.style.fontFamily = f; return span.offsetWidth + 'x' + span.offsetHeight; };
  const cx = document.createElement('canvas').getContext('2d');
  const m = (f) => { cx.font = '72px ' + f; return cx.measureText('mmmmmmmmmmlli1WQ@#').width; };
  R.fonts = {
    mono: w('monospace'), consolas: w('"Consolas", monospace'), ubuntu: w('"Ubuntu", monospace'),
    menlo: w('"Menlo", monospace'), segoe: w('"Segoe UI", serif'), serif: w('serif'),
    cMono: m('monospace'), cConsolas: m('Consolas, monospace'), cUbuntu: m('Ubuntu, monospace'),
    size: document.fonts.size, status: document.fonts.status,
    check: document.fonts.check('12px Arial'), check2: document.fonts.check('12px NoSuchFontX'),
    tag: Object.prototype.toString.call(document.fonts), qlf: typeof window.queryLocalFonts,
  };
  span.remove();
}
// Audio.
async function audioOnce() {
  const ac = new OfflineAudioContext(1, 5000, 44100);
  const osc = ac.createOscillator(); osc.type = 'triangle'; osc.frequency.value = 10000;
  const comp = ac.createDynamicsCompressor();
  comp.threshold.value = -50; comp.knee.value = 40; comp.ratio.value = 12; comp.attack.value = 0; comp.release.value = 0.25;
  osc.connect(comp); comp.connect(ac.destination); osc.start(0);
  const buf = await ac.startRendering();
  const d = buf.getChannelData(0);
  let s = 0; for (let i = 4500; i < 5000; i++) s += Math.abs(d[i]);
  return { s, same: buf.getChannelData(0) === d, reduction: comp.reduction, zero: d[100] };
}
(async () => {
  const a = await audioOnce(), b = await audioOnce();
  const rt = new AudioContext();
  const an = rt.createAnalyser();
  const fd = new Float32Array(an.frequencyBinCount); an.getFloatFrequencyData(fd);
  let completed = false;
  const oc = new OfflineAudioContext(1, 128, 44100);
  oc.oncomplete = (e) => { completed = e.renderedBuffer.length === 128; };
  await oc.startRendering();
  await new Promise(r => setTimeout(r, 0));
  R.audio = {
    sum: a.s, stable: a.s === b.s, same: a.same, reduction: a.reduction, zero: a.zero,
    sr: rt.sampleRate, bl: rt.baseLatency, state: rt.state, mc: rt.destination.maxChannelCount,
    attack: new OfflineAudioContext(1, 1, 44100).createDynamicsCompressor().attack.value,
    bins: an.frequencyBinCount, silent: fd[3], completed,
    tag: Object.prototype.toString.call(a),
  };
  const ff = new FontFace('X', 'local("Consolas")');
  const ff2 = new FontFace('Y', 'local("Menlo")');
  await ff.load();
  let err = '';
  try { await ff2.load(); } catch (e) { err = e.name; }
  R.fonts.face = ff.status + ',' + ff2.status + ',' + err + ',' + ff.unicodeRange;
  try { await window.queryLocalFonts(); } catch (e) { R.fonts.qlfErr = e.name + ': ' + e.message; }
  window.result = JSON.stringify(R);
})();
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
    let mut p = PersonaProfile::from_path(format!(
        "{dir}/reference/chrome/chrome153_win10_rtx3060_1080p.json"
    ))
    .unwrap();
    if let Some(s) = seed {
        p.fingerprint_seeds.canvas_seed = s.to_owned();
        p.fingerprint_seeds.webgl_noise_seed = s.chars().rev().collect();
        p.fingerprint_seeds.audio_seed = format!("{}{}", &s[16..], &s[..16]);
    }
    BrowserContext::new(ContextOptions {
        persona,
        profile: Some(Arc::new(p)),
        tls_profile: tls,
        accept_invalid_certs: false,
        virtual_time: true,
    })
    .unwrap()
}

async fn run(url: &str, seed: Option<&str>) -> serde_json::Value {
    let b = browser();
    let ctx = persona_ctx(seed);
    let page = open(&b, &ctx, url, WaitUntil::Load).await;
    for _ in 0..100 {
        if let JsValue::String(s) = page.evaluate("window.result").await.unwrap() {
            return serde_json::from_str(&s).unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("no result");
}

#[tokio::test]
async fn surfaces_follow_the_persona() {
    let s = serve(vec![("/", Resource::html(PAGE))]).await;
    let r = run(&s.url("/"), None).await;
    let gl = &r["gl"];
    assert_eq!(gl["vendor"], "WebKit", "{gl}");
    assert_eq!(gl["renderer"], "WebKit WebGL");
    assert_eq!(gl["version"], "WebGL 1.0 (OpenGL ES 2.0 Chromium)");
    assert_eq!(
        gl["glsl"],
        "WebGL GLSL ES 1.0 (OpenGL ES GLSL ES 1.0 Chromium)"
    );
    assert_eq!(
        gl["maskedBefore"],
        serde_json::Value::Null,
        "needs the extension"
    );
    assert_eq!(gl["maskedErr"], 0x500);
    assert_eq!(gl["uvendor"], "Google Inc. (NVIDIA)");
    assert_eq!(
        gl["urenderer"],
        "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 (0x00002504) Direct3D11 vs_5_0 ps_5_0, D3D11)"
    );
    assert_eq!(gl["dbgTag"], "[object WebGLDebugRendererInfo]");
    assert_eq!(gl["dbgSame"], true);
    assert_eq!(gl["maxTex"], 16384);
    assert_eq!(gl["dims"], "32767,32767");
    assert_eq!(gl["dimsType"], "[object Int32Array]");
    assert_eq!(gl["points"], "1,1024");
    assert_eq!(gl["vuv"], 4095);
    assert_eq!(gl["varying"], 30);
    assert_eq!(gl["prec"], "127,127,23");
    assert_eq!(gl["lost"], false);
    assert_eq!(gl["err"], 0);
    assert_eq!(gl["link"], true);
    assert_eq!(gl["loc"], 0);
    assert_eq!(gl["uloc"], "[object WebGLUniformLocation]");
    assert_eq!(gl["nouloc"], serde_json::Value::Null);
    assert_eq!(gl["active"], 1);
    assert_eq!(gl["constProto"], 16384);
    assert_eq!(gl["constIface"], 2929);
    assert_eq!(gl["tag"], "[object WebGLRenderingContext]");
    assert_eq!(gl["bad"], serde_json::Value::Null);
    assert_eq!(gl["badErr"], 0x500);
    assert!(
        gl["attrs"]
            .as_str()
            .unwrap()
            .starts_with("{\"alpha\":true,\"antialias\":true,\"depth\":true")
    );
    let exts: Vec<&str> = gl["exts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(exts.contains(&"ANGLE_instanced_arrays"), "{exts:?}");
    assert!(exts.contains(&"WEBGL_debug_renderer_info"));
    assert!(!exts.contains(&"EXT_color_buffer_float"), "WebGL 2 only");
    // Inside the triangle: exact fill; outside: the clear color.
    assert_eq!(gl["center"], "204,51,102,255", "{gl}");
    assert_eq!(gl["corner"], "26,51,77,255");
    assert_eq!(gl["agree"], true, "toDataURL matches readPixels");
    let g2 = &r["gl2"];
    assert_eq!(g2["version"], "WebGL 2.0 (OpenGL ES 3.0 Chromium)");
    assert_eq!(
        g2["glsl"],
        "WebGL GLSL ES 3.00 (OpenGL ES GLSL ES 3.0 Chromium)"
    );
    assert_eq!(g2["tex3d"], 2048);
    assert_eq!(g2["varyingComps"], 120);
    assert_eq!(g2["exts"], 31);
    assert_eq!(g2["tag"], "[object WebGL2RenderingContext]");

    let f = &r["fonts"];
    assert_ne!(
        f["consolas"], f["mono"],
        "installed font measures differently: {f}"
    );
    assert_eq!(
        f["ubuntu"], f["mono"],
        "missing font measures as the fallback"
    );
    assert_eq!(
        f["menlo"], f["mono"],
        "macOS font is not on a Windows persona"
    );
    assert_ne!(f["segoe"], f["serif"]);
    assert_ne!(f["cConsolas"], f["cMono"], "canvas agrees with layout");
    assert_eq!(f["cUbuntu"], f["cMono"]);
    assert_eq!(f["size"], 0);
    assert_eq!(f["status"], "loaded");
    assert_eq!(f["check"], true);
    assert_eq!(f["check2"], true);
    assert_eq!(f["tag"], "[object FontFaceSet]");
    assert_eq!(f["qlf"], "function");
    assert_eq!(f["face"], "loaded,error,NetworkError,U+0-10FFFF");
    assert_eq!(f["qlfErr"], "SecurityError: User activation is required.");

    let a = &r["audio"];
    let sum = a["sum"].as_f64().unwrap();
    assert!((sum - 124.0434).abs() < 0.01, "{a}");
    assert_eq!(a["stable"], true);
    assert_eq!(a["same"], true);
    assert_eq!(a["zero"], 0, "silence before the pre-delay stays exact");
    assert!(
        (a["reduction"].as_f64().unwrap() + 20.538).abs() < 0.01,
        "{a}"
    );
    assert_eq!(a["sr"], 48000);
    assert_eq!(a["bl"], 0.01);
    assert_eq!(a["state"], "suspended");
    assert_eq!(a["mc"], 2);
    assert_eq!(a["attack"], 0.003000000026077032);
    assert_eq!(a["bins"], 1024);
    assert_eq!(a["completed"], true);
}

#[tokio::test]
async fn fingerprints_are_stable_per_seed() {
    let s = serve(vec![("/", Resource::html(PAGE))]).await;
    let url = s.url("/");
    const A: &str = "0123456789abcdef0123456789abcdef";
    const B: &str = "fedcba9876543210fedcba9876543210";
    let a1 = run(&url, Some(A)).await;
    let a2 = run(&url, Some(A)).await;
    let b1 = run(&url, Some(B)).await;
    let keys = |r: &serde_json::Value| {
        (
            r["canvas"].clone(),
            r["gl"]["hash"].clone(),
            r["gl"]["url"].clone(),
            r["audio"]["sum"].clone(),
        )
    };
    let (ka, kb) = (keys(&a1), keys(&a2));
    assert_eq!(ka, kb, "same seed, same canvas/WebGL/audio output");
    let kc = keys(&b1);
    assert_ne!(ka.0, kc.0, "canvas changes with the seed");
    assert_ne!(ka.1, kc.1, "WebGL readPixels changes with the seed");
    assert_ne!(ka.2, kc.2, "WebGL toDataURL changes with the seed");
    assert_ne!(ka.3, kc.3, "audio changes with the seed");
    eprintln!("seed A: {ka:?}\nseed B: {kc:?}");
}
